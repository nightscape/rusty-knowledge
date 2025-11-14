use std::collections::HashMap;
use std::sync::Arc;
use anyhow::{Result, anyhow};
use tokio::sync::RwLock;

use crate::api::render_engine::{RenderEngine, UiState};
use crate::storage::turso::RowChangeStream;
use crate::storage::types::{Value, StorageEntity};
use query_render::RenderSpec;

/// Initialize a render engine with a database at the given path
///
/// # FFI Function
/// This is exposed to Flutter via flutter_rust_bridge
pub async fn init_render_engine(db_path: String) -> Result<Arc<RwLock<RenderEngine>>> {
    let engine = RenderEngine::new(db_path.into()).await?;
    Ok(Arc::new(RwLock::new(engine)))
}

/// Compile a PRQL query with render() into SQL and UI specification
///
/// # FFI Function
/// This is exposed to Flutter via flutter_rust_bridge
///
/// # Returns
/// A tuple of (SQL string, RenderSpec) on success
pub async fn compile_query(
    engine: Arc<RwLock<RenderEngine>>,
    prql: String,
) -> Result<(String, RenderSpec)> {
    let engine = engine.read().await;
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
    engine: Arc<RwLock<RenderEngine>>,
    sql: String,
    params: HashMap<String, Value>,
) -> Result<Vec<StorageEntity>> {
    let engine = engine.read().await;
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
    _engine: Arc<RwLock<RenderEngine>>,
    _op_name: String,
    _params: HashMap<String, Value>,
) -> Result<()> {
    // TODO: Implement operation registry and execution (Phase 3.1-3.2)
    // For now, this is a stub that will be implemented when operations are added
    Err(anyhow!("Operations not yet implemented - coming in Phase 3.1"))
}

/// Update the UI state (cursor position, focused block)
///
/// # FFI Function
/// This is exposed to Flutter via flutter_rust_bridge
pub async fn set_ui_state(
    engine: Arc<RwLock<RenderEngine>>,
    ui_state: UiState,
) -> Result<()> {
    let engine = engine.read().await;
    engine.set_ui_state(ui_state).await
}

/// Get the current UI state
///
/// # FFI Function
/// This is exposed to Flutter via flutter_rust_bridge
pub async fn get_ui_state(
    engine: Arc<RwLock<RenderEngine>>,
) -> Result<UiState> {
    let engine = engine.read().await;
    Ok(engine.get_ui_state().await)
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
/// The CDC connection is stored in the RenderEngine to keep it alive.
/// Currently returns changes from all tables. Full CDC integration with
/// materialized views will be implemented in Phase 1.3.
pub async fn watch_query(
    engine: Arc<RwLock<RenderEngine>>,
    sql: String,
    params: HashMap<String, Value>,
) -> Result<RowChangeStream> {
    let mut engine = engine.write().await;
    engine.watch_query(sql, params).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_init_and_compile() {
        let engine = RenderEngine::new_in_memory().await.unwrap();
        let engine = Arc::new(RwLock::new(engine));

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
    async fn test_ui_state_operations() {
        let engine = RenderEngine::new_in_memory().await.unwrap();
        let engine = Arc::new(RwLock::new(engine));

        let state = UiState {
            cursor_pos: None,
            focused_id: Some("test-block".to_string()),
        };

        set_ui_state(engine.clone(), state.clone()).await.unwrap();
        let retrieved = get_ui_state(engine).await.unwrap();

        assert_eq!(retrieved.focused_id, state.focused_id);
    }

    #[tokio::test]
    async fn test_execute_operation_not_implemented() {
        let engine = RenderEngine::new_in_memory().await.unwrap();
        let engine = Arc::new(RwLock::new(engine));

        let result = execute_operation(
            engine,
            "indent".to_string(),
            HashMap::new(),
        ).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not yet implemented"));
    }
}
