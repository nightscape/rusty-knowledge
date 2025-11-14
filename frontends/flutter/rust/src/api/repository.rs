//! Flutter-Rust-Bridge bindings for rusty-knowledge DocumentRepository
//!
//! This module provides FFI-safe wrappers around the DocumentRepository trait
//! for use from Flutter via flutter_rust_bridge.

use crate::frb_generated::StreamSink;
use rusty_knowledge::api::loro_backend::LoroBackend;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio_stream::StreamExt;

use rusty_knowledge::api::{ChangeNotifications, CoreOperations, Lifecycle, P2POperations};

// Re-export types for FRB generated code
pub use super::types::{ApiError, Block, BlockChange, ChangeOrigin, NewBlock, StreamPosition, Traversal};

// Factory functions for Traversal (opaque type needs top-level constructors)

/// Create a Traversal for only top-level user blocks (level 1)
pub fn traversal_top_level() -> Traversal {
    Traversal::TOP_LEVEL
}

/// Create a Traversal for all blocks including the synthetic root
pub fn traversal_all() -> Traversal {
    Traversal::ALL
}

/// Create a Traversal for all blocks except the synthetic root
pub fn traversal_all_but_root() -> Traversal {
    Traversal::ALL_BUT_ROOT
}

/// Create a custom Traversal with specific depth range
pub fn traversal_new(min_level: usize, max_level: usize) -> Traversal {
    Traversal::new(min_level, max_level)
}

// Factory functions for StreamPosition (opaque type needs top-level constructors)

/// Create a StreamPosition representing the beginning of the stream
pub fn stream_position_beginning() -> StreamPosition {
    StreamPosition::Beginning
}

/// Create a StreamPosition from a version vector
pub fn stream_position_version(version: Vec<u8>) -> StreamPosition {
    StreamPosition::Version(version)
}

// Block accessor functions (for opaque Block type)
// These are synchronous since they just access fields of already-fetched blocks

/// Get the ID of a block
#[flutter_rust_bridge::frb(sync)]
pub fn block_get_id(block: &Block) -> String {
    block.id.clone()
}

/// Get the parent ID of a block
#[flutter_rust_bridge::frb(sync)]
pub fn block_get_parent_id(block: &Block) -> String {
    block.parent_id.clone()
}

/// Get the content of a block
#[flutter_rust_bridge::frb(sync)]
pub fn block_get_content(block: &Block) -> String {
    block.content.clone()
}

/// Get the children IDs of a block
#[flutter_rust_bridge::frb(sync)]
pub fn block_get_children(block: &Block) -> Vec<String> {
    block.children.clone()
}

/// Flutter-accessible wrapper around DocumentRepository.
///
/// This struct provides FFI-safe access to the rusty-knowledge backend from Flutter.
/// All methods are exposed via flutter_rust_bridge annotations.
pub struct RustDocumentRepository {
    backend: Arc<RwLock<LoroBackend>>,
    /// Handle for the change stream task (for proper cleanup)
    change_task: Arc<RwLock<Option<JoinHandle<()>>>>,
}

impl RustDocumentRepository {
    /// Create a new document repository.
    pub async fn create_new(doc_id: String) -> Result<Self, ApiError> {
        let backend = Lifecycle::create_new(doc_id).await.map_err(ApiError::from)?;
        Ok(Self {
            backend: Arc::new(RwLock::new(backend)),
            change_task: Arc::new(RwLock::new(None)),
        })
    }

    /// Open an existing document repository.
    pub async fn open_existing(doc_id: String) -> Result<Self, ApiError> {
        let backend = Lifecycle::open_existing(doc_id).await.map_err(ApiError::from)?;
        Ok(Self {
            backend: Arc::new(RwLock::new(backend)),
            change_task: Arc::new(RwLock::new(None)),
        })
    }

    /// Get a block by ID.
    pub async fn get_block(&self, id: String) -> Result<Block, ApiError> {
        let backend = self.backend.read().await;
        backend.get_block(&id).await.map_err(Into::into)
    }

    /// Get all non-deleted blocks in tree order.
    pub async fn get_all_blocks(&self, traversal: Traversal) -> Result<Vec<Block>, ApiError> {
        let backend = self.backend.read().await;
        backend.get_all_blocks(traversal).await.map_err(Into::into)
    }

    /// List children IDs of a block in display order.
    pub async fn list_children(&self, parent_id: String) -> Result<Vec<String>, ApiError> {
        let backend = self.backend.read().await;
        backend.list_children(&parent_id).await.map_err(Into::into)
    }

    /// Create a new block.
    pub async fn create_block(
        &self,
        parent_id: String,
        content: String,
        id: Option<String>,
    ) -> Result<Block, ApiError> {
        let backend = self.backend.write().await;
        backend.create_block(parent_id, content, id).await.map_err(Into::into)
    }

    /// Update block content.
    pub async fn update_block(&self, id: String, content: String) -> Result<(), ApiError> {
        let backend = self.backend.write().await;
        backend.update_block(&id, content).await.map_err(Into::into)
    }

    /// Delete a block (tombstone).
    pub async fn delete_block(&self, id: String) -> Result<(), ApiError> {
        let backend = self.backend.write().await;
        backend.delete_block(&id).await.map_err(Into::into)
    }

    /// Move block to new parent and position.
    pub async fn move_block(
        &self,
        id: String,
        new_parent: String,
        after: Option<String>,
    ) -> Result<(), ApiError> {
        let backend = self.backend.write().await;
        backend.move_block(&id, new_parent, after).await.map_err(Into::into)
    }

    // ===== Block-Based Mutation Methods =====
    //
    // These methods accept Block references instead of IDs, providing a more
    // ergonomic API for FFI scenarios where IDs should remain internal.

    /// Update block content using a block reference.
    ///
    /// This method is designed for FFI scenarios where the UI works with opaque
    /// block types and shouldn't need to extract IDs explicitly.
    pub async fn update_block_by_ref(&self, block: Block, content: String) -> Result<(), ApiError> {
        let backend = self.backend.write().await;
        backend.update_block_by_ref(&block, content).await.map_err(Into::into)
    }

    /// Delete a block using a block reference.
    ///
    /// This method accepts a block directly rather than requiring ID extraction.
    pub async fn delete_block_by_ref(&self, block: Block) -> Result<(), ApiError> {
        let backend = self.backend.write().await;
        backend.delete_block_by_ref(&block).await.map_err(Into::into)
    }

    /// Move block to new parent and position using block references.
    ///
    /// All arguments are block references instead of IDs, making it easier to
    /// work with opaque types in FFI scenarios.
    ///
    /// # Arguments
    ///
    /// * `block` - Block to move
    /// * `new_parent` - New parent block (None = move to root)
    /// * `after` - Insert after this sibling (None = insert at start)
    pub async fn move_block_by_ref(
        &self,
        block: Block,
        new_parent: Option<Block>,
        after: Option<Block>,
    ) -> Result<(), ApiError> {
        let backend = self.backend.write().await;
        backend
            .move_block_by_ref(&block, new_parent.as_ref(), after.as_ref())
            .await
            .map_err(Into::into)
    }

    /// Get multiple blocks by ID.
    pub async fn get_blocks(&self, ids: Vec<String>) -> Result<Vec<Block>, ApiError> {
        let backend = self.backend.read().await;
        backend.get_blocks(ids).await.map_err(Into::into)
    }

    /// Create multiple blocks in a single transaction.
    pub async fn create_blocks(&self, blocks: Vec<NewBlock>) -> Result<Vec<Block>, ApiError> {
        let backend = self.backend.write().await;
        backend.create_blocks(blocks).await.map_err(Into::into)
    }

    /// Delete multiple blocks in a single transaction.
    pub async fn delete_blocks(&self, ids: Vec<String>) -> Result<(), ApiError> {
        let backend = self.backend.write().await;
        backend.delete_blocks(ids).await.map_err(Into::into)
    }

    /// Get the current version vector of the document.
    pub async fn get_current_version(&self) -> Result<Vec<u8>, ApiError> {
        let backend = self.backend.read().await;
        backend.get_current_version().await.map_err(Into::into)
    }

    /// Subscribe to document changes since a specific position.
    ///
    /// This method uses flutter_rust_bridge's StreamSink pattern to send changes
    /// to Flutter. The stream will continue until either:
    /// - The sink is dropped on the Dart side
    /// - The backend shuts down
    pub async fn watch_changes_since(
        &self,
        position: StreamPosition,
        sink: StreamSink<BlockChange>,
    ) -> Result<(), ApiError> {
        let backend = self.backend.read().await;
        let mut stream = backend.watch_changes_since(position.into()).await.map_err(ApiError::from)?;

        // Spawn a task to forward stream items to the sink
        let handle = tokio::spawn(async move {
            while let Some(result) = stream.next().await {
                match result {
                    Ok(change) => {
                        // Convert from rusty_knowledge::api::BlockChange to our FFI BlockChange
                        if sink.add(change.into()).is_err() {
                            break;
                        }
                    }
                    Err(err) => {
                        let error_msg = format!("Change stream error: {:?}", err);
                        if sink.add_error(anyhow::anyhow!(error_msg)).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        // Store the JoinHandle for cleanup
        let mut task_lock = self.change_task.write().await;
        if let Some(old_task) = task_lock.take() {
            old_task.abort();
        }
        *task_lock = Some(handle);

        Ok(())
    }

    /// Unsubscribe from change stream and stop the background task.
    pub async fn unsubscribe(&self) -> Result<(), ApiError> {
        let mut task_lock = self.change_task.write().await;
        if let Some(task) = task_lock.take() {
            task.abort();
        }
        Ok(())
    }

    /// Get this node's P2P identifier.
    pub async fn get_node_id(&self) -> String {
        let backend = self.backend.read().await;
        backend.get_node_id().await
    }

    /// Connect to a remote peer for P2P synchronization.
    pub async fn connect_to_peer(&self, peer_node_id: String) -> Result<(), ApiError> {
        let backend = self.backend.write().await;
        backend.connect_to_peer(peer_node_id).await.map_err(Into::into)
    }

    /// Start accepting incoming P2P connections.
    pub async fn accept_connections(&self) -> Result<(), ApiError> {
        let backend = self.backend.write().await;
        backend.accept_connections().await.map_err(Into::into)
    }

    /// Dispose of document and release resources.
    pub async fn dispose(&self) -> Result<(), ApiError> {
        // Abort change stream task
        let mut task_lock = self.change_task.write().await;
        if let Some(task) = task_lock.take() {
            task.abort();
        }
        drop(task_lock);

        // Dispose backend
        let backend = self.backend.write().await;
        backend.dispose().await.map_err(Into::into)
    }
}
