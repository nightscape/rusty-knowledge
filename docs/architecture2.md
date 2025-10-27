# Architecture v2: Type-Safe Generic Data Management

## Core Philosophy

This architecture emphasizes:
1. **Type safety in typed land**: Use generic `T` everywhere we work with concrete types
2. **Generic operations when needed**: Switch to dynamic view only for operations that work on arbitrary types
3. **Unified abstractions**: Same patterns for internal (Loro) and external (API) data sources
4. **Zero hardcoding**: No hardcoded entity names, field names, or SQL queries

## Key Abstractions

### 1. DataSource - Universal Interface

Both internal CRDTs and external APIs are `DataSource<T>`. We rely on the [`async-trait`](https://docs.rs/async-trait) crate so that the trait can expose `async fn` without manually writing associated future types:

```rust
/// Common interface for data sources (queryable or not)
#[async_trait]
trait DataSource<T>: Send + Sync
where
    T: Send + Sync + 'static,
{
    /// Fetch all items (may be expensive)
    async fn get_all(&self) -> Result<Vec<T>>;

    /// Fetch single item by ID
    async fn get_by_id(&self, id: &str) -> Result<Option<T>>;

    /// Create new item, returns generated ID
    async fn insert(&mut self, item: T) -> Result<String>;

    /// Update existing item
    async fn update(&mut self, id: &str, updates: &Updates<T>) -> Result<()>;

    /// Delete item
    async fn delete(&mut self, id: &str) -> Result<()>;

    /// Metadata
    fn source_name(&self) -> &str;
}

/// Typed field-level updates keyed by lenses
struct Updates<T> {
    changes: Vec<FieldChange>,
    _phantom: PhantomData<T>,
}

impl<T> Updates<T> {
    fn new() -> Self {
        Self {
            changes: Vec::new(),
            _phantom: PhantomData,
        }
    }

    fn set<L, U>(&mut self, lens: L, value: U)
    where
        L: Lens<T, U> + Copy + 'static,
        U: Into<Value>,
    {
        self.changes.push(FieldChange {
            field_name: lens.field_name(),
            sql_column: lens.sql_column(),
            update: FieldUpdate::Set(value.into()),
        });
    }

    fn clear<L, U>(&mut self, lens: L)
    where
        L: Lens<T, U> + Copy + 'static,
    {
        self.changes.push(FieldChange {
            field_name: lens.field_name(),
            sql_column: lens.sql_column(),
            update: FieldUpdate::Clear,
        });
    }

    fn iter(&self) -> impl Iterator<Item = &FieldChange> {
        self.changes.iter()
    }

    fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

struct FieldChange {
    field_name: &'static str,
    sql_column: Option<&'static str>,
    update: FieldUpdate,
}

#[derive(Clone)]
enum FieldUpdate {
    Clear,
    Set(Value),
}
```

Each `FieldChange` keeps the lens-derived metadata beside the persisted value so callers never have to reach for raw strings. Data sources iterate over `updates.iter()` to build provider payloads, while `QueryableCache` leans on the optional `sql_column` when generating SQL.

Supporting definitions used throughout the examples:

```rust
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{
    sqlite::{SqliteArguments, SqliteRow},
    types::Json,
    Query, Row, Sqlite, SqlitePool,
};
use std::{collections::HashMap, marker::PhantomData, sync::Arc};

type Result<T> = anyhow::Result<T>;

#[derive(Clone, Debug)]
enum Value {
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    DateTime(DateTime<Utc>),
    Json(serde_json::Value),
    Null,
}

impl Value {
    fn bind<'q>(
        self,
        mut query: Query<'q, Sqlite, SqliteArguments<'q>>,
    ) -> Query<'q, Sqlite, SqliteArguments<'q>> {
        match self {
            Value::String(v) => query.bind(v),
            Value::Integer(v) => query.bind(v),
            Value::Float(v) => query.bind(v),
            Value::Boolean(v) => query.bind(v),
            Value::DateTime(v) => query.bind(v.to_rfc3339()),
            Value::Json(v) => query.bind(Json(v)),
            Value::Null => query.bind(Option::<i64>::None),
        }
    }

    fn as_string(&self) -> Result<&String> {
        match self {
            Value::String(value) => Ok(value),
            Value::Null => Err(anyhow::anyhow!("expected string, found null")),
            other => Err(anyhow::anyhow!("expected string, found {:?}", other)),
        }
    }

    fn as_integer(&self) -> Result<i64> {
        match self {
            Value::Integer(value) => Ok(*value),
            Value::Null => Err(anyhow::anyhow!("expected integer, found null")),
            other => Err(anyhow::anyhow!("expected integer, found {:?}", other)),
        }
    }

    fn as_bool(&self) -> Result<bool> {
        match self {
            Value::Boolean(value) => Ok(*value),
            Value::Null => Err(anyhow::anyhow!("expected boolean, found null")),
            other => Err(anyhow::anyhow!("expected boolean, found {:?}", other)),
        }
    }

    fn as_datetime(&self) -> Result<Option<&DateTime<Utc>>> {
        match self {
            Value::DateTime(value) => Ok(Some(value)),
            Value::Null => Ok(None),
            other => Err(anyhow::anyhow!(
                "expected datetime or null, found {:?}",
                other
            )),
        }
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Value::String(value)
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Value::String(value.to_owned())
    }
}

impl From<i32> for Value {
    fn from(value: i32) -> Self {
        Value::Integer(value as i64)
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Value::Integer(value)
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Value::Float(value)
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Value::Boolean(value)
    }
}

impl From<DateTime<Utc>> for Value {
    fn from(value: DateTime<Utc>) -> Self {
        Value::DateTime(value)
    }
}

impl From<serde_json::Value> for Value {
    fn from(value: serde_json::Value) -> Self {
        Value::Json(value)
    }
}

impl<T> From<Option<T>> for Value
where
    T: Into<Value>,
{
    fn from(value: Option<T>) -> Self {
        value.map(Into::into).unwrap_or(Value::Null)
    }
}

/// A thin wrapper that keeps column/value pairs derived from lenses
struct Entity {
    values: HashMap<&'static str, Value>,
}

impl Entity {
    fn new() -> Self {
        Self {
            values: HashMap::new(),
        }
    }

    fn insert(&mut self, field: &'static str, value: Value) {
        self.values.insert(field, value);
    }

    fn insert_opt(&mut self, field: &'static str, value: Option<Value>) {
        self.values
            .insert(field, value.unwrap_or(Value::Null));
    }

    fn value_for(&self, field: &'static str) -> Option<&Value> {
        self.values.get(field)
    }

    fn get(&self, field: &'static str) -> Result<&Value> {
        self.values
            .get(field)
            .ok_or_else(|| anyhow::anyhow!("missing field {}", field))
    }

    fn from_row(row: &SqliteRow, schema: &Schema) -> Result<Self> {
        let mut entity = Entity::new();
        for column in schema.columns() {
            match column.data_type {
                DataType::String => {
                    let value: Option<String> = row.try_get(column.name)?;
            entity.insert_opt(column.name, value.map(Value::String));
        }
                DataType::Integer => {
                    let value: Option<i64> = row.try_get(column.name)?;
                    entity.insert_opt(column.name, value.map(Value::Integer));
                }
                DataType::Float => {
                    let value: Option<f64> = row.try_get(column.name)?;
                    entity.insert_opt(column.name, value.map(Value::Float));
                }
                DataType::Boolean => {
                    let value: Option<i64> = row.try_get(column.name)?;
                    entity.insert_opt(column.name, value.map(|v| Value::Boolean(v != 0)));
                }
                DataType::DateTime => {
                    let value: Option<String> = row.try_get(column.name)?;
                    entity.insert_opt(
                        column.name,
                        value
                            .and_then(|v| DateTime::parse_from_rfc3339(&v).ok())
                            .map(|dt| Value::DateTime(dt.with_timezone(&Utc))),
                    );
                }
                DataType::Json => {
                    let value: Option<serde_json::Value> = row.try_get(column.name)?;
                    entity.insert_opt(column.name, value.map(Value::Json));
                }
            }
        }
        Ok(entity)
    }

    fn iter(&self) -> impl Iterator<Item = (&'static str, &Value)> {
        self.values.iter().map(|(k, v)| (*k, v))
    }
}

#[derive(Default)]
struct SyncStats {
    synced: usize,
}

impl SyncStats {
    fn record_sync(&mut self) {
        self.synced += 1;
    }
}

fn bind_all<'q>(
    mut query: Query<'q, Sqlite, SqliteArguments<'q>>,
    params: impl IntoIterator<Item = Value>,
) -> Query<'q, Sqlite, SqliteArguments<'q>> {
    for value in params {
        query = value.bind(query);
    }
    query
}
```
```

**Implementations:**

```rust
// Loro as DataSource
#[async_trait]
impl DataSource<Task> for LoroDocument {
    async fn get_all(&self) -> Result<Vec<Task>> {
        let tasks = self.doc.get_map("tasks");
        Ok(tasks.iter().map(|(_, v)| Task::from_loro(v)).collect())
    }

    async fn insert(&mut self, task: Task) -> Result<String> {
        let id = task.id.clone();
        let tasks = self.doc.get_map("tasks");
        tasks.insert(&id, task.to_loro())?;
        self.save()?;
        Ok(id)
    }

    fn source_name(&self) -> &str { "loro_internal" }
}

// External provider as DataSource
#[async_trait]
impl DataSource<TodoistTask> for TodoistProvider {
    async fn get_all(&self) -> Result<Vec<TodoistTask>> {
        self.api_client.fetch_tasks().await
    }

    async fn insert(&mut self, task: TodoistTask) -> Result<String> {
        self.api_client.create_task(task).await
    }

    fn source_name(&self) -> &str { "todoist" }
}
```

### 2. Lens - Type-Safe Field Access

Lenses provide functional access to struct fields without hardcoding field names:

```rust
/// Functional lens for field access
trait Lens<T, U>: Send + Sync {
    /// Get field value
    fn get<'a>(&self, obj: &'a T) -> &'a U;

    /// Set field value
    fn set(&self, obj: &mut T, value: U);

    /// Modify field value functionally
    fn modify(&self, obj: &mut T, f: impl FnOnce(&U) -> U) {
        let old_value = self.get(obj);
        let new_value = f(old_value);
        self.set(obj, new_value);
    }

    /// SQL column name (for SQL generation)
    fn sql_column(&self) -> Option<&'static str> {
        None
    }

    /// Field name for schema
    fn field_name(&self) -> &'static str;
}
```

**Macro-Generated Lenses:**

```rust
#[derive(Clone, Debug, Serialize, Deserialize, Lenses)]
struct Task {
    id: String,
    title: String,
    priority: Priority,
    status: TaskStatus,
    due_date: Option<DateTime<Utc>>,
}

// Macro generates:
mod task_lenses {
    use super::*;

    #[derive(Copy, Clone, Debug, Default)]
    pub struct TitleLens;

    impl Lens<Task, String> for TitleLens {
        fn get<'a>(&self, task: &'a Task) -> &'a String {
            &task.title
        }

        fn set(&self, task: &mut Task, value: String) {
            task.title = value;
        }

        fn sql_column(&self) -> Option<&'static str> {
            Some("title")
        }

        fn field_name(&self) -> &'static str {
            "title"
        }
    }

    #[derive(Copy, Clone, Debug, Default)]
    pub struct PriorityLens;

    impl Lens<Task, Priority> for PriorityLens {
        fn get<'a>(&self, task: &'a Task) -> &'a Priority {
            &task.priority
        }

        fn set(&self, task: &mut Task, value: Priority) {
            task.priority = value;
        }

        fn sql_column(&self) -> Option<&'static str> {
            Some("priority")
        }

        fn field_name(&self) -> &'static str {
            "priority"
        }
    }

    #[derive(Copy, Clone, Debug, Default)]
    pub struct DueDateLens;

    impl Lens<Task, Option<DateTime<Utc>>> for DueDateLens {
        fn get<'a>(&self, task: &'a Task) -> &'a Option<DateTime<Utc>> {
            &task.due_date
        }

        fn set(&self, task: &mut Task, value: Option<DateTime<Utc>>) {
            task.due_date = value;
        }

        fn sql_column(&self) -> Option<&'static str> {
            Some("due_date")
        }

        fn field_name(&self) -> &'static str {
            "due_date"
        }
    }
}
```

### 3. Predicate - Type-Safe Queries

Predicates combine in-memory testing with SQL compilation:

```rust
/// A predicate that can be evaluated in-memory and compiled to SQL
trait Predicate<T>: Send + Sync {
    /// Test predicate in memory
    fn test(&self, item: &T) -> bool;

    /// Convert to SQL WHERE clause (if possible)
    fn to_sql(&self, schema: &Schema) -> Option<SqlPredicate>;
}

/// Ergonomic combinators that work for any sized predicate
trait PredicateExt<T>: Predicate<T> + Sized {
    fn and<R>(self, other: R) -> And<T, Self, R>
    where
        R: Predicate<T>,
    {
        And {
            left: self,
            right: other,
            _phantom: PhantomData,
        }
    }

    fn or<R>(self, other: R) -> Or<T, Self, R>
    where
        R: Predicate<T>,
    {
        Or {
            left: self,
            right: other,
            _phantom: PhantomData,
        }
    }

    fn not(self) -> Not<T, Self> {
        Not {
            inner: self,
            _phantom: PhantomData,
        }
    }
}

impl<T, P> PredicateExt<T> for P where P: Predicate<T> + Sized {}

impl<T, P> Predicate<T> for Arc<P>
where
    P: Predicate<T> + ?Sized,
{
    fn test(&self, item: &T) -> bool {
        (**self).test(item)
    }

    fn to_sql(&self, schema: &Schema) -> Option<SqlPredicate> {
        (**self).to_sql(schema)
    }
}

#[derive(Clone)]
struct SqlPredicate {
    where_clause: String,
    params: Vec<Value>,
}

// Equality
struct Eq<T, U, L>
where
    L: Lens<T, U>,
{
    lens: L,
    value: U,
}

impl<T, U, L> Predicate<T> for Eq<T, U, L>
where
    L: Lens<T, U> + Copy + Send + Sync,
    U: PartialEq + Clone + Into<Value> + Send + Sync,
{
    fn test(&self, item: &T) -> bool {
        self.lens.get(item) == &self.value
    }

    fn to_sql(&self, _schema: &Schema) -> Option<SqlPredicate> {
        let column = self.lens.sql_column()?;
        Some(SqlPredicate {
            where_clause: format!("{} = ?", column),
            params: vec![self.value.clone().into()],
        })
    }
}

// Less than
struct Lt<T, U, L>
where
    L: Lens<T, U>,
{
    lens: L,
    value: U,
}

impl<T, U, L> Predicate<T> for Lt<T, U, L>
where
    L: Lens<T, U> + Copy + Send + Sync,
    U: PartialOrd + Clone + Into<Value> + Send + Sync,
{
    fn test(&self, item: &T) -> bool {
        self.lens.get(item) < &self.value
    }

    fn to_sql(&self, _schema: &Schema) -> Option<SqlPredicate> {
        let column = self.lens.sql_column()?;
        Some(SqlPredicate {
            where_clause: format!("{} < ?", column),
            params: vec![self.value.clone().into()],
        })
    }
}

// Greater than
struct Gt<T, U, L>
where
    L: Lens<T, U>,
{
    lens: L,
    value: U,
}

impl<T, U, L> Predicate<T> for Gt<T, U, L>
where
    L: Lens<T, U> + Copy + Send + Sync,
    U: PartialOrd + Clone + Into<Value> + Send + Sync,
{
    fn test(&self, item: &T) -> bool {
        self.lens.get(item) > &self.value
    }

    fn to_sql(&self, _schema: &Schema) -> Option<SqlPredicate> {
        let column = self.lens.sql_column()?;
        Some(SqlPredicate {
            where_clause: format!("{} > ?", column),
            params: vec![self.value.clone().into()],
        })
    }
}

// Is null
struct IsNull<T, U, L>
where
    L: Lens<T, Option<U>>,
{
    lens: L,
}

impl<T, U, L> Predicate<T> for IsNull<T, U, L>
where
    L: Lens<T, Option<U>> + Copy + Send + Sync,
{
    fn test(&self, item: &T) -> bool {
        self.lens.get(item).is_none()
    }

    fn to_sql(&self, _schema: &Schema) -> Option<SqlPredicate> {
        let column = self.lens.sql_column()?;
        Some(SqlPredicate {
            where_clause: format!("{} IS NULL", column),
            params: vec![],
        })
    }
}

// Logical combinators
struct And<T, L, R>
where
    L: Predicate<T>,
    R: Predicate<T>,
{
    left: L,
    right: R,
    _phantom: PhantomData<T>,
}

impl<T, L, R> Predicate<T> for And<T, L, R>
where
    L: Predicate<T>,
    R: Predicate<T>,
{
    fn test(&self, item: &T) -> bool {
        self.left.test(item) && self.right.test(item)
    }

    fn to_sql(&self, schema: &Schema) -> Option<SqlPredicate> {
        let left_sql = self.left.to_sql(schema)?;
        let right_sql = self.right.to_sql(schema)?;

        let mut params = left_sql.params.clone();
        params.extend(right_sql.params.clone());

        Some(SqlPredicate {
            where_clause: format!(
                "({}) AND ({})",
                left_sql.where_clause, right_sql.where_clause
            ),
            params,
        })
    }
}

struct Or<T, L, R>
where
    L: Predicate<T>,
    R: Predicate<T>,
{
    left: L,
    right: R,
    _phantom: PhantomData<T>,
}

impl<T, L, R> Predicate<T> for Or<T, L, R>
where
    L: Predicate<T>,
    R: Predicate<T>,
{
    fn test(&self, item: &T) -> bool {
        self.left.test(item) || self.right.test(item)
    }

    fn to_sql(&self, schema: &Schema) -> Option<SqlPredicate> {
        let left_sql = self.left.to_sql(schema)?;
        let right_sql = self.right.to_sql(schema)?;

        let mut params = left_sql.params.clone();
        params.extend(right_sql.params.clone());

        Some(SqlPredicate {
            where_clause: format!(
                "({}) OR ({})",
                left_sql.where_clause, right_sql.where_clause
            ),
            params,
        })
    }
}

struct Not<T, P>
where
    P: Predicate<T>,
{
    inner: P,
    _phantom: PhantomData<T>,
}

impl<T, P> Predicate<T> for Not<T, P>
where
    P: Predicate<T>,
{
    fn test(&self, item: &T) -> bool {
        !self.inner.test(item)
    }

    fn to_sql(&self, schema: &Schema) -> Option<SqlPredicate> {
        let inner_sql = self.inner.to_sql(schema)?;

        Some(SqlPredicate {
            where_clause: format!("NOT ({})", inner_sql.where_clause),
            params: inner_sql.params.clone(),
        })
    }
}
```

**Usage:**

```rust
use std::sync::Arc;
use task_lenses::*;

// Type-safe predicates with lens-based field access
let high_priority = Eq {
    lens: PriorityLens,
    value: Priority::High,
};

let overdue = Lt {
    lens: DueDateLens,
    value: Some(Utc::now()),
};

// Combine predicates
let critical_tasks: Arc<dyn Predicate<Task>> =
    Arc::new(high_priority.and(overdue));

// Test in memory
if critical_tasks.test(&task) {
    println!("Critical task!");
}

// Or compile to SQL
if let Some(sql) = critical_tasks.to_sql(&schema) {
    println!("WHERE {}", sql.where_clause);
}
```

### 4. QueryableCache - Universal Wrapper

Makes any `DataSource<T>` queryable by caching to SQLite:

```rust
/// Makes any DataSource queryable by caching to SQLite
struct QueryableCache<S, T>
where
    S: DataSource<T>,
    T: Serialize + serde::de::DeserializeOwned + Clone + Send + Sync + HasSchema,
{
    source: S,
    cache: SqlitePool,
    schema: Schema,
    _phantom: PhantomData<T>,
}

impl<S, T> QueryableCache<S, T>
where
    S: DataSource<T>,
    T: Serialize + serde::de::DeserializeOwned + Clone + Send + Sync + HasSchema,
{
    async fn new(source: S, cache_db: &str) -> Result<Self> {
        let cache = SqlitePool::connect(cache_db).await?;
        let schema = T::schema(); // Macro-derived

        // Create cache table from schema
        let create_table_sql = schema.to_create_table_sql();
        sqlx::query(&create_table_sql).execute(&cache).await?;

        Ok(Self {
            source,
            cache,
            schema,
            _phantom: PhantomData,
        })
    }

    /// Sync cache from source
    async fn sync(&mut self) -> Result<SyncStats> {
        let items = self.source.get_all().await?;

        let mut stats = SyncStats::default();

        for item in items {
            // Serialize to Entity (row in cache)
            let entity = item.to_entity(&self.schema)?;

            // Upsert into cache
            self.upsert_to_cache(&entity).await?;
            stats.record_sync();
        }

        Ok(stats)
    }

    /// Private: upsert to cache
    async fn upsert_to_cache(&self, entity: &Entity) -> Result<()> {
        let table_name = self.schema.table_name();

        let upsert_sql = format!(
            "INSERT OR REPLACE INTO {} ({}) VALUES ({})",
            table_name,
            self.schema.column_names().join(", "),
            self.schema.column_placeholders()
        );

        let values = self
            .schema
            .columns()
            .iter()
            .map(|col| entity.value_for(col.name).cloned().unwrap_or(Value::Null))
            .collect::<Vec<_>>();

        let query = bind_all(sqlx::query(&upsert_sql), values);
        query.execute(&self.cache).await?;
        Ok(())
    }

    /// Private: get from cache
    async fn get_from_cache(&self, id: &str) -> Result<Option<T>> {
        let sql = format!("SELECT * FROM {} WHERE id = ?", self.schema.table_name());

        let row = sqlx::query(&sql)
            .bind(id)
            .fetch_optional(&self.cache)
            .await?;

        match row {
            Some(row) => {
                let entity = Entity::from_row(&row, &self.schema)?;
                Ok(Some(T::from_entity(entity, &self.schema)?))
            }
            None => Ok(None),
        }
    }

    /// Private: update cache
    async fn update_cache(&self, id: &str, updates: &Updates<T>) -> Result<()> {
        let mut set_clauses = Vec::new();
        let mut params = Vec::new();

        for change in updates.iter() {
            let column = match change.sql_column {
                Some(column) => column,
                None => continue,
            };

            match &change.update {
                FieldUpdate::Set(value) => {
                    set_clauses.push(format!("{} = ?", column));
                    params.push(value.clone());
                }
                FieldUpdate::Clear => set_clauses.push(format!("{} = NULL", column)),
            }
        }

        if set_clauses.is_empty() {
            return Ok(());
        }

        let sql = format!(
            "UPDATE {} SET {} WHERE id = ?",
            self.schema.table_name(),
            set_clauses.join(", ")
        );

        let mut query = bind_all(sqlx::query(&sql), params);
        query = query.bind(id);

        query.execute(&self.cache).await?;
        Ok(())
    }
}
```

**QueryableCache implements DataSource (transparent pass-through):**

```rust
#[async_trait]
impl<S: DataSource<T>, T> DataSource<T> for QueryableCache<S, T>
where
    T: Serialize + serde::de::DeserializeOwned + Clone + Send + Sync + HasSchema,
{
    async fn get_all(&self) -> Result<Vec<T>> {
        self.source.get_all().await
    }

    async fn get_by_id(&self, id: &str) -> Result<Option<T>> {
        // Try cache first (fast)
        if let Some(cached) = self.get_from_cache(id).await? {
            return Ok(Some(cached));
        }

        // Fall back to source
        self.source.get_by_id(id).await
    }

    async fn insert(&mut self, item: T) -> Result<String> {
        // Insert into source (source of truth)
        let id = self.source.insert(item.clone()).await?;

        // Update cache
        let entity = item.to_entity(&self.schema)?;
        self.upsert_to_cache(&entity).await?;

        Ok(id)
    }

    async fn update(&mut self, id: &str, updates: &Updates<T>) -> Result<()> {
        // Update source
        self.source.update(id, updates).await?;

        // Update cache
        self.update_cache(id, updates).await?;

        Ok(())
    }

    async fn delete(&mut self, id: &str) -> Result<()> {
        // Delete from source
        self.source.delete(id).await?;

        // Delete from cache
        sqlx::query(&format!("DELETE FROM {} WHERE id = ?", self.schema.table_name()))
            .bind(id)
            .execute(&self.cache)
            .await?;

        Ok(())
    }

    fn source_name(&self) -> &str {
        self.source.source_name()
    }
}
```

**QueryableCache ALSO implements Queryable (efficient queries):**

```rust
#[async_trait]
trait Queryable<T>: Send + Sync
where
    T: Send + Sync,
{
    async fn query(&self, predicate: Arc<dyn Predicate<T>>) -> Result<Vec<T>>;
}

#[async_trait]
impl<S, T> Queryable<T> for QueryableCache<S, T>
where
    S: DataSource<T>,
    T: Serialize + serde::de::DeserializeOwned + Clone + Send + Sync + HasSchema,
{
    async fn query(&self, predicate: Arc<dyn Predicate<T>>) -> Result<Vec<T>> {
        // Try to compile predicate to SQL
        if let Some(sql_pred) = predicate.to_sql(&self.schema) {
            // Execute SQL query on cache (fast!)
            let sql = format!(
                "SELECT * FROM {} WHERE {}",
                self.schema.table_name(),
                sql_pred.where_clause
            );

            let query = bind_all(sqlx::query(&sql), sql_pred.params);
            let rows = query.fetch_all(&self.cache).await?;

            // Deserialize from rows
            rows.into_iter()
                .map(|row| {
                    let entity = Entity::from_row(&row, &self.schema)?;
                    T::from_entity(entity, &self.schema)
                })
                .collect()
        } else {
            // Fallback: naive in-memory filtering
            let all_items = self.source.get_all().await?;
            Ok(all_items.into_iter()
                .filter(|item| predicate.test(item))
                .collect())
        }
    }
}
```

### 5. Schema Generation

Macro-derives schema from struct definitions:

```rust
trait HasSchema {
    fn schema() -> Schema;
    fn to_entity(&self, schema: &Schema) -> Result<Entity>;
    fn from_entity(entity: Entity, schema: &Schema) -> Result<Self>;
}

#[derive(Clone, Debug, Serialize, Deserialize, HasSchema, Lenses)]
struct Task {
    #[primary_key]
    id: String,

    #[indexed]
    title: String,

    #[indexed]
    priority: Priority,

    status: TaskStatus,

    #[indexed]
    due_date: Option<DateTime<Utc>>,
}

// Macro generates:
impl HasSchema for Task {
    fn schema() -> Schema {
        Schema {
            table_name: "tasks",
            columns: vec![
                Column {
                    name: "id",
                    data_type: DataType::String,
                    indexed: true,
                    primary_key: true,
                    nullable: false,
                },
                Column {
                    name: "title",
                    data_type: DataType::String,
                    indexed: true,
                    primary_key: false,
                    nullable: false,
                },
                Column {
                    name: "priority",
                    data_type: DataType::Integer,
                    indexed: true,
                    primary_key: false,
                    nullable: false,
                },
                Column {
                    name: "status",
                    data_type: DataType::String,
                    indexed: false,
                    primary_key: false,
                    nullable: false,
                },
                Column {
                    name: "due_date",
                    data_type: DataType::DateTime,
                    indexed: true,
                    primary_key: false,
                    nullable: true,
                },
            ],
        }
    }

    fn to_entity(&self, schema: &Schema) -> Result<Entity> {
        let mut entity = Entity::new();
        entity.insert("id", Value::from(self.id.clone()));
        entity.insert("title", Value::from(self.title.clone()));
        entity.insert("priority", Value::from(self.priority as i32));
        entity.insert("status", Value::from(self.status.to_string()));
        entity.insert_opt("due_date", self.due_date.map(Value::from));
        Ok(entity)
    }

    fn from_entity(entity: Entity, schema: &Schema) -> Result<Self> {
        Ok(Task {
            id: entity.get("id")?.as_string()?.clone(),
            title: entity.get("title")?.as_string()?.clone(),
            priority: Priority::from_i32(entity.get("priority")?.as_integer()? as i32)?,
            status: TaskStatus::from_str(entity.get("status")?.as_string()?)?,
            due_date: entity.get("due_date")?.as_datetime()?.cloned(),
        })
    }
}

struct Schema {
    table_name: &'static str,
    columns: Vec<Column>,
}

impl Schema {
    fn to_create_table_sql(&self) -> String {
        let column_defs: Vec<String> = self.columns.iter()
            .map(|col| {
                let mut def = format!("{} {}", col.name, col.data_type.to_sql_type());
                if col.primary_key {
                    def.push_str(" PRIMARY KEY");
                }
                if !col.nullable {
                    def.push_str(" NOT NULL");
                }
                def
            })
            .collect();

        let indexes: Vec<String> = self.columns.iter()
            .filter(|col| col.indexed && !col.primary_key)
            .map(|col| format!(
                "CREATE INDEX IF NOT EXISTS idx_{}_{} ON {} ({})",
                self.table_name, col.name, self.table_name, col.name
            ))
            .collect();

        format!(
            "CREATE TABLE IF NOT EXISTS {} ({});\n{}",
            self.table_name,
            column_defs.join(", "),
            indexes.join(";\n")
        )
    }

    fn table_name(&self) -> &str {
        self.table_name
    }

    fn columns(&self) -> &[Column] {
        &self.columns
    }

    fn column_names(&self) -> Vec<&'static str> {
        self.columns.iter().map(|c| c.name).collect()
    }

    fn column_placeholders(&self) -> String {
        vec!["?"; self.columns.len()].join(", ")
    }
}

struct Column {
    name: &'static str,
    data_type: DataType,
    indexed: bool,
    primary_key: bool,
    nullable: bool,
}

enum DataType {
    String,
    Integer,
    Float,
    Boolean,
    DateTime,
    Json,
}

impl DataType {
    fn to_sql_type(&self) -> &'static str {
        match self {
            DataType::String => "TEXT",
            DataType::Integer => "INTEGER",
            DataType::Float => "REAL",
            DataType::Boolean => "INTEGER",
            DataType::DateTime => "TEXT",
            DataType::Json => "TEXT",
        }
    }
}
```

## Complete Usage Example

### Internal Content (Loro → QueryableCache)

```rust
// 1. Create Loro document (source of truth)
let loro_doc = LoroDocument::new("tasks.crdt")?;

// 2. Wrap in QueryableCache to make it queryable
let mut internal_store = QueryableCache::new(
    loro_doc,
    "cache/internal.db"
).await?;

// 3. Sync cache from Loro (one-time or periodic)
internal_store.sync().await?;

// 4. Now we can query efficiently using type-safe predicates
use std::sync::Arc;
use task_lenses::*;

let high_priority: Arc<dyn Predicate<Task>> = Arc::new(Eq {
    lens: PriorityLens,
    value: Priority::High,
});
let high_priority_tasks = internal_store.query(high_priority.clone()).await?;

let overdue_critical_predicate: Arc<dyn Predicate<Task>> = Arc::new(
    Eq {
        lens: PriorityLens,
        value: Priority::High,
    }
    .and(Lt {
        lens: DueDateLens,
        value: Some(Utc::now()),
    }),
);
let overdue_critical = internal_store
    .query(overdue_critical_predicate)
    .await?;

// 5. Mutations go through to Loro (source of truth)
internal_store.insert(Task {
    id: uuid::Uuid::new_v4().to_string(),
    title: "New task".into(),
    priority: Priority::High,
    status: TaskStatus::Open,
    due_date: None,
}).await?;

// 6. Updates use lens-based field updates
let mut updates = Updates::new();
updates.set(TitleLens, "Updated title".to_string());
updates.set(PriorityLens, Priority::Medium);

internal_store.update("task_id_123", &updates).await?;
```

### External Content (API → QueryableCache)

```rust
// 1. Create provider (talks to external API)
let todoist = TodoistProvider::new("api_key_here");

// 2. Wrap in QueryableCache for offline access + efficient queries
let mut todoist_cache = QueryableCache::new(
    todoist,
    "cache/todoist.db"
).await?;

// 3. Sync from Todoist API
todoist_cache.sync().await?;

// 4. Now we can query offline
use std::sync::Arc;
use todoist_lenses::*;

let overdue_tasks = todoist_cache
    .query(Arc::new(Lt {
        lens: DueDateLens,
        value: Some(Utc::now()),
    }))
    .await?;

let project_tasks = todoist_cache
    .query(Arc::new(Eq {
        lens: ProjectIdLens,
        value: "project_123".to_string(),
    }))
    .await?;

// 5. Mutations go through to Todoist API
todoist_cache.insert(TodoistTask {
    id: "".into(), // Generated by API
    content: "Buy milk".into(),
    priority: 4,
    due: Some(TodoistDue::Date("2024-01-15".into())),
    project_id: "project_123".into(),
    section_id: None,
    completed: false,
}).await?;

// 6. Updates push to API
let mut updates = Updates::new();
updates.set(CompletedLens, true);

todoist_cache.update("task_456", &updates).await?;
```

### Unified Queries Across Sources

```rust
struct UnifiedQuery {
    sources: Vec<Box<dyn Queryable<Task>>>,
}

impl UnifiedQuery {
    async fn query(&self, predicate: Arc<dyn Predicate<Task>>) -> Result<Vec<Task>> {
        let mut all_tasks = vec![];

        for source in &self.sources {
            let tasks = source.query(predicate.clone()).await?;
            all_tasks.extend(tasks);
        }

        Ok(all_tasks)
    }
}

// Usage:
use std::sync::Arc;
let unified = UnifiedQuery {
    sources: vec![
        Box::new(internal_store),  // QueryableCache<LoroDocument, Task>
        Box::new(todoist_adapter), // Todoist mapped to Task
        Box::new(jira_adapter),    // Jira mapped to Task
    ],
};

// Query across all sources with type-safe predicates
let all_high_priority = unified
    .query(Arc::new(Eq {
        lens: task_lenses::PriorityLens,
        value: Priority::High,
    }))
    .await?;

let urgent_today = unified
    .query(Arc::new(
        Eq {
            lens: task_lenses::PriorityLens,
            value: Priority::High,
        }
        .and(Eq {
            lens: task_lenses::DueDateLens,
            value: Some(Utc::now()),
        }),
    ))
    .await?;
```

Each external provider is wrapped in a thin adapter (e.g. `todoist_adapter`) that implements `Queryable<Task>` by mapping provider-specific entities into the unified `Task` model before delegating to its underlying cache.

## Extensibility Architecture

### Canonical Block Projection

Rather than modeling inheritance directly in Rust, a dedicated projection establishes common ground across richer record types:

```rust
#[derive(Clone)]
struct Block {
    id: String,
    kind: BlockKind,
    parent_id: Option<String>,
    content: String,
    completed: Option<bool>,
    metadata: serde_json::Value,
}

trait Blocklike {
    fn as_block(&self) -> Block;
}
```

Every domain struct (e.g. `Task`, `CalendarEvent`) implements `Blocklike`, allowing a thin adapter to expose a `Queryable<Block>` view:

```rust
struct TaskBlockAdapter<C> {
    cache: C,
}

#[async_trait]
impl<C> Queryable<Block> for TaskBlockAdapter<C>
where
    C: Queryable<Task> + Send + Sync,
{
    async fn query(&self, predicate: Arc<dyn Predicate<Block>>) -> Result<Vec<Block>> {
        match translate_block_predicate(predicate.clone()) {
            Some(task_pred) => self
                .cache
                .query(task_pred)
                .await
                .map(|tasks| tasks.into_iter().map(|t| t.as_block()).collect()),
            None => {
                let tasks = self.cache.query(all_tasks_predicate()).await?;
                Ok(tasks
                    .into_iter()
                    .map(|t| t.as_block())
                    .filter(|b| predicate.test(b))
                    .collect())
            }
        }
    }
}
```

Here `translate_block_predicate` maps lenses from the `Block` view back to `Task` lenses when possible, keeping SQL compilation intact for common filters. Fallback filters still work via in-memory evaluation. Other block-producing types follow the same adapter pattern.

### Dynamic Type Registry

A central registry tracks every type—built-in and user-defined—and the projections it participates in:

```rust
struct TypeRegistry {
    types: HashMap<TypeId, TypeHandle>,
}

struct TypeHandle {
    schema: Arc<SchemaDescriptor>,
    queryables: HashMap<ViewId, Box<dyn ErasedQueryable>>,
}

impl TypeRegistry {
    fn register(&mut self, descriptor: TypeDescriptor) -> Result<()> { /* ... */ }
    fn view(&self, view: ViewId) -> impl Iterator<Item = &Box<dyn ErasedQueryable>> { /* ... */ }
}
```

Built-in sources register at startup, while plugins or user-defined schemas call `register` at runtime. The registry simply stores handles, so the system behaves identically whether a type was compiled in or added dynamically. Views such as `Block` or `Task` just query the registry for the relevant adapters.

### User-Defined Types & Dynamic Entities

For custom record definitions authored by users, runtime metadata replaces static Rust structs:

- `DynamicSchema` stores field definitions (name, type, optional SQL column, indexing hints).
- `DynamicEntity` represents an instance: `struct DynamicEntity { type_id: TypeId, fields: HashMap<FieldId, Value> }`.
- `DynamicLens` implementations look up field metadata and access values from `DynamicEntity`, enabling the existing predicate system to continue working.
- `QueryableCache` gains a specialization `QueryableCache<DynamicSource, DynamicEntity>` that uses `DynamicSchema::to_sql_columns()` to provision tables on the fly.

Storage options for user fields:

1. **Hybrid JSON + Columns (default)**: Persist canonical fields in columns, extras in a JSON blob. SQLite (and Turso) support JSON1 operators, so filters on ad‑hoc keys still work. Frequently queried fields can be promoted to generated columns with indexes.
2. **On-Demand Migrations**: When a user adds a field and marks it as indexed, issue an `ALTER TABLE` to materialize a concrete column. A migration coordinator (part of the registry) keeps schema versions in sync across caches.

The hybrid path keeps migrations optional while preserving acceptable query performance.

### Extension Tooling & Code Generation

Rust keeps a closed-world assumption for generics, so the runtime path uses dynamic metadata instead of shipping a compiler. Developer tooling can still emit Rust code (via a CLI or build script) for advanced users who want fully typed integrations, but the application embeds no compiler. Plugins that need logic can target a lighter-weight runtime (WASM, Lua, JavaScript) and operate on `DynamicEntity` values, calling back into the Rust core through the registry.

### Incremental Query Updates

`QueryableCache` already owns the mutation surface (`insert`, `update`, `delete`). The naive plan is to broadcast a generic `CacheEvent` every time any table changes and re-run all visible predicates. The observer layer looks like:

```rust
enum CacheEvent {
    Insert { table: TableId, id: String },
    Update { table: TableId, id: String },
    Delete { table: TableId, id: String },
}

trait CacheObserver: Send + Sync {
    fn on_event(&self, event: CacheEvent);
}
```

The cache keeps a list of observers and notifies them after every successful write. For now, the UI receives events and refreshes each active query. Because the notification carries table identifiers, upgrading to dependency-aware refreshes later (only refresh queries tied to the touched tables) becomes a matter of tracking those dependencies rather than rewriting the core.

## Component Responsibilities Summary

| Component | Role | Source of Truth? | Queryable? | Implements |
|-----------|------|------------------|------------|------------|
| **LoroDocument** | CRDT operations | ✅ YES (internal) | ❌ NO | `DataSource<T>` |
| **TodoistProvider** | API adapter | ❌ NO (remote is) | ❌ NO | `DataSource<T>` |
| **QueryableCache** | Universal cache | ❌ NO (wraps source) | ✅ YES | `DataSource<T>` + `Queryable<T>` |
| **UnifiedQuery** | Cross-source queries | ❌ NO | ✅ YES | Custom |

## Benefits of This Design

1. ✅ **No hardcoded types**: Everything generic over `T`
2. ✅ **Type-safe field access**: Lenses provide compile-time field names
3. ✅ **SQL-or-memory**: Predicates compile to SQL when possible, fall back to in-memory
4. ✅ **Universal caching pattern**: QueryableCache works for ANY DataSource
5. ✅ **Transparent wrapping**: QueryableCache implements DataSource, so it's a drop-in replacement
6. ✅ **Naive fallback**: If SQL conversion fails, iterate and filter in-memory
7. ✅ **Schema-driven**: No hardcoded SQL, everything derived from macros
8. ✅ **Composable predicates**: And/Or/Not combinators with type safety
9. ✅ **Same pattern everywhere**: Internal (Loro) and external (APIs) use identical patterns
10. ✅ **Projection-friendly**: Canonical views (e.g. `Block`) keep richer types searchable without inheritance
11. ✅ **Observable cache**: Mutation events let the UI start with “refresh all” and evolve to dependency-aware updates later

## Open Questions & Future Refinements

### 1. Lens Serialization
**Current:** We rely on `Into<Value>` implementations for field types when emitting SQL or building updates.

**Question:** Should `#[derive(HasSchema)]` auto-generate those conversions for all fields, or do we codify a separate `IntoValue` trait to keep domain enums decoupled from storage concerns?

### 2. Predicate Sharing
**Current:** Predicates are shared via `Arc<dyn Predicate<T>>`.

**Question:** Is the `Arc` allocation acceptable for hot query paths, or do we also want zero-cost adapters (e.g. stack-only combinators) for performance-critical filters?

### 3. Type Registry Lifecycle
**Current:** Registry registration logic is sketched but not implemented.

**Question:** How should dynamic registrations be persisted, ordered, and versioned across restarts and multiple devices?

### 4. Dirty Tracking
**Current:** Not shown in QueryableCache.

**Question:** Where does dirty tracking fit?
- Inside QueryableCache?
- Separate trait `DirtyTrackable`?
- Only for external sources (not Loro)?

### 5. Sync Strategy
**Current:** Manual `sync()` call.

**Options:**
- Periodic background sync (timer-based)
- Event-driven sync (on source change)
- Smart sync (only changed items)

### 6. Conflict Resolution
**Current:** Not addressed.

**For external sources:**
- Last-write-wins
- Version tracking (etags)
- User prompt for conflicts

### 7. Multi-Provider Mapping
**Question:** How to map provider-specific types (TodoistTask) to unified Task?

**Option A:** model-mapper for conversions
**Option B:** Dual storage (provider table + unified table)
**Option C:** Provider types implement `Into<Task>`

### 8. Error Handling
**Current:** Simple `Result<T>` with generic error.

**Question:** Need structured error types?
```rust
enum StorageError {
    NotFound(String),
    Conflict { local: Entity, remote: Entity },
    NetworkError(reqwest::Error),
    SerializationError(serde_json::Error),
}
```

## Implementation Priorities

### Phase 1: Core Abstractions
1. Define `DataSource<T>` trait
2. Implement for LoroDocument
3. Define `Lens<T, U>` trait
4. Create basic lens derive macro
5. Define `Predicate<T>` trait
6. Implement basic predicates (Eq, Lt, Gt, IsNull)
7. Implement logical combinators (And, Or, Not)

### Phase 2: QueryableCache
1. Implement QueryableCache wrapper
2. Add SQLite table generation from schema
3. Implement sync logic
4. Add SQL compilation for predicates
5. Add in-memory fallback

### Phase 3: External Providers
1. Implement TodoistProvider as DataSource
2. Wrap in QueryableCache
3. Test bidirectional sync
4. Add conflict detection

### Phase 4: Unified Queries
1. Implement UnifiedQuery coordinator
2. Test cross-provider queries
3. Add result merging/deduplication

### Phase 5: Advanced Features
1. Dirty tracking
2. Background sync
3. Conflict resolution UI
4. Schema migrations

## Comparison with Original Architecture

| Aspect | Original (architecture.md) | New (architecture2.md) |
|--------|---------------------------|------------------------|
| **Type Safety** | `Entity = HashMap<String, Value>` | Generic `T` with lenses |
| **Field Access** | String keys `entity.get("title")` | Type-safe lenses `TitleLens.get(task)` |
| **Queries** | `Filter` enum with strings | `Predicate<T>` with lenses |
| **SQL Generation** | Manual filter_to_sql() | Automatic via Predicate.to_sql() |
| **Caching** | Separate ExternalCache/QueryableStore | Unified QueryableCache |
| **Provider Interface** | StorageBackend trait | DataSource<T> trait |
| **Abstraction** | Entity-based (dynamic) | Type-based (static) |

**Key Improvement:** Stay in typed land as long as possible, only drop to dynamic (Entity) when serializing to SQLite.
