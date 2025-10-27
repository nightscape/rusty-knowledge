//! Shared API crate for rusty-knowledge frontends
//!
//! This crate provides technology-agnostic types and traits for all
//! rusty-knowledge frontends (Tauri, Flutter, future REST API, etc.).
//!
//! # Architecture
//!
//! - `types`: Core data types (Block, InitialState, ApiError, etc.)
//! - `repository`: DocumentRepository trait defining backend operations
//!
//! # Design Principles
//!
//! - Technology-agnostic: No frontend-specific dependencies
//! - Clean domain model: Hides CRDT implementation details
//! - Type-safe errors: Structured error handling across FFI boundaries
//! - Async-first: All operations return Futures for flexibility

pub mod loro_backend;
pub mod memory_backend;
pub mod pbt_infrastructure;
pub mod repository;
pub mod types;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod loro_backend_pbt;

// Re-export commonly used types
pub use loro_backend::LoroBackend;
pub use memory_backend::MemoryBackend;
pub use repository::{
    ChangeNotifications, CoreOperations, DocumentRepository, Lifecycle, P2POperations,
};
pub use types::{
    ApiError, Block, BlockChange, BlockMetadata, BlockWithDepth, ChangeOrigin, NewBlock,
    StreamPosition, Traversal,
};
