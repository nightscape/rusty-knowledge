use async_trait::async_trait;
use std::marker::PhantomData;
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast};
use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio_stream::Stream;
use tracing;

use super::entity::Entity;
use super::traits::{HasSchema, Predicate, Queryable, Result, Schema};
use super::datasource::{DataSource, CrudOperationProvider, OperationProvider, OperationRegistry, OperationDescriptor};
use crate::storage::types::Value;
use crate::storage::turso::TursoBackend;
use crate::api::streaming::{ChangeNotifications, Change, StreamPosition};
use crate::api::types::ApiError;
use crate::storage::types::StorageEntity;

pub struct QueryableCache<S, T>
where
    S: DataSource<T>,
    T: HasSchema + Send + Sync + 'static,
{
    source: Arc<S>,
    backend: Arc<RwLock<TursoBackend>>, // Changed from Database to TursoBackend
    // CDC connection kept alive for streaming
    // CRITICAL: This must stay alive for CDC callbacks to work
    // The callback closure captures the channel sender, which closes the stream if dropped
    _cdc_conn: Option<Arc<tokio::sync::Mutex<turso::Connection>>>,
    _phantom: PhantomData<T>,
}

impl<S, T> QueryableCache<S, T>
where
    S: DataSource<T>,
    T: HasSchema + Send + Sync + 'static,
{
    /// Create QueryableCache with TursoBackend
    ///
    /// The backend is shared with BackendEngine to enable CDC streaming.
    pub async fn new_with_backend(source: S, backend: Arc<RwLock<TursoBackend>>) -> Result<Self> {
        let cache = Self {
            source: Arc::new(source),
            backend,
            _cdc_conn: None, // Will be initialized when watch_changes_since is called
            _phantom: PhantomData,
        };

        cache.initialize_schema().await?;
        Ok(cache)
    }

    // Keep old methods for backward compatibility during transition
    #[allow(dead_code)]
    pub async fn new(source: S) -> Result<Self> {
        // Create in-memory backend for backward compatibility
        let backend = Arc::new(RwLock::new(
            TursoBackend::new_in_memory().await
                .map_err(|e| format!("Failed to create backend: {}", e))?
        ));
        Self::new_with_backend(source, backend).await
    }

    #[allow(dead_code)]
    pub async fn with_database(source: S, db_path: &str) -> Result<Self> {
        let backend = Arc::new(RwLock::new(
            TursoBackend::new(db_path).await
                .map_err(|e| format!("Failed to create backend: {}", e))?
        ));
        Self::new_with_backend(source, backend).await
    }

    async fn initialize_schema(&self) -> Result<()> {
        let backend = self.backend.read().await;
        let conn = backend.get_connection()
            .map_err(|e| format!("Failed to get connection: {}", e))?;

        let schema = T::schema();
        let create_table_sql = schema.to_create_table_sql();
        conn.execute(&create_table_sql, ()).await
            .map_err(|e| format!("Failed to create table: {}", e))?;

        for index_sql in schema.to_index_sql() {
            conn.execute(&index_sql, ()).await
                .map_err(|e| format!("Failed to create index: {}", e))?;
        }

        Ok(())
    }

    pub async fn sync(&self) -> Result<()> {
        let items = self.source.get_all().await?;

        for item in items {
            self.upsert_to_cache(&item).await?;
        }

        Ok(())
    }

    pub async fn upsert_to_cache(&self, item: &T) -> Result<()> {
        let backend = self.backend.read().await;
        let conn = backend.get_connection()
            .map_err(|e| format!("Failed to get connection: {}", e))?;
        let entity = item.to_entity();
        let schema = T::schema();

        let mut columns = Vec::new();
        let mut placeholders = Vec::new();
        let mut values = Vec::new();

        for field in &schema.fields {
            if let Some(value) = entity.fields.get(&field.name) {
                columns.push(field.name.clone());
                placeholders.push("?");

                let libsql_value = match value {
                    super::value::Value::String(s) => turso::Value::Text(s.clone()),
                    super::value::Value::Integer(i) => turso::Value::Integer(*i),
                    super::value::Value::Float(f) => turso::Value::Real(*f),
                    super::value::Value::Boolean(b) => turso::Value::Integer(if *b { 1 } else { 0 }),
                    super::value::Value::Null => turso::Value::Null,
                    _ => turso::Value::Null,
                };
                values.push(libsql_value);
            }
        }

        let id_field = schema
            .fields
            .iter()
            .find(|f| f.primary_key)
            .map(|f| f.name.as_str())
            .unwrap_or("id");

        let update_clause = columns
            .iter()
            .map(|c| format!("{} = excluded.{}", c, c))
            .collect::<Vec<_>>()
            .join(", ");

        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT({}) DO UPDATE SET {}",
            schema.table_name,
            columns.join(", "),
            placeholders.join(", "),
            id_field,
            update_clause
        );

        conn.execute(&sql, turso::params_from_iter(values)).await
            .map_err(|e| format!("Failed to execute upsert: {}", e))?;
        Ok(())
    }

    async fn get_from_cache(&self, id: &str) -> Result<Option<T>> {
        let backend = self.backend.read().await;
        let conn = backend.get_connection()
            .map_err(|e| format!("Failed to get connection: {}", e))?;

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

        let mut rows = conn.query(&sql, [turso::Value::Text(id.to_string())]).await?;

        if let Some(row) = rows.next().await? {
            let entity = self.row_to_entity(&row, &schema)?;
            T::from_entity(entity).map(Some)
        } else {
            Ok(None)
        }
    }

    async fn update_cache(&self, _id: &str, item: &T) -> Result<()> {
        self.upsert_to_cache(item).await
    }

    pub async fn delete_from_cache(&self, id: &str) -> Result<()> {
        let backend = self.backend.read().await;
        let conn = backend.get_connection()
            .map_err(|e| format!("Failed to get connection: {}", e))?;

        let schema = T::schema();
        let id_field = schema
            .fields
            .iter()
            .find(|f| f.primary_key)
            .map(|f| f.name.as_str())
            .unwrap_or("id");

        let sql = format!("DELETE FROM {} WHERE {} = ?", schema.table_name, id_field);
        conn.execute(&sql, [turso::Value::Text(id.to_string())]).await
            .map_err(|e| format!("Failed to execute delete: {}", e))?;

        Ok(())
    }

    /// Wire up stream ingestion from a broadcast receiver (spawns background task)
    ///
    /// This method subscribes to a broadcast channel and updates the local cache
    /// as changes arrive from the provider. The background task runs until the
    /// stream is closed or the cache is dropped.
    /// ExternalServiceDiscovery
    pub fn ingest_stream(&self, rx: broadcast::Receiver<Vec<Change<T>>>)
    where
        T: Clone + Send + Sync + 'static,
    {
        let backend = Arc::clone(&self.backend);
        let schema = T::schema();
        let table_name = schema.table_name.clone();
        let id_field = schema
            .fields
            .iter()
            .find(|f| f.primary_key)
            .map(|f| f.name.clone())
            .unwrap_or_else(|| "id".to_string());

        // Spawn the ingestion task on the current runtime
        // IMPORTANT: This must be called from an async context on a runtime that stays alive
        // If called from a blocking thread with a temporary runtime, the task will be dropped
        // when that runtime is dropped. The caller should ensure this is called from a persistent runtime.
        tokio::spawn(async move {
            let mut rx = rx;
            tracing::info!("[QueryableCache] Started ingesting stream for table: {}", table_name);
            loop {
                match rx.recv().await {
                    Ok(changes) => {
                        tracing::info!("[QueryableCache] Received {} changes for table: {}", changes.len(), table_name);
                        for change in changes {
                            match change {
                                Change::Created { data, .. } | Change::Updated { data, .. } => {
                                    // Upsert to cache
                                    tracing::debug!("[QueryableCache] Upserting change to table: {}, id_field: {}", table_name, id_field);
                                    if let Err(e) = Self::apply_change_to_cache(
                                        &backend,
                                        &table_name,
                                        &id_field,
                                        &data,
                                    ).await {
                                        tracing::error!("[QueryableCache] Error ingesting change into cache: {}", e);
                                    } else {
                                        tracing::debug!("[QueryableCache] Successfully upserted change to table: {}", table_name);
                                    }
                                }
                                Change::Deleted { id, .. } => {
                                    if let Err(e) = Self::delete_from_cache_internal(
                                        &backend,
                                        &table_name,
                                        &id_field,
                                        &id,
                                    ).await {
                                        tracing::error!("Error deleting from cache: {}", e);
                                    }
                                }
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Stream lagged by {} messages, triggering resync", n);
                        // TODO: Trigger full resync
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        tracing::info!("Change stream closed");
                        break;
                    }
                }
            }
        });
    }

    // Helper method for applying changes to cache
    // ExternalServiceDiscovery
    async fn apply_change_to_cache(
        backend: &Arc<RwLock<TursoBackend>>,
        table_name: &str,
        id_field: &str,
        item: &T,
    ) -> Result<()>
    where
        T: HasSchema + Clone,
    {
        let backend_guard = backend.read().await;
        let conn = backend_guard.get_connection()
            .map_err(|e| format!("Failed to get connection: {}", e))?;
        let entity = item.to_entity();
        let schema = T::schema();

        let mut columns = Vec::new();
        let mut placeholders = Vec::new();
        let mut values = Vec::new();

        for field in &schema.fields {
            if let Some(value) = entity.fields.get(&field.name) {
                columns.push(field.name.clone());
                placeholders.push("?");

                let libsql_value = match value {
                    super::value::Value::String(s) => turso::Value::Text(s.clone()),
                    super::value::Value::Integer(i) => turso::Value::Integer(*i),
                    super::value::Value::Float(f) => turso::Value::Real(*f),
                    super::value::Value::Boolean(b) => turso::Value::Integer(if *b { 1 } else { 0 }),
                    super::value::Value::Null => turso::Value::Null,
                    _ => turso::Value::Null,
                };
                values.push(libsql_value);
            }
        }

        let update_clause = columns
            .iter()
            .map(|c| format!("{} = excluded.{}", c, c))
            .collect::<Vec<_>>()
            .join(", ");

        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT({}) DO UPDATE SET {}",
            table_name,
            columns.join(", "),
            placeholders.join(", "),
            id_field,
            update_clause
        );

        conn.execute(&sql, turso::params_from_iter(values)).await
            .map_err(|e| format!("Failed to execute upsert: {}", e))?;
        Ok(())
    }

    // Helper method for deleting from cache
    // ExternalServiceDiscovery
    async fn delete_from_cache_internal(
        backend: &Arc<RwLock<TursoBackend>>,
        table_name: &str,
        id_field: &str,
        id: &str,
    ) -> Result<()> {
        let backend_guard = backend.read().await;
        let conn = backend_guard.get_connection()
            .map_err(|e| format!("Failed to get connection: {}", e))?;

        let sql = format!("DELETE FROM {} WHERE {} = ?", table_name, id_field);
        conn.execute(&sql, [turso::Value::Text(id.to_string())]).await
            .map_err(|e| format!("Failed to execute delete: {}", e))?;

        Ok(())
    }

    fn row_to_entity(&self, row: &turso::Row, schema: &Schema) -> Result<Entity> {
        let mut entity = Entity::new(&schema.table_name);

        for (idx, field) in schema.fields.iter().enumerate() {
            let value = row.get_value(idx).map_err(|e| e.to_string())?;

            let converted_value = match value {
                turso::Value::Null => super::value::Value::Null,
                turso::Value::Integer(i) => super::value::Value::Integer(i),
                turso::Value::Real(f) => super::value::Value::Float(f),
                turso::Value::Text(s) => super::value::Value::String(s),
                turso::Value::Blob(_) => super::value::Value::Null,
            };

            entity.set(&field.name, converted_value);
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
        // Try cache first
        if let Ok(cached) = self.get_from_cache(id).await {
            if cached.is_some() {
                return Ok(cached);
            }
        }

        // Fall back to source
        self.source.get_by_id(id).await
    }
}

// Implement CrudOperationProvider when source also implements it
#[async_trait]
impl<S, T> CrudOperationProvider<T> for QueryableCache<S, T>
where
    S: DataSource<T> + CrudOperationProvider<T>,
    T: HasSchema + Send + Sync + 'static,
{
    async fn set_field(&self, id: &str, field: &str, value: Value) -> Result<()> {
        self.source.set_field(id, field, value).await?;
        // Update cache if we have the item
        if let Ok(Some(item)) = self.source.get_by_id(id).await {
            let _ = self.update_cache(id, &item).await;
        }
        Ok(())
    }

    async fn create(&self, fields: HashMap<String, Value>) -> Result<String> {
        let id = self.source.create(fields).await?;
        // Update cache if we have the item
        if let Ok(Some(item)) = self.source.get_by_id(&id).await {
            let _ = self.update_cache(&id, &item).await;
        }
        Ok(id)
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.source.delete(id).await?;
        let _ = self.delete_from_cache(id).await;
        Ok(())
    }
}

// Implement OperationProvider for QueryableCache
// This enables QueryableCache to be registered with OperationDispatcher
#[async_trait]
impl<S, T> OperationProvider for QueryableCache<S, T>
where
    S: DataSource<T> + CrudOperationProvider<T>,
    T: HasSchema + Send + Sync + 'static + OperationRegistry,
{
    fn operations(&self) -> Vec<OperationDescriptor> {
        // Delegate to OperationRegistry for the entity type
        T::all_operations()
    }

    async fn execute_operation(
        &self,
        entity_name: &str,
        op_name: &str,
        params: StorageEntity,
    ) -> Result<()> {
        // Validate entity name matches the registry
        let expected_entity_name = T::entity_name();
        if entity_name != expected_entity_name {
            return Err(format!(
                "Expected entity_name '{}', got '{}'",
                expected_entity_name, entity_name
            ).into());
        }

        // Dispatch to CrudOperationProvider methods
        match op_name {
            "set_field" => {
                let id = params.get("id")
                    .and_then(|v| v.as_string())
                    .ok_or_else(|| "Missing 'id' parameter".to_string())?;
                let field = params.get("field")
                    .and_then(|v| v.as_string())
                    .ok_or_else(|| "Missing 'field' parameter".to_string())?;
                let value = params.get("value")
                    .ok_or_else(|| "Missing 'value' parameter".to_string())?
                    .clone();
                self.set_field(&id, &field, value).await
            }
            "create" => {
                // Create expects fields as params (excluding id which is generated)
                let id = self.create(params).await?;
                Ok(())
            }
            "delete" => {
                let id = params.get("id")
                    .and_then(|v| v.as_string())
                    .ok_or_else(|| "Missing 'id' parameter".to_string())?;
                self.delete(&id).await
            }
            _ => {
                // Try dispatching to trait methods via generated dispatch functions
                // This handles operations from MutableBlockDataSource, MutableTaskDataSource, etc.
                use super::datasource::__operations_crud_operation_provider;

                // Try CrudOperationProvider operations first
                let result = __operations_crud_operation_provider::dispatch_operation(
                    self.source.as_ref(),
                    op_name,
                    &params,
                ).await;

                if result.is_ok() {
                    return result;
                }

                // For other operations (from MutableBlockDataSource, MutableTaskDataSource, etc.),
                // we need to check if T implements the required traits.
                // Since we can't conditionally compile based on trait bounds, we'll try to
                // dispatch through the source datasource if it implements OperationProvider.
                // Otherwise, return an error indicating the operation is not supported.
                Err(format!(
                    "Operation '{}' is not supported for entity '{}'",
                    op_name, entity_name
                ).into())
            }
        }
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
        if let Some(sql_pred) = predicate.to_sql() {
            let backend = self.backend.read().await;
            let conn = backend.get_connection()
                .map_err(|e| format!("Failed to get connection: {}", e))?;
            let schema = T::schema();
            let sql = format!("SELECT * FROM {} WHERE {}", schema.table_name, sql_pred.sql);

            let params: Vec<turso::Value> = sql_pred.params.iter().map(|param| {
                match param {
                    super::value::Value::String(s) => turso::Value::Text(s.clone()),
                    super::value::Value::Integer(i) => turso::Value::Integer(*i),
                    super::value::Value::Float(f) => turso::Value::Real(*f),
                    super::value::Value::Boolean(b) => turso::Value::Integer(if *b { 1 } else { 0 }),
                    super::value::Value::Null => turso::Value::Null,
                    _ => turso::Value::Null,
                }
            }).collect();

            let mut rows = conn.query(&sql, turso::params_from_iter(params)).await
                .map_err(|e| format!("Failed to execute query: {}", e))?;
            let mut results = Vec::new();

            while let Some(row) = rows.next().await
                .map_err(|e| format!("Failed to read row: {}", e))? {
                let entity = self.row_to_entity(&row, &schema)?;
                if let Ok(item) = T::from_entity(entity) {
                    results.push(item);
                }
            }

            return Ok(results);
        }

        // Fall back to in-memory filtering if no SQL predicate
        let all_items = self.source.get_all().await?;
        Ok(all_items
            .into_iter()
            .filter(|item| predicate.test(item))
            .collect())
    }
}

// Implement ChangeNotifications<StorageEntity> via TursoBackend
// TODO: Option A - Each QueryableCache filters by table name
// This is inefficient when multiple caches share the same backend (all receive all events).
// Consider optimizing to Option B (table-specific subscriptions) in the future.
#[async_trait]
impl<S, T> ChangeNotifications<StorageEntity> for QueryableCache<S, T>
where
    S: DataSource<T>,
    T: HasSchema + Send + Sync + 'static,
{
    async fn watch_changes_since(
        &self,
        _position: StreamPosition,
    ) -> Pin<Box<dyn Stream<Item = std::result::Result<Vec<Change<StorageEntity>>, ApiError>> + Send>> {
        // IMPORTANT: No auto-sync here - caller must sync first
        // This allows offline startup without sync attempts

        let schema = T::schema();
        let table_name = schema.table_name.clone();
        let backend = self.backend.read().await;

        // Get CDC stream from TursoBackend
        let (cdc_conn, row_change_stream) = match backend.row_changes() {
            Ok(result) => result,
            Err(e) => {
                // Return an error stream if we can't get the CDC stream
                let error = ApiError::InternalError { message: e.to_string() };
                return Box::pin(tokio_stream::once(Err(error)));
            }
        };

        // Store connection to keep it alive for CDC callbacks
        // CRITICAL: The connection MUST stay alive for the callback closure to stay alive
        // The callback closure captures the channel sender (tx), which closes the stream if dropped
        // We keep it in an Arc<Mutex> and move it into the stream state
        let conn_guard = Arc::new(tokio::sync::Mutex::new(cdc_conn));

        // TODO: Option A - Filter stream for this table and convert RowChange to Change<StorageEntity>
        // This is inefficient when multiple QueryableCache instances share the same backend.
        // Consider optimizing to Option B (table-specific subscriptions) in the future.
        use tokio_stream::StreamExt;
        use crate::storage::turso::{RowChange, ChangeData};

        // Create a wrapper stream that holds the connection to keep it alive
        // The connection must stay alive for CDC callbacks to work
        let table_name_clone = table_name.clone();

        // Use a custom stream wrapper that holds the connection
        // This ensures the connection stays alive for the lifetime of the stream
        struct ConnectionStream<S> {
            _conn: Arc<tokio::sync::Mutex<turso::Connection>>,
            stream: S,
        }

        impl<S> Stream for ConnectionStream<S>
        where
            S: Stream + Unpin,
        {
            type Item = S::Item;

            fn poll_next(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Option<Self::Item>> {
                Pin::new(&mut self.stream).poll_next(cx)
            }
        }

        let wrapped_stream = ConnectionStream {
            _conn: conn_guard,
            stream: row_change_stream,
        };

        let filtered_stream = wrapped_stream
            .filter(move |row_change: &RowChange| {
                row_change.relation_name == table_name_clone
            })
            .map(move |row_change: RowChange| {
                // Convert RowChange to Change<StorageEntity>
                // StorageEntity is HashMap<String, Value>, so we can use data directly
                match row_change.change {
                    ChangeData::Created { data, origin } => {
                        Ok(vec![Change::Created {
                            data, // data is already HashMap<String, Value> = StorageEntity
                            origin,
                        }])
                    }
                    ChangeData::Updated { id: _rowid, data, origin } => {
                        // Extract entity ID from data, not ROWID
                        let entity_id = data.get("id")
                            .and_then(|v| v.as_string())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| "unknown".to_string());
                        Ok(vec![Change::Updated {
                            id: entity_id,
                            data, // data is already HashMap<String, Value> = StorageEntity
                            origin,
                        }])
                    }
                    ChangeData::Deleted { id: _rowid, origin } => {
                        // TODO: For deletes, we need the entity ID, not ROWID
                        // This is a limitation - we may need to track entity_id separately
                        // For now, use a placeholder - proper fix requires enhancing CDC system
                        Ok(vec![Change::Deleted {
                            id: format!("rowid_{}", _rowid), // Placeholder - not ideal
                            origin,
                        }])
                    }
                }
            });

        Box::pin(filtered_stream)
    }

    async fn get_current_version(&self) -> std::result::Result<Vec<u8>, ApiError> {
        // Return empty version vector for now
        // Could be enhanced to track sync tokens
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::traits::{FieldSchema, SqlPredicate};
    use crate::core::value::Value as CoreValue;
    use crate::storage::types::Value;

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
    }

    #[async_trait]
    impl CrudOperationProvider<TestTask> for InMemoryDataSource {
        async fn set_field(&self, id: &str, field: &str, value: Value) -> Result<()> {
            let mut items = self.items.write().await;
            if let Some(task) = items.iter_mut().find(|t| t.id == id) {
                match field {
                    "title" => {
                        if let Value::String(s) = value {
                            task.title = s;
                        }
                    }
                    "priority" => {
                        if let Value::Integer(i) = value {
                            task.priority = i;
                        }
                    }
                    _ => {}
                }
            }
            Ok(())
        }

        async fn create(&self, fields: HashMap<String, Value>) -> Result<String> {
            let id = fields.get("id")
                .and_then(|v| v.as_string().map(|s| s.to_string()))
                .unwrap_or_else(|| format!("task-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
            let title = fields.get("title")
                .and_then(|v| v.as_string().map(|s| s.to_string()))
                .unwrap_or_else(|| "Untitled".to_string());
            let priority = fields.get("priority")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);

            let task = TestTask {
                id: id.clone(),
                title,
                priority,
            };
            self.items.write().await.push(task);
            Ok(id)
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
                vec![CoreValue::Integer(self.min)],
            ))
        }
    }

    #[tokio::test]
    async fn test_queryable_cache_creation() {
        let source = InMemoryDataSource::new();
        let cache = QueryableCache::new(source).await.unwrap();
        // Verify backend exists and can get a connection
        let backend = cache.backend.read().await;
        let conn = backend.get_connection();
        assert!(conn.is_ok());
    }

    #[tokio::test]
    async fn test_queryable_cache_with_database() {
        let source = InMemoryDataSource::new();
        let cache = QueryableCache::with_database(source, ":memory:")
            .await
            .unwrap();
        // Verify backend exists and can get a connection
        let backend = cache.backend.read().await;
        let conn = backend.get_connection();
        assert!(conn.is_ok());
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

        let mut fields = HashMap::new();
        fields.insert("id".to_string(), Value::String(task.id.clone()));
        fields.insert("title".to_string(), Value::String(task.title.clone()));
        fields.insert("priority".to_string(), Value::Integer(task.priority));
        let id = cache.create(fields).await.unwrap();
        assert_eq!(id, "1");

        let retrieved = cache.get_by_id(&id).await.unwrap();
        assert_eq!(retrieved, Some(task));
    }

    #[tokio::test]
    async fn test_sync() {
        let source = InMemoryDataSource::new();

        let mut fields1 = HashMap::new();
        fields1.insert("id".to_string(), Value::String("1".to_string()));
        fields1.insert("title".to_string(), Value::String("Task 1".to_string()));
        fields1.insert("priority".to_string(), Value::Integer(3));
        source.create(fields1).await.unwrap();

        let mut fields2 = HashMap::new();
        fields2.insert("id".to_string(), Value::String("2".to_string()));
        fields2.insert("title".to_string(), Value::String("Task 2".to_string()));
        fields2.insert("priority".to_string(), Value::Integer(7));
        source.create(fields2).await.unwrap();

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

        let mut fields1 = HashMap::new();
        fields1.insert("id".to_string(), Value::String("1".to_string()));
        fields1.insert("title".to_string(), Value::String("Low Priority".to_string()));
        fields1.insert("priority".to_string(), Value::Integer(2));
        cache.create(fields1).await.unwrap();

        let mut fields2 = HashMap::new();
        fields2.insert("id".to_string(), Value::String("2".to_string()));
        fields2.insert("title".to_string(), Value::String("High Priority".to_string()));
        fields2.insert("priority".to_string(), Value::Integer(8));
        cache.create(fields2).await.unwrap();

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

        let mut fields = HashMap::new();
        fields.insert("id".to_string(), Value::String("1".to_string()));
        fields.insert("title".to_string(), Value::String("Original".to_string()));
        fields.insert("priority".to_string(), Value::Integer(3));
        cache.create(fields).await.unwrap();

        cache.set_field("1", "title", Value::String("Updated".to_string())).await.unwrap();
        cache.set_field("1", "priority", Value::Integer(5)).await.unwrap();

        let updated = cache.get_by_id("1").await.unwrap().unwrap();
        assert_eq!(updated.title, "Updated");

        cache.delete("1").await.unwrap();
        let deleted = cache.get_by_id("1").await.unwrap();
        assert!(deleted.is_none());
    }
}
