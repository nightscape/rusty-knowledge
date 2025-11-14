pub mod block_ops;
pub mod block_movements;
pub mod delete;
pub mod registry;
pub mod row_view;

use anyhow::Result;
use async_trait::async_trait;

use crate::storage::types::StorageEntity;
use crate::storage::turso::TursoBackend;
use crate::api::render_engine::UiState;

/// Generic operation trait for all block operations
///
/// Operations follow a one-directional flow:
/// Operation → Database mutation → CDC → Query re-run → Render → UI
///
/// Operations do NOT return results directly - UI updates happen via CDC stream.
#[async_trait]
pub trait Operation: Send + Sync {
    /// Operation name used for registry lookup
    fn name(&self) -> &str;

    /// Execute the operation
    ///
    /// # Arguments
    /// * `row_data` - Current row data from the query (block data, metadata, etc.)
    /// * `ui_state` - Current UI state (cursor position, focused block)
    /// * `db` - Database backend for mutations
    ///
    /// # Returns
    /// Result indicating success or failure. On success, UI updates happen via CDC.
    async fn execute(
        &self,
        row_data: &StorageEntity,
        ui_state: &UiState,
        db: &mut TursoBackend,
    ) -> Result<()>;
}

pub use registry::OperationRegistry;
pub use row_view::RowView;
