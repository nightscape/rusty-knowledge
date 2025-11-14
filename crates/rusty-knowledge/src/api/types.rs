//! Core data types for rusty-knowledge API
//!
//! This module defines the types used across all frontends (Tauri, Flutter, etc.)
//! to interact with the rusty-knowledge backend.

use serde::{Deserialize, Serialize};

/// ID of the root block in the document tree.
/// The root block is a synthetic container for all top-level blocks.
pub const ROOT_PARENT_ID: &str = "__root_parent__";

/// Sentinel value indicating a block has no parent (used for root block's parent_id).
/// This prevents the root block from forming a cycle with itself.
pub const NO_PARENT_ID: &str = "__no_parent__";

/// Configuration for filtering blocks by tree depth when traversing.
///
/// Depth levels:
/// - Level 0: Root of the current document
/// - Level 1: Top-level user blocks (direct children of root)
/// - Level 2+: Nested blocks
///
/// # Examples
///
/// ```rust
/// use rusty_knowledge::api::Traversal;
///
/// // Get only top-level blocks
/// let top_level = Traversal::TOP_LEVEL;
///
/// // Get all blocks including root
/// let all = Traversal::ALL;
///
/// // Get outline headers only (levels 1-2)
/// let headers = Traversal::new(1, 2);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Traversal {
    /// Minimum depth level to include (inclusive)
    pub min_level: usize,
    /// Maximum depth level to include (inclusive)
    pub max_level: usize,
}

impl Traversal {
    /// Only top-level user blocks (level 1)
    pub const TOP_LEVEL: Self = Self {
        min_level: 1,
        max_level: 1,
    };

    /// All blocks including the synthetic root (levels 0 to MAX)
    pub const ALL: Self = Self {
        min_level: 0,
        max_level: usize::MAX,
    };

    /// All blocks except the synthetic root (levels 1 to MAX)
    pub const ALL_BUT_ROOT: Self = Self {
        min_level: 1,
        max_level: usize::MAX,
    };

    /// Create a custom depth range filter
    pub const fn new(min_level: usize, max_level: usize) -> Self {
        Self {
            min_level,
            max_level,
        }
    }

    /// Check if a given depth level should be included
    pub const fn includes_level(&self, level: usize) -> bool {
        level >= self.min_level && level <= self.max_level
    }

}

/// A block in the hierarchical document structure.
///
/// Blocks use URI-based IDs to support integration with external systems:
/// - Local blocks: `local://<uuid-v4>` (e.g., `local://550e8400-e29b-41d4-a716-446655440000`)
/// - External systems: `todoist://task/12345`, `logseq://page/abc123`
///
/// # Example
///
/// ```rust
/// use rusty_knowledge::api::Block;
///
/// let block = Block {
///     id: "local://550e8400-e29b-41d4-a716-446655440000".to_string(),
///     parent_id: "parent_id".to_string(),
///     content: "My first block".to_string(),
///     children: vec![],
///     metadata: Default::default(),
/// };
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Block {
    /// URI-based unique identifier
    pub id: String,
    /// Parent block ID
    pub parent_id: String,
    /// Text content of the block
    pub content: String,
    /// IDs of child blocks in display order
    pub children: Vec<String>,
    /// Block metadata (timestamps, etc.)
    pub metadata: BlockMetadata,
}

impl Block {
    /// Get the depth/nesting level of this block by following parent chain.
    ///
    /// This requires a lookup function to resolve parent IDs to blocks.
    /// Returns 0 for root blocks, 1 for children of roots, etc.
    ///
    /// # Arguments
    ///
    /// * `get_block` - Function to look up a block by ID
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use rusty_knowledge::api::Block;
    /// use std::collections::HashMap;
    ///
    /// let mut blocks = HashMap::new();
    /// // ... populate blocks ...
    ///
    /// let block = blocks.get("some-id").unwrap();
    /// let depth = block.depth(&|id| blocks.get(id));
    /// ```
    pub fn depth<'blk, F>(&self, mut get_block: F) -> usize
    where
        F: for<'a> FnMut(&'a str) -> Option<&'blk Block>,
    {
        let mut depth = 0;
        let mut current_parent = Some(self.parent_id.as_str());

        while let Some(parent_id) = current_parent {
            depth += 1;
            current_parent = get_block(parent_id).map(|b| b.parent_id.as_str());
        }

        depth
    }

}

/// A block with its tree depth/nesting level.
///
/// Used for tree-ordered iteration and diffing. The depth indicates
/// how deeply nested the block is (0 = root, 1 = child of root, etc.).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BlockWithDepth {
    /// The block data
    pub block: Block,
    /// Nesting depth (0 = root level)
    pub depth: usize,
}

/// Metadata associated with a block.
///
/// Note: UI state like `collapsed` is NOT stored here - it's kept locally
/// in the frontend to avoid cross-user UI churn in collaborative sessions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BlockMetadata {
    /// Unix timestamp (milliseconds) when block was created
    pub created_at: i64,
    /// Unix timestamp (milliseconds) when block was last updated
    pub updated_at: i64,
}


/// Structured error types for API operations.
///
/// These errors are designed to cross FFI boundaries (e.g., Rust to Dart)
/// and provide type-safe error handling in frontends.
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

/// Template for creating a new block in a batch operation.
///
/// # ID Generation
///
/// - `id = None`: Generate `local://<uuid-v4>`
/// - `id = Some(uri)`: Use provided URI (e.g., `todoist://task/123`)
///
/// # Positioning
///
/// - `after = None`: Insert at start of parent's children
/// - `after = Some(sibling_id)`: Insert after specified sibling
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewBlock {
    /// Parent block ID
    pub parent_id: String,
    /// Initial content
    pub content: String,
    /// Position anchor: insert after this sibling (None = insert at start)
    pub after: Option<String>,
    /// Optional custom ID (None = generate local URI)
    pub id: Option<String>,
}

