//! Shared API crate for rusty-knowledge frontends
//!
//! This crate provides technology-agnostic types and traits for all
//! rusty-knowledge frontends (Tauri, Flutter, future REST API, etc.).
//!
//! # Architecture
//!
//! - `types`: Core data types (Block, InitialState, ApiError, etc.)
//! - `repository`: DocumentRepository trait defining backend operations
//! - `backend_engine`: PRQL render engine for reactive UI (Phase 4.1)
//! - `ffi_bridge`: FFI functions exposed to Flutter (Phase 4.1)
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

pub mod backend_engine;
pub mod ffi_bridge;
pub mod operation_dispatcher;
pub mod streaming;
pub mod ui_types;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod loro_backend_pbt;

// Re-export commonly used types
pub use loro_backend::LoroBackend;
pub use memory_backend::MemoryBackend;
pub use repository::{
    CoreOperations, DocumentRepository, Lifecycle, P2POperations,
};
pub use types::{
    ApiError, Block, BlockMetadata, BlockWithDepth, NewBlock,
    Traversal,
};
pub use streaming::{Change, ChangeOrigin, StreamPosition};

// Re-export render engine types for FFI
pub use backend_engine::BackendEngine;
pub use ui_types::{UiState, CursorPosition};
pub use ffi_bridge::{
    init_render_engine, compile_query, execute_query, watch_query,
    execute_operation,
};
pub use operation_dispatcher::OperationDispatcher;

// Re-export CDC streaming types
pub use crate::storage::turso::{RowChange, ChangeData, RowChangeStream};
