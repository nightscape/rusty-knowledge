use async_trait::async_trait;
use std::fmt::Debug;
use std::sync::Arc;

use super::entity::Entity;
use super::value::Value;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[async_trait]
pub trait DataSource<T>: Send + Sync
where
    T: Send + Sync + 'static,
{
    async fn get_all(&self) -> Result<Vec<T>>;
    async fn get_by_id(&self, id: &str) -> Result<Option<T>>;
    async fn insert(&self, item: T) -> Result<String>;
    async fn update(&self, id: &str, item: T) -> Result<()>;
    async fn delete(&self, id: &str) -> Result<()>;
}

pub trait Lens<T, U>: Clone + Send + Sync + 'static {
    fn get(&self, source: &T) -> Option<U>;
    fn set(&self, source: &mut T, value: U);
    fn sql_column(&self) -> &'static str {
        self.field_name()
    }
    fn field_name(&self) -> &'static str;
}

pub trait Predicate<T>: Send + Sync {
    fn test(&self, item: &T) -> bool;
    fn to_sql(&self) -> Option<SqlPredicate>;

    fn and<P>(self, other: P) -> And<T, Self, P>
    where
        Self: Sized,
        P: Predicate<T>,
    {
        And {
            left: self,
            right: other,
            _phantom: std::marker::PhantomData,
        }
    }

    fn or<P>(self, other: P) -> Or<T, Self, P>
    where
        Self: Sized,
        P: Predicate<T>,
    {
        Or {
            left: self,
            right: other,
            _phantom: std::marker::PhantomData,
        }
    }

    fn not(self) -> Not<T, Self>
    where
        Self: Sized,
    {
        Not {
            inner: self,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<T> Predicate<T> for Arc<dyn Predicate<T>>
where
    T: Send + Sync,
{
    fn test(&self, item: &T) -> bool {
        (**self).test(item)
    }

    fn to_sql(&self) -> Option<SqlPredicate> {
        (**self).to_sql()
    }
}

#[derive(Debug, Clone)]
pub struct SqlPredicate {
    pub sql: String,
    pub params: Vec<Value>,
}

impl SqlPredicate {
    pub fn new(sql: String, params: Vec<Value>) -> Self {
        Self { sql, params }
    }

    pub fn bind_all_sqlite<'q>(
        &'q self,
        mut query: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    ) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
        for param in &self.params {
            query = match param {
                Value::String(s) => query.bind(s),
                Value::Integer(i) => query.bind(i),
                Value::Float(f) => query.bind(f),
                Value::Boolean(b) => query.bind(b),
                Value::Null => query.bind(None::<String>),
                _ => query,
            };
        }
        query
    }
}

pub struct And<T, L, R> {
    left: L,
    right: R,
    _phantom: std::marker::PhantomData<T>,
}

impl<T, L, R> Predicate<T> for And<T, L, R>
where
    T: Send + Sync,
    L: Predicate<T>,
    R: Predicate<T>,
{
    fn test(&self, item: &T) -> bool {
        self.left.test(item) && self.right.test(item)
    }

    fn to_sql(&self) -> Option<SqlPredicate> {
        match (self.left.to_sql(), self.right.to_sql()) {
            (Some(left), Some(right)) => {
                let mut params = left.params;
                params.extend(right.params);
                Some(SqlPredicate::new(
                    format!("({}) AND ({})", left.sql, right.sql),
                    params,
                ))
            }
            _ => None,
        }
    }
}

pub struct Or<T, L, R> {
    left: L,
    right: R,
    _phantom: std::marker::PhantomData<T>,
}

impl<T, L, R> Predicate<T> for Or<T, L, R>
where
    T: Send + Sync,
    L: Predicate<T>,
    R: Predicate<T>,
{
    fn test(&self, item: &T) -> bool {
        self.left.test(item) || self.right.test(item)
    }

    fn to_sql(&self) -> Option<SqlPredicate> {
        match (self.left.to_sql(), self.right.to_sql()) {
            (Some(left), Some(right)) => {
                let mut params = left.params;
                params.extend(right.params);
                Some(SqlPredicate::new(
                    format!("({}) OR ({})", left.sql, right.sql),
                    params,
                ))
            }
            _ => None,
        }
    }
}

pub struct Not<T, P> {
    inner: P,
    _phantom: std::marker::PhantomData<T>,
}

impl<T, P> Predicate<T> for Not<T, P>
where
    T: Send + Sync,
    P: Predicate<T>,
{
    fn test(&self, item: &T) -> bool {
        !self.inner.test(item)
    }

    fn to_sql(&self) -> Option<SqlPredicate> {
        self.inner
            .to_sql()
            .map(|pred| SqlPredicate::new(format!("NOT ({})", pred.sql), pred.params))
    }
}

#[async_trait]
pub trait Queryable<T>: Send + Sync
where
    T: Send + Sync + 'static,
{
    async fn query<P>(&self, predicate: P) -> Result<Vec<T>>
    where
        P: Predicate<T> + Send + 'static;
}

pub trait HasSchema {
    fn schema() -> Schema;
    fn to_entity(&self) -> Entity;
    fn from_entity(entity: Entity) -> Result<Self>
    where
        Self: Sized;
}

#[derive(Debug, Clone)]
pub struct Schema {
    pub table_name: String,
    pub fields: Vec<FieldSchema>,
}

impl Schema {
    pub fn new(table_name: impl Into<String>, fields: Vec<FieldSchema>) -> Self {
        Self {
            table_name: table_name.into(),
            fields,
        }
    }

    pub fn to_create_table_sql(&self) -> String {
        let mut columns = Vec::new();

        for field in &self.fields {
            let mut col = format!("{} {}", field.name, field.sql_type);

            if field.primary_key {
                col.push_str(" PRIMARY KEY");
            }

            if !field.nullable {
                col.push_str(" NOT NULL");
            }

            columns.push(col);
        }

        format!(
            "CREATE TABLE IF NOT EXISTS {} (\n  {}\n)",
            self.table_name,
            columns.join(",\n  ")
        )
    }

    pub fn to_index_sql(&self) -> Vec<String> {
        self.fields
            .iter()
            .filter(|f| f.indexed && !f.primary_key)
            .map(|f| {
                format!(
                    "CREATE INDEX IF NOT EXISTS idx_{}_{} ON {} ({})",
                    self.table_name, f.name, self.table_name, f.name
                )
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct FieldSchema {
    pub name: String,
    pub sql_type: String,
    pub nullable: bool,
    pub primary_key: bool,
    pub indexed: bool,
}

impl FieldSchema {
    pub fn new(name: impl Into<String>, sql_type: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            sql_type: sql_type.into(),
            nullable: false,
            primary_key: false,
            indexed: false,
        }
    }

    pub fn nullable(mut self) -> Self {
        self.nullable = true;
        self
    }

    pub fn primary_key(mut self) -> Self {
        self.primary_key = true;
        self
    }

    pub fn indexed(mut self) -> Self {
        self.indexed = true;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestItem {
        value: i64,
    }

    struct TestPredicate;

    impl Predicate<TestItem> for TestPredicate {
        fn test(&self, item: &TestItem) -> bool {
            item.value > 10
        }

        fn to_sql(&self) -> Option<SqlPredicate> {
            Some(SqlPredicate::new(
                "value > ?".to_string(),
                vec![Value::Integer(10)],
            ))
        }
    }

    #[test]
    fn test_predicate_and() {
        let item = TestItem {
            value: 15,
        };

        let pred = TestPredicate.and(TestPredicate);
        assert!(pred.test(&item));
    }

    #[test]
    fn test_predicate_or() {
        let item = TestItem {
            value: 5,
        };

        let pred = TestPredicate.or(TestPredicate);
        assert!(!pred.test(&item));
    }

    #[test]
    fn test_predicate_not() {
        let item = TestItem {
            value: 5,
        };

        let pred = TestPredicate.not();
        assert!(pred.test(&item));
    }

    #[test]
    fn test_sql_generation() {
        let pred = TestPredicate.and(TestPredicate);
        let sql = pred.to_sql().unwrap();
        assert_eq!(sql.sql, "(value > ?) AND (value > ?)");
        assert_eq!(sql.params.len(), 2);
    }

    #[test]
    fn test_schema_to_sql() {
        let schema = Schema::new(
            "tasks",
            vec![
                FieldSchema::new("id", "TEXT").primary_key(),
                FieldSchema::new("title", "TEXT"),
                FieldSchema::new("priority", "INTEGER").indexed().nullable(),
            ],
        );

        let sql = schema.to_create_table_sql();
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS tasks"));
        assert!(sql.contains("id TEXT PRIMARY KEY"));
        assert!(sql.contains("title TEXT NOT NULL"));
        assert!(sql.contains("priority INTEGER"));

        let indexes = schema.to_index_sql();
        assert_eq!(indexes.len(), 1);
        assert!(indexes[0].contains("CREATE INDEX IF NOT EXISTS idx_tasks_priority"));
    }
}
