//! Type wrappers and re-exports for Flutter-Rust-Bridge
//!
//! This module re-exports opaque types and defines enums for proper Dart pattern matching.

use flutter_rust_bridge::frb;
use serde::{Deserialize, Serialize};

// Re-export opaque types from backend
// Block will be generated as an opaque type in Dart
// Fields are accessed via BlockOps methods (getId, getContent, etc.)
pub use rusty_knowledge::api::{Block, BlockMetadata, NewBlock, Traversal};

// Define enums directly for proper Dart codegen (mirror doesn't work well for enums)

/// Origin of a change event (local vs. remote).
#[frb]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChangeOrigin {
    /// Change initiated by this client
    Local,
    /// Change received from P2P sync
    Remote,
}

impl From<rusty_knowledge::api::ChangeOrigin> for ChangeOrigin {
    fn from(origin: rusty_knowledge::api::ChangeOrigin) -> Self {
        match origin {
            rusty_knowledge::api::ChangeOrigin::Local => ChangeOrigin::Local,
            rusty_knowledge::api::ChangeOrigin::Remote => ChangeOrigin::Remote,
        }
    }
}

/// Position in the change stream to start watching from.
#[frb]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum StreamPosition {
    /// Start from the beginning
    Beginning,
    /// Start from a specific version
    Version(Vec<u8>),
}

impl From<StreamPosition> for rusty_knowledge::api::StreamPosition {
    fn from(pos: StreamPosition) -> Self {
        match pos {
            StreamPosition::Beginning => rusty_knowledge::api::StreamPosition::Beginning,
            StreamPosition::Version(v) => rusty_knowledge::api::StreamPosition::Version(v),
        }
    }
}

/// Change notification event.
#[frb]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BlockChange {
    /// Block was created
    Created {
        id: String,
        parent_id: String,
        content: String,
        children: Vec<String>,
        origin: ChangeOrigin,
    },
    /// Block content was updated
    Updated {
        id: String,
        content: String,
        origin: ChangeOrigin,
    },
    /// Block was deleted
    Deleted { id: String, origin: ChangeOrigin },
    /// Block was moved
    Moved {
        id: String,
        new_parent: String,
        after: Option<String>,
        origin: ChangeOrigin,
    },
}

impl From<rusty_knowledge::api::BlockChange> for BlockChange {
    fn from(change: rusty_knowledge::api::BlockChange) -> Self {
        match change {
            rusty_knowledge::api::BlockChange::Created { block, origin } => {
                BlockChange::Created {
                    id: block.id,
                    parent_id: block.parent_id,
                    content: block.content,
                    children: block.children,
                    origin: origin.into(),
                }
            }
            rusty_knowledge::api::BlockChange::Updated {
                id,
                content,
                origin,
            } => BlockChange::Updated {
                id,
                content,
                origin: origin.into(),
            },
            rusty_knowledge::api::BlockChange::Deleted { id, origin } => BlockChange::Deleted {
                id,
                origin: origin.into(),
            },
            rusty_knowledge::api::BlockChange::Moved {
                id,
                new_parent,
                after,
                origin,
            } => BlockChange::Moved {
                id,
                new_parent,
                after,
                origin: origin.into(),
            },
        }
    }
}

/// Structured error types for API operations.
#[frb]
#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
pub enum ApiError {
    #[error("Block not found: {id}")]
    BlockNotFound { id: String },

    #[error("Document not found: {doc_id}")]
    DocumentNotFound { doc_id: String },

    #[error("Cyclic move detected: cannot move block {id} to descendant {target_parent}")]
    CyclicMove { id: String, target_parent: String },

    #[error("Invalid operation: {message}")]
    InvalidOperation { message: String },

    #[error("Network error: {message}")]
    NetworkError { message: String },

    #[error("Internal error: {message}")]
    InternalError { message: String },
}

impl From<rusty_knowledge::api::ApiError> for ApiError {
    fn from(err: rusty_knowledge::api::ApiError) -> Self {
        match err {
            rusty_knowledge::api::ApiError::BlockNotFound { id } => ApiError::BlockNotFound { id },
            rusty_knowledge::api::ApiError::DocumentNotFound { doc_id } => {
                ApiError::DocumentNotFound { doc_id }
            }
            rusty_knowledge::api::ApiError::CyclicMove { id, target_parent } => {
                ApiError::CyclicMove { id, target_parent }
            }
            rusty_knowledge::api::ApiError::InvalidOperation { message } => {
                ApiError::InvalidOperation { message }
            }
            rusty_knowledge::api::ApiError::NetworkError { message } => {
                ApiError::NetworkError { message }
            }
            rusty_knowledge::api::ApiError::InternalError { message } => {
                ApiError::InternalError { message }
            }
        }
    }
}
