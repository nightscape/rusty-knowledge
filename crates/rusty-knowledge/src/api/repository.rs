//! DocumentRepository trait and related types
//!
//! This module defines the repository pattern interface for interacting
//! with hierarchical block documents. All frontends use this trait.
//!
//! # Trait Architecture
//!
//! The API is split into 4 focused traits that backends can implement selectively:
//!
//! - `CoreOperations`: CRUD and batch operations (required for all backends)
//! - `Lifecycle`: Document creation and disposal (required for all backends)
//! - `ChangeNotifications`: Real-time state sync and change streams
//! - `P2POperations`: Peer-to-peer networking and synchronization
//!
//! The `DocumentRepository` supertrait combines all four for convenience.
//! Backends implementing all four automatically satisfy `DocumentRepository`.
//!
//! ## Examples
//!
//! ```rust,no_run
//! use rusty_knowledge::api::{CoreOperations, Lifecycle};
//!
//! // Minimal backend (no networking, no change notifications)
//! struct MemoryBackend { /* ... */ }
//! impl CoreOperations for MemoryBackend { /* ... */ }
//! impl Lifecycle for MemoryBackend { /* ... */ }
//!
//! // Full-featured backend
//! struct LoroBackend { /* ... */ }
//! impl CoreOperations for LoroBackend { /* ... */ }
//! impl Lifecycle for LoroBackend { /* ... */ }
//! impl ChangeNotifications for LoroBackend { /* ... */ }
//! impl P2POperations for LoroBackend { /* ... */ }
//! // LoroBackend automatically implements DocumentRepository via blanket impl
//! ```

use super::types::{ApiError, Block, BlockChange, NewBlock, StreamPosition, Traversal};
use async_trait::async_trait;
use std::pin::Pin;
use tokio_stream::Stream;

/// Core CRUD and batch operations for block documents.
///
/// This trait provides the fundamental operations for creating, reading,
/// updating, and deleting blocks. All backends must implement this trait.
///
/// # Example
///
/// ```rust,no_run
/// use rusty_knowledge::api::CoreOperations;
///
/// async fn example(repo: impl CoreOperations) -> anyhow::Result<()> {
///     // Create a new root block
///     let block = repo.create_block(None, "Hello".to_string(), None).await?;
///
///     // Update its content
///     repo.update_block(&block.id, "Hello, world!".to_string()).await?;
///
///     // Get it back
///     let updated = repo.get_block(&block.id).await?;
///
///     Ok(())
/// }
/// ```
#[async_trait]
pub trait CoreOperations: Send + Sync {
    // ===== Single-Block Operations =====

    /// Get a block by ID.
    ///
    /// # Arguments
    ///
    /// * `id` - Block ID (URI format)
    ///
    /// # Errors
    ///
    /// Returns `ApiError::BlockNotFound` if block doesn't exist or is deleted.
    async fn get_block(&self, id: &str) -> Result<Block, ApiError>;

    /// Get ancestor chain by traversing parent_id links.
    ///
    /// Returns a vector of parent IDs from immediate parent up to (but not including) the root.
    /// Stops when encountering NO_PARENT_ID sentinel value or the root block itself.
    ///
    /// # Arguments
    ///
    /// * `id` - Starting block ID
    ///
    /// # Returns
    ///
    /// Vector of ancestor IDs in order: [immediate_parent, grandparent, ..., root_child]
    ///
    /// # Errors
    ///
    /// Returns `ApiError::BlockNotFound` if any block in the chain doesn't exist.
    async fn get_ancestor_chain(&self, id: &str) -> Result<Vec<String>, ApiError> {
        use super::types::NO_PARENT_ID;

        let mut ancestors = Vec::new();
        let mut current_id = id.to_string();

        loop {
            let block = self.get_block(&current_id).await?;

            if block.parent_id == NO_PARENT_ID {
                break;
            }

            ancestors.push(block.parent_id.clone());
            current_id = block.parent_id;
        }

        Ok(ancestors)
    }

    /// Get all non-deleted blocks in tree order, filtered by depth.
    ///
    /// Returns blocks in depth-first traversal order. Use `Block::depth()`
    /// if you need nesting level information.
    ///
    /// # Example Order
    ///
    /// ```text
    /// Block A (root)
    ///   Block A1 (child)
    ///     Block A1a (grandchild)
    ///   Block A2 (child)
    /// Block B (root)
    /// ```
    ///
    /// # Arguments
    ///
    /// * `traversal` - Depth filter configuration (use `Traversal::ALL_BUT_ROOT` for typical use)
    ///
    /// # Performance
    ///
    /// This method may be expensive for large documents. Use with caution
    /// in performance-critical code paths. The traversal can stop descending
    /// once max_level is reached for optimization.
    async fn get_all_blocks(&self, traversal: Traversal) -> Result<Vec<Block>, ApiError>;

    /// List children IDs of a block in display order.
    ///
    /// # Arguments
    ///
    /// * `parent_id` - Parent block ID
    ///
    /// # Errors
    ///
    /// Returns `ApiError::BlockNotFound` if parent doesn't exist.
    async fn list_children(&self, parent_id: &str) -> Result<Vec<String>, ApiError>;

    /// Create a new block.
    ///
    /// # Arguments
    ///
    /// * `parent_id` - Parent block ID
    /// * `content` - Initial text content
    /// * `id` - Optional custom ID (None = generate `local://<uuid-v4>`)
    ///
    /// # Returns
    ///
    /// The created `Block` with generated or provided ID.
    ///
    /// # Errors
    ///
    /// Returns `ApiError::BlockNotFound` if parent_id doesn't exist.
    async fn create_block(
        &self,
        parent_id: String,
        content: String,
        id: Option<String>,
    ) -> Result<Block, ApiError>;

    /// Update block content.
    ///
    /// # Arguments
    ///
    /// * `id` - Block ID to update
    /// * `content` - New content (replaces existing)
    ///
    /// # Errors
    ///
    /// Returns `ApiError::BlockNotFound` if block doesn't exist.
    async fn update_block(&self, id: &str, content: String) -> Result<(), ApiError>;

    /// Delete a block (tombstone).
    ///
    /// Sets `deleted_at` timestamp but keeps block in CRDT for consistency.
    /// Children are NOT deleted (no cascading delete).
    ///
    /// # Arguments
    ///
    /// * `id` - Block ID to delete
    ///
    /// # Errors
    ///
    /// Returns `ApiError::BlockNotFound` if block doesn't exist.
    async fn delete_block(&self, id: &str) -> Result<(), ApiError>;

    /// Move block to new parent and position.
    ///
    /// Uses anchor-based positioning (after sibling) which is more CRDT-friendly
    /// than index-based positioning.
    ///
    /// # Arguments
    ///
    /// * `id` - Block to move
    /// * `new_parent` - New parent ID
    /// * `after` - Insert after this sibling (None = insert at start)
    ///
    /// # Errors
    ///
    /// * `ApiError::BlockNotFound` - Block or parent doesn't exist
    /// * `ApiError::CyclicMove` - Would create a cycle (block to its descendant)
    async fn move_block(
        &self,
        id: &str,
        new_parent: String,
        after: Option<String>,
    ) -> Result<(), ApiError>;

    // ===== Batch Operations =====

    /// Get multiple blocks by ID.
    ///
    /// More efficient than multiple `get_block()` calls when fetching many blocks.
    ///
    /// # Arguments
    ///
    /// * `ids` - List of block IDs to fetch
    ///
    /// # Returns
    ///
    /// Vector of blocks that exist. Non-existent blocks are omitted (no error).
    async fn get_blocks(&self, ids: Vec<String>) -> Result<Vec<Block>, ApiError>;

    /// Create multiple blocks in a single transaction.
    ///
    /// All blocks are created atomically. If any fails, entire operation rolls back.
    ///
    /// # Arguments
    ///
    /// * `blocks` - List of block templates
    ///
    /// # Returns
    ///
    /// Vector of created blocks in same order as input.
    ///
    /// # Errors
    ///
    /// Returns error if any parent doesn't exist or operation fails.
    async fn create_blocks(&self, blocks: Vec<NewBlock>) -> Result<Vec<Block>, ApiError>;

    /// Delete multiple blocks in a single transaction.
    ///
    /// All blocks are deleted atomically (tombstone pattern).
    ///
    /// # Arguments
    ///
    /// * `ids` - List of block IDs to delete
    ///
    /// # Errors
    ///
    /// Returns error if operation fails. Non-existent blocks are silently ignored.
    async fn delete_blocks(&self, ids: Vec<String>) -> Result<(), ApiError>;
}

/// Document lifecycle management.
///
/// This trait handles creating new documents, opening existing ones,
/// and resource cleanup. All backends must implement this trait.
///
/// # Example
///
/// ```rust,no_run
/// use rusty_knowledge::api::Lifecycle;
///
/// async fn example<R: Lifecycle>() -> anyhow::Result<()> {
///     // Create a new document
///     let repo = R::create_new("my-doc".to_string()).await?;
///
///     // ... use the repository ...
///
///     // Clean up resources
///     repo.dispose().await?;
///
///     Ok(())
/// }
/// ```
#[async_trait]
pub trait Lifecycle: Send + Sync {
    /// Create a new document.
    ///
    /// # Arguments
    ///
    /// * `doc_id` - Unique identifier for the document
    ///
    /// # Errors
    ///
    /// Returns `ApiError::InternalError` if document creation fails.
    async fn create_new(doc_id: String) -> Result<Self, ApiError>
    where
        Self: Sized;

    /// Open an existing document.
    ///
    /// # Arguments
    ///
    /// * `doc_id` - Identifier of document to open
    ///
    /// # Errors
    ///
    /// Returns `ApiError::DocumentNotFound` if document doesn't exist.
    async fn open_existing(doc_id: String) -> Result<Self, ApiError>
    where
        Self: Sized;

    /// Dispose of document and release resources.
    ///
    /// Should be called before dropping the repository to ensure clean shutdown.
    /// Critical for Flutter hot-restart support.
    async fn dispose(&self) -> Result<(), ApiError>;
}

/// Real-time change notification and state synchronization.
///
/// This trait provides race-free state sync by streaming the current document state
/// followed by all subsequent changes. Backends that support real-time updates implement this trait.
///
/// # Architecture
///
/// This trait uses vendor-neutral Rust async Streams (`tokio_stream::Stream`)
/// which can be adapted to any frontend technology:
/// - Flutter: Adapted via `StreamSink` in FRB bridge layer
/// - Tauri: Adapted via event emission in command layer
/// - REST/Web: Adapted via Server-Sent Events or WebSocket
///
/// # Example
///
/// ```rust,no_run
/// use rusty_knowledge::api::ChangeNotifications;
/// use tokio_stream::StreamExt;
///
/// async fn example(repo: impl ChangeNotifications) -> anyhow::Result<()> {
///     // Start watching - first receives all current blocks as Created events,
///     // then streams subsequent changes
///     let mut stream = repo.watch_changes().await?;
///
///     // Process changes as they arrive
///     while let Some(result) = stream.next().await {
///         match result {
///             Ok(change) => println!("Block changed: {:?}", change),
///             Err(e) => eprintln!("Change stream error: {:?}", e),
///         }
///     }
///
///     // Stream automatically unsubscribes when dropped
///     Ok(())
/// }
/// ```
#[async_trait]
pub trait ChangeNotifications: Send + Sync {
    /// Subscribe to document changes since a specific position.
    ///
    /// Returns a Stream that emits document changes. Behavior depends on the `position` parameter:
    /// - `StreamPosition::Beginning`: First emits all current blocks as `BlockChange::Created` events,
    ///   then continues streaming subsequent changes (initial sync mode)
    /// - `StreamPosition::Version(v)`: Streams only changes that occurred after version `v`
    ///   (incremental sync mode)
    ///
    /// # Arguments
    ///
    /// * `position` - Stream position to start from (beginning or specific version)
    ///
    /// # Returns
    ///
    /// A Stream that yields `Result<BlockChange, ApiError>` items. The stream
    /// continues until either:
    /// - It is explicitly dropped (automatic unsubscription)
    /// - An error occurs (yielded as `Err`)
    /// - The backend shuts down (stream closes)
    ///
    /// # Error Propagation
    ///
    /// Errors are propagated through the stream's Result type rather than
    /// terminating the stream. Backends may choose to:
    /// - Continue streaming after recoverable errors
    /// - Close the stream after fatal errors
    ///
    /// # Resource Management
    ///
    /// The stream automatically unsubscribes and releases resources when dropped.
    /// No explicit cleanup method needed.
    async fn watch_changes_since(
        &self,
        position: StreamPosition,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<BlockChange, ApiError>> + Send>>, ApiError>;

    /// Get the current version vector of the document.
    ///
    /// Returns the version vector representing the current state of the document.
    /// This can be used to track document evolution over time.
    ///
    /// # Returns
    ///
    /// A version vector as a byte array.
    async fn get_current_version(&self) -> Result<Vec<u8>, ApiError>;
}

/// Peer-to-peer networking and synchronization.
///
/// This trait provides P2P connectivity for distributed document synchronization.
/// Backends that support networking implement this trait.
///
/// # Example
///
/// ```rust,no_run
/// use rusty_knowledge::api::P2POperations;
///
/// async fn example(repo: impl P2POperations) -> anyhow::Result<()> {
///     // Get this node's ID
///     let my_id = repo.get_node_id().await;
///     println!("Node ID: {}", my_id);
///
///     // Start accepting connections
///     repo.accept_connections().await?;
///
///     // Connect to a peer
///     repo.connect_to_peer("peer-node-id".to_string()).await?;
///
///     Ok(())
/// }
/// ```
#[async_trait]
pub trait P2POperations: Send + Sync {
    /// Get this node's P2P identifier.
    ///
    /// Used by other peers to connect to this instance.
    async fn get_node_id(&self) -> String;

    /// Connect to a remote peer for P2P synchronization.
    ///
    /// # Arguments
    ///
    /// * `peer_node_id` - Remote peer's node ID
    ///
    /// # Errors
    ///
    /// Returns `ApiError::NetworkError` if connection fails.
    async fn connect_to_peer(&self, peer_node_id: String) -> Result<(), ApiError>;

    /// Start accepting incoming P2P connections.
    ///
    /// # Errors
    ///
    /// Returns `ApiError::NetworkError` if unable to listen.
    async fn accept_connections(&self) -> Result<(), ApiError>;
}

/// Complete repository interface combining all capabilities.
///
/// This is a convenience supertrait that combines `CoreOperations`, `Lifecycle`,
/// `ChangeNotifications`, and `P2POperations`. Any type implementing all four
/// automatically satisfies this trait via the blanket implementation.
///
/// # Example
///
/// ```rust,no_run
/// use rusty_knowledge::api::DocumentRepository;
///
/// async fn example(repo: impl DocumentRepository) -> anyhow::Result<()> {
///     // Can use all operations from all four traits
///     let initial = repo.get_initial_state().await?;
///     let block = repo.create_block(None, "Hello".to_string(), None).await?;
///     let node_id = repo.get_node_id().await;
///     repo.dispose().await?;
///     Ok(())
/// }
/// ```
pub trait DocumentRepository:
    CoreOperations + Lifecycle + ChangeNotifications + P2POperations
{
}

/// Blanket implementation: any type implementing all four traits automatically
/// implements DocumentRepository.
impl<T> DocumentRepository for T where
    T: CoreOperations + Lifecycle + ChangeNotifications + P2POperations
{
}
