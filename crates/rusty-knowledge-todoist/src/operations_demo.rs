//! Demonstration of the hybrid operations() approach
//!
//! This module shows how operations metadata is automatically available through:
//! 1. CrudOperationProvider trait's default operations() method (delegates to T::all_operations())
//! 2. QueryableCache's blanket impl of CacheOperations (delegates to T::all_operations())
//! 3. Manual OperationRegistry impl on TodoistTask (one-time, simple aggregation)

#[cfg(test)]
mod tests {
    use crate::{TodoistClient, TodoistSyncProvider};
    use crate::models::TodoistTask;
    use crate::todoist_datasource::TodoistTaskDataSource;
    use rusty_knowledge::core::datasource::{CrudOperationProvider, OperationRegistry};
    use rusty_knowledge::core::StreamCache as QueryableCache;
    use rusty_knowledge::storage::turso::TursoBackend;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    #[tokio::test]
    async fn test_operations_on_entity_type() {
        // Entity types expose operations via OperationRegistry
        let ops = TodoistTask::all_operations();

        // Should have operations from all three traits:
        // - CrudOperationProvider: set_field, create, delete (3 ops)
        // - MutableBlockDataSource: indent_block, move_block, outdent_block (3 ops)
        // - MutableTaskDataSource: set_completion, set_priority, set_due_date (3 ops)
        assert_eq!(ops.len(), 9, "TodoistTask should have 9 operations total");

        // Check for presence of operations from each trait
        let op_names: Vec<String> = ops.iter().map(|op| op.name.clone()).collect();

        // CrudOperationProvider operations
        assert!(op_names.contains(&"set_field".to_string()));
        assert!(op_names.contains(&"create".to_string()));
        assert!(op_names.contains(&"delete".to_string()));

        // MutableBlockDataSource operations
        assert!(op_names.contains(&"indent_block".to_string()));
        assert!(op_names.contains(&"move_block".to_string()));
        assert!(op_names.contains(&"outdent_block".to_string()));

        // MutableTaskDataSource operations
        assert!(op_names.contains(&"set_completion".to_string()));
        assert!(op_names.contains(&"set_priority".to_string()));
        assert!(op_names.contains(&"set_due_date".to_string()));
    }

    #[tokio::test]
    async fn test_operations_on_cache() {
        // QueryableCache gets operations() automatically via blanket impl of CacheOperations
        let datasource = Arc::new(TodoistTaskDataSource::new(Arc::new(TodoistSyncProvider::new(TodoistClient::new("test_api_key")))));
        let db = Arc::new(RwLock::new(
            Box::new(TursoBackend::new_in_memory().await.unwrap())
                as Box<dyn rusty_knowledge::storage::backend::StorageBackend>
        ));

        let cache = QueryableCache::<TodoistTask>::new(
            datasource,
            db,
            "todoist_tasks".to_string(),
        );

        // operations() is automatically available via CacheOperations blanket impl
        let ops = cache.operations();

        // Should delegate to TodoistTask::all_operations()
        assert_eq!(ops.len(), 9, "Cache should expose all 9 operations");

        // Verify a few operation details
        let set_field_op = ops.iter().find(|op| op.name == "set_field").unwrap();
        assert_eq!(set_field_op.required_params.len(), 3); // id, field, value
        assert_eq!(set_field_op.required_params[0].name, "id");
        assert_eq!(set_field_op.required_params[1].name, "field");
        assert_eq!(set_field_op.required_params[2].name, "value");
    }

    #[test]
    fn test_operation_descriptors_have_metadata() {
        // Verify that generated operations have rich metadata
        let ops = TodoistTask::all_operations();

        for op in &ops {
            // Every operation should have a name
            assert!(!op.name.is_empty(), "Operation should have non-empty name");

            // Parameters should have type information
            for param in &op.required_params {
                assert!(!param.name.is_empty(), "Param should have name");
                // TypeHint is an enum, not a string, so we check it exists
                match &param.type_hint {
                    query_render::TypeHint::Bool
                    | query_render::TypeHint::String
                    | query_render::TypeHint::Number
                    | query_render::TypeHint::EntityId { .. } => {}
                }
            }
        }

        // Check a specific operation for correctness
        let indent_op = ops.iter().find(|op| op.name == "indent_block").unwrap();
        assert_eq!(indent_op.required_params.len(), 2); // id and new_parent_id
        assert_eq!(indent_op.required_params[0].name, "id");
        // id could be String or EntityId - both are valid
        match &indent_op.required_params[0].type_hint {
            query_render::TypeHint::String | query_render::TypeHint::EntityId { .. } => {}
            _ => panic!("Expected String or EntityId type hint for id param, got {:?}", indent_op.required_params[0].type_hint),
        }
        assert_eq!(indent_op.required_params[1].name, "new_parent_id");
        // new_parent_id could be String or EntityId - both are valid
        match &indent_op.required_params[1].type_hint {
            query_render::TypeHint::String | query_render::TypeHint::EntityId { .. } => {}
            _ => panic!("Expected String or EntityId type hint for new_parent_id param, got {:?}", indent_op.required_params[1].type_hint),
        }
    }
}
