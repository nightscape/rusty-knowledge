//! OperationProvider wrapper for TodoistTaskFake
//!
//! This wrapper implements OperationProvider for TodoistTaskFake, enabling
//! generic property-based testing using GenericProviderState.

use async_trait::async_trait;
use std::sync::Arc;
use rusty_knowledge::core::datasource::{
    OperationProvider, OperationDescriptor, OperationRegistry, CrudOperationProvider, Result,
    __operations_crud_operation_provider, __operations_mutable_block_data_source, __operations_mutable_task_data_source,
};
use rusty_knowledge::storage::types::StorageEntity;
use crate::fake::TodoistTaskFake;
use crate::models::TodoistTask;

/// OperationProvider wrapper for TodoistTaskFake
///
/// This wrapper enables TodoistTaskFake to be used with GenericProviderState
/// for property-based testing. It delegates to the generated dispatch_operation
/// functions from the #[operations_trait] macro.
pub struct TodoistFakeOperationProvider {
    fake: Arc<TodoistTaskFake>,
    /// Store the last created entity ID (for GenericProviderState to retrieve)
    last_created_id: Arc<std::sync::Mutex<Option<String>>>,
}

impl TodoistFakeOperationProvider {
    /// Create a new TodoistFakeOperationProvider wrapping the given fake datasource
    pub fn new(fake: Arc<TodoistTaskFake>) -> Self {
        Self {
            fake,
            last_created_id: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Get a reference to the underlying fake datasource
    pub fn fake(&self) -> &Arc<TodoistTaskFake> {
        &self.fake
    }

    /// Get the last created entity ID (for GenericProviderState)
    pub fn get_last_created_id(&self) -> Option<String> {
        self.last_created_id.lock().unwrap().take()
    }
}

#[async_trait]
impl OperationProvider for TodoistFakeOperationProvider {
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
            // Dereference Arc to get &TodoistTaskFake, then call trait method
            let id = <TodoistTaskFake as CrudOperationProvider<TodoistTask>>::create(
                self.fake.as_ref(),
                params,
            ).await?;
            // Store the ID for GenericProviderState to retrieve
            *self.last_created_id.lock().unwrap() = Some(id);
            return Ok(());
        }

        // Try dispatching to each trait module in order
        // The first one that succeeds wins
        let result = __operations_crud_operation_provider::dispatch_operation(
            self.fake.as_ref(),
            op_name,
            &params,
        )
        .await;

        if result.is_ok() {
            return result;
        }

        let result = __operations_mutable_block_data_source::dispatch_operation(
            self.fake.as_ref(),
            op_name,
            &params,
        )
        .await;

        if result.is_ok() {
            return result;
        }

        __operations_mutable_task_data_source::dispatch_operation(
            self.fake.as_ref(),
            op_name,
            &params,
        )
        .await
    }

    fn get_last_created_id(&self) -> Option<String> {
        // Call the struct method, not the trait method (to avoid infinite recursion)
        TodoistFakeOperationProvider::get_last_created_id(self)
    }
}

