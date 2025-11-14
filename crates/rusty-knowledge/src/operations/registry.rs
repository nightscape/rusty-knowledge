use std::collections::HashMap;
use std::sync::Arc;
use anyhow::{Result, Context};

use super::Operation;
use crate::storage::types::StorageEntity;
use crate::storage::turso::TursoBackend;
use crate::api::render_engine::UiState;

/// Registry for managing operations
///
/// Provides a centralized place to register and execute operations.
/// Operations are looked up by name and executed with row data and UI state.
pub struct OperationRegistry {
    operations: HashMap<String, Arc<dyn Operation>>,
}

impl OperationRegistry {
    /// Create a new empty registry
    pub fn new() -> Self {
        Self {
            operations: HashMap::new(),
        }
    }

    /// Register an operation
    pub fn register(&mut self, operation: Arc<dyn Operation>) {
        let name = operation.name().to_string();
        self.operations.insert(name, operation);
    }

    /// Execute an operation by name
    ///
    /// # Arguments
    /// * `name` - Operation name to execute
    /// * `row_data` - Current row data from query
    /// * `ui_state` - Current UI state
    /// * `db` - Database backend for mutations
    ///
    /// # Returns
    /// Result indicating success or failure
    pub async fn execute(
        &self,
        name: &str,
        row_data: &StorageEntity,
        ui_state: &UiState,
        db: &mut TursoBackend,
    ) -> Result<()> {
        let operation = self
            .operations
            .get(name)
            .with_context(|| format!("Operation not found: {}", name))?;

        operation.execute(row_data, ui_state, db).await
    }

    /// Check if an operation is registered
    pub fn has_operation(&self, name: &str) -> bool {
        self.operations.contains_key(name)
    }

    /// Get list of registered operation names
    pub fn operation_names(&self) -> Vec<&str> {
        self.operations.keys().map(|s| s.as_str()).collect()
    }
}

impl Default for OperationRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct TestOperation {
        name: String,
    }

    #[async_trait]
    impl Operation for TestOperation {
        fn name(&self) -> &str {
            &self.name
        }

        async fn execute(
            &self,
            _row_data: &StorageEntity,
            _ui_state: &UiState,
            _db: &mut TursoBackend,
        ) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_registry_registration() {
        let mut registry = OperationRegistry::new();

        let op = Arc::new(TestOperation {
            name: "test_op".to_string(),
        });

        registry.register(op);

        assert!(registry.has_operation("test_op"));
        assert!(!registry.has_operation("nonexistent"));
    }

    #[test]
    fn test_registry_operation_names() {
        let mut registry = OperationRegistry::new();

        registry.register(Arc::new(TestOperation {
            name: "op1".to_string(),
        }));
        registry.register(Arc::new(TestOperation {
            name: "op2".to_string(),
        }));

        let names = registry.operation_names();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"op1"));
        assert!(names.contains(&"op2"));
    }

    #[tokio::test]
    async fn test_registry_execute() {
        let mut registry = OperationRegistry::new();

        registry.register(Arc::new(TestOperation {
            name: "test_op".to_string(),
        }));

        let entity = StorageEntity::new();
        let ui_state = UiState {
            cursor_pos: None,
            focused_id: None,
        };
        let mut db = TursoBackend::new_in_memory().await.unwrap();

        let result = registry.execute("test_op", &entity, &ui_state, &mut db).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_registry_execute_nonexistent() {
        let registry = OperationRegistry::new();

        let entity = StorageEntity::new();
        let ui_state = UiState {
            cursor_pos: None,
            focused_id: None,
        };
        let mut db = TursoBackend::new_in_memory().await.unwrap();

        let result = registry.execute("nonexistent", &entity, &ui_state, &mut db).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }
}
