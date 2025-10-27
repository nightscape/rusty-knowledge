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
pub use super::types::{ApiError, Block, BlockChange, NewBlock, StreamPosition, Traversal};

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
        let backend = Lifecycle::create_new(doc_id).await?;
        Ok(Self {
            backend: Arc::new(RwLock::new(backend)),
            change_task: Arc::new(RwLock::new(None)),
        })
    }

    /// Open an existing document repository.
    pub async fn open_existing(doc_id: String) -> Result<Self, ApiError> {
        let backend = Lifecycle::open_existing(doc_id).await?;
        Ok(Self {
            backend: Arc::new(RwLock::new(backend)),
            change_task: Arc::new(RwLock::new(None)),
        })
    }

    /// Get a block by ID.
    pub async fn get_block(&self, id: String) -> Result<Block, ApiError> {
        let backend = self.backend.read().await;
        backend.get_block(&id).await
    }

    /// Get all non-deleted blocks in tree order.
    pub async fn get_all_blocks(&self, traversal: Traversal) -> Result<Vec<Block>, ApiError> {
        let backend = self.backend.read().await;
        backend.get_all_blocks(traversal).await
    }

    /// List children IDs of a block in display order.
    pub async fn list_children(&self, parent_id: String) -> Result<Vec<String>, ApiError> {
        let backend = self.backend.read().await;
        backend.list_children(&parent_id).await
    }

    /// Create a new block.
    pub async fn create_block(
        &self,
        parent_id: String,
        content: String,
        id: Option<String>,
    ) -> Result<Block, ApiError> {
        let backend = self.backend.write().await;
        backend.create_block(parent_id, content, id).await
    }

    /// Update block content.
    pub async fn update_block(&self, id: String, content: String) -> Result<(), ApiError> {
        let backend = self.backend.write().await;
        backend.update_block(&id, content).await
    }

    /// Delete a block (tombstone).
    pub async fn delete_block(&self, id: String) -> Result<(), ApiError> {
        let backend = self.backend.write().await;
        backend.delete_block(&id).await
    }

    /// Move block to new parent and position.
    pub async fn move_block(
        &self,
        id: String,
        new_parent: String,
        after: Option<String>,
    ) -> Result<(), ApiError> {
        let backend = self.backend.write().await;
        backend.move_block(&id, new_parent, after).await
    }

    /// Get multiple blocks by ID.
    pub async fn get_blocks(&self, ids: Vec<String>) -> Result<Vec<Block>, ApiError> {
        let backend = self.backend.read().await;
        backend.get_blocks(ids).await
    }

    /// Create multiple blocks in a single transaction.
    pub async fn create_blocks(&self, blocks: Vec<NewBlock>) -> Result<Vec<Block>, ApiError> {
        let backend = self.backend.write().await;
        backend.create_blocks(blocks).await
    }

    /// Delete multiple blocks in a single transaction.
    pub async fn delete_blocks(&self, ids: Vec<String>) -> Result<(), ApiError> {
        let backend = self.backend.write().await;
        backend.delete_blocks(ids).await
    }

    /// Get the current version vector of the document.
    pub async fn get_current_version(&self) -> Result<Vec<u8>, ApiError> {
        let backend = self.backend.read().await;
        backend.get_current_version().await
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
        let mut stream = backend.watch_changes_since(position).await?;

        // Spawn a task to forward stream items to the sink
        let handle = tokio::spawn(async move {
            while let Some(result) = stream.next().await {
                match result {
                    Ok(change) => {
                        if sink.add(change).is_err() {
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
        backend.connect_to_peer(peer_node_id).await
    }

    /// Start accepting incoming P2P connections.
    pub async fn accept_connections(&self) -> Result<(), ApiError> {
        let backend = self.backend.write().await;
        backend.accept_connections().await
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
        backend.dispose().await
    }
}
