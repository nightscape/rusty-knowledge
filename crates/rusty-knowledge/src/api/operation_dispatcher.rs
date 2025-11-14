//! OperationDispatcher - Composite pattern implementation for operation routing
//!
//! The OperationDispatcher aggregates multiple OperationProvider instances and routes
//! operation execution to the correct provider based on entity_name.
//!
//! This implements the Composite Pattern - both individual caches (QueryableCache<T>)
//! and the dispatcher implement OperationProvider, allowing recursive composition.

use std::collections::HashMap;
use std::sync::Arc;
use async_trait::async_trait;
use tokio::sync::Mutex;
use tracing::{error, info};

use crate::core::datasource::{OperationProvider, SyncableProvider, Result, generate_sync_operation};
use crate::storage::types::StorageEntity;
use query_render::OperationDescriptor;

/// Composite dispatcher that aggregates multiple OperationProvider instances
///
/// Routes operations to the correct provider based on entity_name.
/// Implements OperationProvider itself, enabling recursive composition.
pub struct OperationDispatcher {
    /// Map from entity_name to operation provider
    providers: HashMap<String, Arc<dyn OperationProvider>>,
    
    /// Map from provider_name to syncable provider
    /// Key: provider_name (e.g., "todoist", "jira")
    /// Value: SyncableProvider wrapped in Mutex for mutable sync access
    syncable_providers: HashMap<String, Arc<Mutex<dyn SyncableProvider>>>,
}

impl OperationDispatcher {
    /// Create a new empty dispatcher
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
            syncable_providers: HashMap::new(),
        }
    }
    
    /// Register a syncable provider
    ///
    /// # Arguments
    /// * `provider_name` - Provider identifier (e.g., "todoist", "jira")
    /// * `provider` - The SyncableProvider instance to register
    pub fn register_syncable_provider(&mut self, provider_name: String, provider: Arc<Mutex<dyn SyncableProvider>>) {
        self.syncable_providers.insert(provider_name.clone(), provider);
    }
    
    /// Sync a specific provider by name
    ///
    /// Provider name can be extracted from operation name using `provider.operation` convention.
    /// Example: "todoist.sync" → provider_name = "todoist"
    pub async fn sync_provider(&self, provider_name: &str) -> Result<()> {
        let provider = self.syncable_providers
            .get(provider_name)
            .ok_or_else(|| format!("No syncable provider registered: {}", provider_name))?;
        
        let mut provider_guard = provider.lock().await;
        provider_guard.sync().await
    }
    
    /// Sync provider from operation name
    ///
    /// Extracts provider name from `provider.operation` format.
    /// Example: "todoist.sync" → syncs "todoist" provider
    pub async fn sync_provider_from_operation(&self, operation_name: &str) -> Result<()> {
        let provider_name = operation_name
            .split('.')
            .next()
            .ok_or_else(|| format!("Invalid operation name format: {}", operation_name))?;
        self.sync_provider(provider_name).await
    }
    
    /// Sync all registered providers
    pub async fn sync_all_providers(&self) -> Result<()> {
        info!("[OperationDispatcher] Syncing all providers: count={}", self.syncable_providers.len());
        for (name, provider) in self.syncable_providers.iter() {
            info!("[OperationDispatcher] Syncing provider: {}", name);
            let mut provider_guard = provider.lock().await;
            match provider_guard.sync().await {
                Ok(_) => {
                    info!("[OperationDispatcher] Successfully synced provider: {}", name);
                }
                Err(e) => {
                    error!("[OperationDispatcher] Failed to sync provider {}: {}", name, e);
                    // Continue syncing other providers
                }
            }
        }
        Ok(())
    }
    
    /// Get list of registered syncable provider names
    pub fn syncable_provider_names(&self) -> Vec<String> {
        self.syncable_providers.keys().cloned().collect()
    }

    /// Register a provider for an entity type
    ///
    /// # Arguments
    /// * `entity_name` - Entity identifier (e.g., "todoist-task", "logseq-block")
    /// * `provider` - The OperationProvider instance to register
    ///
    /// # Example
    /// ```rust
    /// let mut dispatcher = OperationDispatcher::new();
    /// dispatcher.register("todoist-task".to_string(), Arc::new(cache));
    /// ```
    pub fn register(&mut self, entity_name: String, provider: Arc<dyn OperationProvider>) {
        self.providers.insert(entity_name, provider);
    }

    /// Unregister a provider for an entity type
    ///
    /// # Arguments
    /// * `entity_name` - Entity identifier to unregister
    ///
    /// # Returns
    /// `true` if a provider was removed, `false` if no provider was registered
    pub fn unregister(&mut self, entity_name: &str) -> bool {
        self.providers.remove(entity_name).is_some()
    }

    /// Check if a provider is registered for an entity type
    pub fn has_provider(&self, entity_name: &str) -> bool {
        self.providers.contains_key(entity_name)
    }

    /// Get list of registered entity names
    pub fn registered_entities(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }

    /// Get the number of registered providers
    pub fn provider_count(&self) -> usize {
        self.providers.len()
    }
}

impl Default for OperationDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl OperationProvider for OperationDispatcher {
    /// Get all operations from all registered providers
    ///
    /// Aggregates operations from all providers and syncable providers, returns them as a flat list.
    fn operations(&self) -> Vec<OperationDescriptor> {
        let mut ops: Vec<OperationDescriptor> = self.providers
            .values()
            .flat_map(|provider| provider.operations())
            .collect();
        
        // Add sync operations for all registered syncable providers
        for provider_name in self.syncable_providers.keys() {
            ops.push(generate_sync_operation(provider_name));
        }
        
        ops
    }

    /// Find operations that can be executed with given arguments
    ///
    /// Filters operations based on entity_name and available_args.
    /// Uses the default implementation from the trait.
    fn find_operations(
        &self,
        entity_name: &str,
        available_args: &[String],
    ) -> Vec<OperationDescriptor> {
        // Filter operations from all providers
        self.operations()
            .into_iter()
            .filter(|op| {
                op.entity_name == entity_name &&
                op.required_params.iter().all(|p| available_args.contains(&p.name))
            })
            .collect()
    }

    /// Execute an operation by routing to the correct provider
    ///
    /// # Arguments
    /// * `entity_name` - Entity identifier (e.g., "todoist-task" or "todoist.sync")
    /// * `op_name` - Operation name (e.g., "set_completion" or "sync")
    /// * `params` - Operation parameters as StorageEntity
    ///
    /// # Returns
    /// Result indicating success or failure
    ///
    /// # Errors
    /// Returns an error if:
    /// - No provider is registered for the entity_name
    /// - The provider's execute_operation returns an error
    async fn execute_operation(
        &self,
        entity_name: &str,
        op_name: &str,
        params: StorageEntity,
    ) -> Result<()> {
        // Check if this is a sync operation (format: "provider.sync")
        if op_name == "sync" && entity_name.contains('.') {
            return self.sync_provider_from_operation(entity_name).await;
        }
        
        // Otherwise, route to regular operation provider
        let provider = self
            .providers
            .get(entity_name)
            .ok_or_else(|| format!("No provider registered for entity: {}", entity_name))?;

        provider.execute_operation(entity_name, op_name, params).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use crate::storage::types::{StorageEntity, Value};

    // Mock OperationProvider for testing
    struct MockProvider {
        entity_name: String,
        operations_list: Vec<OperationDescriptor>,
    }

    #[async_trait]
    impl OperationProvider for MockProvider {
        fn operations(&self) -> Vec<OperationDescriptor> {
            self.operations_list.clone()
        }

        async fn execute_operation(
            &self,
            entity_name: &str,
            op_name: &str,
            _params: StorageEntity,
        ) -> Result<()> {
            if entity_name != self.entity_name {
                return Err(format!("Entity mismatch: expected {}, got {}", self.entity_name, entity_name).into());
            }
            if op_name == "test_op" {
                Ok(())
            } else {
                Err(format!("Unknown operation: {}", op_name).into())
            }
        }
    }

    fn create_test_operation(entity_name: &str, op_name: &str) -> OperationDescriptor {
        OperationDescriptor {
            entity_name: entity_name.to_string(),
            table: format!("{}_table", entity_name),
            id_column: "id".to_string(),
            name: op_name.to_string(),
            display_name: format!("Test {}", op_name),
            description: format!("Test operation {}", op_name),
            required_params: vec![],
            precondition: None,
        }
    }

    #[tokio::test]
    async fn test_register_and_unregister() {
        let mut dispatcher = OperationDispatcher::new();

        let provider1 = Arc::new(MockProvider {
            entity_name: "entity1".to_string(),
            operations_list: vec![create_test_operation("entity1", "op1")],
        });

        // Register provider
        dispatcher.register("entity1".to_string(), provider1);
        assert!(dispatcher.has_provider("entity1"));
        assert_eq!(dispatcher.provider_count(), 1);

        // Unregister provider
        assert!(dispatcher.unregister("entity1"));
        assert!(!dispatcher.has_provider("entity1"));
        assert_eq!(dispatcher.provider_count(), 0);
    }

    #[tokio::test]
    async fn test_operations_aggregation() {
        let mut dispatcher = OperationDispatcher::new();

        let provider1 = Arc::new(MockProvider {
            entity_name: "entity1".to_string(),
            operations_list: vec![
                create_test_operation("entity1", "op1"),
                create_test_operation("entity1", "op2"),
            ],
        });

        let provider2 = Arc::new(MockProvider {
            entity_name: "entity2".to_string(),
            operations_list: vec![create_test_operation("entity2", "op3")],
        });

        dispatcher.register("entity1".to_string(), provider1);
        dispatcher.register("entity2".to_string(), provider2);

        let all_ops = dispatcher.operations();
        assert_eq!(all_ops.len(), 3);
        assert!(all_ops.iter().any(|op| op.name == "op1"));
        assert!(all_ops.iter().any(|op| op.name == "op2"));
        assert!(all_ops.iter().any(|op| op.name == "op3"));
    }

    #[tokio::test]
    async fn test_find_operations() {
        let mut dispatcher = OperationDispatcher::new();

        let provider1 = Arc::new(MockProvider {
            entity_name: "entity1".to_string(),
            operations_list: vec![
                OperationDescriptor {
                    entity_name: "entity1".to_string(),
                    table: "entity1_table".to_string(),
                    id_column: "id".to_string(),
                    name: "op1".to_string(),
                    display_name: "Op1".to_string(),
                    description: "Operation 1".to_string(),
                    required_params: vec![
                        query_render::OperationParam {
                            name: "id".to_string(),
                            type_hint: TypeHint::String,
                            description: "ID".to_string(),
                        },
                    ],
                    precondition: None,
                },
                OperationDescriptor {
                    entity_name: "entity1".to_string(),
                    table: "entity1_table".to_string(),
                    id_column: "id".to_string(),
                    name: "op2".to_string(),
                    display_name: "Op2".to_string(),
                    description: "Operation 2".to_string(),
                    required_params: vec![
                        query_render::OperationParam {
                            name: "id".to_string(),
                            type_hint: TypeHint::String,
                            description: "ID".to_string(),
                        },
                        query_render::OperationParam {
                            name: "field".to_string(),
                            type_hint: TypeHint::String,
                            description: "Field".to_string(),
                        },
                    ],
                    precondition: None,
                },
            ],
        });

        dispatcher.register("entity1".to_string(), provider1);

        // Find operations with only "id" available
        let available_args = vec!["id".to_string()];
        let ops = dispatcher.find_operations("entity1", &available_args);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].name, "op1");

        // Find operations with "id" and "field" available
        let available_args = vec!["id".to_string(), "field".to_string()];
        let ops = dispatcher.find_operations("entity1", &available_args);
        assert_eq!(ops.len(), 2);
    }

    #[tokio::test]
    async fn test_execute_operation_routing() {
        let mut dispatcher = OperationDispatcher::new();

        let provider1 = Arc::new(MockProvider {
            entity_name: "entity1".to_string(),
            operations_list: vec![create_test_operation("entity1", "test_op")],
        });

        dispatcher.register("entity1".to_string(), provider1);

        // Execute operation on registered entity
        let params = StorageEntity::new();
        let result = dispatcher.execute_operation("entity1", "test_op", params).await;
        assert!(result.is_ok());

        // Try to execute on unregistered entity
        let params = StorageEntity::new();
        let result = dispatcher.execute_operation("entity2", "test_op", params).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No provider registered"));
    }

    #[tokio::test]
    async fn test_registered_entities() {
        let mut dispatcher = OperationDispatcher::new();

        let provider1 = Arc::new(MockProvider {
            entity_name: "entity1".to_string(),
            operations_list: vec![],
        });
        let provider2 = Arc::new(MockProvider {
            entity_name: "entity2".to_string(),
            operations_list: vec![],
        });

        dispatcher.register("entity1".to_string(), provider1);
        dispatcher.register("entity2".to_string(), provider2);

        let entities = dispatcher.registered_entities();
        assert_eq!(entities.len(), 2);
        assert!(entities.contains(&"entity1".to_string()));
        assert!(entities.contains(&"entity2".to_string()));
    }
}

