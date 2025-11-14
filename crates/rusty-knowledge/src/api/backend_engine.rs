use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use anyhow::Result;
use tokio::sync::RwLock;

use crate::storage::turso::{TursoBackend, RowChangeStream};
use crate::storage::types::{Value, StorageEntity};
use crate::api::operation_dispatcher::OperationDispatcher;
use crate::core::datasource::OperationProvider;
use query_render::RenderSpec;

/// Main render engine managing database, query compilation, and operations
pub struct BackendEngine {
    backend: Arc<RwLock<TursoBackend>>,
    dispatcher: Arc<OperationDispatcher>, // Operation dispatcher for routing operations
    table_to_entity_map: Arc<RwLock<HashMap<String, String>>>, // Maps table names to entity names
    // CDC connection kept alive for streaming
    // CRITICAL: This must stay alive for CDC callbacks to work
    // The callback closure captures the channel sender, which closes the stream if dropped
    // Uses interior mutability so watch_query can take &self
    _cdc_conn: Arc<tokio::sync::Mutex<Option<Arc<tokio::sync::Mutex<turso::Connection>>>>>,
}

impl BackendEngine {
    /// Create BackendEngine from dependencies (for dependency injection)
    ///
    /// This constructor allows creating BackendEngine with pre-constructed dependencies,
    /// useful for dependency injection frameworks.
    pub fn from_dependencies(
        backend: Arc<RwLock<TursoBackend>>,
        dispatcher: Arc<OperationDispatcher>,
    ) -> Result<Self> {

        // Operations are now provided via OperationProvider implementations
        // No legacy operations need to be registered

        Ok(Self {
            backend,
            dispatcher,
            table_to_entity_map: Arc::new(RwLock::new(HashMap::new())),
            _cdc_conn: Arc::new(tokio::sync::Mutex::new(None)),
        })
    }

    // Legacy operations have been migrated to trait-based operations
    // set_field, create, delete are available via CrudOperationProvider
    // Block movement operations are available via MutableBlockDataSource

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
            query_render::RenderExpr::FunctionCall { name: _, args, operations: _ } => {
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

    /// Watch a query for changes via CDC streaming
    ///
    /// Returns a stream of RowChange events from the underlying database.
    /// The CDC connection is stored in the BackendEngine to keep it alive.
    ///
    /// Note: Currently returns changes from all tables. Full implementation in Phase 1.3
    /// will create materialized views from SQL queries and filter changes appropriately.
    pub async fn watch_query(
        &self,
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
        let mut cdc_conn_guard = self._cdc_conn.lock().await;
        *cdc_conn_guard = Some(Arc::new(tokio::sync::Mutex::new(cdc_conn)));

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
        &self,
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
    /// use rusty_knowledge::api::backend_engine::BackendEngine;
    /// use rusty_knowledge::storage::types::Value;
    ///
    /// # async fn example() -> anyhow::Result<()> {
    /// let engine = BackendEngine::new_in_memory().await?;
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
        entity_name: &str,
        op_name: &str,
        params: StorageEntity,
    ) -> Result<()> {
        // Execute via dispatcher using entity_name
        self.dispatcher
            .execute_operation(entity_name, op_name, params)
            .await
            .map_err(|e| anyhow::anyhow!("Operation '{}' on entity '{}' failed: {}", op_name, entity_name, e))
    }

    /// Register a custom OperationProvider
    ///
    /// This allows registering additional operation providers for entity types.
    /// Operations are automatically discovered via the OperationProvider trait.
    ///
    /// # Example
    /// ```no_run
    /// use std::sync::Arc;
    /// use rusty_knowledge::api::backend_engine::BackendEngine;
    /// use rusty_knowledge::core::datasource::OperationProvider;
    ///
    /// # async fn example() -> anyhow::Result<()> {
    /// let engine = BackendEngine::new_in_memory().await?;
    ///
    /// // Register custom provider
    /// // engine.register_provider("my-entity", my_provider).await?;
    /// # Ok(())
    /// # }
    /// ```

    pub async fn available_operations(&self, entity_name: &str) -> Vec<String> {
        self.dispatcher
            .operations()
            .into_iter()
            .filter(|op| op.entity_name == entity_name)
            .map(|op| op.name)
            .collect()
    }

    pub async fn has_operation(&self, entity_name: &str, op_name: &str) -> bool {
        self.dispatcher
            .operations()
            .into_iter()
            .any(|op| op.entity_name == entity_name && op.name == op_name)
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

    /// Get a clone of the operation dispatcher Arc
    ///
    /// This allows querying available operations without mutating the dispatcher.
    pub fn get_dispatcher(&self) -> Arc<OperationDispatcher> {
        self.dispatcher.clone()
    }

    /// Get a clone of the backend Arc
    ///
    /// This allows sharing the backend with QueryableCache instances.
    pub fn get_backend(&self) -> Arc<RwLock<TursoBackend>> {
        self.backend.clone()
    }

    /// Initialize database schema and sample data if the database doesn't exist
    ///
    /// This creates the blocks table and inserts sample data for new databases.
    /// Should be called after creating the BackendEngine for a new database.
    pub async fn initialize_database_if_needed(&self, db_path: &PathBuf) -> Result<()> {
        let db_exists = db_path.exists();

        if !db_exists {
            // Create blocks table schema
            let create_table_sql = r#"
                CREATE TABLE IF NOT EXISTS blocks (
                    id TEXT PRIMARY KEY,
                    parent_id TEXT,
                    depth INTEGER NOT NULL DEFAULT 0,
                    sort_key TEXT NOT NULL,
                    content TEXT NOT NULL,
                    collapsed INTEGER NOT NULL DEFAULT 0,
                    completed INTEGER NOT NULL DEFAULT 0,
                    block_type TEXT NOT NULL DEFAULT 'text',
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
                )
            "#;

            self.execute_query(create_table_sql.to_string(), HashMap::new())
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create blocks table: {}", e))?;

            // Generate proper fractional index keys for sample data
            use crate::storage::fractional_index::gen_key_between;

            let root_1_key = gen_key_between(None, None)
                .map_err(|e| anyhow::anyhow!("Failed to generate root-1 key: {}", e))?;
            let root_2_key = gen_key_between(Some(&root_1_key), None)
                .map_err(|e| anyhow::anyhow!("Failed to generate root-2 key: {}", e))?;

            let child_1_key = gen_key_between(None, None)
                .map_err(|e| anyhow::anyhow!("Failed to generate child-1 key: {}", e))?;
            let child_2_key = gen_key_between(Some(&child_1_key), None)
                .map_err(|e| anyhow::anyhow!("Failed to generate child-2 key: {}", e))?;

            let grandchild_1_key = gen_key_between(None, None)
                .map_err(|e| anyhow::anyhow!("Failed to generate grandchild-1 key: {}", e))?;

            // Insert sample data for testing with fractional indexing sort_keys
            let sample_data_sql = format!(r#"
                INSERT OR IGNORE INTO blocks (id, parent_id, depth, sort_key, content, block_type, completed)
                VALUES
                    ('root-1', NULL, 0, '{}', 'Welcome to Block Outliner', 'heading', 0),
                    ('child-1', 'root-1', 1, '{}', 'This is a child block', 'text', 0),
                    ('child-2', 'root-1', 1, '{}', 'Another child block', 'text', 1),
                    ('grandchild-1', 'child-1', 2, '{}', 'A nested grandchild', 'text', 0),
                    ('root-2', NULL, 0, '{}', 'Second top-level block', 'heading', 0)
            "#, root_1_key, child_1_key, child_2_key, grandchild_1_key, root_2_key);

            self.execute_query(sample_data_sql.to_string(), HashMap::new())
                .await
                .map_err(|e| anyhow::anyhow!("Failed to insert sample data: {}", e))?;
        }

        Ok(())
    }


    /// Sync all registered providers (delegates to dispatcher)
    pub async fn sync_all_providers(&self) -> Result<()> {
        use tracing::info;
        info!("[BackendEngine] sync_all_providers() called");
        self.dispatcher.sync_all_providers().await
            .map_err(|e| {
                use tracing::error;
                error!("[BackendEngine] sync_all_providers() failed: {}", e);
                anyhow::anyhow!("Failed to sync providers: {}", e)
            })
    }

    /// Sync a specific provider (delegates to dispatcher)
    ///
    /// # Arguments
    /// * `provider_name` - Provider identifier (e.g., "todoist")
    pub async fn sync_provider(&self, provider_name: &str) -> Result<()> {
        let _new_token = self.dispatcher.sync_provider(provider_name).await
            .map_err(|e| anyhow::anyhow!("Failed to sync provider {}: {}", provider_name, e))?;
        // TODO: Persist the new_token to database/file
        Ok(())
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::di::test_helpers::{create_test_engine, create_test_engine_with_providers};
    use crate::core::datasource::{OperationProvider, Result as DatasourceResult};
    use crate::core::datasource::crud_operation_provider_operations;
    use query_render::OperationDescriptor;
    use std::sync::Arc;
    use async_trait::async_trait;

    // Simple SQL-based provider for testing
    struct SqlOperationProvider {
        backend: Arc<RwLock<TursoBackend>>,
        table_name: String,
        entity_name: String,
    }

    impl SqlOperationProvider {
        fn new(backend: Arc<RwLock<TursoBackend>>, table_name: String, entity_name: String) -> Self {
            Self {
                backend,
                table_name,
                entity_name,
            }
        }
    }

    #[async_trait]
    impl OperationProvider for SqlOperationProvider {
        fn operations(&self) -> Vec<OperationDescriptor> {
            crud_operation_provider_operations(&self.entity_name, &self.table_name, "id")
        }

        async fn execute_operation(
            &self,
            entity_name: &str,
            op_name: &str,
            params: StorageEntity,
        ) -> DatasourceResult<()> {
            if entity_name != self.entity_name {
                return Err(format!("Expected entity_name '{}', got '{}'", self.entity_name, entity_name).into());
            }

            match op_name {
                "set_field" => {
                    let id = params.get("id")
                        .and_then(|v| v.as_string())
                        .ok_or_else(|| "Missing 'id' parameter".to_string())?;
                    let field = params.get("field")
                        .and_then(|v| v.as_string())
                        .ok_or_else(|| "Missing 'field' parameter".to_string())?;
                    let value = params.get("value")
                        .ok_or_else(|| "Missing 'value' parameter".to_string())?;

                    let backend = self.backend.write().await;
                    let conn = backend.get_connection()
                        .map_err(|e| format!("Failed to get connection: {}", e))?;

                    // Convert value to SQL
                    let sql_value = match value {
                        Value::String(s) => format!("'{}'", s.replace("'", "''")),
                        Value::Integer(i) => i.to_string(),
                        Value::Boolean(b) => if *b { "1" } else { "0" }.to_string(),
                        Value::Null => "NULL".to_string(),
                        Value::DateTime(dt) => format!("'{}'", dt.to_rfc3339()),
                        Value::Json(j) => format!("'{}'", serde_json::to_string(j).unwrap_or_default().replace("'", "''")),
                        Value::Reference(r) => format!("'{}'", r.replace("'", "''")),
                    };

                    let sql = format!("UPDATE {} SET {} = {} WHERE id = '{}'",
                        self.table_name, field, sql_value, id.replace("'", "''"));
                    conn.execute(&sql, ()).await
                        .map_err(|e| format!("Failed to execute SQL: {}", e))?;
                    Ok(())
                }
                "create" => {
                    let backend = self.backend.write().await;
                    let conn = backend.get_connection()
                        .map_err(|e| format!("Failed to get connection: {}", e))?;

                    let mut columns = Vec::new();
                    let mut values = Vec::new();
                    for (key, value) in params.iter() {
                        columns.push(key.clone());
                        let sql_value = match value {
                            Value::String(s) => format!("'{}'", s.replace("'", "''")),
                            Value::Integer(i) => i.to_string(),
                            Value::Boolean(b) => if *b { "1" } else { "0" }.to_string(),
                            Value::Null => "NULL".to_string(),
                            Value::DateTime(dt) => format!("'{}'", dt.to_rfc3339()),
                            Value::Json(j) => format!("'{}'", serde_json::to_string(j).unwrap_or_default().replace("'", "''")),
                            Value::Reference(r) => format!("'{}'", r.replace("'", "''")),
                        };
                        values.push(sql_value);
                    }

                    let sql = format!("INSERT INTO {} ({}) VALUES ({})",
                        self.table_name,
                        columns.join(", "),
                        values.join(", "));
                    conn.execute(&sql, ()).await
                        .map_err(|e| format!("Failed to execute SQL: {}", e))?;
                    Ok(())
                }
                "delete" => {
                    let id = params.get("id")
                        .and_then(|v| v.as_string())
                        .ok_or_else(|| "Missing 'id' parameter".to_string())?;

                    let backend = self.backend.write().await;
                    let conn = backend.get_connection()
                        .map_err(|e| format!("Failed to get connection: {}", e))?;

                    let sql = format!("DELETE FROM {} WHERE id = '{}'",
                        self.table_name, id.replace("'", "''"));
                    conn.execute(&sql, ()).await
                        .map_err(|e| format!("Failed to execute SQL: {}", e))?;
                    Ok(())
                }
                _ => Err(format!("Unknown operation: {}", op_name).into()),
            }
        }
    }

    #[tokio::test]
    async fn test_render_engine_creation() {
        let engine = create_test_engine().await;
        assert!(engine.is_ok());
    }

    #[tokio::test]
    async fn test_compile_simple_query() {
        let engine = create_test_engine().await.unwrap();

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
    async fn test_execute_query_with_parameters() {
        let engine = create_test_engine().await.unwrap();
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
        let engine = create_test_engine().await.unwrap();
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
        // Create a temporary engine to get the backend for the provider
        let temp_engine = create_test_engine().await.unwrap();
        let provider = Arc::new(SqlOperationProvider::new(
            temp_engine.backend.clone(),
            "blocks".to_string(),
            "blocks".to_string(),
        ));

        // Create engine with SqlOperationProvider registered via TestProviderModule
        let engine = create_test_engine_with_providers(":memory:".into(), |module| {
            module.with_operation_provider(provider)
        }).await.unwrap();

        // Create test table (using the engine's backend)
        {
            let backend = engine.backend.write().await;
            let conn = backend.get_connection().unwrap();
            conn.execute(
                "CREATE TABLE blocks (id TEXT PRIMARY KEY, content TEXT, completed BOOLEAN)",
                ()
            ).await.unwrap();

            conn.execute(
                "INSERT INTO blocks (id, content, completed) VALUES ('block-1', 'Test task', 0)",
                ()
            ).await.unwrap();
        }

        // Execute operation to update completed field
        let mut params = HashMap::new();
        params.insert("id".to_string(), Value::String("block-1".to_string()));
        params.insert("field".to_string(), Value::String("completed".to_string()));
        params.insert("value".to_string(), Value::Boolean(true));

        let result = engine.execute_operation("blocks", "set_field", params).await;
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
        let engine = create_test_engine().await.unwrap();

        // Try to execute non-existent operation
        let params = HashMap::new();
        let result = engine.execute_operation("blocks", "nonexistent", params).await;

        assert!(result.is_err(), "Should fail for non-existent operation");
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("nonexistent"), "Error should mention operation name");
    }

    #[tokio::test]
    async fn test_register_custom_operation() {
        // Create engine with SqlOperationProvider registered via TestProviderModule
        let temp_engine = create_test_engine().await.unwrap();
        let provider = Arc::new(SqlOperationProvider::new(
            temp_engine.backend.clone(),
            "blocks".to_string(),
            "blocks".to_string(),
        ));

        let engine = create_test_engine_with_providers(":memory:".into(), |module| {
            module.with_operation_provider(provider)
        }).await.unwrap();

        // Verify operations are available
        let ops = engine.available_operations("blocks").await;
        assert!(!ops.is_empty(), "Should have operations available");
    }

    #[tokio::test]
    async fn test_operations_inference() {
        let engine = create_test_engine().await.unwrap();

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
