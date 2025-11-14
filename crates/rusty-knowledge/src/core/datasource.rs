//! Stream-based datasource traits for external system integration
//!
//! This module provides traits for reactive sync architecture where:
//! - DataSources provide read-only access (from cache)
//! - CrudOperationProviders provide write operations (fire-and-forget to external systems)
//! - Changes flow via ChangeNotifications streams from SyncProviders to Caches
//! - QueryableCache wraps datasources and implements both read and write traits

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::collections::HashMap;

use crate::storage::types::{StorageEntity, Value};

// Re-export OperationDescriptor and OperationParam from query-render
pub use query_render::{OperationDescriptor, OperationParam};

// Re-export Change types from api::streaming for unified change representation
pub use crate::api::streaming::{Change, ChangeOrigin, StreamPosition};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Parameter descriptor for operation metadata (legacy, kept for backward compatibility)
#[derive(Debug, Clone)]
pub struct ParamDescriptor {
    pub name: String,
    pub param_type: String, // "String", "bool", "i64", etc.
    pub required: bool,
    pub default: Option<Value>,
}

/// Read-only data access (from cache)
#[async_trait]
pub trait DataSource<T>: Send + Sync
where
    T: Send + Sync + 'static,
{
    async fn get_all(&self) -> Result<Vec<T>>;
    async fn get_by_id(&self, id: &str) -> Result<Option<T>>;

    // Helper queries (default implementations)
    async fn get_children(&self, parent_id: &str) -> Result<Vec<T>>
    where
        T: BlockEntity,
    {
        Ok(self.get_all().await?
            .into_iter()
            .filter(|t| t.parent_id() == Some(parent_id))
            .collect())
    }
}

/// CRUD operations provider (fire-and-forget to external system)
///
/// Provides create, update, and delete operations. Changes are confirmed
/// via ChangeNotifications streams, not return values.
///
/// **Note**: This trait is conceptually `CrudOperationProvider` but is named
/// `CrudOperationProvider` for backward compatibility with macro-generated code.
/// New code should refer to it as `CrudOperationProvider` in documentation.
#[rusty_knowledge_macros::operations_trait]
#[async_trait]
pub trait CrudOperationProvider<T>: Send + Sync
where
    T: Send + Sync + 'static,
{
    /// Set single field (returns () - update arrives via stream)
    async fn set_field(&self, id: &str, field: &str, value: Value) -> Result<()>;

    /// Create new entity (returns new ID immediately, full data via stream)
    async fn create(&self, fields: HashMap<String, Value>) -> Result<String>;

    /// Delete entity (returns () - deletion confirmed via stream)
    async fn delete(&self, id: &str) -> Result<()>;

    /// Get operations metadata (automatically delegates to entity type)
    fn operations(&self) -> Vec<OperationDescriptor>
    where
        T: OperationRegistry,
    {
        T::all_operations()
    }
}

/// Entities that support hierarchical tree structure
pub trait BlockEntity: Send + Sync {
    /// Get the entity's unique identifier
    fn id(&self) -> &str;

    fn parent_id(&self) -> Option<&str>;
    fn sort_key(&self) -> &str;
    fn depth(&self) -> i64;

    /// Get the block content (text content of the block)
    fn content(&self) -> &str;
}

/// Entities that support task management (completion, priority, etc.)
pub trait TaskEntity: Send + Sync {
    fn completed(&self) -> bool;
    fn priority(&self) -> Option<i64>;
    fn due_date(&self) -> Option<DateTime<Utc>>;
}

/// Helper methods for block data source operations
/// These are not operations themselves, but utilities used by operations
#[async_trait]
pub trait BlockDataSourceHelpers<T>: CrudOperationProvider<T> + DataSource<T>
where
    T: BlockEntity + Send + Sync + 'static,
{
    /// Get all siblings of a block, sorted by sort_key
    async fn get_siblings(&self, block_id: &str) -> Result<Vec<T>> {
        let block = self.get_by_id(block_id).await?
            .ok_or_else(|| anyhow::anyhow!("Block not found"))?;
        let parent_id = block.parent_id();

        let siblings = if let Some(pid) = parent_id {
            self.get_children(pid).await?
        } else {
            // For root blocks, get all root blocks
            // We need to filter from all blocks - this is a limitation
            // For now, return empty - will need get_root_blocks method
            return Ok(vec![]);
        };

        Ok(siblings.into_iter()
            .filter(|s| s.id() != block_id)
            .collect())
    }

    /// Get the previous sibling (sibling with sort_key < current sort_key)
    async fn get_prev_sibling(&self, block_id: &str) -> Result<Option<T>> {
        let block = self.get_by_id(block_id).await?
            .ok_or_else(|| anyhow::anyhow!("Block not found"))?;
        let parent_id = block.parent_id();

        let siblings = if let Some(pid) = parent_id {
            self.get_children(pid).await?
        } else {
            return Ok(None);
        };

        let prev = siblings.into_iter()
            .filter(|s| s.sort_key() < block.sort_key())
            .max_by(|a, b| a.sort_key().cmp(b.sort_key()));
        Ok(prev)
    }

    /// Get the next sibling (sibling with sort_key > current sort_key)
    async fn get_next_sibling(&self, block_id: &str) -> Result<Option<T>> {
        let block = self.get_by_id(block_id).await?
            .ok_or_else(|| anyhow::anyhow!("Block not found"))?;
        let parent_id = block.parent_id();

        let siblings = if let Some(pid) = parent_id {
            self.get_children(pid).await?
        } else {
            return Ok(None);
        };

        let next = siblings.into_iter()
            .filter(|s| s.sort_key() > block.sort_key())
            .min_by(|a, b| a.sort_key().cmp(b.sort_key()));
        Ok(next)
    }

    /// Get the first child of a parent (lowest sort_key)
    async fn get_first_child(&self, parent_id: Option<&str>) -> Result<Option<T>> {
        let children = if let Some(pid) = parent_id {
            self.get_children(pid).await?
        } else {
            // For root blocks, we need to get all blocks with no parent
            // This is a limitation - we'd need a method to get root blocks
            // For now, return None and handle in move_block
            return Ok(None);
        };

        Ok(children.into_iter()
            .min_by(|a, b| a.sort_key().cmp(b.sort_key())))
    }

    /// Get the last child of a parent (highest sort_key)
    async fn get_last_child(&self, parent_id: Option<&str>) -> Result<Option<T>> {
        let children = if let Some(pid) = parent_id {
            self.get_children(pid).await?
        } else {
            return Ok(None);
        };

        Ok(children.into_iter()
            .max_by(|a, b| a.sort_key().cmp(b.sort_key())))
    }

    /// Recursively update depths of all descendants when a parent's depth changes
    async fn update_descendant_depths(&self, parent_id: &str, depth_delta: i64) -> Result<()> {
        if depth_delta == 0 {
            return Ok(());
        }

        let mut queue = vec![parent_id.to_string()];

        while let Some(current_parent_id) = queue.pop() {
            let children = self.get_children(&current_parent_id).await?;

            for child in children {
                let current_depth = child.depth();
                let new_depth = current_depth + depth_delta;
                self.set_field(child.id(), "depth", Value::Integer(new_depth)).await?;
                queue.push(child.id().to_string());
            }
        }

        Ok(())
    }

    /// Rebalance all siblings of a parent to create uniform spacing
    async fn rebalance_siblings(&self, parent_id: Option<&str>) -> Result<()> {
        let children = if let Some(pid) = parent_id {
            self.get_children(pid).await?
        } else {
            // For root blocks, we'd need a get_root_blocks method
            // For now, skip rebalancing root blocks
            return Ok(());
        };

        // Sort by current sort_key
        let mut sorted_children: Vec<_> = children.into_iter().collect();
        sorted_children.sort_by(|a, b| a.sort_key().cmp(b.sort_key()));

        // Generate evenly-spaced keys
        use crate::storage::fractional_index::gen_n_keys;
        let new_keys = gen_n_keys(sorted_children.len())?;

        // Update all siblings
        for (child, new_key) in sorted_children.iter().zip(new_keys.iter()) {
            self.set_field(child.id(), "sort_key", Value::String(new_key.clone())).await?;
        }

        Ok(())
    }
}

/// Hierarchical structure operations (for any block-like entity)
#[rusty_knowledge_macros::operations_trait]
#[async_trait]
pub trait MutableBlockDataSource<T>: BlockDataSourceHelpers<T>
where
    T: BlockEntity + Send + Sync + 'static,
{
    /// Move block under a new parent (increase indentation)
    async fn indent_block(&self, id: &str, new_parent_id: &str) -> Result<()> {
        // Query cache for current state (fast - no network)
        let parent = self.get_by_id(new_parent_id).await?
            .ok_or_else(|| anyhow::anyhow!("Parent not found"))?;
        let siblings = self.get_children(new_parent_id).await?;

        // Calculate new position via fractional indexing
        let sort_key = crate::storage::fractional_index::gen_key_between(
            siblings.last().map(|s| s.sort_key()),
            None,
        )?;

        // Execute primitives (delegates to self.set_field)
        self.set_field(id, "parent_id", Value::String(new_parent_id.to_string())).await?;
        self.set_field(id, "depth", Value::Integer(parent.depth() + 1)).await?;
        self.set_field(id, "sort_key", Value::String(sort_key)).await?;
        Ok(())
    }

    /// Move block to different position (reorder within same parent or different parent)
    ///
    /// # Parameters
    /// * `id` - Block ID to move
    /// * `new_parent_id` - Target parent ID (or None for root)
    /// * `after_block_id` - Optional anchor block (move after this block, or beginning if None)
    async fn move_block(&self, id: &str, new_parent_id: Option<&str>, after_block_id: Option<&str>) -> Result<()> {
        use crate::storage::fractional_index::{gen_key_between, MAX_SORT_KEY_LENGTH};

        let block = self.get_by_id(id).await?
            .ok_or_else(|| anyhow::anyhow!("Block not found"))?;
        let old_depth = block.depth();

        // Query predecessor and successor sort_keys
        let (prev_key, next_key) = if after_block_id.is_none() {
            // No after_block_id means "move to beginning" - insert before first child
            let first_child = self.get_first_child(new_parent_id).await?;
            let first_key = first_child.map(|c| c.sort_key().to_string());
            (None, first_key)
        } else {
            // Insert after specific block
            let after_block = self.get_by_id(after_block_id.unwrap()).await?
                .ok_or_else(|| anyhow::anyhow!("Reference block not found"))?;
            let prev_key = Some(after_block.sort_key().to_string());

            // Find next sibling after the anchor block
            let next_sibling = self.get_next_sibling(after_block_id.unwrap()).await?;
            let next_key = next_sibling.map(|s| s.sort_key().to_string());
            (prev_key, next_key)
        };

        // Generate new sort_key
        let mut new_sort_key = gen_key_between(
            prev_key.as_deref(),
            next_key.as_deref()
        )?;

        // Check if rebalancing needed
        if new_sort_key.len() > MAX_SORT_KEY_LENGTH {
            self.rebalance_siblings(new_parent_id).await?;

            // Re-query neighbors after rebalancing
            let (prev_key, next_key) = if after_block_id.is_none() {
                let first_child = self.get_first_child(new_parent_id).await?;
                let first_key = first_child.map(|c| c.sort_key().to_string());
                (None, first_key)
            } else {
                let after_block = self.get_by_id(after_block_id.unwrap()).await?
                    .ok_or_else(|| anyhow::anyhow!("Reference block not found"))?;
                let prev_key = Some(after_block.sort_key().to_string());
                let next_sibling = self.get_next_sibling(after_block_id.unwrap()).await?;
                let next_key = next_sibling.map(|s| s.sort_key().to_string());
                (prev_key, next_key)
            };

            new_sort_key = gen_key_between(
                prev_key.as_deref(),
                next_key.as_deref()
            )?;
        }

        // Calculate new depth based on parent
        let new_depth = if let Some(ref parent_id) = new_parent_id {
            let parent = self.get_by_id(parent_id).await?
                .ok_or_else(|| anyhow::anyhow!("Parent not found"))?;
            parent.depth() + 1
        } else {
            0 // Root level
        };

        // Calculate depth delta for recursive updates
        let depth_delta = new_depth - old_depth;

        // Update block atomically
        if let Some(parent_id) = new_parent_id {
            self.set_field(id, "parent_id", Value::String(parent_id.to_string())).await?;
        } else {
            self.set_field(id, "parent_id", Value::Null).await?;
        }
        self.set_field(id, "sort_key", Value::String(new_sort_key)).await?;
        self.set_field(id, "depth", Value::Integer(new_depth)).await?;

        // Recursively update all descendants' depths by the same delta
        if depth_delta != 0 {
            self.update_descendant_depths(id, depth_delta).await?;
        }

        Ok(())
    }

    /// Move block out to parent's level (decrease indentation)
    async fn outdent_block(&self, id: &str) -> Result<()> {
        let block = self.get_by_id(id).await?
            .ok_or_else(|| anyhow::anyhow!("Block not found"))?;
        let parent_id = block.parent_id()
            .ok_or_else(|| anyhow::anyhow!("Cannot outdent root block"))?;

        let parent = self.get_by_id(parent_id).await?
            .ok_or_else(|| anyhow::anyhow!("Parent not found"))?;
        let grandparent_id = parent.parent_id();

        // Move to grandparent's children, after parent
        if let Some(gp_id) = grandparent_id {
            self.move_block(id, Some(gp_id), Some(parent_id)).await?;
        } else {
            self.move_block(id, None, Some(parent_id)).await?;
        }

        Ok(())
    }

    /// Split a block at a given position
    ///
    /// Creates a new block with content after the cursor and truncates
    /// the original block to content before the cursor. The new block
    /// appears directly below the original block using fractional indexing.
    ///
    /// # Parameters
    /// * `id` - Block ID to split
    /// * `position` - Character position to split at (as i64, will be converted to usize)
    async fn split_block(&self, id: &str, position: i64) -> Result<()> {
        use uuid::Uuid;
        use crate::storage::fractional_index::gen_key_between;

        let block = self.get_by_id(id).await?
            .ok_or_else(|| anyhow::anyhow!("Block not found"))?;

        let content = block.content();

        // Convert i64 to usize (validate it's non-negative and fits in usize)
        if position < 0 {
            return Err(anyhow::anyhow!("Position must be non-negative").into());
        }
        let position = position as usize;

        // Validate offset is within bounds
        if position > content.len() {
            return Err(anyhow::anyhow!(
                "Split position {} exceeds content length {}",
                position,
                content.len()
            ).into());
        }

        // Split content at cursor
        let mut content_before = content[..position].to_string();
        let mut content_after = content[position..].to_string();

        // Strip trailing whitespace from the old block
        content_before = content_before.trim_end().to_string();

        // Strip leading whitespace from the new block
        content_after = content_after.trim_start().to_string();

        // Generate new block ID
        let new_block_id = Uuid::new_v4().to_string();

        // Get next sibling's sort_key to position new block correctly
        let next_sibling = self.get_next_sibling(id).await?;
        let next_sort_key = next_sibling.map(|s| s.sort_key().to_string());

        // Generate sort_key for new block (between current block and next sibling)
        let new_sort_key = gen_key_between(
            Some(block.sort_key()),
            next_sort_key.as_deref()
        )?;

        // Get current timestamp
        let now = chrono::Utc::now().timestamp_millis();

        // Create new block using create method
        let mut new_block_fields = HashMap::new();
        new_block_fields.insert("id".to_string(), Value::String(new_block_id.clone()));
        new_block_fields.insert("content".to_string(), Value::String(content_after));
        new_block_fields.insert("parent_id".to_string(), {
            if let Some(ref pid) = block.parent_id() {
                Value::String(pid.to_string())
            } else {
                Value::Null
            }
        });
        new_block_fields.insert("depth".to_string(), Value::Integer(block.depth()));
        new_block_fields.insert("sort_key".to_string(), Value::String(new_sort_key));
        new_block_fields.insert("created_at".to_string(), Value::Integer(now));
        new_block_fields.insert("updated_at".to_string(), Value::Integer(now));
        new_block_fields.insert("collapsed".to_string(), Value::Boolean(false));
        new_block_fields.insert("completed".to_string(), Value::Boolean(false));
        new_block_fields.insert("block_type".to_string(), Value::String("text".to_string()));

        self.create(new_block_fields).await?;

        // Update current block with truncated content
        self.set_field(id, "content", Value::String(content_before)).await?;
        self.set_field(id, "updated_at", Value::Integer(now)).await?;

        Ok(())
    }

    /// Move a block up (swap with previous sibling)
    async fn move_up(&self, id: &str) -> Result<()> {
        let prev_sibling = self.get_prev_sibling(id).await?
            .ok_or_else(|| anyhow::anyhow!("Cannot move up: no previous sibling"))?;

        let block = self.get_by_id(id).await?
            .ok_or_else(|| anyhow::anyhow!("Block not found"))?;
        let parent_id = block.parent_id();

        // Get the sibling before prev_sibling
        let before_prev = self.get_prev_sibling(prev_sibling.id()).await?;

        if let Some(before_id) = before_prev {
            self.move_block(id, parent_id, Some(before_id.id())).await
        } else {
            // Move to beginning
            self.move_block(id, parent_id, None).await
        }
    }

    /// Move a block down (swap with next sibling)
    async fn move_down(&self, id: &str) -> Result<()> {
        let next_sibling = self.get_next_sibling(id).await?
            .ok_or_else(|| anyhow::anyhow!("Cannot move down: no next sibling"))?;

        let block = self.get_by_id(id).await?
            .ok_or_else(|| anyhow::anyhow!("Block not found"))?;
        let parent_id = block.parent_id();

        self.move_block(id, parent_id, Some(next_sibling.id())).await
    }
}

/// Task management operations (for any task-like entity)
#[rusty_knowledge_macros::operations_trait]
#[async_trait]
pub trait MutableTaskDataSource<T>: CrudOperationProvider<T> + DataSource<T>
where
    T: TaskEntity + Send + Sync + 'static,
{
    /// Toggle or set task completion status
    async fn set_completion(&self, id: &str, completed: bool) -> Result<()> {
        self.set_field(id, "completed", Value::Boolean(completed)).await
    }

    /// Set task priority (1=highest, 4=lowest in Todoist)
    async fn set_priority(&self, id: &str, priority: i64) -> Result<()> {
        self.set_field(id, "priority", Value::Integer(priority)).await
    }

    /// Set task due date
    async fn set_due_date(&self, id: &str, due_date: Option<DateTime<Utc>>) -> Result<()> {
        self.set_field(
            id,
            "due_date",
            due_date.map(|d| Value::DateTime(d)).unwrap_or(Value::Null),
        ).await
    }
}

// Blanket implementations: Automatically provide helper methods for any compatible type
impl<T, D> BlockDataSourceHelpers<T> for D
where
    T: BlockEntity + Send + Sync + 'static,
    D: CrudOperationProvider<T> + DataSource<T>,
{
    // All methods have default implementations, so nothing to implement here
}

// Blanket implementations: Automatically provide operations for any compatible type
impl<T, D> MutableBlockDataSource<T> for D
where
    T: BlockEntity + Send + Sync + 'static,
    D: BlockDataSourceHelpers<T>,
{
}

impl<T, D> MutableTaskDataSource<T> for D
where
    T: TaskEntity + Send + Sync + 'static,
    D: DataSource<T> + CrudOperationProvider<T>,
{
}

/// Unified interface for operation providers
///
/// Implemented by:
/// - Leaf nodes: QueryableCache<T> (handle specific entity types)
/// - Composite: OperationDispatcher (aggregates providers, routes operations)
///
/// Uses Composite Pattern - both caches and dispatcher implement the same trait.
#[async_trait]
pub trait OperationProvider: Send + Sync {
    /// Get all operations this provider supports
    fn operations(&self) -> Vec<OperationDescriptor>;

    /// Find operations that can be executed with given arguments
    ///
    /// Filters to operations where required_params ⊆ available_args.
    ///
    /// Example:
    /// ```
    /// // Lineage: checkbox modifies "completed" field
    /// // Available: ["id", "completed"]
    /// let ops = provider.find_operations("todoist-task", &["id", "completed"]);
    /// // Returns: ["set_field", "set_completion", "delete"]
    /// // Excludes: ["create"] (needs more fields), ["indent_block"] (needs parent_id)
    /// ```
    fn find_operations(
        &self,
        entity_name: &str,
        available_args: &[String]
    ) -> Vec<OperationDescriptor> {
        self.operations()
            .into_iter()
            .filter(|op| {
                op.entity_name == entity_name &&
                op.required_params.iter().all(|p| available_args.contains(&p.name))
            })
            .collect()
    }

    /// Execute an operation
    ///
    /// - Individual caches: validate entity_name, dispatch to trait methods
    /// - Composite dispatcher: route to correct registered provider
    async fn execute_operation(
        &self,
        entity_name: &str,
        op_name: &str,
        params: StorageEntity
    ) -> Result<()>;

    /// Get the last created entity ID (if any)
    ///
    /// This is used by GenericProviderState to track entity creation.
    /// Providers that support this should override this method to return
    /// the ID of the last entity created via execute_operation.
    /// Default implementation returns None.
    fn get_last_created_id(&self) -> Option<String> {
        None
    }
}

/// Trait for aggregating operation metadata from multiple trait sources
///
/// Entity types implement this trait to declare which operations they support.
/// The implementation aggregates operations from all applicable traits:
/// - `CrudOperationProvider` (CrudOperationProvider) operations (set_field, create, delete)
/// - `MutableBlockDataSource` operations (if entity implements `BlockEntity`)
/// - `MutableTaskDataSource` operations (if entity implements `TaskEntity`)
///
/// Example:
/// ```rust
/// impl OperationRegistry for TodoistTask {
///     fn all_operations() -> Vec<OperationDescriptor> {
///         let entity_name = Self::entity_name();
///         let table = "todoist_tasks";
///         let id_column = "id";
///
///         __operations_crud_operation_provider::mutable_data_source_operations(entity_name, table, id_column)
///             .into_iter()
///             .chain(__operations_mutable_block_data_source::mutable_block_data_source_operations(entity_name, table, id_column).into_iter())
///             .chain(__operations_mutable_task_data_source::mutable_task_data_source_operations(entity_name, table, id_column).into_iter())
///             .collect()
///     }
///
///     fn entity_name() -> &'static str {
///         "todoist-task"
///     }
/// }
/// ```
pub trait OperationRegistry: Send + Sync {
    /// Returns all operations supported by this entity type
    fn all_operations() -> Vec<OperationDescriptor>;

    /// Returns the entity name for this registry (e.g., "todoist-task", "logseq-block")
    fn entity_name() -> &'static str;
}

/// Type-independent sync trait for providers
///
/// Providers that can sync from external systems implement this trait.
/// Sync operations are generated dynamically when providers are registered,
/// using the format "{provider_name}.sync" (e.g., "todoist.sync", "jira.sync").
#[async_trait]
pub trait SyncableProvider: Send + Sync {
    /// Get the provider name (e.g., "todoist", "jira")
    ///
    /// This name is used to generate sync operations and identify the provider.
    fn provider_name(&self) -> &str;

    /// Sync data from the external system
    ///
    /// This method should:
    /// - Fetch updates from the external system using the provided stream position
    /// - Emit changes via streams (if applicable)
    /// - Return the new stream position for persistence
    ///
    /// # Arguments
    /// * `position` - Current stream position (StreamPosition::Beginning for full sync, StreamPosition::Version(token) for incremental sync)
    ///
    /// # Returns
    /// The new stream position (typically StreamPosition::Version with new token, or StreamPosition::Beginning if no token)
    async fn sync(&self, position: StreamPosition) -> Result<StreamPosition>;
}

/// Trait for external sync providers that emit typed change streams
///
/// This trait allows QueryableCache to register and consume change streams
/// from external systems (Todoist, etc.) in a type-safe way.
/// ExternalServiceDiscovery
pub trait StreamProvider<T>: Send + Sync
where
    T: Send + Sync + 'static,
{
    /// Get a receiver for changes of type T
    ///
    /// Returns a broadcast receiver that emits batches of changes.
    /// Multiple QueryableCache instances can subscribe to the same stream.
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Vec<Change<T>>>;
}

/// Generate a sync operation descriptor for a provider
///
/// This is used by OperationDispatcher when registering SyncableProviders
/// to create operation descriptors with the correct entity_name format.
pub fn generate_sync_operation(provider_name: &str) -> OperationDescriptor {
    OperationDescriptor {
        entity_name: format!("{}.sync", provider_name),
        table: String::new(), // Sync operations don't operate on a specific table
        id_column: String::new(), // Sync operations don't need an ID column
        name: "sync".to_string(),
        display_name: format!("Sync {}", provider_name),
        description: format!("Sync data from {} provider", provider_name),
        required_params: vec![],
        precondition: None,
    }
}

// Tests moved to operation_dispatcher.rs

// Re-export macro-generated operation functions
pub use __operations_crud_operation_provider::crud_operation_provider_operations;
pub use __operations_mutable_block_data_source::mutable_block_data_source_operations;
pub use __operations_mutable_task_data_source::mutable_task_data_source_operations;

#[cfg(test)]
#[path = "datasource_tests.rs"]
mod datasource_tests;
