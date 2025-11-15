//! Dependency Injection module for Todoist integration
//!
//! This module provides DI registration for Todoist-specific services using ferrous-di.

use std::sync::Arc;
use tokio::sync::Mutex;
use ferrous_di::{DiResult, ServiceCollection, ServiceModule, Resolver};

use crate::TodoistClient;
use crate::TodoistSyncProvider;
use rusty_knowledge::core::datasource::SyncableProvider;

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
/// - `Arc<Mutex<dyn SyncableProvider>>` - The syncable provider (if API key is provided)
pub struct TodoistModule;

impl ServiceModule for TodoistModule {
    fn register_services(self, services: &mut ServiceCollection) -> DiResult<()> {
        // Register syncable provider factory that reads config from DI at resolution time
        // The factory will only create the provider if TodoistConfig exists with an API key
        // Note: No longer wrapped in Mutex since sync() doesn't require &mut
        services.add_singleton_factory::<Arc<dyn SyncableProvider>, _>(|resolver| {
            // Try to get TodoistConfig - use get_required which will panic if not found
            // This is OK because the module should only be registered if config exists
            let config_arc_arc = resolver.get_required::<TodoistConfig>();
            let config = (*config_arc_arc).clone();

            if let Some(ref api_key) = config.api_key {
                Arc::new(TodoistSyncProvider::new(TodoistClient::new(api_key))) as Arc<dyn SyncableProvider>
            } else {
                // No API key - this shouldn't happen if module is used correctly
                panic!("TodoistModule: TodoistConfig must have api_key set");
            }
        });

        Ok(())
    }
}

