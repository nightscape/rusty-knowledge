//! Test helpers for dependency injection
//!
//! Provides utilities for setting up DI containers in tests.

use std::path::PathBuf;
use std::sync::Arc;
use anyhow::Result;
use ferrous_di::{ServiceCollection, ServiceCollectionModuleExt, Resolver, ServiceModule, DiResult};

use crate::api::backend_engine::BackendEngine;
use crate::api::operation_dispatcher::OperationModule;
use crate::core::datasource::{OperationProvider, SyncableProvider};
use super::register_core_services;

/// Create a BackendEngine for testing using dependency injection
///
/// This sets up a complete DI container with all core services, using an in-memory database.
/// This ensures tests use the same setup as production code.
///
/// # Example
/// ```rust
/// #[tokio::test]
/// async fn test_something() {
///     let engine = create_test_engine().await.unwrap();
///     // Use engine for testing...
/// }
/// ```
pub async fn create_test_engine() -> Result<Arc<BackendEngine>> {
    create_test_engine_with_path(":memory:".into()).await
}

/// Create a BackendEngine for testing with a specific database path
///
/// This sets up a complete DI container with all core services.
/// Useful for tests that need a specific database path or want to persist data.
///
/// # Arguments
/// * `db_path` - Path to the database file (use ":memory:" for in-memory)
///
/// # Example
/// ```rust
/// #[tokio::test]
/// async fn test_with_persistence() {
///     let engine = create_test_engine_with_path("/tmp/test.db".into()).await.unwrap();
///     // Use engine for testing...
/// }
/// ```
pub async fn create_test_engine_with_path(db_path: PathBuf) -> Result<Arc<BackendEngine>> {
    // Set up dependency injection container
    let mut services = ServiceCollection::new();

    // Register OperationModule to collect providers from DI
    services.add_module_mut(OperationModule)
        .map_err(|e| anyhow::anyhow!("Failed to register OperationModule: {}", e))?;

    // Register core services
    register_core_services(&mut services, db_path)
        .map_err(|e| anyhow::anyhow!("Failed to register core services: {}", e))?;

    // Build the DI container and resolve BackendEngine
    let provider = services.build();
    let engine = Resolver::get_required::<BackendEngine>(&provider);

    Ok(engine)
}

/// Create a test engine with custom providers
///
/// This allows tests to register additional providers before creating the engine.
/// The providers will be collected by OperationModule and included in the OperationDispatcher.
///
/// # Arguments
/// * `db_path` - Path to the database file (use ":memory:" for in-memory)
/// * `setup_fn` - Closure that registers additional services/providers
///
/// # Example
/// ```rust
/// #[tokio::test]
/// async fn test_with_custom_provider() {
///     let engine = create_test_engine_with_setup(":memory:".into(), |services| {
///         // Register a custom provider
///         services.add_singleton(my_provider as Arc<dyn OperationProvider>);
///         Ok(())
///     }).await.unwrap();
/// }
/// ```
pub async fn create_test_engine_with_setup<F>(
    db_path: PathBuf,
    setup_fn: F,
) -> Result<Arc<BackendEngine>>
where
    F: FnOnce(&mut ServiceCollection) -> Result<()>,
{
    // Set up dependency injection container
    let mut services = ServiceCollection::new();

    // Allow caller to register custom services/providers
    setup_fn(&mut services)?;

    // Register OperationModule to collect providers from DI
    services.add_module_mut(OperationModule)
        .map_err(|e| anyhow::anyhow!("Failed to register OperationModule: {}", e))?;

    // Register core services
    register_core_services(&mut services, db_path)
        .map_err(|e| anyhow::anyhow!("Failed to register core services: {}", e))?;

    // Build the DI container and resolve BackendEngine
    let provider = services.build();
    let engine = Resolver::get_required::<BackendEngine>(&provider);

    Ok(engine)
}

/// Test-specific ServiceModule for registering providers
///
/// This module makes it easy to register operation providers and syncable providers
/// for testing. It wraps the standard OperationModule and allows adding test-specific
/// providers before the dispatcher is created.
///
/// # Example
/// ```rust
/// use crate::di::test_helpers::TestProviderModule;
///
/// let module = TestProviderModule::new()
///     .with_operation_provider(my_provider)
///     .with_syncable_provider("todoist", todoist_provider);
///
/// services.add_module_mut(module);
/// ```
pub struct TestProviderModule {
    operation_providers: Vec<Arc<dyn OperationProvider>>,
    syncable_providers: Vec<(String, Arc<dyn SyncableProvider>)>,
}

impl TestProviderModule {
    /// Create a new TestProviderModule
    pub fn new() -> Self {
        Self {
            operation_providers: Vec::new(),
            syncable_providers: Vec::new(),
        }
    }

    /// Add an operation provider
    pub fn with_operation_provider(mut self, provider: Arc<dyn OperationProvider>) -> Self {
        self.operation_providers.push(provider);
        self
    }

    /// Add a syncable provider
    pub fn with_syncable_provider(mut self, name: String, provider: Arc<dyn SyncableProvider>) -> Self {
        self.syncable_providers.push((name, provider));
        self
    }
}

impl Default for TestProviderModule {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceModule for TestProviderModule {
    fn register_services(self, services: &mut ServiceCollection) -> DiResult<()> {
        // Register all operation providers
        for provider in self.operation_providers {
            services.add_singleton(provider);
        }

        // Register all syncable providers
        // OperationModule will collect them via get_all_trait and map by provider_name()
        for (_name, provider) in self.syncable_providers {
            services.add_singleton(provider);
        }

        // Register the standard OperationModule to collect all providers
        services.add_module_mut(OperationModule)?;

        Ok(())
    }
}

/// Create a test engine with providers registered via TestProviderModule
///
/// This is a convenience function that makes it easy to create a test engine
/// with custom providers using the builder pattern.
///
/// # Arguments
/// * `db_path` - Path to the database file (use ":memory:" for in-memory)
/// * `setup_fn` - Closure that builds a TestProviderModule with providers
///
/// # Example
/// ```rust
/// let engine = create_test_engine_with_providers(":memory:".into(), |module| {
///     module
///         .with_operation_provider(my_provider)
///         .with_syncable_provider("todoist", todoist_provider)
/// }).await.unwrap();
/// ```
pub async fn create_test_engine_with_providers<F>(
    db_path: PathBuf,
    setup_fn: F,
) -> Result<Arc<BackendEngine>>
where
    F: FnOnce(TestProviderModule) -> TestProviderModule,
{
    // Set up dependency injection container
    let mut services = ServiceCollection::new();

    // Build and register the test provider module
    let provider_module = setup_fn(TestProviderModule::new());
    services.add_module_mut(provider_module)
        .map_err(|e| anyhow::anyhow!("Failed to register TestProviderModule: {}", e))?;

    // Register core services
    register_core_services(&mut services, db_path)
        .map_err(|e| anyhow::anyhow!("Failed to register core services: {}", e))?;

    // Build the DI container and resolve BackendEngine
    let provider = services.build();
    let engine = Resolver::get_required::<BackendEngine>(&provider);

    Ok(engine)
}
