//! Loro-based implementation of DocumentRepository
//!
//! This module provides a CRDT-backed implementation using Loro with a normalized
//! adjacency-list data model for hierarchical blocks.

use super::repository::{ChangeNotifications, CoreOperations, Lifecycle, P2POperations};
use super::types::{
    ApiError, Block, BlockChange, BlockMetadata, ChangeOrigin, NewBlock, StreamPosition,
};
use crate::sync::CollaborativeDoc;
use async_trait::async_trait;
use iroh::{NodeAddr, PublicKey};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_stream::{Stream, StreamExt, wrappers::ReceiverStream};
use uuid::Uuid;

/// Type alias for change notification subscribers
type ChangeSubscribers = Arc<Mutex<Vec<mpsc::Sender<Result<BlockChange, ApiError>>>>>;

/// Helper trait for collecting and searching values in Loro containers
trait LoroListExt {
    /// Collect values by applying a function to each element, keeping only Some results
    fn collect_map<T, F>(&self, f: F) -> Vec<T>
    where
        F: FnMut(loro::ValueOrContainer) -> Option<T>;

    /// Find the index of the first element where the function returns Some(true)
    fn find_index<F>(&self, f: F) -> Option<usize>
    where
        F: FnMut(loro::ValueOrContainer) -> Option<bool>;
}

impl LoroListExt for loro::LoroList {
    fn collect_map<T, F>(&self, mut f: F) -> Vec<T>
    where
        F: FnMut(loro::ValueOrContainer) -> Option<T>,
    {
        let mut result = Vec::new();
        self.for_each(|v| {
            if let Some(value) = f(v) {
                result.push(value);
            }
        });
        result
    }

    fn find_index<F>(&self, mut f: F) -> Option<usize>
    where
        F: FnMut(loro::ValueOrContainer) -> Option<bool>,
    {
        let mut index = 0;
        let mut found = None;
        self.for_each(|v| {
            if found.is_none()
                && let Some(true) = f(v)
            {
                found = Some(index);
            }
            index += 1;
        });
        found
    }
}

/// Helper trait for extracting typed values from Loro maps
trait LoroMapExt {
    /// Get a value from the map and apply a function to the LoroValue
    /// Automatically unwraps the ValueOrContainer::Value variant
    fn get_typed<T, F>(&self, key: &str, f: F) -> Option<T>
    where
        F: FnOnce(&loro::LoroValue) -> Option<T>;
}

impl LoroMapExt for loro::LoroMap {
    fn get_typed<T, F>(&self, key: &str, f: F) -> Option<T>
    where
        F: FnOnce(&loro::LoroValue) -> Option<T>,
    {
        self.get(key).and_then(|v| match v {
            loro::ValueOrContainer::Value(val) => f(&val),
            _ => None,
        })
    }
}

/// Loro-backed document repository implementation.
///
/// Uses a normalized data model:
/// - `blocks_by_id`: LoroMap<String, BlockData> - O(1) block lookup
/// - `children_by_parent`: LoroMap<String, LoroList<String>> - Parent → children mapping
///
/// The tree has a single root block with `id` and `parent_id` both equal to ROOT_PARENT_ID.
/// All other blocks nest under this root transitively.
///
/// # Data Model
///
/// Each block in `blocks_by_id` contains:
/// - `content`: LoroText - CRDT text content
/// - `parent_id`: String - Parent block ID
/// - `created_at`: i64 - Unix timestamp (milliseconds)
/// - `updated_at`: i64 - Unix timestamp (milliseconds)
/// - `deleted_at`: i64 or null - Tombstone timestamp (null = not deleted)
pub struct LoroBackend {
    /// Collaborative document wrapper (includes Iroh endpoint for P2P)
    collab_doc: Arc<CollaborativeDoc>,
    /// Document ID
    doc_id: String,
    /// Active change notification subscribers
    subscribers: ChangeSubscribers,
    /// In-memory log of emitted changes for late subscribers
    event_log: Arc<Mutex<Vec<BlockChange>>>,
}

impl Clone for LoroBackend {
    fn clone(&self) -> Self {
        Self {
            collab_doc: self.collab_doc.clone(),
            doc_id: self.doc_id.clone(),
            subscribers: self.subscribers.clone(),
            event_log: self.event_log.clone(),
        }
    }
}

impl LoroBackend {
    // Container name constants (our "schema")
    const BLOCKS_BY_ID: &'static str = "blocks_by_id";
    const CHILDREN_BY_PARENT: &'static str = "children_by_parent";
    const SCHEMA_VERSION: &'static str = "_schema_version";

    /// Get current Unix timestamp in milliseconds.
    fn now_millis() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    }

    /// Generate a local URI-based block ID.
    fn generate_block_id() -> String {
        format!("local://{}", Uuid::new_v4())
    }

    /// Emit a change to all subscribers and record it for late listeners.
    pub(crate) fn emit_change(&self, change: BlockChange) {
        self.event_log.lock().unwrap().push(change.clone());
        let mut subscribers = self.subscribers.lock().unwrap();
        subscribers.retain(|sender| sender.try_send(Ok(change.clone())).is_ok());
    }

    /// Helper to get a block's LoroMap from the blocks_by_id map
    fn get_block_map(blocks_map: &loro::LoroMap, id: &str) -> anyhow::Result<loro::LoroMap> {
        let block_data = blocks_map
            .get(id)
            .ok_or_else(|| anyhow::anyhow!(ApiError::BlockNotFound { id: id.to_string() }))?;

        match block_data {
            loro::ValueOrContainer::Container(loro::Container::Map(m)) => Ok(m),
            _ => Err(anyhow::anyhow!("Block {} is not a map", id)),
        }
    }

    /// Helper to get or create a children list for a parent
    fn get_or_create_children_list(
        doc: &loro::LoroDoc,
        parent_id: &str,
    ) -> anyhow::Result<loro::LoroList> {
        let children_map = doc.get_map(Self::CHILDREN_BY_PARENT);
        match children_map.get(parent_id) {
            Some(loro::ValueOrContainer::Container(loro::Container::List(list))) => Ok(list),
            Some(_) => Err(anyhow::anyhow!("Children container is not a list")),
            None => Ok(children_map.insert_container(parent_id, loro::LoroList::new())?),
        }
    }

    /// Helper to remove a block ID from a list
    fn remove_from_list(list: &loro::LoroList, block_id: &str) -> anyhow::Result<()> {
        if let Some(index) = list.find_index(|v| match v {
            loro::ValueOrContainer::Value(val) => val.as_string().map(|s| s.as_ref() == block_id),
            _ => None,
        }) {
            list.delete(index, 1)?;
        }
        Ok(())
    }

    /// Helper to insert a block ID into a list, optionally after a specific block
    fn insert_into_list(
        list: &loro::LoroList,
        block_id: &str,
        after: Option<&str>,
    ) -> anyhow::Result<()> {
        if let Some(after_id) = after {
            if let Some(index) = list.find_index(|v| match v {
                loro::ValueOrContainer::Value(val) => {
                    val.as_string().map(|s| s.as_ref() == after_id)
                }
                _ => None,
            }) {
                list.insert(index + 1, loro::LoroValue::from(block_id))?;
            } else {
                // If 'after' block not found, append to end
                list.push(loro::LoroValue::from(block_id))?;
            }
        } else {
            list.push(loro::LoroValue::from(block_id))?;
        }
        Ok(())
    }

    /// Check if `ancestor_id` is an ancestor of `descendant_id` (cycle detection helper)
    fn is_ancestor(
        ancestor_id: &str,
        descendant_id: &str,
        doc: &loro::LoroDoc,
    ) -> anyhow::Result<bool> {
        use super::types::NO_PARENT_ID;

        let blocks_map = doc.get_map(Self::BLOCKS_BY_ID);
        let mut current_id = Some(descendant_id.to_string());

        while let Some(id) = current_id {
            if id == ancestor_id {
                return Ok(true);
            }

            // Get parent of current block
            let parent_id = Self::get_block_map(&blocks_map, &id)?
                .get_typed("parent_id", |val| val.as_string().map(|s| s.to_string()));

            // Stop if we reached the root (NO_PARENT_ID sentinel)
            current_id = if parent_id.as_deref() == Some(NO_PARENT_ID) {
                None
            } else {
                parent_id
            };
        }

        Ok(false)
    }

    /// Initialize schema containers in the document.
    ///
    /// Called once during create_new() to set up the data model.
    async fn initialize_schema(collab_doc: &CollaborativeDoc) -> Result<(), ApiError> {
        collab_doc
            .with_write(|doc| {
                // Initialize containers (Loro creates them if they don't exist)
                doc.get_map(Self::BLOCKS_BY_ID);
                doc.get_map(Self::CHILDREN_BY_PARENT);

                // Create the root block
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as i64;

                let blocks_map = doc.get_map(Self::BLOCKS_BY_ID);
                let root_map = blocks_map
                    .insert_container(super::types::ROOT_PARENT_ID, loro::LoroMap::new())?;

                root_map.insert_container("content", loro::LoroText::new())?;
                root_map.insert(
                    "parent_id",
                    loro::LoroValue::from(super::types::NO_PARENT_ID),
                )?;
                root_map.insert("created_at", loro::LoroValue::from(now))?;
                root_map.insert("updated_at", loro::LoroValue::from(now))?;

                // Set schema version for future migrations
                let meta = doc.get_map("_meta");
                meta.insert(Self::SCHEMA_VERSION, loro::LoroValue::from(1i64))?;

                Ok(())
            })
            .await
            .map_err(|e| ApiError::InternalError {
                message: format!("Failed to initialize schema: {}", e),
            })
    }
}

// Lifecycle trait implementation
#[async_trait]
impl Lifecycle for LoroBackend {
    async fn create_new(doc_id: String) -> Result<Self, ApiError>
    where
        Self: Sized,
    {
        let collab_doc = CollaborativeDoc::with_new_endpoint(doc_id.clone())
            .await
            .map_err(|e| ApiError::InternalError {
                message: format!("Failed to create endpoint: {}", e),
            })?;

        let collab_doc = Arc::new(collab_doc);

        // Initialize schema
        Self::initialize_schema(&collab_doc).await?;

        Ok(Self {
            collab_doc,
            doc_id,
            subscribers: Arc::new(Mutex::new(Vec::new())),
            event_log: Arc::new(Mutex::new(Vec::new())),
        })
    }

    async fn open_existing(doc_id: String) -> Result<Self, ApiError>
    where
        Self: Sized,
    {
        // For now, same as create_new (will need persistence layer later)
        // TODO: Load from disk and validate schema version
        Self::create_new(doc_id).await
    }

    async fn dispose(&self) -> Result<(), ApiError> {
        // Release resources (CollaborativeDoc drops automatically)
        Ok(())
    }
}

// ChangeNotifications trait implementation
#[async_trait]
impl ChangeNotifications for LoroBackend {
    async fn watch_changes_since(
        &self,
        position: StreamPosition,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<BlockChange, ApiError>> + Send>>, ApiError> {
        // Collect replay items synchronously
        let mut replay_items = Vec::new();

        // If position is Beginning, collect all current blocks as Created events first
        if matches!(position, StreamPosition::Beginning) {
            let current_blocks = self
                .collab_doc
                .with_read(|doc| {
                    let blocks_map = doc.get_map(Self::BLOCKS_BY_ID);
                    let children_map = doc.get_map(Self::CHILDREN_BY_PARENT);

                    // Collect all non-deleted blocks
                    let mut blocks = Vec::new();
                    blocks_map.for_each(|k, v| {
                        if let loro::ValueOrContainer::Container(loro::Container::Map(block_map)) =
                            v
                        {
                            // Skip deleted blocks
                            let is_deleted = block_map
                                .get_typed("deleted_at", |val| {
                                    Some(!matches!(val, loro::LoroValue::Null))
                                })
                                .unwrap_or(false);

                            if !is_deleted {
                                let content = block_map
                                    .get_typed("content", |val| {
                                        val.as_string().map(|s| s.to_string())
                                    })
                                    .unwrap_or_default();

                                let parent_id = block_map
                                    .get_typed("parent_id", |val| {
                                        val.as_string().map(|s| s.to_string())
                                    })
                                    .expect("Block must have parent_id");

                                let created_at = block_map
                                    .get_typed("created_at", |val| val.as_i64().copied())
                                    .unwrap_or(0);

                                let updated_at = block_map
                                    .get_typed("updated_at", |val| val.as_i64().copied())
                                    .unwrap_or(0);

                                // Get children IDs for this block
                                let children = if let Some(loro::ValueOrContainer::Container(
                                    loro::Container::List(children_list),
                                )) = children_map.get(k.as_ref())
                                {
                                    children_list.collect_map(|v| match v {
                                        loro::ValueOrContainer::Value(val) => {
                                            val.as_string().map(|s| s.to_string())
                                        }
                                        _ => None,
                                    })
                                } else {
                                    Vec::new()
                                };

                                blocks.push(Block {
                                    id: k.to_string(),
                                    parent_id,
                                    content,
                                    children,
                                    metadata: BlockMetadata {
                                        created_at,
                                        updated_at,
                                    },
                                });
                            }
                        }
                    });

                    anyhow::Ok(blocks)
                })
                .await
                .map_err(|e| ApiError::InternalError {
                    message: format!("Failed to get current blocks: {}", e),
                })?;

            // Collect current blocks as replay items
            for block in current_blocks {
                replay_items.push(BlockChange::Created {
                    block,
                    origin: ChangeOrigin::Remote,
                });
            }
        }

        // Collect buffered changes from event log
        let backlog = self.event_log.lock().unwrap().clone();
        replay_items.extend(backlog);

        // Create channel for live updates
        let (tx, rx) = mpsc::channel::<Result<BlockChange, ApiError>>(100);

        // Subscribe to future changes
        {
            let mut subscribers = self.subscribers.lock().unwrap();
            subscribers.push(tx);
        }

        // Create a stream that first yields replay items, then live updates
        // This avoids spawning tasks which can cause runtime deadlocks when used with block_on
        let replay_stream = tokio_stream::iter(replay_items.into_iter().map(Ok));
        let live_stream = ReceiverStream::new(rx);
        let combined = replay_stream.chain(live_stream);

        Ok(Box::pin(combined))
    }

    async fn get_current_version(&self) -> Result<Vec<u8>, ApiError> {
        self.collab_doc
            .with_read(|doc| Ok(doc.export(loro::ExportMode::Snapshot)?))
            .await
            .map_err(|e| ApiError::InternalError {
                message: format!("Failed to get current version: {}", e),
            })
    }
}

// CoreOperations trait implementation
#[async_trait]
impl CoreOperations for LoroBackend {
    async fn get_block(&self, id: &str) -> Result<Block, ApiError> {
        self.collab_doc
            .with_read(|doc| {
                let blocks_map = doc.get_map(Self::BLOCKS_BY_ID);

                // Get block data
                let block_data = blocks_map.get(id).ok_or_else(|| {
                    anyhow::anyhow!(ApiError::BlockNotFound { id: id.to_string() })
                })?;

                let block_map = match block_data {
                    loro::ValueOrContainer::Container(loro::Container::Map(m)) => m,
                    _ => {
                        return Err(ApiError::InternalError {
                            message: format!("Block {} is not a map", id),
                        }
                        .into());
                    }
                };

                // Extract fields using helper trait
                let content = block_map
                    .get_typed("content", |val| val.as_string().map(|s| s.to_string()))
                    .unwrap_or_default();

                let parent_id = block_map
                    .get_typed("parent_id", |val| val.as_string().map(|s| s.to_string()))
                    .expect("Block must have parent_id");

                let created_at = block_map
                    .get_typed("created_at", |val| val.as_i64().copied())
                    .unwrap_or(0);

                let updated_at = block_map
                    .get_typed("updated_at", |val| val.as_i64().copied())
                    .unwrap_or(0);

                // TODO: Get children from children_by_parent map

                Ok(Block {
                    id: id.to_string(),
                    parent_id,
                    content,
                    children: vec![],
                    metadata: BlockMetadata {
                        created_at,
                        updated_at,
                    },
                })
            })
            .await
            .map_err(|e| ApiError::InternalError {
                message: format!("Failed to get block: {}", e),
            })
    }

    async fn get_all_blocks(
        &self,
        traversal: super::types::Traversal,
    ) -> Result<Vec<Block>, ApiError> {
        self.collab_doc
            .with_read(|doc| {
                let blocks_map = doc.get_map(Self::BLOCKS_BY_ID);
                let children_map = doc.get_map(Self::CHILDREN_BY_PARENT);
                let mut result = Vec::new();

                // Helper for depth-first traversal with level tracking
                fn traverse(
                    block_id: &str,
                    current_level: usize,
                    blocks_map: &loro::LoroMap,
                    children_map: &loro::LoroMap,
                    traversal: &super::types::Traversal,
                    result: &mut Vec<Block>,
                ) {
                    // Get block data
                    let block_container = match blocks_map.get(block_id) {
                        Some(loro::ValueOrContainer::Container(loro::Container::Map(m))) => m,
                        _ => return, // Block not found
                    };

                    // Skip deleted blocks
                    let is_deleted = block_container
                        .get_typed("deleted_at", |val| {
                            Some(!matches!(val, loro::LoroValue::Null))
                        })
                        .unwrap_or(false);

                    if is_deleted {
                        return;
                    }

                    let content = block_container
                        .get_typed("content", |val| val.as_string().map(|s| s.to_string()))
                        .unwrap_or_default();

                    let parent_id = block_container
                        .get_typed("parent_id", |val| val.as_string().map(|s| s.to_string()))
                        .unwrap_or_else(|| super::types::ROOT_PARENT_ID.to_string());

                    let created_at = block_container
                        .get_typed("created_at", |val| val.as_i64().copied())
                        .unwrap_or(0);

                    let updated_at = block_container
                        .get_typed("updated_at", |val| val.as_i64().copied())
                        .unwrap_or(0);

                    // Get children IDs
                    let children = if let Some(loro::ValueOrContainer::Container(
                        loro::Container::List(children_list),
                    )) = children_map.get(block_id)
                    {
                        children_list.collect_map(|v| match v {
                            loro::ValueOrContainer::Value(val) => {
                                val.as_string().map(|s| s.to_string())
                            }
                            _ => None,
                        })
                    } else {
                        Vec::new()
                    };

                    // Add current block if it's within the level range
                    if traversal.includes_level(current_level) {
                        result.push(Block {
                            id: block_id.to_string(),
                            parent_id,
                            content,
                            children: children.clone(),
                            metadata: BlockMetadata {
                                created_at,
                                updated_at,
                            },
                        });
                    }

                    // Recursively traverse children only if we haven't reached max_level
                    if current_level < traversal.max_level {
                        for child_id in &children {
                            traverse(
                                child_id,
                                current_level + 1,
                                blocks_map,
                                children_map,
                                traversal,
                                result,
                            );
                        }
                    }
                }

                // Start traversal from the root block itself (level 0)
                traverse(
                    super::types::ROOT_PARENT_ID,
                    0,
                    &blocks_map,
                    &children_map,
                    &traversal,
                    &mut result,
                );

                Ok(result)
            })
            .await
            .map_err(|e| ApiError::InternalError {
                message: format!("Failed to get all blocks: {}", e),
            })
    }

    async fn list_children(&self, parent_id: &str) -> Result<Vec<String>, ApiError> {
        self.collab_doc
            .with_read(|doc| {
                let children_map = doc.get_map(Self::CHILDREN_BY_PARENT);

                let children = match children_map.get(parent_id) {
                    Some(loro::ValueOrContainer::Container(loro::Container::List(list))) => list
                        .collect_map(|v| match v {
                            loro::ValueOrContainer::Value(val) => {
                                val.as_string().map(|s| s.to_string())
                            }
                            _ => None,
                        }),
                    _ => Vec::new(),
                };

                Ok(children)
            })
            .await
            .map_err(|e| ApiError::InternalError {
                message: format!("Failed to list children: {}", e),
            })
    }

    async fn create_block(
        &self,
        parent_id: String,
        content: String,
        id: Option<String>,
    ) -> Result<Block, ApiError> {
        let block_id = id.unwrap_or_else(Self::generate_block_id);
        let now = Self::now_millis();
        let parent_id_clone = parent_id.clone();

        let created_block = self
            .collab_doc
            .with_write(|doc| {
                let blocks_map = doc.get_map(Self::BLOCKS_BY_ID);

                // Verify parent exists
                if blocks_map.get(&parent_id).is_none() {
                    return Err(anyhow::anyhow!("Parent block not found: {}", parent_id));
                }

                // Create block data map using insert_container
                let block_map = blocks_map.insert_container(&block_id, loro::LoroMap::new())?;
                block_map.insert("content", loro::LoroValue::from(content.clone()))?;
                block_map.insert("parent_id", loro::LoroValue::from(parent_id.as_str()))?;
                block_map.insert("created_at", loro::LoroValue::from(now))?;
                block_map.insert("updated_at", loro::LoroValue::from(now))?;
                block_map.insert("deleted_at", loro::LoroValue::Null)?;

                // Add to parent's children list
                let children_map = doc.get_map(Self::CHILDREN_BY_PARENT);
                let children_list = match children_map.get(&parent_id) {
                    Some(loro::ValueOrContainer::Container(loro::Container::List(list))) => list,
                    Some(_) => {
                        return Err(anyhow::anyhow!("Children container is not a list"));
                    }
                    None => children_map.insert_container(&parent_id, loro::LoroList::new())?,
                };
                children_list.push(loro::LoroValue::from(block_id.as_str()))?;

                // Commit to trigger event subscribers
                doc.commit();

                Ok(Block {
                    id: block_id,
                    parent_id,
                    content,
                    children: vec![],
                    metadata: BlockMetadata {
                        created_at: now,
                        updated_at: now,
                    },
                })
            })
            .await
            .map_err(|e| {
                if e.to_string().contains("Parent block not found") {
                    ApiError::BlockNotFound {
                        id: parent_id_clone,
                    }
                } else {
                    ApiError::InternalError {
                        message: format!("Failed to create block: {}", e),
                    }
                }
            })?;

        self.emit_change(BlockChange::Created {
            block: created_block.clone(),
            origin: ChangeOrigin::Local,
        });

        Ok(created_block)
    }

    async fn update_block(&self, id: &str, content: String) -> Result<(), ApiError> {
        let content_for_doc = content.clone();
        self.collab_doc
            .with_write(|doc| {
                let blocks_map = doc.get_map(Self::BLOCKS_BY_ID);

                // Get the block
                let block_data = blocks_map.get(id).ok_or_else(|| {
                    anyhow::anyhow!(ApiError::BlockNotFound { id: id.to_string() })
                })?;

                let block_map = match block_data {
                    loro::ValueOrContainer::Container(loro::Container::Map(m)) => m,
                    _ => {
                        return Err(anyhow::anyhow!("Block {} is not a map", id));
                    }
                };

                // Update content and timestamp
                block_map.insert("content", loro::LoroValue::from(content_for_doc.as_str()))?;
                block_map.insert("updated_at", loro::LoroValue::from(Self::now_millis()))?;

                // Commit to trigger event subscribers
                doc.commit();

                Ok(())
            })
            .await
            .map_err(|e| ApiError::InternalError {
                message: format!("Failed to update block: {}", e),
            })?;

        self.emit_change(BlockChange::Updated {
            id: id.to_string(),
            content,
            origin: ChangeOrigin::Local,
        });

        Ok(())
    }

    async fn delete_block(&self, id: &str) -> Result<(), ApiError> {
        self.collab_doc
            .with_write(|doc| {
                let blocks_map = doc.get_map(Self::BLOCKS_BY_ID);

                // Get the block
                let block_data = blocks_map.get(id).ok_or_else(|| {
                    anyhow::anyhow!(ApiError::BlockNotFound { id: id.to_string() })
                })?;

                let block_map = match block_data {
                    loro::ValueOrContainer::Container(loro::Container::Map(m)) => m,
                    _ => {
                        return Err(anyhow::anyhow!("Block {} is not a map", id));
                    }
                };

                // Set tombstone timestamp
                block_map.insert("deleted_at", loro::LoroValue::from(Self::now_millis()))?;
                block_map.insert("updated_at", loro::LoroValue::from(Self::now_millis()))?;

                // Commit to trigger event subscribers
                doc.commit();

                Ok(())
            })
            .await
            .map_err(|e| ApiError::InternalError {
                message: format!("Failed to delete block: {}", e),
            })?;

        self.emit_change(BlockChange::Deleted {
            id: id.to_string(),
            origin: ChangeOrigin::Local,
        });

        Ok(())
    }

    async fn move_block(
        &self,
        id: &str,
        new_parent: String,
        after: Option<String>,
    ) -> Result<(), ApiError> {
        let new_parent_for_notify = new_parent.clone();
        let after_for_notify = after.clone();

        self.collab_doc
            .with_write(|doc| {
                let blocks_map = doc.get_map(Self::BLOCKS_BY_ID);
                let block_map = Self::get_block_map(&blocks_map, id)?;

                // Get old parent_id
                let old_parent = block_map
                    .get_typed("parent_id", |val| val.as_string().map(|s| s.to_string()))
                    .ok_or_else(|| anyhow::anyhow!("Block {} has no parent_id", id))?;

                // Cycle detection
                if Self::is_ancestor(id, &new_parent, doc)? {
                    return Err(anyhow::anyhow!(
                        "Cannot move block {} under its descendant {}",
                        id,
                        new_parent
                    ));
                }

                // Verify new parent exists
                Self::get_block_map(&blocks_map, &new_parent)?;

                // Remove from old location
                let old_children_list = Self::get_or_create_children_list(doc, &old_parent)?;
                Self::remove_from_list(&old_children_list, id)?;

                // Add to new location
                let new_children_list = Self::get_or_create_children_list(doc, &new_parent)?;
                Self::insert_into_list(&new_children_list, id, after.as_deref())?;

                // Update block's parent_id
                block_map.insert("parent_id", loro::LoroValue::from(new_parent.as_str()))?;
                block_map.insert("updated_at", loro::LoroValue::from(Self::now_millis()))?;

                doc.commit();

                Ok(())
            })
            .await
            .map_err(|e| ApiError::InternalError {
                message: format!("Failed to move block: {}", e),
            })?;

        self.emit_change(BlockChange::Moved {
            id: id.to_string(),
            new_parent: new_parent_for_notify,
            after: after_for_notify,
            origin: ChangeOrigin::Local,
        });

        Ok(())
    }

    async fn get_blocks(&self, ids: Vec<String>) -> Result<Vec<Block>, ApiError> {
        self.collab_doc
            .with_read(|doc| {
                let blocks_map = doc.get_map(Self::BLOCKS_BY_ID);
                let mut blocks = Vec::new();

                // Collect successful results, skip blocks that don't exist
                for id in ids {
                    if let Some(loro::ValueOrContainer::Container(loro::Container::Map(
                        block_map,
                    ))) = blocks_map.get(&id)
                    {
                        // Extract fields using helper trait
                        let content = block_map
                            .get_typed("content", |val| val.as_string().map(|s| s.to_string()))
                            .unwrap_or_default();

                        let parent_id = block_map
                            .get_typed("parent_id", |val| val.as_string().map(|s| s.to_string()))
                            .expect("Block must have parent_id");

                        let created_at = block_map
                            .get_typed("created_at", |val| val.as_i64().copied())
                            .unwrap_or(0);

                        let updated_at = block_map
                            .get_typed("updated_at", |val| val.as_i64().copied())
                            .unwrap_or(0);

                        blocks.push(Block {
                            id: id.clone(),
                            parent_id,
                            content,
                            children: vec![],
                            metadata: BlockMetadata {
                                created_at,
                                updated_at,
                            },
                        });
                    }
                    // Silently skip blocks that don't exist (partial success pattern)
                }

                Ok(blocks)
            })
            .await
            .map_err(|e| ApiError::InternalError {
                message: format!("Failed to get blocks: {}", e),
            })
    }

    async fn create_blocks(&self, blocks: Vec<NewBlock>) -> Result<Vec<Block>, ApiError> {
        let now = Self::now_millis();

        let created_blocks = self
            .collab_doc
            .with_write(|doc| {
                let blocks_map = doc.get_map(Self::BLOCKS_BY_ID);
                let mut created_blocks = Vec::new();

                // Single transaction for entire batch
                for new_block in blocks {
                    let block_id = new_block.id.unwrap_or_else(Self::generate_block_id);
                    let parent_id = new_block.parent_id.clone();

                    // Validate parent exists
                    if blocks_map.get(&parent_id).is_none() {
                        return Err(anyhow::anyhow!("Parent block not found: {}", parent_id));
                    }

                    // Create block data map
                    let block_map = blocks_map.insert_container(&block_id, loro::LoroMap::new())?;
                    block_map
                        .insert("content", loro::LoroValue::from(new_block.content.as_str()))?;
                    block_map.insert("parent_id", loro::LoroValue::from(parent_id.as_str()))?;
                    block_map.insert("created_at", loro::LoroValue::from(now))?;
                    block_map.insert("updated_at", loro::LoroValue::from(now))?;
                    block_map.insert("deleted_at", loro::LoroValue::Null)?;

                    // Add to parent's children list
                    let children_list = Self::get_or_create_children_list(doc, &parent_id)?;
                    Self::insert_into_list(&children_list, &block_id, new_block.after.as_deref())?;

                    created_blocks.push(Block {
                        id: block_id,
                        parent_id,
                        content: new_block.content,
                        children: vec![],
                        metadata: BlockMetadata {
                            created_at: now,
                            updated_at: now,
                        },
                    });
                }

                doc.commit();

                Ok(created_blocks)
            })
            .await
            .map_err(|e| {
                if e.to_string().contains("Parent block not found") {
                    ApiError::BlockNotFound { id: e.to_string() }
                } else {
                    ApiError::InternalError {
                        message: format!("Failed to create blocks: {}", e),
                    }
                }
            })?;

        for block in &created_blocks {
            self.emit_change(BlockChange::Created {
                block: block.clone(),
                origin: ChangeOrigin::Local,
            });
        }

        Ok(created_blocks)
    }

    async fn delete_blocks(&self, ids: Vec<String>) -> Result<(), ApiError> {
        let now = Self::now_millis();

        // Deduplicate IDs to handle cases where the same ID appears multiple times
        let mut seen = std::collections::HashSet::new();
        let unique_ids: Vec<_> = ids
            .into_iter()
            .filter(|id| seen.insert(id.clone()))
            .collect();
        let ids_for_doc = unique_ids.clone();

        self.collab_doc
            .with_write(move |doc| {
                let blocks_map = doc.get_map(Self::BLOCKS_BY_ID);

                // Single transaction for entire batch
                for id in &ids_for_doc {
                    // Get the block - error if doesn't exist
                    let block_map = match blocks_map.get(id) {
                        Some(loro::ValueOrContainer::Container(loro::Container::Map(m))) => m,
                        _ => return Err(anyhow::anyhow!("Block not found: {}", id)),
                    };

                    // Set tombstone timestamp
                    block_map.insert("deleted_at", loro::LoroValue::from(now))?;
                    block_map.insert("updated_at", loro::LoroValue::from(now))?;
                }

                doc.commit();

                Ok(())
            })
            .await
            .map_err(|e| ApiError::InternalError {
                message: format!("Failed to delete blocks: {}", e),
            })?;

        for id in unique_ids {
            self.emit_change(BlockChange::Deleted {
                id,
                origin: ChangeOrigin::Local,
            });
        }

        Ok(())
    }
}

// P2POperations trait implementation
#[async_trait]
impl P2POperations for LoroBackend {
    async fn get_node_id(&self) -> String {
        self.collab_doc.node_id().to_string()
    }

    async fn connect_to_peer(&self, peer_node_id: String) -> Result<(), ApiError> {
        let public_key: PublicKey = peer_node_id.parse().map_err(|e| ApiError::NetworkError {
            message: format!("Invalid peer node ID: {}", e),
        })?;

        let node_addr = NodeAddr::new(public_key);

        self.collab_doc
            .connect_and_sync_to_peer(node_addr)
            .await
            .map_err(|e| ApiError::NetworkError {
                message: format!("Failed to connect to peer: {}", e),
            })
    }

    async fn accept_connections(&self) -> Result<(), ApiError> {
        self.collab_doc
            .accept_sync_from_peer()
            .await
            .map_err(|e| ApiError::NetworkError {
                message: format!("Failed to accept connections: {}", e),
            })
    }
}
