//! Dependency Injection module for Todoist integration
//!
//! This module provides DI registration for Todoist-specific services using ferrous-di.

use std::sync::Arc;
use ferrous_di::Resolver;
use ferrous_di::{DiResult, ServiceCollection, ServiceModule, Lifetime};
use tokio::sync::{RwLock, broadcast};

use crate::TodoistClient;
use crate::TodoistSyncProvider;
use crate::todoist_datasource::TodoistTaskDataSource;
use crate::models::TodoistTask;
use rusty_knowledge::core::datasource::{SyncableProvider, OperationProvider};
use rusty_knowledge::core::queryable_cache::QueryableCache;
use rusty_knowledge::storage::turso::TursoBackend;

/// Configuration for Todoist API key
#[derive(Clone, Debug)]
pub struct TodoistConfig {
    pub api_key: Option<String>,
}

impl TodoistConfig {
    pub fn new(api_key: Option<String>) -> Self {
        Self { api_key }
    }
}


/// ServiceModule for Todoist integration
///
/// Registers Todoist-specific services in the DI container:
/// - `TodoistConfig` - Configuration with API key
/// - `Arc<dyn SyncableProvider>` - The syncable provider (if API key is provided)
///
/// Note: Providers are registered as `Arc<dyn SyncableProvider>` (not wrapped in Mutex)
/// because `sync()` no longer requires `&mut self` - it takes and returns `StreamPosition`.
pub struct TodoistModule;

impl ServiceModule for TodoistModule {
    fn register_services(self, services: &mut ServiceCollection) -> DiResult<()> {
        use tracing::info;
        // Read API key from environment variable
        if let Ok(api_key) = std::env::var("TODOIST_API_KEY") {
            info!("[TodoistModule] API key found, setting up Todoist integration");

            // Create TodoistSyncProvider
            let sync_provider = Arc::new(TodoistSyncProvider::new(TodoistClient::new(&api_key)));

            // Register SyncableProvider trait implementation (for sync operations)
            let provider_trait = sync_provider.clone() as Arc<dyn SyncableProvider>;
            services.add_trait_implementation(provider_trait, Lifetime::Singleton);

            // Also register the concrete type so we can access subscribe_tasks()
            services.add_singleton(sync_provider.clone());

            // Register QueryableCache as a factory that:
            // 1. Gets the backend from DI
            // 2. Creates cache with datasource and backend
            // 3. Subscribes to sync provider's stream to write changes to database
            let sync_provider_for_cache = sync_provider.clone();
            services.add_singleton_factory::<QueryableCache<TodoistTaskDataSource, TodoistTask>, _>(move |resolver| {
                use ferrous_di::Resolver;
                // Get backend from DI (same one used by BackendEngine)
                let backend = Resolver::get_required::<RwLock<TursoBackend>>(resolver);

                // Create cache in a blocking thread (since we're in a sync factory)
                let sync_provider_clone = sync_provider_for_cache.clone();
                let cache = std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new()
                        .expect("Failed to create tokio runtime");
                    rt.block_on(async {
                        // Create a new TodoistTaskDataSource from the sync provider
                        // (TodoistTaskDataSource doesn't implement Clone, so we create a new one)
                        let datasource = TodoistTaskDataSource::new(sync_provider_clone.clone());

                        // Create cache with datasource and backend
                        let cache = QueryableCache::new_with_backend(
                            datasource,
                            backend.clone(),
                        ).await.expect("Failed to create QueryableCache");

                        // Note: Stream subscription is deferred to the launcher
                        // This ensures the spawned task runs on the main runtime, not a temporary one
                        use tracing::info;
                        info!("[TodoistModule] QueryableCache created (stream subscription will be set up in launcher)");

                        cache
                    })
                })
                .join()
                .expect("Thread panicked while creating QueryableCache");

                cache
            });

            // Register QueryableCache as OperationProvider so it can be discovered by OperationDispatcher
            // This enables operations like set_field to work on todoist_tasks
            // The cache will be created when OperationModule collects providers (during BackendEngine creation)
            //
            // IMPORTANT: This factory is called during BackendEngine creation, which happens in the
            // launcher's async context on the main runtime. This means we can safely subscribe the
            // cache to the stream here - tokio::spawn will use the main runtime, not a temporary one.
            let sync_provider_for_trait = sync_provider.clone();
            services.add_trait_factory::<dyn OperationProvider, _>(Lifetime::Singleton, move |resolver| {
                use tracing::info;

                // Get the cache (creates it if needed)
                let cache = resolver.get_required::<QueryableCache<TodoistTaskDataSource, TodoistTask>>();

                // Subscribe cache to sync provider's stream
                // This runs in the launcher's async context, so tokio::spawn uses the main runtime
                info!("[Todoist] Subscribing cache to sync provider stream");
                let rx = sync_provider_for_trait.subscribe_tasks();
                cache.ingest_stream(rx);
                info!("[Todoist] Stream subscription complete - fully plug&play!");

                cache
            });

            info!("[TodoistModule] Todoist integration registered successfully");
        } else {
            info!("[TodoistModule] No TODOIST_API_KEY found, skipping provider registration");
        }

        Ok(())
    }
}

