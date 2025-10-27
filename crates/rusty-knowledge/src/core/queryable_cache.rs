use async_trait::async_trait;
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use std::marker::PhantomData;
use std::sync::Arc;
use tokio::sync::RwLock;

use super::entity::Entity;
use super::traits::{DataSource, HasSchema, Predicate, Queryable, Result, Schema};

pub struct QueryableCache<S, T>
where
    S: DataSource<T>,
    T: HasSchema + Send + Sync + 'static,
{
    source: Arc<S>,
    pool: Arc<RwLock<Option<SqlitePool>>>,
    _phantom: PhantomData<T>,
}

impl<S, T> QueryableCache<S, T>
where
    S: DataSource<T>,
    T: HasSchema + Send + Sync + 'static,
{
    pub async fn new(source: S) -> Result<Self> {
        Ok(Self {
            source: Arc::new(source),
            pool: Arc::new(RwLock::new(None)),
            _phantom: PhantomData,
        })
    }

    pub async fn new_with_pool(source: S, pool: SqlitePool) -> Result<Self> {
        let cache = Self {
            source: Arc::new(source),
            pool: Arc::new(RwLock::new(Some(pool))),
            _phantom: PhantomData,
        };

        cache.initialize_schema().await?;
        Ok(cache)
    }

    pub async fn with_database(source: S, db_path: &str) -> Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(db_path)
            .await?;

        Self::new_with_pool(source, pool).await
    }

    async fn initialize_schema(&self) -> Result<()> {
        let pool_guard = self.pool.read().await;
        let pool = pool_guard.as_ref().ok_or("Pool not initialized")?;

        let schema = T::schema();
        let create_table_sql = schema.to_create_table_sql();
        sqlx::query(&create_table_sql).execute(pool).await?;

        for index_sql in schema.to_index_sql() {
            sqlx::query(&index_sql).execute(pool).await?;
        }

        Ok(())
    }

    pub async fn sync(&self) -> Result<()> {
        let pool_guard = self.pool.read().await;
        let pool = pool_guard.as_ref().ok_or("Pool not initialized")?;

        let items = self.source.get_all().await?;

        for item in items {
            self.upsert_to_cache(&item, pool).await?;
        }

        Ok(())
    }

    async fn upsert_to_cache(&self, item: &T, pool: &SqlitePool) -> Result<()> {
        let entity = item.to_entity();
        let schema = T::schema();

        let mut columns = Vec::new();
        let mut placeholders = Vec::new();
        let mut values = Vec::new();

        for field in &schema.fields {
            if let Some(value) = entity.fields.get(&field.name) {
                columns.push(field.name.clone());
                placeholders.push("?");
                values.push(value.clone());
            }
        }

        let sql = format!(
            "INSERT OR REPLACE INTO {} ({}) VALUES ({})",
            schema.table_name,
            columns.join(", "),
            placeholders.join(", ")
        );

        let mut query = sqlx::query(&sql);
        for value in &values {
            query = match value {
                super::value::Value::String(s) => query.bind(s),
                super::value::Value::Integer(i) => query.bind(i),
                super::value::Value::Float(f) => query.bind(f),
                super::value::Value::Boolean(b) => query.bind(b),
                super::value::Value::Null => query.bind(None::<String>),
                _ => query,
            };
        }

        query.execute(pool).await?;
        Ok(())
    }

    async fn get_from_cache(&self, id: &str) -> Result<Option<T>> {
        let pool_guard = self.pool.read().await;
        let pool = pool_guard.as_ref().ok_or("Pool not initialized")?;

        let schema = T::schema();
        let id_field = schema
            .fields
            .iter()
            .find(|f| f.primary_key)
            .map(|f| f.name.as_str())
            .unwrap_or("id");

        let sql = format!(
            "SELECT * FROM {} WHERE {} = ? LIMIT 1",
            schema.table_name, id_field
        );

        let row = sqlx::query(&sql).bind(id).fetch_optional(pool).await?;

        if let Some(row) = row {
            let entity = self.row_to_entity(&row, &schema)?;
            T::from_entity(entity).map(Some)
        } else {
            Ok(None)
        }
    }

    async fn update_cache(&self, _id: &str, item: &T) -> Result<()> {
        let pool_guard = self.pool.read().await;
        let pool = pool_guard.as_ref().ok_or("Pool not initialized")?;

        self.upsert_to_cache(item, pool).await
    }

    async fn delete_from_cache(&self, id: &str) -> Result<()> {
        let pool_guard = self.pool.read().await;
        let pool = pool_guard.as_ref().ok_or("Pool not initialized")?;

        let schema = T::schema();
        let id_field = schema
            .fields
            .iter()
            .find(|f| f.primary_key)
            .map(|f| f.name.as_str())
            .unwrap_or("id");

        let sql = format!("DELETE FROM {} WHERE {} = ?", schema.table_name, id_field);
        sqlx::query(&sql).bind(id).execute(pool).await?;

        Ok(())
    }

    fn row_to_entity(&self, row: &sqlx::sqlite::SqliteRow, schema: &Schema) -> Result<Entity> {
        use sqlx::Row;

        let mut entity = Entity::new(&schema.table_name);

        for field in &schema.fields {
            let value = match field.sql_type.as_str() {
                "TEXT" => {
                    let v: Option<String> = row.try_get(field.name.as_str()).ok();
                    v.map(super::value::Value::String)
                        .unwrap_or(super::value::Value::Null)
                }
                "INTEGER" => {
                    let v: Option<i64> = row.try_get(field.name.as_str()).ok();
                    v.map(super::value::Value::Integer)
                        .unwrap_or(super::value::Value::Null)
                }
                "REAL" => {
                    let v: Option<f64> = row.try_get(field.name.as_str()).ok();
                    v.map(super::value::Value::Float)
                        .unwrap_or(super::value::Value::Null)
                }
                _ => super::value::Value::Null,
            };

            entity.set(&field.name, value);
        }

        Ok(entity)
    }
}

#[async_trait]
impl<S, T> DataSource<T> for QueryableCache<S, T>
where
    S: DataSource<T>,
    T: HasSchema + Send + Sync + 'static,
{
    async fn get_all(&self) -> Result<Vec<T>> {
        self.source.get_all().await
    }

    async fn get_by_id(&self, id: &str) -> Result<Option<T>> {
        if let Ok(pool_guard) = self.pool.try_read()
            && pool_guard.is_some()
        {
            drop(pool_guard);
            if let Ok(Some(cached)) = self.get_from_cache(id).await {
                return Ok(Some(cached));
            }
        }

        self.source.get_by_id(id).await
    }

    async fn insert(&self, item: T) -> Result<String> {
        let id = self.source.insert(item).await?;

        if let Ok(Some(inserted_item)) = self.source.get_by_id(&id).await {
            let _ = self.update_cache(&id, &inserted_item).await;
        }

        Ok(id)
    }

    async fn update(&self, id: &str, item: T) -> Result<()> {
        self.source.update(id, item).await?;

        if let Ok(Some(updated_item)) = self.source.get_by_id(id).await {
            let _ = self.update_cache(id, &updated_item).await;
        }

        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.source.delete(id).await?;
        let _ = self.delete_from_cache(id).await;
        Ok(())
    }
}

#[async_trait]
impl<S, T> Queryable<T> for QueryableCache<S, T>
where
    S: DataSource<T>,
    T: HasSchema + Send + Sync + 'static,
{
    async fn query<P>(&self, predicate: P) -> Result<Vec<T>>
    where
        P: Predicate<T> + Send + 'static,
    {
        let pool_guard = self.pool.read().await;

        if let Some(pool) = pool_guard.as_ref()
            && let Some(sql_pred) = predicate.to_sql()
        {
            let schema = T::schema();
            let sql = format!("SELECT * FROM {} WHERE {}", schema.table_name, sql_pred.sql);

            let mut query = sqlx::query(&sql);
            for param in &sql_pred.params {
                query = match param {
                    super::value::Value::String(s) => query.bind(s),
                    super::value::Value::Integer(i) => query.bind(i),
                    super::value::Value::Float(f) => query.bind(f),
                    super::value::Value::Boolean(b) => query.bind(b),
                    super::value::Value::Null => query.bind(None::<String>),
                    _ => query,
                };
            }

            let rows = query.fetch_all(pool).await?;
            let mut results = Vec::new();

            for row in rows {
                let entity = self.row_to_entity(&row, &schema)?;
                if let Ok(item) = T::from_entity(entity) {
                    results.push(item);
                }
            }

            return Ok(results);
        }

        drop(pool_guard);

        let all_items = self.source.get_all().await?;
        Ok(all_items
            .into_iter()
            .filter(|item| predicate.test(item))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::traits::{FieldSchema, SqlPredicate};
    use crate::core::value::Value;

    #[derive(Debug, Clone, PartialEq)]
    struct TestTask {
        id: String,
        title: String,
        priority: i64,
    }

    impl HasSchema for TestTask {
        fn schema() -> Schema {
            Schema::new(
                "test_tasks",
                vec![
                    FieldSchema::new("id", "TEXT").primary_key(),
                    FieldSchema::new("title", "TEXT"),
                    FieldSchema::new("priority", "INTEGER"),
                ],
            )
        }

        fn to_entity(&self) -> Entity {
            Entity::new("TestTask")
                .with_field("id", self.id.clone())
                .with_field("title", self.title.clone())
                .with_field("priority", self.priority)
        }

        fn from_entity(entity: Entity) -> Result<Self> {
            Ok(TestTask {
                id: entity.get_string("id").ok_or("Missing id")?,
                title: entity.get_string("title").ok_or("Missing title")?,
                priority: entity.get_i64("priority").ok_or("Missing priority")?,
            })
        }
    }

    struct InMemoryDataSource {
        items: Arc<RwLock<Vec<TestTask>>>,
    }

    impl InMemoryDataSource {
        fn new() -> Self {
            Self {
                items: Arc::new(RwLock::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl DataSource<TestTask> for InMemoryDataSource {
        async fn get_all(&self) -> Result<Vec<TestTask>> {
            Ok(self.items.read().await.clone())
        }

        async fn get_by_id(&self, id: &str) -> Result<Option<TestTask>> {
            Ok(self.items.read().await.iter().find(|t| t.id == id).cloned())
        }

        async fn insert(&self, item: TestTask) -> Result<String> {
            let id = item.id.clone();
            self.items.write().await.push(item);
            Ok(id)
        }

        async fn update(&self, id: &str, item: TestTask) -> Result<()> {
            let mut items = self.items.write().await;
            if let Some(pos) = items.iter().position(|t| t.id == id) {
                items[pos] = item;
            }
            Ok(())
        }

        async fn delete(&self, id: &str) -> Result<()> {
            let mut items = self.items.write().await;
            items.retain(|t| t.id != id);
            Ok(())
        }
    }

    struct PriorityPredicate {
        min: i64,
    }

    impl Predicate<TestTask> for PriorityPredicate {
        fn test(&self, item: &TestTask) -> bool {
            item.priority >= self.min
        }

        fn to_sql(&self) -> Option<SqlPredicate> {
            Some(SqlPredicate::new(
                "priority >= ?".to_string(),
                vec![Value::Integer(self.min)],
            ))
        }
    }

    #[tokio::test]
    async fn test_queryable_cache_creation() {
        let source = InMemoryDataSource::new();
        let cache = QueryableCache::new(source).await.unwrap();
        assert!(cache.pool.read().await.is_none());
    }

    #[tokio::test]
    async fn test_queryable_cache_with_database() {
        let source = InMemoryDataSource::new();
        let cache = QueryableCache::with_database(source, ":memory:")
            .await
            .unwrap();
        assert!(cache.pool.read().await.is_some());
    }

    #[tokio::test]
    async fn test_insert_and_retrieve() {
        let source = InMemoryDataSource::new();
        let cache = QueryableCache::with_database(source, ":memory:")
            .await
            .unwrap();

        let task = TestTask {
            id: "1".to_string(),
            title: "Test Task".to_string(),
            priority: 5,
        };

        let id = cache.insert(task.clone()).await.unwrap();
        assert_eq!(id, "1");

        let retrieved = cache.get_by_id(&id).await.unwrap();
        assert_eq!(retrieved, Some(task));
    }

    #[tokio::test]
    async fn test_sync() {
        let source = InMemoryDataSource::new();

        source
            .insert(TestTask {
                id: "1".to_string(),
                title: "Task 1".to_string(),
                priority: 3,
            })
            .await
            .unwrap();

        source
            .insert(TestTask {
                id: "2".to_string(),
                title: "Task 2".to_string(),
                priority: 7,
            })
            .await
            .unwrap();

        let cache = QueryableCache::with_database(source, ":memory:")
            .await
            .unwrap();
        cache.sync().await.unwrap();

        let task1 = cache.get_by_id("1").await.unwrap();
        assert!(task1.is_some());

        let task2 = cache.get_by_id("2").await.unwrap();
        assert!(task2.is_some());
    }

    #[tokio::test]
    async fn test_query_with_sql() {
        let source = InMemoryDataSource::new();
        let cache = QueryableCache::with_database(source, ":memory:")
            .await
            .unwrap();

        cache
            .insert(TestTask {
                id: "1".to_string(),
                title: "Low Priority".to_string(),
                priority: 2,
            })
            .await
            .unwrap();

        cache
            .insert(TestTask {
                id: "2".to_string(),
                title: "High Priority".to_string(),
                priority: 8,
            })
            .await
            .unwrap();

        let results = cache.query(PriorityPredicate { min: 5 }).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "High Priority");
    }

    #[tokio::test]
    async fn test_update_and_delete() {
        let source = InMemoryDataSource::new();
        let cache = QueryableCache::with_database(source, ":memory:")
            .await
            .unwrap();

        cache
            .insert(TestTask {
                id: "1".to_string(),
                title: "Original".to_string(),
                priority: 3,
            })
            .await
            .unwrap();

        cache
            .update(
                "1",
                TestTask {
                    id: "1".to_string(),
                    title: "Updated".to_string(),
                    priority: 5,
                },
            )
            .await
            .unwrap();

        let updated = cache.get_by_id("1").await.unwrap().unwrap();
        assert_eq!(updated.title, "Updated");

        cache.delete("1").await.unwrap();
        let deleted = cache.get_by_id("1").await.unwrap();
        assert!(deleted.is_none());
    }
}
