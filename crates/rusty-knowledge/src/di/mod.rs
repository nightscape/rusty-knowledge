//! Dependency Injection module for rusty-knowledge
//!
//! This module provides service registration and resolution using ferrous-di.
//! It centralizes dependency wiring and makes it easier to test and configure services.

use std::path::PathBuf;
use std::sync::Arc;
use anyhow::Result;
use tokio::sync::RwLock;
use ferrous_di::{ServiceCollection, Resolver, ServiceModule, ServiceCollectionModuleExt};

use crate::storage::turso::TursoBackend;
use crate::api::operation_dispatcher::{OperationDispatcher, OperationModule};
use crate::api::render_engine::RenderEngine;
use std::collections::HashMap;

/// Configuration for database path
#[derive(Clone, Debug)]
pub struct DatabasePathConfig {
    pub path: PathBuf,
}

impl DatabasePathConfig {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

/// Register core services in the DI container
///
/// This registers:
/// - `DatabasePathConfig` (singleton) - Database path configuration
/// - `Arc<RwLock<TursoBackend>>` (singleton) - Database backend (wrapped in RwLock for RenderEngine)
/// - `Arc<RwLock<OperationDispatcher>>` (singleton) - Operation dispatcher
/// - `Arc<RwLock<RenderEngine>>` (singleton) - Render engine
///
/// Note: Services are registered as Arc-wrapped types to avoid Clone requirements.
/// The async initialization is handled by blocking in sync factories.
///
/// # Arguments
/// * `services` - Service collection to register services in
/// * `db_path` - Path to the database file
pub fn register_core_services(
    services: &mut ServiceCollection,
    db_path: PathBuf,
) -> Result<()> {
    // Register database path configuration
    services.add_singleton(DatabasePathConfig::new(db_path.clone()));

    // Register Arc<RwLock<TursoBackend>> as singleton factory with blocking async initialization
    // This matches what RenderEngine::from_dependencies expects
    let db_path_clone = db_path.clone();
    services.add_singleton_factory::<Arc<RwLock<TursoBackend>>, _>(move |_resolver| {
        // Create backend in a new thread with its own runtime to avoid "runtime within runtime" error
        let db_path_for_thread = db_path_clone.clone();
        let backend = std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new()
                .expect("Failed to create tokio runtime");
            rt.block_on(TursoBackend::new(db_path_for_thread))
                .expect("Failed to create TursoBackend")
        })
        .join()
        .expect("Thread panicked while creating TursoBackend");
        Arc::new(RwLock::new(backend))
    });

    // Register OperationModule to collect providers from DI
    services.add_module_mut(OperationModule).map_err(|e| anyhow::anyhow!("Failed to register OperationModule: {}", e))?;

    // Register Arc<RwLock<OperationDispatcher>> as singleton factory
    // This wraps the Arc<OperationDispatcher> from OperationModule in RwLock
    // We need to reconstruct it with syncable providers collected from DI
    services.add_singleton_factory::<Arc<RwLock<OperationDispatcher>>, _>(|resolver| {
        let dispatcher_arc_arc = resolver.get_required::<Arc<OperationDispatcher>>();
        let dispatcher_arc = (*dispatcher_arc_arc).clone();
        // Get providers from the dispatcher created by OperationModule
        let providers = dispatcher_arc.providers();

        // Collect syncable providers from DI using get_all_trait
        // Now that sync() doesn't require &mut, providers can be Arc<dyn SyncableProvider> instead of Arc<Mutex<...>>
        let mut syncable_providers = HashMap::new();
        if let Ok(providers) = resolver.get_all_trait::<dyn crate::core::datasource::SyncableProvider>() {
            for provider in providers {
                let name = provider.provider_name().to_string();
                syncable_providers.insert(name, provider);
            }
        }

        Arc::new(RwLock::new(OperationDispatcher::new(
            providers,
            syncable_providers,
        )))
    });

    // Register Arc<RwLock<RenderEngine>> as singleton factory with blocking async initialization
    // This matches what State expects (Arc<RwLock<RenderEngine>>)
    services.add_singleton_factory::<Arc<RwLock<RenderEngine>>, _>(|resolver| {
        // ferrous-di wraps services in Arc, so we get Arc<Arc<T>> when registering Arc<T>
        // Extract the inner Arc<T> by cloning the outer Arc
        let backend_arc_arc = resolver.get_required::<Arc<RwLock<TursoBackend>>>();
        let backend_arc = (*backend_arc_arc).clone(); // Get Arc<RwLock<TursoBackend>>

        let dispatcher_arc_arc = resolver.get_required::<Arc<RwLock<OperationDispatcher>>>();
        let dispatcher = (*dispatcher_arc_arc).clone(); // Get Arc<RwLock<OperationDispatcher>>

        // Create engine in a new thread with its own runtime to avoid "runtime within runtime" error
        let backend_arc_clone = backend_arc.clone();
        let dispatcher_clone = dispatcher.clone();
        let engine = std::thread::spawn(move || {
            RenderEngine::from_dependencies(
                backend_arc_clone,
                dispatcher_clone,
            ).expect("Failed to create RenderEngine")
        })
        .join()
        .expect("Thread panicked while creating RenderEngine");

        Arc::new(RwLock::new(engine))
    });

    Ok(())
}


