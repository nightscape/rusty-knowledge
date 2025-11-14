//! Generic testing infrastructure for OperationProvider implementations
//!
//! This module provides property-based testing infrastructure that works
//! for any provider implementing the `OperationProvider` trait.
//!
//! Key components:
//! - `GenericProviderState`: Tracks entity state and generates valid operation sequences
//! - Integration with `proptest-state-machine` for automatic test generation

pub mod generic_provider_state;

pub use generic_provider_state::GenericProviderState;

