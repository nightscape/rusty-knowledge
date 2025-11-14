use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use anyhow::Result;
use tokio::sync::RwLock;

use crate::storage::turso::{TursoBackend, RowChangeStream};
use crate::storage::types::{Value, StorageEntity};
use crate::operations::OperationRegistry;
use crate::api::operation_dispatcher::OperationDispatcher;
use crate::core::datasource::OperationProvider;
use query_render::RenderSpec;

/// CDC event representing a row change
/// flutter_rust_bridge:non_opaque
#[derive(Debug, Clone)]
pub enum RowEvent {
    Added { id: String, data: HashMap<String, Value> },
    Updated { id: String, data: HashMap<String, Value> },
    Removed { id: String },
}

/// UI state containing cursor position and focused block
/// flutter_rust_bridge:non_opaque
#[derive(Debug, Clone)]
pub struct UiState {
    pub cursor_pos: Option<CursorPosition>,
    pub focused_id: Option<String>,
}

/// Cursor position within a block
/// flutter_rust_bridge:non_opaque
#[derive(Debug, Clone)]
pub struct CursorPosition {
    pub block_id: String,
    pub offset: u32,
}

/// Main render engine managing database, query compilation, and operations
pub struct RenderEngine {
    backend: Arc<RwLock<TursoBackend>>,
    ui_state: Arc<RwLock<UiState>>,
    operations: Arc<OperationRegistry>, // Legacy registry (kept for backward compatibility)
    dispatcher: Arc<RwLock<OperationDispatcher>>, // New trait-based operation dispatcher
    table_to_entity_map: Arc<RwLock<HashMap<String, String>>>, // Maps table names to entity names
    // CDC connection kept alive for streaming
    // CRITICAL: This must stay alive for CDC callbacks to work
    // The callback closure captures the channel sender, which closes the stream if dropped
    _cdc_conn: Option<Arc<tokio::sync::Mutex<turso::Connection>>>,
}

impl RenderEngine {
    /// Create a new render engine with a database at the given path
    ///
    /// Automatically registers default block operations:
    /// - UpdateField
    /// - Indent
    /// - Outdent
    /// - MoveBlock
    pub async fn new(db_path: PathBuf) -> Result<Self> {
        let backend = TursoBackend::new(db_path).await?;
        let operations = Self::create_default_registry();

        Ok(Self {
            backend: Arc::new(RwLock::new(backend)),
            ui_state: Arc::new(RwLock::new(UiState {
                cursor_pos: None,
                focused_id: None,
            })),
            operations: Arc::new(operations),
            dispatcher: Arc::new(RwLock::new(OperationDispatcher::new())),
            table_to_entity_map: Arc::new(RwLock::new(HashMap::new())),
            _cdc_conn: None,
        })
    }

    /// Create a new in-memory render engine for testing
    ///
    /// Automatically registers default block operations.
    pub async fn new_in_memory() -> Result<Self> {
        let backend = TursoBackend::new_in_memory().await?;
        let operations = Self::create_default_registry();

        Ok(Self {
            backend: Arc::new(RwLock::new(backend)),
            ui_state: Arc::new(RwLock::new(UiState {
                cursor_pos: None,
                focused_id: None,
            })),
            operations: Arc::new(operations),
            dispatcher: Arc::new(RwLock::new(OperationDispatcher::new())),
            table_to_entity_map: Arc::new(RwLock::new(HashMap::new())),
            _cdc_conn: None,
        })
    }
    // TODO: Get rid of this, no defaults should be hard-coded
    /// Create and populate the default operation registry
    fn create_default_registry() -> OperationRegistry {
        use crate::operations::block_ops::{UpdateField, SplitBlock};
        use crate::operations::block_movements::{Indent, Outdent, MoveBlock, MoveUp, MoveDown};

        let mut registry = OperationRegistry::new();
        registry.register(Arc::new(UpdateField));
        registry.register(Arc::new(SplitBlock));
        registry.register(Arc::new(Indent));
        registry.register(Arc::new(Outdent));
        registry.register(Arc::new(MoveBlock));
        registry.register(Arc::new(MoveUp));
        registry.register(Arc::new(MoveDown));
        registry
    }

    /// Compile a PRQL query with render() into SQL and UI specification
    ///
    /// Automatically infers operation wirings from PRQL lineage analysis.
    /// Widgets that reference direct table columns will have operations populated.
    ///
    /// This method:
    /// 1. Parses the PRQL query and extracts table name
    /// 2. Looks up entity_name from table_to_entity_map
    /// 3. Extracts available columns from the query
    /// 4. Queries OperationDispatcher for compatible operations
    /// 5. Replaces placeholder operations with real OperationDescriptors
    pub fn compile_query(&self, prql: String) -> Result<(String, RenderSpec)> {
        // Step 1: Parse query and get basic structure with placeholder operations
        let (sql, mut render_spec) = query_render::parse_query_render_with_operations(&prql)?;

        // Step 2: Extract table name from query (needed for entity lookup)
        let table_name = self.extract_table_name_from_prql(&prql)?;

        // Step 3: Walk the tree and enhance operations with real descriptors from dispatcher
        // Extract available columns from the query for each function call
        self.enhance_operations_with_dispatcher(&mut render_spec.root, &table_name)?;

        Ok((sql, render_spec))
    }

    /// Extract table name from PRQL query string
    fn extract_table_name_from_prql(&self, prql: &str) -> Result<String> {
        // Simple extraction - look for "from <table_name>" pattern
        // Split by whitespace and look for "from" followed by a word
        let words: Vec<&str> = prql.split_whitespace().collect();
        for (i, word) in words.iter().enumerate() {
            if word.eq_ignore_ascii_case("from") && i + 1 < words.len() {
                return Ok(words[i + 1].to_string());
            }
        }
        anyhow::bail!("Could not extract table name from PRQL query")
    }

    /// Enhance operations in the render tree with real descriptors from OperationDispatcher
    ///
    /// Walks the tree and for each FunctionCall with operations:
    /// 1. Extracts available columns from the function call context
    /// 2. Looks up entity_name from table_to_entity_map (using table from descriptor)
    /// 3. Queries dispatcher.find_operations() with entity_name and available columns
    /// 4. Replaces placeholder operations with real ones
    fn enhance_operations_with_dispatcher(
        &self,
        expr: &mut query_render::RenderExpr,
        table_name: &str,
    ) -> Result<()> {
        match expr {
            query_render::RenderExpr::FunctionCall { name: _, args, operations } => {
                // Extract available columns from this function call's arguments
                let _available_args = self.extract_available_columns_from_args(args);

                // Note: Operation enhancement is skipped because compile_query is synchronous
                // but dispatcher access requires async. Placeholder operations from
                // parse_query_render_with_operations will be used instead.
                // TODO: Make compile_query async or implement a different enhancement strategy

                // Recurse into nested expressions
                for arg in args.iter_mut() {
                    self.enhance_operations_with_dispatcher(&mut arg.value, table_name)?;
                }
            }
            query_render::RenderExpr::Array { items } => {
                for item in items.iter_mut() {
                    self.enhance_operations_with_dispatcher(item, table_name)?;
                }
            }
            query_render::RenderExpr::BinaryOp { left, right, .. } => {
                self.enhance_operations_with_dispatcher(left, table_name)?;
                self.enhance_operations_with_dispatcher(right, table_name)?;
            }
            query_render::RenderExpr::Object { fields } => {
                for value in fields.values_mut() {
                    self.enhance_operations_with_dispatcher(value, table_name)?;
                }
            }
            _ => {}  // ColumnRef, Literal - no recursion needed
        }
        Ok(())
    }

    /// Extract available column names from function call arguments
    ///
    /// This extracts column names that are available in the context, which can be used
    /// to filter operations (operations that require columns not available won't be shown).
    fn extract_available_columns_from_args(
        &self,
        args: &[query_render::Arg],
    ) -> Vec<String> {
        let mut columns = Vec::new();
        for arg in args {
            match &arg.value {
                query_render::RenderExpr::ColumnRef { name } => {
                    // Strip "this." prefix if present
                    let col_name = name.strip_prefix("this.").unwrap_or(name);
                    columns.push(col_name.to_string());
                }
                _ => {
                    // Recurse into nested expressions
                    self.collect_columns_from_expr(&arg.value, &mut columns);
                }
            }
        }
        // Always include "id" as it's typically available
        if !columns.contains(&"id".to_string()) {
            columns.push("id".to_string());
        }
        columns
    }

    /// Recursively collect column names from an expression
    fn collect_columns_from_expr(
        &self,
        expr: &query_render::RenderExpr,
        columns: &mut Vec<String>,
    ) {
        match expr {
            query_render::RenderExpr::ColumnRef { name } => {
                let col_name = name.strip_prefix("this.").unwrap_or(name);
                if !columns.contains(&col_name.to_string()) {
                    columns.push(col_name.to_string());
                }
            }
            query_render::RenderExpr::FunctionCall { args, .. } => {
                for arg in args {
                    self.collect_columns_from_expr(&arg.value, columns);
                }
            }
            query_render::RenderExpr::Array { items } => {
                for item in items {
                    self.collect_columns_from_expr(item, columns);
                }
            }
            query_render::RenderExpr::BinaryOp { left, right, .. } => {
                self.collect_columns_from_expr(left, columns);
                self.collect_columns_from_expr(right, columns);
            }
            query_render::RenderExpr::Object { fields } => {
                for value in fields.values() {
                    self.collect_columns_from_expr(value, columns);
                }
            }
            _ => {}  // Literal - no columns
        }
    }

    /// Execute a SQL query and return the result set
    ///
    /// Supports parameter binding by replacing `$param_name` placeholders with actual values.
    /// Parameters are bound safely using SQL parameter binding to prevent SQL injection.
    pub async fn execute_query(
        &self,
        sql: String,
        params: HashMap<String, Value>,
    ) -> Result<Vec<StorageEntity>> {
        let backend = self.backend.read().await;
        backend.execute_sql(&sql, params).await
            .map_err(|e| anyhow::anyhow!("SQL execution failed: {}", e))
    }

    /// Get current UI state
    pub async fn get_ui_state(&self) -> UiState {
        self.ui_state.read().await.clone()
    }

    /// Update UI state
    pub async fn set_ui_state(&self, state: UiState) -> Result<()> {
        let mut ui_state = self.ui_state.write().await;
        *ui_state = state;
        Ok(())
    }

    /// Watch a query for changes via CDC streaming
    ///
    /// Returns a stream of RowChange events from the underlying database.
    /// The CDC connection is stored in the RenderEngine to keep it alive.
    ///
    /// Note: Currently returns changes from all tables. Full implementation in Phase 1.3
    /// will create materialized views from SQL queries and filter changes appropriately.
    pub async fn watch_query(
        &mut self,
        sql: String,
        _params: HashMap<String, Value>,
    ) -> Result<RowChangeStream> {
        // Generate a unique view name for this query
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        sql.hash(&mut hasher);
        let view_name = format!("watch_view_{:x}", hasher.finish());

        // Create materialized view for the query
        let backend = self.backend.read().await;
        let conn = backend.get_connection()
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        // Drop the view if it exists
        // Turso doesn't support IF NOT EXISTS for materialized views
        // Dropping the view will automatically cascade to its internal state table
        let _ = conn.execute(&format!("DROP VIEW IF EXISTS {}", view_name), ()).await;

        // Check if view exists and drop without IF EXISTS if needed
        let check_view_sql = format!(
            "SELECT name FROM sqlite_master WHERE type='view' AND name='{}'",
            view_name
        );
        if let Ok(mut stmt) = conn.prepare(&check_view_sql).await {
            if stmt.query_row(()).await.is_ok() {
                // View still exists, drop it without IF EXISTS
                let _ = conn.execute(&format!("DROP VIEW {}", view_name), ()).await;
            }
        }

        // Create the materialized view
        let create_view_sql = format!("CREATE MATERIALIZED VIEW {} AS {}", view_name, sql);
        conn.execute(&create_view_sql, ())
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create materialized view: {}", e))?;

        drop(backend); // Release read lock before acquiring for row_changes

        // Set up change stream for the view
        let backend = self.backend.read().await;
        let (cdc_conn, stream) = backend.row_changes()
            .map_err(|e| anyhow::anyhow!("Failed to set up CDC stream: {}", e))?;

        // Store the connection to keep it alive for CDC callbacks
        // CRITICAL: The connection MUST stay alive for the callback closure to stay alive
        // The callback closure captures the channel sender (tx), which closes the stream if dropped
        self._cdc_conn = Some(Arc::new(tokio::sync::Mutex::new(cdc_conn)));

        Ok(stream)
    }

    /// Convenience method that compiles a PRQL query, executes it, and sets up CDC streaming
    ///
    /// This combines `compile_query`, `execute_query`, and `watch_query` into a single call.
    /// Returns the render specification, current table data, and a stream of ongoing changes.
    ///
    /// # Returns
    /// A tuple containing:
    /// - `RenderSpec`: UI rendering specification from the PRQL query
    /// - `Vec<Entity>`: Current query results
    /// - `RowChangeStream`: Stream of ongoing changes to the query results
    pub async fn query_and_watch(
        &mut self,
        prql: String,
        params: HashMap<String, Value>,
    ) -> Result<(RenderSpec, Vec<StorageEntity>, RowChangeStream)> {
        let (sql, render_spec) = self.compile_query(prql)?;
        let current_data = self.execute_query(sql.clone(), params.clone()).await?;
        let change_stream = self.watch_query(sql, params).await?;

        Ok((render_spec, current_data, change_stream))
    }

    /// Execute a block operation
    ///
    /// This method provides a clean interface for executing operations without exposing
    /// the internal TursoBackend. It handles locking and passes the current UI state.
    ///
    /// # Arguments
    /// * `op_name` - Name of the operation to execute (e.g., "indent", "outdent", "move_block")
    /// * `params` - Parameters for the operation (typically includes block ID and operation-specific fields)
    ///
    /// # Returns
    /// Result indicating success or failure. On success, UI should re-query to get updated data.
    ///
    /// # Example
    /// ```no_run
    /// use std::collections::HashMap;
    /// use rusty_knowledge::api::render_engine::RenderEngine;
    /// use rusty_knowledge::storage::types::Value;
    ///
    /// # async fn example() -> anyhow::Result<()> {
    /// let engine = RenderEngine::new_in_memory().await?;
    ///
    /// let mut params = HashMap::new();
    /// params.insert("id".to_string(), Value::String("block-1".to_string()));
    ///
    /// engine.execute_operation("indent", params).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn execute_operation(
        &self,
        op_name: &str,
        params: StorageEntity,
    ) -> Result<()> {
        let mut backend = self.backend.write().await;
        let ui_state = self.ui_state.read().await.clone();

        self.operations
            .execute(op_name, &params, &ui_state, &mut *backend)
            .await
            .map_err(|e| anyhow::anyhow!("Operation '{}' failed: {}", op_name, e))
    }

    /// Register a custom operation
    ///
    /// This allows registering additional operations beyond the defaults.
    /// Can only be called before the RenderEngine is shared via Arc.
    ///
    /// # Example
    /// ```no_run
    /// use std::sync::Arc;
    /// use rusty_knowledge::api::render_engine::RenderEngine;
    /// use rusty_knowledge::operations::block_ops::UpdateField;
    ///
    /// # async fn example() -> anyhow::Result<()> {
    /// let mut engine = RenderEngine::new_in_memory().await?;
    ///
    /// // Register custom operation before sharing
    /// engine.register_operation(Arc::new(UpdateField))?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn register_operation(&mut self, operation: Arc<dyn crate::operations::Operation>) -> Result<()> {
        Arc::get_mut(&mut self.operations)
            .ok_or_else(|| anyhow::anyhow!("Cannot register operations after RenderEngine is shared"))?
            .register(operation);
        Ok(())
    }

    pub fn available_operations(&self) -> Vec<String> {
        self.operations.operation_names()
            .into_iter()
            .map(|s| s.to_string())
            .collect()
    }

    pub fn has_operation(&self, op_name: &str) -> bool {
        self.operations.has_operation(op_name)
    }

    /// Execute a closure with read access to the backend
    ///
    /// This is a helper for testing and advanced use cases where direct
    /// backend access is needed.
    pub async fn with_backend_read<F, Fut, R>(&self, f: F) -> R
    where
        F: FnOnce(&TursoBackend) -> Fut,
        Fut: std::future::Future<Output = R>,
    {
        let backend = self.backend.read().await;
        f(&*backend).await
    }

    /// Execute a closure with write access to the backend
    ///
    /// This is a helper for testing and advanced use cases where direct
    /// backend access is needed.
    pub async fn with_backend_write<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut TursoBackend) -> R,
    {
        let mut backend = self.backend.write().await;
        f(&mut *backend)
    }

    /// Register an OperationProvider for an entity type
    ///
    /// This registers a provider (typically a QueryableCache<T>) with the dispatcher.
    /// The provider will handle operations for the specified entity_name.
    ///
    /// # Arguments
    /// * `entity_name` - Entity identifier (e.g., "todoist-task", "logseq-block")
    /// * `provider` - The OperationProvider instance to register
    ///
    /// # Example
    /// ```no_run
    /// use std::sync::Arc;
    /// use rusty_knowledge::api::render_engine::RenderEngine;
    /// use rusty_knowledge::api::operation_dispatcher::OperationDispatcher;
    /// use rusty_knowledge::core::datasource::OperationProvider;
    ///
    /// # async fn example() -> anyhow::Result<()> {
    /// let engine = RenderEngine::new_in_memory().await?;
    /// // ... create provider ...
    /// // engine.register_provider("todoist-task".to_string(), provider).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn register_provider(
        &self,
        entity_name: String,
        provider: Arc<dyn OperationProvider>,
    ) -> Result<()> {
        let mut dispatcher = self.dispatcher.write().await;
        dispatcher.register(entity_name.clone(), provider);
        Ok(())
    }

    /// Unregister an OperationProvider for an entity type
    ///
    /// # Arguments
    /// * `entity_name` - Entity identifier to unregister
    ///
    /// # Returns
    /// `true` if a provider was removed, `false` if no provider was registered
    pub async fn unregister_provider(&self, entity_name: &str) -> bool {
        let mut dispatcher = self.dispatcher.write().await;
        dispatcher.unregister(entity_name)
    }

    /// Map a table name to an entity name
    ///
    /// This mapping is used during query compilation to determine which
    /// entity type operations are available for a given table.
    ///
    /// # Arguments
    /// * `table_name` - Database table name (e.g., "todoist_tasks", "logseq_blocks")
    /// * `entity_name` - Entity identifier (e.g., "todoist-task", "logseq-block")
    pub async fn map_table_to_entity(&self, table_name: String, entity_name: String) {
        let mut map = self.table_to_entity_map.write().await;
        map.insert(table_name, entity_name);
    }

    /// Get the entity name for a table
    ///
    /// # Arguments
    /// * `table_name` - Database table name
    ///
    /// # Returns
    /// `Some(entity_name)` if mapped, `None` otherwise
    pub async fn get_entity_for_table(&self, table_name: &str) -> Option<String> {
        let map = self.table_to_entity_map.read().await;
        map.get(table_name).cloned()
    }

    /// Get a read-only reference to the operation dispatcher
    ///
    /// This allows querying available operations without mutating the dispatcher.
    pub async fn get_dispatcher(&self) -> tokio::sync::RwLockReadGuard<'_, OperationDispatcher> {
        self.dispatcher.read().await
    }

    /// Get a clone of the backend Arc
    ///
    /// This allows sharing the backend with QueryableCache instances.
    pub fn get_backend(&self) -> Arc<RwLock<TursoBackend>> {
        self.backend.clone()
    }

    /// Register a syncable provider (delegates to dispatcher)
    ///
    /// # Arguments
    /// * `provider_name` - Provider identifier (e.g., "todoist", "jira")
    /// * `provider` - The SyncableProvider instance to register
    pub async fn register_syncable_provider(
        &self,
        provider_name: String,
        provider: Arc<tokio::sync::Mutex<dyn crate::core::datasource::SyncableProvider>>,
    ) {
        let mut dispatcher = self.dispatcher.write().await;
        dispatcher.register_syncable_provider(provider_name, provider);
    }

    /// Sync all registered providers (delegates to dispatcher)
    pub async fn sync_all_providers(&self) -> Result<()> {
        let dispatcher = self.dispatcher.read().await;
        dispatcher.sync_all_providers().await
            .map_err(|e| anyhow::anyhow!("Failed to sync providers: {}", e))
    }

    /// Sync a specific provider (delegates to dispatcher)
    ///
    /// # Arguments
    /// * `provider_name` - Provider identifier (e.g., "todoist")
    pub async fn sync_provider(&self, provider_name: &str) -> Result<()> {
        let dispatcher = self.dispatcher.read().await;
        dispatcher.sync_provider(provider_name).await
            .map_err(|e| anyhow::anyhow!("Failed to sync provider {}: {}", provider_name, e))
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_render_engine_creation() {
        let engine = RenderEngine::new_in_memory().await;
        assert!(engine.is_ok());
    }

    #[tokio::test]
    async fn test_compile_simple_query() {
        let engine = RenderEngine::new_in_memory().await.unwrap();

        let prql = r#"
            from blocks
            render (text "Hello")
        "#;

        let result = engine.compile_query(prql.to_string());
        assert!(result.is_ok());

        let (_sql, spec) = result.unwrap();
        match spec.root {
            query_render::RenderExpr::FunctionCall { name, .. } => {
                assert_eq!(name, "text");
            }
            _ => panic!("Expected function call"),
        }
    }

    #[tokio::test]
    async fn test_ui_state_management() {
        let engine = RenderEngine::new_in_memory().await.unwrap();

        let state = UiState {
            cursor_pos: Some(CursorPosition {
                block_id: "block-1".to_string(),
                offset: 42,
            }),
            focused_id: Some("block-1".to_string()),
        };

        engine.set_ui_state(state.clone()).await.unwrap();
        let retrieved = engine.get_ui_state().await;

        assert_eq!(retrieved.focused_id, state.focused_id);
        assert!(retrieved.cursor_pos.is_some());
        assert_eq!(retrieved.cursor_pos.unwrap().offset, 42);
    }

    #[tokio::test]
    async fn test_execute_query_with_parameters() {
        let engine = RenderEngine::new_in_memory().await.unwrap();
        let backend = engine.backend.write().await;

        // Create a test table
        let conn = backend.get_connection().unwrap();
        conn.execute("CREATE TABLE test_blocks (id TEXT PRIMARY KEY, title TEXT, depth INTEGER)", ())
            .await
            .unwrap();

        // Insert test data
        conn.execute(
            "INSERT INTO test_blocks (id, title, depth) VALUES ('block-1', 'Test Block', 0)",
            ()
        ).await.unwrap();

        conn.execute(
            "INSERT INTO test_blocks (id, title, depth) VALUES ('block-2', 'Nested Block', 1)",
            ()
        ).await.unwrap();

        drop(conn);
        drop(backend);

        // Test query with parameter binding
        let mut params = HashMap::new();
        params.insert("min_depth".to_string(), Value::Integer(0));

        let sql = "SELECT id, title, depth FROM test_blocks WHERE depth >= $min_depth ORDER BY id";
        let results = engine.execute_query(sql.to_string(), params).await.unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].get("id").unwrap().as_string(), Some("block-1"));
        assert_eq!(results[1].get("id").unwrap().as_string(), Some("block-2"));
    }

    #[tokio::test]
    async fn test_parameter_binding() {
        let engine = RenderEngine::new_in_memory().await.unwrap();
        let backend = engine.backend.write().await;

        let conn = backend.get_connection().unwrap();
        conn.execute("CREATE TABLE users (id TEXT, name TEXT, age INTEGER)", ())
            .await
            .unwrap();

        conn.execute(
            "INSERT INTO users VALUES ('u1', 'Alice', 30), ('u2', 'Bob', 25), ('u3', 'Charlie', 35)",
            ()
        ).await.unwrap();

        drop(conn);
        drop(backend);

        // Test multiple parameters
        let mut params = HashMap::new();
        params.insert("min_age".to_string(), Value::Integer(25));
        params.insert("max_age".to_string(), Value::Integer(35));

        let sql = "SELECT name, age FROM users WHERE age >= $min_age AND age <= $max_age ORDER BY age";
        let results = engine.execute_query(sql.to_string(), params).await.unwrap();

        assert_eq!(results.len(), 3);
        assert_eq!(results[0].get("name").unwrap().as_string(), Some("Bob"));
        assert_eq!(results[2].get("name").unwrap().as_string(), Some("Charlie"));
    }

    #[tokio::test]
    async fn test_execute_operation() {
        let engine = RenderEngine::new_in_memory().await.unwrap();
        let backend = engine.backend.write().await;

        // Create test table
        let conn = backend.get_connection().unwrap();
        conn.execute(
            "CREATE TABLE blocks (id TEXT PRIMARY KEY, content TEXT, completed BOOLEAN)",
            ()
        ).await.unwrap();

        conn.execute(
            "INSERT INTO blocks (id, content, completed) VALUES ('block-1', 'Test task', 0)",
            ()
        ).await.unwrap();

        drop(conn);
        drop(backend);

        // Execute operation to update completed field
        // Note: No need to set up registry - it's already initialized in RenderEngine!
        let mut params = HashMap::new();
        params.insert("id".to_string(), Value::String("block-1".to_string()));
        params.insert("field".to_string(), Value::String("completed".to_string()));
        params.insert("value".to_string(), Value::Boolean(true));

        let result = engine.execute_operation("update_field", params).await;
        assert!(result.is_ok(), "Operation should succeed: {:?}", result);

        // Verify the update
        let sql = "SELECT id, completed FROM blocks WHERE id = 'block-1'";
        let results = engine.execute_query(sql.to_string(), HashMap::new()).await.unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].get("id").unwrap().as_string(), Some("block-1"));

        // SQLite stores booleans as integers (0/1), so check for Integer value
        match results[0].get("completed").unwrap() {
            Value::Integer(i) => assert_eq!(*i, 1, "Expected completed=1 (true)"),
            Value::Boolean(b) => assert!(b, "Expected completed=true"),
            other => panic!("Unexpected value type for completed: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_execute_operation_failure() {
        let engine = RenderEngine::new_in_memory().await.unwrap();

        // Try to execute non-existent operation
        let params = HashMap::new();
        let result = engine.execute_operation("nonexistent", params).await;

        assert!(result.is_err(), "Should fail for non-existent operation");
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("nonexistent"), "Error should mention operation name");
    }

    #[tokio::test]
    async fn test_execute_operation_with_ui_state() {
        let engine = RenderEngine::new_in_memory().await.unwrap();
        let backend = engine.backend.write().await;

        // Create test table
        let conn = backend.get_connection().unwrap();
        conn.execute(
            "CREATE TABLE blocks (id TEXT PRIMARY KEY, content TEXT)",
            ()
        ).await.unwrap();

        conn.execute(
            "INSERT INTO blocks (id, content) VALUES ('block-1', 'Original content')",
            ()
        ).await.unwrap();

        drop(conn);
        drop(backend);

        // Set UI state
        let ui_state = UiState {
            cursor_pos: Some(CursorPosition {
                block_id: "block-1".to_string(),
                offset: 5,
            }),
            focused_id: Some("block-1".to_string()),
        };
        engine.set_ui_state(ui_state).await.unwrap();

        // Execute operation (registry already initialized in RenderEngine)
        let mut params = HashMap::new();
        params.insert("id".to_string(), Value::String("block-1".to_string()));
        params.insert("field".to_string(), Value::String("content".to_string()));
        params.insert("value".to_string(), Value::String("Updated content".to_string()));

        let result = engine.execute_operation("update_field", params).await;
        assert!(result.is_ok());

        // Verify the update
        let sql = "SELECT content FROM blocks WHERE id = 'block-1'";
        let results = engine.execute_query(sql.to_string(), HashMap::new()).await.unwrap();
        assert_eq!(results[0].get("content").unwrap().as_string(), Some("Updated content"));

        // Verify UI state is preserved
        let final_state = engine.get_ui_state().await;
        assert_eq!(final_state.focused_id, Some("block-1".to_string()));
        assert!(final_state.cursor_pos.is_some());
    }

    #[tokio::test]
    async fn test_register_custom_operation() {
        use crate::operations::block_ops::UpdateField;

        let mut engine = RenderEngine::new_in_memory().await.unwrap();

        // Register custom operation (UpdateField already registered, but this tests the mechanism)
        let result = engine.register_operation(Arc::new(UpdateField));
        assert!(result.is_ok(), "Should be able to register operation before sharing");

        // Try to register after sharing (this should fail)
        let _engine_arc = Arc::new(engine);
        // Can't call register_operation on Arc<RenderEngine> - this is intentional
        // to prevent runtime panics from Arc::get_mut
    }

    #[tokio::test]
    async fn test_operations_inference() {
        let engine = RenderEngine::new_in_memory().await.unwrap();

        // PRQL query with widgets that reference direct table columns
        let prql = r#"
from blocks
select {id, content, completed}
render (list item_template:(row (checkbox checked:this.completed) (text content:this.content)))
        "#;

        let result = engine.compile_query(prql.to_string());
        assert!(result.is_ok(), "Should compile query with render: {:?}", result.err());

        let (_sql, spec) = result.unwrap();

        // Debug: print the tree structure
        eprintln!("Root expr: {:#?}", spec.root);

        // Helper to find all widgets with operations in the tree
        fn find_all_operations(expr: &query_render::RenderExpr) -> Vec<&query_render::OperationWiring> {
            let mut ops = Vec::new();
            match expr {
                query_render::RenderExpr::FunctionCall { operations, args, .. } => {
                    ops.extend(operations.iter());
                    for arg in args {
                        ops.extend(find_all_operations(&arg.value));
                    }
                }
                query_render::RenderExpr::Array { items } => {
                    for item in items {
                        ops.extend(find_all_operations(item));
                    }
                }
                query_render::RenderExpr::BinaryOp { left, right, .. } => {
                    ops.extend(find_all_operations(left));
                    ops.extend(find_all_operations(right));
                }
                _ => {}
            }
            ops
        }

        let all_ops = find_all_operations(&spec.root);
        assert!(!all_ops.is_empty(), "Should have auto-inferred operations in tree");

        // Find checkbox operation
        let checkbox_op = all_ops.iter()
            .find(|op| op.widget_type == "checkbox");
        assert!(checkbox_op.is_some(), "Should find checkbox operation");

        let checkbox = checkbox_op.unwrap();
        assert_eq!(checkbox.modified_param, "checked");
        assert_eq!(checkbox.descriptor.table, "blocks");
        assert_eq!(checkbox.descriptor.id_column, "id");

        // Find text operation
        let text_op = all_ops.iter()
            .find(|op| op.widget_type == "text");
        assert!(text_op.is_some(), "Should find text operation");

        let text = text_op.unwrap();
        assert_eq!(text.modified_param, "content");
        assert_eq!(text.descriptor.table, "blocks");
        assert_eq!(text.descriptor.id_column, "id");
    }
}
