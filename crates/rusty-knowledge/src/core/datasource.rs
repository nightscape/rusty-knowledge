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
    fn parent_id(&self) -> Option<&str>;
    fn sort_key(&self) -> &str;
    fn depth(&self) -> i64;
}

/// Entities that support task management (completion, priority, etc.)
pub trait TaskEntity: Send + Sync {
    fn completed(&self) -> bool;
    fn priority(&self) -> Option<i64>;
    fn due_date(&self) -> Option<DateTime<Utc>>;
}

/// Hierarchical structure operations (for any block-like entity)
#[rusty_knowledge_macros::operations_trait]
#[async_trait]
pub trait MutableBlockDataSource<T>: CrudOperationProvider<T> + DataSource<T>
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
    async fn move_block(&self, id: &str, after_id: Option<&str>) -> Result<()> {
        // Calculate new sort_key based on neighbors
        let (prev_key, next_key) = if let Some(after) = after_id {
            let after_block = self.get_by_id(after).await?
                .ok_or_else(|| anyhow::anyhow!("Reference block not found"))?;
            // TODO: Implement get_next_sibling helper
            let next = None; // Placeholder - need to implement sibling lookup
            (Some(after_block.sort_key().to_string()), next.map(|b: T| b.sort_key().to_string()))
        } else {
            // TODO: Implement get_first_sibling helper
            let first = None; // Placeholder
            (None, first.map(|b: T| b.sort_key().to_string()))
        };

        let new_key = crate::storage::fractional_index::gen_key_between(
            prev_key.as_deref(),
            next_key.as_deref(),
        )?;
        self.set_field(id, "sort_key", Value::String(new_key)).await
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

        // Move to grandparent's children
        let new_depth = block.depth() - 1;
        if let Some(gp_id) = grandparent_id {
            self.set_field(id, "parent_id", Value::String(gp_id.to_string())).await?;
        } else {
            self.set_field(id, "parent_id", Value::Null).await?;
        }
        self.set_field(id, "depth", Value::Integer(new_depth)).await?;
        Ok(())
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

// Blanket implementations: Automatically provide operations for any compatible type
impl<T, D> MutableBlockDataSource<T> for D
where
    T: BlockEntity + Send + Sync + 'static,
    D: DataSource<T> + CrudOperationProvider<T>,
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
    /// Sync data from the external system
    ///
    /// This method should:
    /// - Fetch updates from the external system
    /// - Emit changes via streams (if applicable)
    /// - Update internal state (sync tokens, etc.)
    async fn sync(&mut self) -> Result<()>;
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
