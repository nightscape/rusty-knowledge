//! OperationProvider wrapper for TodoistTaskDataSource
//!
//! This wrapper implements OperationProvider for TodoistTaskDataSource, enabling
//! generic property-based testing using GenericProviderState.

use async_trait::async_trait;
use std::sync::Arc;
use rusty_knowledge::core::datasource::{
    OperationProvider, OperationDescriptor, OperationRegistry, CrudOperationProvider, Result,
    __operations_crud_operation_provider, __operations_mutable_block_data_source, __operations_mutable_task_data_source,
};
use rusty_knowledge::storage::types::StorageEntity;
use crate::todoist_datasource::TodoistTaskDataSource;
use crate::models::TodoistTask;

/// OperationProvider wrapper for TodoistTaskDataSource
///
/// This wrapper enables TodoistTaskDataSource to be used with GenericProviderState
/// for property-based testing. It delegates to the generated dispatch_operation
/// functions from the #[operations_trait] macro.
pub struct TodoistOperationProvider {
    datasource: Arc<TodoistTaskDataSource>,
    /// Store the last created entity ID (for GenericProviderState to retrieve)
    last_created_id: Arc<std::sync::Mutex<Option<String>>>,
}

impl TodoistOperationProvider {
    /// Create a new TodoistOperationProvider wrapping the given datasource
    pub fn new(datasource: Arc<TodoistTaskDataSource>) -> Self {
        Self {
            datasource,
            last_created_id: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Get a reference to the underlying datasource
    pub fn datasource(&self) -> &Arc<TodoistTaskDataSource> {
        &self.datasource
    }

    /// Get the last created entity ID (for GenericProviderState)
    pub fn get_last_created_id(&self) -> Option<String> {
        self.last_created_id.lock().unwrap().take()
    }
}

#[async_trait]
impl OperationProvider for TodoistOperationProvider {
    fn operations(&self) -> Vec<OperationDescriptor> {
        // Delegate to TodoistTask::all_operations() which aggregates operations
        // from all applicable traits (CrudOperationProvider, MutableBlockDataSource, MutableTaskDataSource)
        <TodoistTask as OperationRegistry>::all_operations()
    }

    async fn execute_operation(
        &self,
        entity_name: &str,
        op_name: &str,
        params: StorageEntity,
    ) -> Result<()> {
        // Validate entity name
        if entity_name != "todoist-task" {
            return Err(format!(
                "Expected entity_name 'todoist-task', got '{}'",
                entity_name
            ).into());
        }

        // Special handling for create operation - need to extract the returned ID
        // The dispatch_operation functions map Result<String> -> Result<()>, so we
        // need to call create() directly to get the ID for state tracking
        if op_name == "create" {
            // Call create() directly to get the ID
            // Dereference Arc to get &TodoistTaskDataSource, then call trait method
            let id = <TodoistTaskDataSource as CrudOperationProvider<TodoistTask>>::create(
                self.datasource.as_ref(),
                params,
            ).await?;
            // Store the ID for GenericProviderState to retrieve
            *self.last_created_id.lock().unwrap() = Some(id);
            return Ok(());
        }

        // Try dispatching to each trait module in order
        // The first one that succeeds wins
        let result = __operations_crud_operation_provider::dispatch_operation(
            self.datasource.as_ref(),
            op_name,
            &params,
        )
        .await;

        if result.is_ok() {
            return result;
        }

        let result = __operations_mutable_block_data_source::dispatch_operation(
            self.datasource.as_ref(),
            op_name,
            &params,
        )
        .await;

        if result.is_ok() {
            return result;
        }

        __operations_mutable_task_data_source::dispatch_operation(
            self.datasource.as_ref(),
            op_name,
            &params,
        )
        .await
    }

    fn get_last_created_id(&self) -> Option<String> {
        // Call the struct method, not the trait method (to avoid infinite recursion)
        TodoistOperationProvider::get_last_created_id(self)
    }
}

