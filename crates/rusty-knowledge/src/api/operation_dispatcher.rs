//! OperationDispatcher - Composite pattern implementation for operation routing
//!
//! The OperationDispatcher aggregates multiple OperationProvider instances and routes
//! operation execution to the correct provider based on entity_name.
//!
//! This implements the Composite Pattern - both individual caches (QueryableCache<T>)
//! and the dispatcher implement OperationProvider, allowing recursive composition.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use async_trait::async_trait;
use ferrous_di::{DiResult, Resolver, ServiceCollection, ServiceModule};
use tracing::{error, info};

use crate::core::datasource::{OperationProvider, SyncableProvider, Result, generate_sync_operation, StreamPosition};
use crate::storage::types::StorageEntity;
use query_render::OperationDescriptor;

/// Composite dispatcher that aggregates multiple OperationProvider instances
///
/// Routes operations to the correct provider based on entity_name.
/// Implements OperationProvider itself, enabling recursive composition.
pub struct OperationDispatcher {
    /// List of operation providers
    providers: Vec<Arc<dyn OperationProvider>>,

    /// Map from provider_name to syncable provider
    /// Key: provider_name (e.g., "todoist", "jira")
    /// Value: SyncableProvider (no longer needs Mutex since sync() doesn't require &mut)
    syncable_providers: HashMap<String, Arc<dyn SyncableProvider>>,

    /// Map from provider_name to current sync token (persisted externally)
    /// Key: provider_name (e.g., "todoist", "jira")
    /// Value: Current sync token as StreamPosition::Version
    /// Note: This should be persisted to database/file, not just in memory
    /// Uses RwLock for interior mutability so execute_operation can stay &self
    sync_tokens: Arc<tokio::sync::RwLock<HashMap<String, StreamPosition>>>,
}

impl OperationDispatcher {
    /// Create a new dispatcher with the given providers
    ///
    /// # Arguments
    /// * `providers` - Vector of OperationProvider instances to register
    /// * `syncable_providers` - Map of syncable provider names to providers
    ///
    /// # Example
    /// ```rust
    /// let providers = vec![Arc::new(cache1), Arc::new(cache2)];
    /// let mut syncable_providers = HashMap::new();
    /// syncable_providers.insert("todoist".to_string(), todoist_provider);
    /// let dispatcher = OperationDispatcher::new(providers, syncable_providers);
    /// ```
    pub fn new(providers: Vec<Arc<dyn OperationProvider>>, syncable_providers: HashMap<String, Arc<dyn SyncableProvider>>) -> Self {
        Self {
            providers,
            syncable_providers,
            sync_tokens: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        }
    }

    /// Create a new dispatcher with sync tokens (for restoring from persistence)
    pub fn new_with_tokens(
        providers: Vec<Arc<dyn OperationProvider>>,
        syncable_providers: HashMap<String, Arc<dyn SyncableProvider>>,
        sync_tokens: HashMap<String, StreamPosition>,
    ) -> Self {
        Self {
            providers,
            syncable_providers,
            sync_tokens: Arc::new(tokio::sync::RwLock::new(sync_tokens)),
        }
    }

    /// Sync a specific provider by name
    ///
    /// Provider name can be extracted from operation name using `provider.operation` convention.
    /// Example: "todoist.sync" → provider_name = "todoist"
    ///
    /// Returns the new sync token which should be persisted by the caller.
    pub async fn sync_provider(&self, provider_name: &str) -> Result<StreamPosition> {
        let provider = self.syncable_providers
            .get(provider_name)
            .ok_or_else(|| format!("No syncable provider registered: {}", provider_name))?;

        // Get current stream position (or Beginning for first sync)
        let current_position = {
            let tokens = self.sync_tokens.read().await;
            tokens.get(provider_name)
                .cloned()
                .unwrap_or(StreamPosition::Beginning)
        };

        // Call sync with current position
        let new_position = provider.sync(current_position).await?;

        // Update stored position (caller should persist this)
        {
            let mut tokens = self.sync_tokens.write().await;
            tokens.insert(provider_name.to_string(), new_position.clone());
        }

        Ok(new_position)
    }

    /// Sync provider from operation name
    ///
    /// Extracts provider name from `provider.operation` format.
    /// Example: "todoist.sync" → syncs "todoist" provider
    pub async fn sync_provider_from_operation(&self, operation_name: &str) -> Result<StreamPosition> {
        let provider_name = operation_name
            .split('.')
            .next()
            .ok_or_else(|| format!("Invalid operation name format: {}", operation_name))?;
        self.sync_provider(provider_name).await
    }

    /// Sync all registered providers
    pub async fn sync_all_providers(&self) -> Result<()> {
        let provider_count = self.syncable_providers.len();
        info!("[OperationDispatcher] Syncing all providers: count={}", provider_count);

        if provider_count == 0 {
            info!("[OperationDispatcher] No syncable providers registered - sync will do nothing");
            return Ok(());
        }

        let provider_names: Vec<String> = self.syncable_providers.keys().cloned().collect();
        info!("[OperationDispatcher] Registered syncable providers: {:?}", provider_names);

        for (name, provider) in self.syncable_providers.iter() {
            info!("[OperationDispatcher] Syncing provider: {}", name);

            // Get current stream position for this provider
            let current_position = {
                let tokens = self.sync_tokens.read().await;
                tokens.get(name)
                    .cloned()
                    .unwrap_or(StreamPosition::Beginning)
            };

            match provider.sync(current_position).await {
                Ok(new_position) => {
                    // Update stored position (caller should persist this)
                    {
                        let mut tokens = self.sync_tokens.write().await;
                        tokens.insert(name.clone(), new_position);
                    }
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


    /// Check if a provider is registered for an entity type
    pub fn has_provider(&self, entity_name: &str) -> bool {
        self.providers.iter().any(|provider| {
            provider.operations().iter().any(|op| op.entity_name == entity_name)
        })
    }

    /// Get list of registered entity names
    pub fn registered_entities(&self) -> Vec<String> {
        let mut entity_names = HashSet::new();
        for provider in &self.providers {
            for op in provider.operations() {
                entity_names.insert(op.entity_name);
            }
        }
        entity_names.into_iter().collect()
    }

    /// Get the number of registered providers
    pub fn provider_count(&self) -> usize {
        self.providers.len()
    }

    /// Get a copy of all providers (for reconstructing dispatcher with additional providers)
    pub fn providers(&self) -> Vec<Arc<dyn OperationProvider>> {
        self.providers.clone()
    }

    /// Get a copy of all syncable providers (for reconstructing dispatcher with additional providers)
    pub fn syncable_providers(&self) -> HashMap<String, Arc<dyn SyncableProvider>> {
        self.syncable_providers.clone()
    }

    /// Get a copy of all sync tokens (for persistence)
    pub async fn sync_tokens(&self) -> HashMap<String, StreamPosition> {
        let tokens = self.sync_tokens.read().await;
        tokens.clone()
    }

    /// Set sync tokens (for restoring from persistence)
    pub async fn set_sync_tokens(&self, tokens: HashMap<String, StreamPosition>) {
        let mut sync_tokens = self.sync_tokens.write().await;
        *sync_tokens = tokens;
    }

}

impl Default for OperationDispatcher {
    fn default() -> Self {
        Self::new(Vec::new(), HashMap::new())
    }
}

#[async_trait]
impl OperationProvider for OperationDispatcher {
    /// Get all operations from all registered providers
    ///
    /// Aggregates operations from all providers and syncable providers, returns them as a flat list.
    fn operations(&self) -> Vec<OperationDescriptor> {
        let mut ops: Vec<OperationDescriptor> = self.providers
            .iter()
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
            self.sync_provider_from_operation(entity_name).await?;
            return Ok(());
        }

        // Otherwise, route to regular operation provider
        // Find first provider that has an operation matching entity_name and op_name
        // Debug: Log available providers and operations
        let available_ops: Vec<_> = self.providers
            .iter()
            .flat_map(|p| p.operations())
            .collect();
        let matching_ops: Vec<_> = available_ops
            .iter()
            .filter(|op| op.entity_name == entity_name && op.name == op_name)
            .collect();

        if matching_ops.is_empty() {
            // Log all available entity names for debugging
            let entity_names: std::collections::HashSet<_> = available_ops
                .iter()
                .map(|op| &op.entity_name)
                .collect();
            error!(
                "No provider registered for entity: '{}' (operation: '{}'). Available entities: {:?}",
                entity_name, op_name, entity_names
            );
            return Err(format!("No provider registered for entity: {}", entity_name).into());
        }

        let provider = self.providers
            .iter()
            .find(|provider| {
                provider.operations().iter().any(|op| {
                    op.entity_name == entity_name && op.name == op_name
                })
            })
            .ok_or_else(|| format!("No provider registered for entity: {}", entity_name))?;

        provider.execute_operation(entity_name, op_name, params).await
    }
}
pub struct OperationModule;

impl ServiceModule for OperationModule {
    fn register_services(self, services: &mut ServiceCollection) -> DiResult<()> {
        services.add_singleton_factory::<OperationDispatcher, _>(|r| {
            let providers = r.get_all_trait::<dyn OperationProvider>().expect("Failed to get all operation providers");
            info!("[OperationModule] Found {} operation providers", providers.len());

            let syncable_provider_list = r.get_all_trait::<dyn SyncableProvider>().expect("Failed to get all syncable providers");
            info!("[OperationModule] Found {} syncable providers via get_all_trait", syncable_provider_list.len());

            for provider in &syncable_provider_list {
                info!("[OperationModule] Syncable provider: {}", provider.provider_name());
            }
            let syncable_providers = syncable_provider_list.into_iter().map(|provider| (provider.provider_name().to_string(), provider)).collect();
            OperationDispatcher::new(providers, syncable_providers)
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use self::super::*;

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
    async fn test_provider_registration() {
        let provider1 = Arc::new(MockProvider {
            entity_name: "entity1".to_string(),
            operations_list: vec![create_test_operation("entity1", "op1")],
        });

        let dispatcher = OperationDispatcher::new(vec![provider1], HashMap::new());
        assert!(dispatcher.has_provider("entity1"));
        assert_eq!(dispatcher.provider_count(), 1);
    }

    #[tokio::test]
    async fn test_operations_aggregation() {
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

        let dispatcher = OperationDispatcher::new(vec![provider1, provider2], HashMap::new());

        let all_ops = dispatcher.operations();
        assert_eq!(all_ops.len(), 3);
        assert!(all_ops.iter().any(|op| op.name == "op1"));
        assert!(all_ops.iter().any(|op| op.name == "op2"));
        assert!(all_ops.iter().any(|op| op.name == "op3"));
    }


    #[tokio::test]
    async fn test_execute_operation_routing() {
        let provider1 = Arc::new(MockProvider {
            entity_name: "entity1".to_string(),
            operations_list: vec![create_test_operation("entity1", "test_op")],
        });

        let dispatcher = OperationDispatcher::new(vec![provider1], HashMap::new());

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
        let provider1 = Arc::new(MockProvider {
            entity_name: "entity1".to_string(),
            operations_list: vec![create_test_operation("entity1", "op1")],
        });
        let provider2 = Arc::new(MockProvider {
            entity_name: "entity2".to_string(),
            operations_list: vec![create_test_operation("entity2", "op2")],
        });

        let dispatcher = OperationDispatcher::new(vec![provider1, provider2], HashMap::new());

        let entities = dispatcher.registered_entities();
        assert_eq!(entities.len(), 2);
        assert!(entities.contains(&"entity1".to_string()));
        assert!(entities.contains(&"entity2".to_string()));
    }
}

