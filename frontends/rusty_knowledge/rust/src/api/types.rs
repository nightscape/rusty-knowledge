//! Type re-exports for Flutter-Rust-Bridge
//!
//! With FRB automatic scanning of rusty-knowledge::api, all types can be used
//! directly without Mirror wrappers. This module just re-exports for convenience.

// Re-export types from rusty_knowledge for convenience
pub use rusty_knowledge::api::{
    ApiError, Block, BlockChange, BlockMetadata, ChangeOrigin, NewBlock, StreamPosition, Traversal,
};
