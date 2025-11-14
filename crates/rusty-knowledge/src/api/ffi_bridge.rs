use std::collections::HashMap;
use std::sync::Arc;
use anyhow::{Result, anyhow};
use ferrous_di::{ServiceCollection, ServiceCollectionModuleExt, Resolver};

use crate::api::backend_engine::BackendEngine;
use crate::storage::turso::RowChangeStream;
use crate::storage::types::{Value, StorageEntity};
use crate::di;
use crate::api::operation_dispatcher::OperationModule;
use query_render::RenderSpec;

/// Initialize a render engine with a database at the given path
///
/// # FFI Function
/// This is exposed to Flutter via flutter_rust_bridge
///
/// Uses dependency injection to properly configure the engine with all registered providers.
pub async fn init_render_engine(db_path: String) -> Result<Arc<BackendEngine>> {
    // Set up dependency injection container
    let mut services = ServiceCollection::new();

    // Register OperationModule to collect providers from DI
    services.add_module_mut(OperationModule)
        .map_err(|e| anyhow::anyhow!("Failed to register OperationModule: {}", e))?;

    // Register core services (BackendEngine, TursoBackend, OperationDispatcher)
    di::register_core_services(&mut services, db_path.into())
        .map_err(|e| anyhow::anyhow!("Failed to register core services: {}", e))?;

    // Build the DI container and resolve BackendEngine
    let provider = services.build();
    let engine = Resolver::get_required::<BackendEngine>(&provider);

    Ok(engine)
}

/// Compile a PRQL query with render() into SQL and UI specification
///
/// # FFI Function
/// This is exposed to Flutter via flutter_rust_bridge
///
/// # Returns
/// A tuple of (SQL string, RenderSpec) on success
pub async fn compile_query(
    engine: Arc<BackendEngine>,
    prql: String,
) -> Result<(String, RenderSpec)> {
    engine.compile_query(prql)
}

/// Execute a SQL query and return results
///
/// # FFI Function
/// This is exposed to Flutter via flutter_rust_bridge
///
/// Note: This is a simple synchronous query execution for initial rendering.
/// For reactive updates, use watch_query instead.
pub async fn execute_query(
    engine: Arc<BackendEngine>,
    sql: String,
    params: HashMap<String, Value>,
) -> Result<Vec<StorageEntity>> {
    engine.execute_query(sql, params).await
}

/// Execute an operation on the database
///
/// # FFI Function
/// This is exposed to Flutter via flutter_rust_bridge
///
/// Operations mutate the database directly. UI updates happen via CDC streams.
/// This follows the unidirectional data flow: Action → Model → View
///
/// # Note
/// This function does NOT return new data. Changes propagate through:
/// Operation → DB mutation → CDC event → watch_query stream → UI update
pub async fn execute_operation(
    _engine: Arc<BackendEngine>,
    _op_name: String,
    _params: HashMap<String, Value>,
) -> Result<()> {
    // TODO: Implement operation registry and execution (Phase 3.1-3.2)
    // For now, this is a stub that will be implemented when operations are added
    Err(anyhow!("Operations not yet implemented - coming in Phase 3.1"))
}

/// Watch a SQL query for changes via CDC streaming
///
/// # FFI Function
/// This is exposed to Flutter via flutter_rust_bridge
///
/// Returns a stream of RowChange events. UI should:
/// 1. Subscribe to this stream using StreamBuilder in Flutter
/// 2. Key widgets by entity ID from data.get("id"), NOT by rowid
/// 3. Handle Added/Updated/Removed events to update UI
///
/// # Note
/// The CDC connection is stored in the BackendEngine to keep it alive.
/// Currently returns changes from all tables. Full CDC integration with
/// materialized views will be implemented in Phase 1.3.
pub async fn watch_query(
    engine: Arc<BackendEngine>,
    sql: String,
    params: HashMap<String, Value>,
) -> Result<RowChangeStream> {
    engine.watch_query(sql, params).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_init_and_compile() {
        // Use DI to create engine (same as production)
        let engine = init_render_engine(":memory:".to_string()).await.unwrap();

        let prql = r#"
            from blocks
            filter depth > 0
            render (text title)
        "#;

        let result = compile_query(engine, prql.to_string()).await;
        assert!(result.is_ok());

        let (sql, _spec) = result.unwrap();
        assert!(sql.contains("SELECT"));
        assert!(sql.contains("FROM"));
    }


    #[tokio::test]
    async fn test_execute_operation_not_implemented() {
        // Use DI to create engine (same as production)
        let engine = init_render_engine(":memory:".to_string()).await.unwrap();

        let result = execute_operation(
            engine,
            "indent".to_string(),
            HashMap::new(),
        ).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not yet implemented"));
    }
}
