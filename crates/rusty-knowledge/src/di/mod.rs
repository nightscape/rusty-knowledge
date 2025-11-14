//! Dependency Injection module for rusty-knowledge
//!
//! This module provides service registration and resolution using ferrous-di.
//! It centralizes dependency wiring and makes it easier to test and configure services.

#[cfg(test)]
pub mod test_helpers;

use std::path::PathBuf;
use std::sync::Arc;
use anyhow::Result;
use tokio::sync::RwLock;
use ferrous_di::{ServiceCollection, Resolver, ServiceCollectionModuleExt};

use crate::storage::turso::TursoBackend;
use crate::api::operation_dispatcher::{OperationDispatcher, OperationModule};
use crate::api::backend_engine::BackendEngine;

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
/// - `RwLock<TursoBackend>` (singleton) - Database backend (wrapped in RwLock for BackendEngine)
/// - `OperationDispatcher` (singleton) - Operation dispatcher
/// - `BackendEngine` (singleton) - Render engine (no longer wrapped in RwLock)
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
    // This matches what BackendEngine::from_dependencies expects
    let db_path_clone = db_path.clone();
    services.add_singleton_factory::<RwLock<TursoBackend>, _>(move |_resolver| {
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
        RwLock::new(backend)
    });

    // Register OperationModule to collect providers from DI and create OperationDispatcher
    services.add_module_mut(OperationModule).map_err(|e| anyhow::anyhow!("Failed to register OperationModule: {}", e))?;


    // Register BackendEngine as singleton factory with blocking async initialization
    // BackendEngine no longer needs RwLock wrapper since it uses interior mutability
    services.add_singleton_factory::<BackendEngine, _>(move |resolver| {
        // ferrous-di wraps services in Arc, so we get Arc<Arc<T>> when registering Arc<T>
        // Extract the inner Arc<T> by dereferencing the outer Arc
        let backend_arc = resolver.get_required::<RwLock<TursoBackend>>();
        let backend = backend_arc.clone(); // Dereference to get Arc<RwLock<TursoBackend>>

        // Get dispatcher - ferrous-di wraps singletons in Arc, so we get Arc<OperationDispatcher>
        // No need to clone since we're moving it into the closure
        let dispatcher = resolver.get_required::<OperationDispatcher>();

        let db_path_config: Arc<DatabasePathConfig> = resolver.get_required::<DatabasePathConfig>();
        let db_path_for_thread = db_path_config.path.clone();

        // Create engine in a new thread with its own runtime to avoid "runtime within runtime" error
        let engine = std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new()
                .expect("Failed to create tokio runtime");

            let engine = rt.block_on(async {
                let engine = BackendEngine::from_dependencies(
                    backend,
                    dispatcher,
                ).expect("Failed to create BackendEngine");

                // Initialize database schema and sample data if needed
                engine.initialize_database_if_needed(&db_path_for_thread).await
                    .expect("Failed to initialize database");

                engine
            });

            engine
        })
        .join()
        .expect("Thread panicked while creating BackendEngine");

        engine
    });

    Ok(())
}


