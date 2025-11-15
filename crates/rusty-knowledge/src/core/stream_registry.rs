//! Stream registry for wiring up external sync providers to QueryableCache instances
//!
//! This module provides a registry that allows external systems to register their
//! change streams with QueryableCache instances in a type-safe way using dependency injection.
//! ExternalServiceDiscovery

use std::sync::Arc;
use anyhow::Result;

use crate::core::datasource::{StreamProvider, DataSource};
use crate::core::queryable_cache::QueryableCache;
use crate::core::traits::HasSchema;

/// Registry for wiring up stream providers to QueryableCache instances
///
/// This allows external systems to register their change streams with
/// QueryableCache instances in a type-safe way using dependency injection.
/// ExternalServiceDiscovery
pub struct StreamRegistry;

impl StreamRegistry {
    /// Create a new StreamRegistry
    pub fn new() -> Self {
        Self
    }

    /// Register a stream provider and wire it to a QueryableCache
    ///
    /// This is a generic method that works for any T where:
    /// - Provider implements StreamProvider<T>
    /// - Cache is QueryableCache<S, T>
    /// - T implements HasSchema + Clone
    /// ExternalServiceDiscovery
    pub fn register_stream_to_cache<T, P, S>(
        provider: Arc<P>,
        cache: Arc<QueryableCache<S, T>>,
    ) -> Result<()>
    where
        T: HasSchema + Send + Sync + Clone + 'static,
        P: StreamProvider<T> + Send + Sync + 'static,
        S: DataSource<T> + Send + Sync + 'static,
    {
        let rx = provider.subscribe();
        cache.ingest_stream(rx);
        Ok(())
    }
}

impl Default for StreamRegistry {
    fn default() -> Self {
        Self::new()
    }
}

