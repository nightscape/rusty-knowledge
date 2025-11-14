use anyhow::{Context, Result};
use async_trait::async_trait;

use super::{Operation, RowView};
use crate::storage::types::{StorageEntity, Value};
use crate::storage::turso::TursoBackend;
use crate::storage::backend::StorageBackend;
use crate::storage::{gen_key_between, gen_n_keys, MAX_SORT_KEY_LENGTH};
use crate::api::render_engine::UiState;

/// Helper to convert turso_core::Value to Option<String>
fn turso_value_to_option_string(value: turso_core::Value) -> Option<String> {
    match value {
        turso_core::Value::Null => None,
        turso_core::Value::Text(s) => Some(s.to_string()),
        turso_core::Value::Integer(i) => Some(i.to_string()),
        turso_core::Value::Float(f) => Some(f.to_string()),
        turso_core::Value::Blob(_) => None,
    }
}

/// Helper to convert turso_core::Value to String
fn turso_value_to_string(value: turso_core::Value) -> String {
    match value {
        turso_core::Value::Null => String::new(),
        turso_core::Value::Text(s) => s.to_string(),
        turso_core::Value::Integer(i) => i.to_string(),
        turso_core::Value::Float(f) => f.to_string(),
        turso_core::Value::Blob(_) => String::new(),
    }
}

/// Move a block to a new parent and position
///
/// Uses fractional indexing to generate sort_key between neighbors.
/// Triggers rebalancing if generated key exceeds MAX_SORT_KEY_LENGTH.
///
/// # Parameters
/// - `id`: Block ID to move
/// - `new_parent_id`: Target parent ID (or null for root)
/// - `after_block_id`: Optional anchor block (move after this block, or beginning if None)
///
/// # Design Decision: Anchor-Based Approach
/// - UI sends semantic intent (move after block X)
/// - Rust queries DB for actual sort_keys (source of truth)
/// - Handles filtered views correctly (UI might not see all siblings)
pub struct MoveBlock;

impl MoveBlock {
    /// Get the depth of a parent block
    async fn get_parent_depth(db: &TursoBackend, parent_id: &str) -> Result<i64> {
        let conn = db.get_connection()?;
        let mut stmt = conn
            .prepare("SELECT depth FROM blocks WHERE id = ?")
            .await?;
        let row = stmt.query_row((parent_id,)).await?;
        let value = row.get_value(0)?;

        match value.into() {
            turso_core::Value::Integer(i) => Ok(i),
            _ => Ok(0),
        }
    }

    /// Get the current depth of a block
    async fn get_block_depth(db: &TursoBackend, block_id: &str) -> Result<i64> {
        let conn = db.get_connection()?;
        let mut stmt = conn
            .prepare("SELECT depth FROM blocks WHERE id = ?")
            .await?;
        let row = stmt.query_row((block_id,)).await?;
        let value = row.get_value(0)?;

        match value.into() {
            turso_core::Value::Integer(i) => Ok(i),
            _ => Ok(0),
        }
    }

    /// Recursively update depths of all descendants when a parent's depth changes
    ///
    /// When a block moves, its depth changes by `depth_delta`. All its descendants
    /// must have their depths updated by the same delta to maintain correct hierarchy.
    ///
    /// Uses an iterative approach (queue-based) to avoid recursion issues with async.
    async fn update_descendant_depths(
        db: &mut TursoBackend,
        parent_id: &str,
        depth_delta: i64,
    ) -> Result<()> {
        if depth_delta == 0 {
            // No change needed
            return Ok(());
        }

        let conn = db.get_connection()?;

        // Use a queue to process all descendants iteratively
        let mut queue = vec![parent_id.to_string()];

        while let Some(current_parent_id) = queue.pop() {
            // Get all direct children of this parent
            let mut stmt = conn
                .prepare("SELECT id FROM blocks WHERE parent_id = ?")
                .await?;
            let mut rows = stmt.query((current_parent_id.as_str(),)).await?;

            let mut child_ids = Vec::new();
            while let Some(row) = rows.next().await? {
                let value = row.get_value(0)?;
                let child_id = turso_value_to_string(value.into());
                child_ids.push(child_id);
            }
            drop(stmt);

            // Update each child's depth and add to queue for processing its descendants
            for child_id in child_ids {
                // Get current depth
                let current_depth = Self::get_block_depth(db, &child_id).await?;
                let new_depth = current_depth + depth_delta;

                // Update child's depth
                let mut updates = StorageEntity::new();
                updates.insert("depth".to_string(), Value::Integer(new_depth));
                db.update("blocks", &child_id, updates).await?;

                // Add to queue to process this child's descendants
                queue.push(child_id);
            }
        }

        Ok(())
    }

    /// Query the sort_key of the predecessor block (if any)
    async fn get_prev_sort_key(
        db: &TursoBackend,
        after_block_id: Option<&str>,
    ) -> Result<Option<String>> {
        if let Some(after_id) = after_block_id {
            let conn = db.get_connection()?;
            let mut stmt = conn
                .prepare("SELECT sort_key FROM blocks WHERE id = ?")
                .await?;
            let row = stmt.query_row((after_id,)).await?;
            let value = row.get_value(0)?;
            let sort_key = turso_value_to_string(value.into());
            Ok(Some(sort_key))
        } else {
            Ok(None)
        }
    }

    /// Query the sort_key of the last child (if any) for a given parent
    async fn get_last_child_sort_key(
        db: &TursoBackend,
        parent_id: Option<&str>,
    ) -> Result<Option<String>> {
        let conn = db.get_connection()?;

        let result = if let Some(parent) = parent_id {
            let mut stmt = conn.prepare(
                "SELECT sort_key FROM blocks
                 WHERE parent_id = ?
                 ORDER BY sort_key DESC LIMIT 1"
            ).await?;
            stmt.query_row((parent,)).await
        } else {
            let mut stmt = conn.prepare(
                "SELECT sort_key FROM blocks
                 WHERE parent_id IS NULL
                 ORDER BY sort_key DESC LIMIT 1"
            ).await?;
            stmt.query_row(()).await
        };

        match result {
            Ok(row) => {
                let value = row.get_value(0)?;
                let sort_key = turso_value_to_string(value.into());
                Ok(Some(sort_key))
            }
            Err(_) => Ok(None), // No children found
        }
    }

    /// Query the sort_key of the first child (if any) for a given parent
    async fn get_first_child_sort_key(
        db: &TursoBackend,
        parent_id: Option<&str>,
    ) -> Result<Option<String>> {
        let conn = db.get_connection()?;

        let result = if let Some(parent) = parent_id {
            let mut stmt = conn.prepare(
                "SELECT sort_key FROM blocks
                 WHERE parent_id = ?
                 ORDER BY sort_key ASC LIMIT 1"
            ).await?;
            stmt.query_row((parent,)).await
        } else {
            let mut stmt = conn.prepare(
                "SELECT sort_key FROM blocks
                 WHERE parent_id IS NULL
                 ORDER BY sort_key ASC LIMIT 1"
            ).await?;
            stmt.query_row(()).await
        };

        match result {
            Ok(row) => {
                let value = row.get_value(0)?;
                let sort_key = turso_value_to_string(value.into());
                Ok(Some(sort_key))
            }
            Err(_) => Ok(None), // No children found
        }
    }

    /// Query the sort_key of the successor block (if any)
    async fn get_next_sort_key(
        db: &TursoBackend,
        new_parent_id: Option<&str>,
        prev_sort_key: Option<&str>,
    ) -> Result<Option<String>> {
        let conn = db.get_connection()?;

        // Handle NULL parent_id properly by using separate queries
        let result = match (new_parent_id, prev_sort_key) {
            (Some(parent), Some(prev)) => {
                let mut stmt = conn.prepare(
                    "SELECT sort_key FROM blocks
                     WHERE parent_id = ? AND sort_key > ?
                     ORDER BY sort_key LIMIT 1"
                ).await?;
                stmt.query_row((parent, prev)).await
            }
            (Some(parent), None) => {
                let mut stmt = conn.prepare(
                    "SELECT sort_key FROM blocks
                     WHERE parent_id = ?
                     ORDER BY sort_key LIMIT 1"
                ).await?;
                stmt.query_row((parent,)).await
            }
            (None, Some(prev)) => {
                let mut stmt = conn.prepare(
                    "SELECT sort_key FROM blocks
                     WHERE parent_id IS NULL AND sort_key > ?
                     ORDER BY sort_key LIMIT 1"
                ).await?;
                stmt.query_row((prev,)).await
            }
            (None, None) => {
                let mut stmt = conn.prepare(
                    "SELECT sort_key FROM blocks
                     WHERE parent_id IS NULL
                     ORDER BY sort_key LIMIT 1"
                ).await?;
                stmt.query_row(()).await
            }
        };

        match result {
            Ok(row) => {
                let value = row.get_value(0)?;
                let sort_key = turso_value_to_string(value.into());
                Ok(Some(sort_key))
            }
            Err(_) => Ok(None), // No successor found
        }
    }

    /// Rebalance all siblings of a parent to create uniform spacing
    async fn rebalance_siblings(
        db: &mut TursoBackend,
        parent_id: Option<&str>,
    ) -> Result<()> {
        let conn = db.get_connection()?;

        // Query all siblings in order
        // Handle NULL parent_id properly by using separate queries
        let mut sibling_ids = Vec::new();

        if let Some(parent) = parent_id {
            let mut stmt = conn
                .prepare(
                    "SELECT id FROM blocks
                     WHERE parent_id = ?
                     ORDER BY sort_key"
                )
                .await?;

            let mut rows = stmt.query((parent,)).await?;

            while let Some(row) = rows.next().await? {
                let value = row.get_value(0)?;
                let id = turso_value_to_string(value.into());
                sibling_ids.push(id);
            }
        } else {
            let mut stmt = conn
                .prepare(
                    "SELECT id FROM blocks
                     WHERE parent_id IS NULL
                     ORDER BY sort_key"
                )
                .await?;

            let mut rows = stmt.query(()).await?;

            while let Some(row) = rows.next().await? {
                let value = row.get_value(0)?;
                let id = turso_value_to_string(value.into());
                sibling_ids.push(id);
            }
        }

        // Generate evenly-spaced keys
        let new_keys = gen_n_keys(sibling_ids.len())?;

        // Update all siblings
        for (id, new_key) in sibling_ids.iter().zip(new_keys.iter()) {
            let mut updates = StorageEntity::new();
            updates.insert("sort_key".to_string(), Value::String(new_key.clone()));
            db.update("blocks", id, updates).await?;
        }

        Ok(())
    }
}

#[async_trait]
impl Operation for MoveBlock {
    fn name(&self) -> &str {
        "move_block"
    }

    async fn execute(
        &self,
        row_data: &StorageEntity,
        _ui_state: &UiState,
        db: &mut TursoBackend,
    ) -> Result<()> {
        let view = RowView::new(row_data);
        let block_id = view.id()?;

        // Extract parameters
        let new_parent_id = row_data
            .get("new_parent_id")
            .and_then(|v| v.as_string())
            .map(String::from);

        let after_block_id = row_data
            .get("after_block_id")
            .and_then(|v| v.as_string())
            .map(String::from);

        // Query predecessor and successor sort_keys
        let (prev_key, next_key) = if after_block_id.is_none() {
            // No after_block_id means "move to beginning" - insert before first child
            let first_child_key = Self::get_first_child_sort_key(db, new_parent_id.as_deref()).await?;
            (None, first_child_key)
        } else {
            // Insert after specific block
            let prev_key = Self::get_prev_sort_key(db, after_block_id.as_deref()).await?;
            let next_key = Self::get_next_sort_key(
                db,
                new_parent_id.as_deref(),
                prev_key.as_deref()
            ).await?;
            (prev_key, next_key)
        };

        // Generate new sort_key
        let mut new_sort_key = gen_key_between(
            prev_key.as_deref(),
            next_key.as_deref()
        )?;

        // Check if rebalancing needed
        if new_sort_key.len() > MAX_SORT_KEY_LENGTH {
            Self::rebalance_siblings(db, new_parent_id.as_deref()).await?;

            // Re-query neighbors after rebalancing
            let (prev_key, next_key) = if after_block_id.is_none() {
                // No after_block_id means "move to beginning" - insert before first child
                let first_child_key = Self::get_first_child_sort_key(db, new_parent_id.as_deref()).await?;
                (None, first_child_key)
            } else {
                // Insert after specific block
                let prev_key = Self::get_prev_sort_key(db, after_block_id.as_deref()).await?;
                let next_key = Self::get_next_sort_key(
                    db,
                    new_parent_id.as_deref(),
                    prev_key.as_deref()
                ).await?;
                (prev_key, next_key)
            };

            new_sort_key = gen_key_between(
                prev_key.as_deref(),
                next_key.as_deref()
            )?;
        }

        // Get old depth before updating (needed to calculate delta for descendants)
        let old_depth = Self::get_block_depth(db, block_id).await?;

        // Calculate new depth based on parent
        let new_depth = if let Some(ref parent_id) = new_parent_id {
            Self::get_parent_depth(db, parent_id).await? + 1
        } else {
            0 // Root level
        };

        // Calculate depth delta for recursive updates
        let depth_delta = new_depth - old_depth;

        // Update block atomically
        let mut updates = StorageEntity::new();
        updates.insert("sort_key".to_string(), Value::String(new_sort_key));
        updates.insert("depth".to_string(), Value::Integer(new_depth));

        if let Some(parent_id) = new_parent_id {
            updates.insert("parent_id".to_string(), Value::String(parent_id));
        } else {
            updates.insert("parent_id".to_string(), Value::Null);
        }

        db.update("blocks", block_id, updates).await?;

        // Recursively update all descendants' depths by the same delta
        // This ensures that when a parent is indented/outdented, all its children
        // maintain the correct relative depth
        if depth_delta != 0 {
            Self::update_descendant_depths(db, block_id, depth_delta).await?;
        }

        Ok(())
    }
}

/// Indent a block (move it under the previous sibling)
///
/// Moves the block to become the last child of its previous sibling.
/// If there is no previous sibling, operation fails.
///
/// # Parameters
/// - `id`: Block ID to indent
pub struct Indent;

impl Indent {
    /// Find the previous sibling at the same level
    async fn find_prev_sibling(
        db: &TursoBackend,
        block_id: &str,
    ) -> Result<Option<String>> {
        let conn = db.get_connection()?;

        // Get current block's parent and sort_key
        let mut stmt = conn
            .prepare("SELECT parent_id, sort_key FROM blocks WHERE id = ?")
            .await?;
        let row = stmt.query_row((block_id,)).await?;
        let parent_id = turso_value_to_option_string(row.get_value(0)?.into());
        let sort_key = turso_value_to_string(row.get_value(1)?.into());
        drop(stmt);

        // Find previous sibling (same parent, sort_key < current)
        // Handle NULL parent_id properly by using separate queries
        let query_result = if let Some(ref parent) = parent_id {
            let mut stmt = conn
                .prepare(
                    "SELECT id FROM blocks
                     WHERE parent_id = ? AND sort_key < ?
                     ORDER BY sort_key DESC LIMIT 1"
                )
                .await?;
            stmt.query_row((parent.as_str(), sort_key.as_str())).await
        } else {
            let mut stmt = conn
                .prepare(
                    "SELECT id FROM blocks
                     WHERE parent_id IS NULL AND sort_key < ?
                     ORDER BY sort_key DESC LIMIT 1"
                )
                .await?;
            stmt.query_row((sort_key.as_str(),)).await
        };

        match query_result {
            Ok(row) => {
                let value = row.get_value(0)?;
                let prev_id = turso_value_to_string(value.into());
                Ok(Some(prev_id))
            }
            Err(_) => Ok(None),
        }
    }
}

#[async_trait]
impl Operation for Indent {
    fn name(&self) -> &str {
        "indent"
    }

    async fn execute(
        &self,
        row_data: &StorageEntity,
        ui_state: &UiState,
        db: &mut TursoBackend,
    ) -> Result<()> {
        let view = RowView::new(row_data);
        let block_id = view.id()?;

        // Find previous sibling
        let prev_sibling = Self::find_prev_sibling(db, block_id).await?
            .context("Cannot indent: no previous sibling")?;

        // Delegate to MoveBlock: move block as last child of previous sibling
        let mut move_params = StorageEntity::new();
        move_params.insert("id".to_string(), Value::String(block_id.to_string()));
        move_params.insert("new_parent_id".to_string(), Value::String(prev_sibling));
        // after_block_id omitted = move to end

        let move_op = MoveBlock;
        move_op.execute(&move_params, ui_state, db).await
    }
}

/// Outdent a block (move it to parent's level, after parent)
///
/// Moves the block to become the next sibling of its parent.
/// If the block has no parent (already at root), operation fails.
///
/// # Parameters
/// - `id`: Block ID to outdent
pub struct Outdent;

impl Outdent {
    /// Get the parent block ID
    async fn get_parent_id(
        db: &TursoBackend,
        block_id: &str,
    ) -> Result<Option<String>> {
        let conn = db.get_connection()?;
        let mut stmt = conn
            .prepare("SELECT parent_id FROM blocks WHERE id = ?")
            .await?;
        let row = stmt.query_row((block_id,)).await?;
        let value = row.get_value(0)?;
        let parent_id = turso_value_to_option_string(value.into());
        Ok(parent_id)
    }

    /// Get the grandparent block ID (parent's parent)
    async fn get_grandparent_id(
        db: &TursoBackend,
        parent_id: &str,
    ) -> Result<Option<String>> {
        let conn = db.get_connection()?;
        let mut stmt = conn
            .prepare("SELECT parent_id FROM blocks WHERE id = ?")
            .await?;
        let row = stmt.query_row((parent_id,)).await?;
        let value = row.get_value(0)?;
        let grandparent_id = turso_value_to_option_string(value.into());
        Ok(grandparent_id)
    }
}

#[async_trait]
impl Operation for Outdent {
    fn name(&self) -> &str {
        "outdent"
    }

    async fn execute(
        &self,
        row_data: &StorageEntity,
        ui_state: &UiState,
        db: &mut TursoBackend,
    ) -> Result<()> {
        let view = RowView::new(row_data);
        let block_id = view.id()?;

        // Get parent
        let parent_id = Self::get_parent_id(db, block_id).await?
            .context("Cannot outdent: block has no parent (already at root)")?;

        // Get grandparent (parent's parent)
        let grandparent_id = Self::get_grandparent_id(db, &parent_id).await?;

        // Delegate to MoveBlock: move block after parent, under grandparent
        let mut move_params = StorageEntity::new();
        move_params.insert("id".to_string(), Value::String(block_id.to_string()));

        if let Some(gp_id) = grandparent_id {
            move_params.insert("new_parent_id".to_string(), Value::String(gp_id));
        } else {
            move_params.insert("new_parent_id".to_string(), Value::Null);
        }

        move_params.insert("after_block_id".to_string(), Value::String(parent_id));

        let move_op = MoveBlock;
        move_op.execute(&move_params, ui_state, db).await
    }
}

/// Move a block up (swap with previous sibling)
///
/// Moves the block before its previous sibling, keeping the same parent.
/// If there is no previous sibling, operation fails.
///
/// # Parameters
/// - `id`: Block ID to move up
pub struct MoveUp;

impl MoveUp {
    /// Find the previous sibling and its predecessor
    async fn find_prev_siblings(
        db: &TursoBackend,
        block_id: &str,
    ) -> Result<(Option<String>, Option<String>)> {
        let conn = db.get_connection()?;

        // Get current block's parent and sort_key
        let mut stmt = conn
            .prepare("SELECT parent_id, sort_key FROM blocks WHERE id = ?")
            .await?;
        let row = stmt.query_row((block_id,)).await?;
        let parent_id = turso_value_to_option_string(row.get_value(0)?.into());
        let sort_key = turso_value_to_string(row.get_value(1)?.into());
        drop(stmt);

        // Find previous sibling (same parent, sort_key < current)
        // Handle NULL parent_id properly by using separate queries
        let query_result = if let Some(ref parent) = parent_id {
            let mut stmt = conn
                .prepare(
                    "SELECT id, sort_key FROM blocks
                     WHERE parent_id = ? AND sort_key < ?
                     ORDER BY sort_key DESC LIMIT 1"
                )
                .await?;
            stmt.query_row((parent.as_str(), sort_key.as_str())).await
        } else {
            let mut stmt = conn
                .prepare(
                    "SELECT id, sort_key FROM blocks
                     WHERE parent_id IS NULL AND sort_key < ?
                     ORDER BY sort_key DESC LIMIT 1"
                )
                .await?;
            stmt.query_row((sort_key.as_str(),)).await
        };

        let prev_sibling = match query_result {
            Ok(row) => {
                let prev_id = turso_value_to_string(row.get_value(0)?.into());
                let prev_sort_key = turso_value_to_string(row.get_value(1)?.into());

                // Find the sibling before the previous sibling
                let before_prev_result = if let Some(ref parent) = parent_id {
                    let mut stmt = conn
                        .prepare(
                            "SELECT id FROM blocks
                             WHERE parent_id = ? AND sort_key < ?
                             ORDER BY sort_key DESC LIMIT 1"
                        )
                        .await?;
                    stmt.query_row((parent.as_str(), prev_sort_key.as_str())).await
                } else {
                    let mut stmt = conn
                        .prepare(
                            "SELECT id FROM blocks
                             WHERE parent_id IS NULL AND sort_key < ?
                             ORDER BY sort_key DESC LIMIT 1"
                        )
                        .await?;
                    stmt.query_row((prev_sort_key.as_str(),)).await
                };

                let before_prev = match before_prev_result {
                    Ok(row) => {
                        let value = row.get_value(0)?;
                        let id = turso_value_to_string(value.into());
                        Some(id)
                    }
                    Err(_) => None,
                };

                (Some(prev_id), before_prev)
            }
            Err(_) => (None, None),
        };

        Ok(prev_sibling)
    }
}

#[async_trait]
impl Operation for MoveUp {
    fn name(&self) -> &str {
        "move_up"
    }

    async fn execute(
        &self,
        row_data: &StorageEntity,
        ui_state: &UiState,
        db: &mut TursoBackend,
    ) -> Result<()> {
        let view = RowView::new(row_data);
        let block_id = view.id()?;

        // Find previous sibling and the one before it
        let (prev_sibling, before_prev) = Self::find_prev_siblings(db, block_id).await?;

        // Check if there's a previous sibling to move before
        if prev_sibling.is_none() {
            return Err(anyhow::anyhow!("Cannot move up: no previous sibling"));
        }

        // Get current parent
        let conn = db.get_connection()?;
        let mut stmt = conn
            .prepare("SELECT parent_id FROM blocks WHERE id = ?")
            .await?;
        let row = stmt.query_row((block_id,)).await?;
        let value = row.get_value(0)?;
        let parent_id = turso_value_to_option_string(value.into());
        drop(stmt);

        // Delegate to MoveBlock: move before previous sibling
        // If before_prev exists, we can use MoveBlock normally (move after before_prev)
        // If before_prev doesn't exist, we need to move to the beginning (before prev_sibling)
        // MoveBlock interprets missing after_block_id as "move to end", so we need special handling

        if let Some(before_id) = before_prev {
            // Normal case: move after before_prev (which places us before prev_sibling)
            let mut move_params = StorageEntity::new();
            move_params.insert("id".to_string(), Value::String(block_id.to_string()));

            if let Some(p_id) = parent_id {
                move_params.insert("new_parent_id".to_string(), Value::String(p_id));
            } else {
                move_params.insert("new_parent_id".to_string(), Value::Null);
            }

            move_params.insert("after_block_id".to_string(), Value::String(before_id));

            let move_op = MoveBlock;
            move_op.execute(&move_params, ui_state, db).await
        } else {
            // Special case: move to beginning (before prev_sibling)
            // When before_prev is None, we want to move to the beginning of the siblings list
            // Get the first child's sort_key and generate a key before it
            let conn = db.get_connection()?;
            let first_child_key = if let Some(ref parent) = parent_id {
                let mut stmt = conn.prepare(
                    "SELECT sort_key FROM blocks WHERE parent_id = ? ORDER BY sort_key ASC LIMIT 1"
                ).await?;
                match stmt.query_row((parent.as_str(),)).await {
                    Ok(row) => Some(turso_value_to_string(row.get_value(0)?.into())),
                    Err(_) => None,
                }
            } else {
                let mut stmt = conn.prepare(
                    "SELECT sort_key FROM blocks WHERE parent_id IS NULL ORDER BY sort_key ASC LIMIT 1"
                ).await?;
                match stmt.query_row(()).await {
                    Ok(row) => Some(turso_value_to_string(row.get_value(0)?.into())),
                    Err(_) => None,
                }
            };

            // Generate a sort_key before the first child's key
            use crate::storage::gen_key_between;
            let new_sort_key = match first_child_key {
                Some(first_key) => gen_key_between(None, Some(&first_key))?,
                None => {
                    // No children exist, generate a key at the beginning
                    gen_key_between(None, None)?
                }
            };

            // Calculate depth
            let new_depth = if let Some(ref p_id) = parent_id {
                MoveBlock::get_parent_depth(db, p_id).await? + 1
            } else {
                0
            };

            // Update block directly
            let mut updates = StorageEntity::new();
            updates.insert("sort_key".to_string(), Value::String(new_sort_key));
            updates.insert("depth".to_string(), Value::Integer(new_depth));
            if let Some(p_id) = parent_id {
                updates.insert("parent_id".to_string(), Value::String(p_id));
            } else {
                updates.insert("parent_id".to_string(), Value::Null);
            }
            db.update("blocks", block_id, updates).await?;
            Ok(())
        }
    }
}

/// Move a block down (swap with next sibling)
///
/// Moves the block after its next sibling, keeping the same parent.
/// If there is no next sibling, operation fails.
///
/// # Parameters
/// - `id`: Block ID to move down
pub struct MoveDown;

impl MoveDown {
    /// Find the next sibling
    async fn find_next_sibling(
        db: &TursoBackend,
        block_id: &str,
    ) -> Result<Option<String>> {
        let conn = db.get_connection()?;

        // Get current block's parent and sort_key
        let mut stmt = conn
            .prepare("SELECT parent_id, sort_key FROM blocks WHERE id = ?")
            .await?;
        let row = stmt.query_row((block_id,)).await?;
        let parent_id = turso_value_to_option_string(row.get_value(0)?.into());
        let sort_key = turso_value_to_string(row.get_value(1)?.into());
        drop(stmt);

        // Find next sibling (same parent, sort_key > current)
        // Handle NULL parent_id properly by using separate queries
        let query_result = if let Some(ref parent) = parent_id {
            let mut stmt = conn
                .prepare(
                    "SELECT id FROM blocks
                     WHERE parent_id = ? AND sort_key > ?
                     ORDER BY sort_key ASC LIMIT 1"
                )
                .await?;
            stmt.query_row((parent.as_str(), sort_key.as_str())).await
        } else {
            let mut stmt = conn
                .prepare(
                    "SELECT id FROM blocks
                     WHERE parent_id IS NULL AND sort_key > ?
                     ORDER BY sort_key ASC LIMIT 1"
                )
                .await?;
            stmt.query_row((sort_key.as_str(),)).await
        };

        match query_result {
            Ok(row) => {
                let value = row.get_value(0)?;
                let next_id = turso_value_to_string(value.into());
                Ok(Some(next_id))
            }
            Err(_) => Ok(None),
        }
    }
}

#[async_trait]
impl Operation for MoveDown {
    fn name(&self) -> &str {
        "move_down"
    }

    async fn execute(
        &self,
        row_data: &StorageEntity,
        ui_state: &UiState,
        db: &mut TursoBackend,
    ) -> Result<()> {
        let view = RowView::new(row_data);
        let block_id = view.id()?;

        // Find next sibling
        let next_sibling = Self::find_next_sibling(db, block_id).await?
            .context("Cannot move down: no next sibling")?;

        // Get current parent
        let conn = db.get_connection()?;
        let mut stmt = conn
            .prepare("SELECT parent_id FROM blocks WHERE id = ?")
            .await?;
        let row = stmt.query_row((block_id,)).await?;
        let value = row.get_value(0)?;
        let parent_id = turso_value_to_option_string(value.into());
        drop(stmt);

        // Delegate to MoveBlock: move after next sibling
        let mut move_params = StorageEntity::new();
        move_params.insert("id".to_string(), Value::String(block_id.to_string()));

        if let Some(p_id) = parent_id {
            move_params.insert("new_parent_id".to_string(), Value::String(p_id));
        } else {
            move_params.insert("new_parent_id".to_string(), Value::Null);
        }

        move_params.insert("after_block_id".to_string(), Value::String(next_sibling));

        let move_op = MoveBlock;
        move_op.execute(&move_params, ui_state, db).await
    }
}

#[cfg(test)]
mod tests {
  use super::*;

  // Helper to create test blocks table
  async fn create_blocks_table(db: &TursoBackend) {
      let conn = db.get_connection().unwrap();
      conn.execute(
          "CREATE TABLE blocks (
              id TEXT PRIMARY KEY,
              parent_id TEXT,
              depth INTEGER NOT NULL DEFAULT 0,
              sort_key TEXT NOT NULL,
              content TEXT
          )",
          ()
      ).await.unwrap();
  }
  // Helper to insert test block with valid fractional index sort_key
  async fn insert_block(
      db: &TursoBackend,
      id: &str,
      parent_id: Option<&str>,
      prev_key: Option<&str>,
  ) {
      // Generate proper fractional index key
      let sort_key = gen_key_between(prev_key, None).unwrap();

      let conn = db.get_connection().unwrap();
      conn.execute(
          "INSERT INTO blocks (id, parent_id, sort_key, content) VALUES (?, ?, ?, ?)",
          (id, parent_id, sort_key, format!("Content {}", id))
      ).await.unwrap();
  }

  // Helper to get block's sort_key
  async fn get_sort_key(db: &TursoBackend, id: &str) -> String {
      let conn = db.get_connection().unwrap();
      let mut stmt = conn.prepare("SELECT sort_key FROM blocks WHERE id = ?").await.unwrap();
      let row = stmt.query_row((id,)).await.unwrap();
      row.get(0).unwrap()
  }

  // Helper to get block's parent_id
  async fn get_parent_id(db: &TursoBackend, id: &str) -> Option<String> {
      let conn = db.get_connection().unwrap();
      let mut stmt = conn.prepare("SELECT parent_id FROM blocks WHERE id = ?").await.unwrap();
      let row = stmt.query_row((id,)).await.unwrap();
      row.get(0).unwrap()
  }

  #[tokio::test]
  async fn test_move_block_to_beginning() {
      let mut db = TursoBackend::new_in_memory().await.unwrap();
      create_blocks_table(&db).await;

      // Create siblings: A, B, C under parent P
      insert_block(&db, "P", None, None).await;
      insert_block(&db, "A", Some("P"), None).await;
      let sort_a = get_sort_key(&db, "A").await;
      insert_block(&db, "B", Some("P"), Some(&sort_a)).await;
      let sort_b = get_sort_key(&db, "B").await;
      insert_block(&db, "C", Some("P"), Some(&sort_b)).await;

      // Move C to beginning (before A)
      let mut params = StorageEntity::new();
      params.insert("id".to_string(), Value::String("C".to_string()));
      params.insert("new_parent_id".to_string(), Value::String("P".to_string()));
      // No after_block_id = move to beginning

      let op = MoveBlock;
      let ui_state = UiState { cursor_pos: None, focused_id: None };
      op.execute(&params, &ui_state, &mut db).await.unwrap();

      // Verify order: C < A < B
      let sort_c = get_sort_key(&db, "C").await;
      let sort_a = get_sort_key(&db, "A").await;
      let sort_b = get_sort_key(&db, "B").await;

      assert!(sort_c < sort_a);
      assert!(sort_a < sort_b);
  }

  #[tokio::test]
  async fn test_move_block_to_end() {
      let mut db = TursoBackend::new_in_memory().await.unwrap();
      create_blocks_table(&db).await;

      // Create siblings: A, B, C under parent P
      insert_block(&db, "P", None, None).await;
      insert_block(&db, "A", Some("P"), None).await;
      let sort_a = get_sort_key(&db, "A").await;
      insert_block(&db, "B", Some("P"), Some(&sort_a)).await;
      let sort_b = get_sort_key(&db, "B").await;
      insert_block(&db, "C", Some("P"), Some(&sort_b)).await;

      // Move A to end (after C)
      let mut params = StorageEntity::new();
      params.insert("id".to_string(), Value::String("A".to_string()));
      params.insert("new_parent_id".to_string(), Value::String("P".to_string()));
      params.insert("after_block_id".to_string(), Value::String("C".to_string()));

      let op = MoveBlock;
      let ui_state = UiState { cursor_pos: None, focused_id: None };
      op.execute(&params, &ui_state, &mut db).await.unwrap();

      // Verify order: B < C < A
      let sort_a = get_sort_key(&db, "A").await;
      let sort_b = get_sort_key(&db, "B").await;
      let sort_c = get_sort_key(&db, "C").await;

      assert!(sort_b < sort_c);
      assert!(sort_c < sort_a);
  }

  #[tokio::test]
  async fn test_move_block_between() {
      let mut db = TursoBackend::new_in_memory().await.unwrap();
      create_blocks_table(&db).await;

      // Create siblings: A, B, C under parent P
      insert_block(&db, "P", None, None).await;
      insert_block(&db, "A", Some("P"), None).await;
      let sort_a = get_sort_key(&db, "A").await;
      insert_block(&db, "B", Some("P"), Some(&sort_a)).await;
      let sort_b = get_sort_key(&db, "B").await;
      insert_block(&db, "C", Some("P"), Some(&sort_b)).await;

      // Move C between A and B
      let mut params = StorageEntity::new();
      params.insert("id".to_string(), Value::String("C".to_string()));
      params.insert("new_parent_id".to_string(), Value::String("P".to_string()));
      params.insert("after_block_id".to_string(), Value::String("A".to_string()));

      let op = MoveBlock;
      let ui_state = UiState { cursor_pos: None, focused_id: None };
      op.execute(&params, &ui_state, &mut db).await.unwrap();

      // Verify order: A < C < B
      let sort_a = get_sort_key(&db, "A").await;
      let sort_b = get_sort_key(&db, "B").await;
      let sort_c = get_sort_key(&db, "C").await;

      assert!(sort_a < sort_c);
      assert!(sort_c < sort_b);
  }

  #[tokio::test]
  async fn test_move_block_change_parent() {
      let mut db = TursoBackend::new_in_memory().await.unwrap();
      create_blocks_table(&db).await;

      // Create structure: P1 -> A, P2 -> B
      insert_block(&db, "P1", None, None).await;
      insert_block(&db, "A", Some("P1"), None).await;
      let sort_p1 = get_sort_key(&db, "P1").await;
      insert_block(&db, "P2", None, Some(&sort_p1)).await;
      insert_block(&db, "B", Some("P2"), None).await;

      // Move A under P2 (after B)
      let mut params = StorageEntity::new();
      params.insert("id".to_string(), Value::String("A".to_string()));
      params.insert("new_parent_id".to_string(), Value::String("P2".to_string()));
      params.insert("after_block_id".to_string(), Value::String("B".to_string()));

      let op = MoveBlock;
      let ui_state = UiState { cursor_pos: None, focused_id: None };
      op.execute(&params, &ui_state, &mut db).await.unwrap();

      // Verify A's parent changed to P2
      let parent = get_parent_id(&db, "A").await;
      assert_eq!(parent, Some("P2".to_string()));

      // Verify order: B < A under P2
      let sort_a = get_sort_key(&db, "A").await;
      let sort_b = get_sort_key(&db, "B").await;
      assert!(sort_b < sort_a);
  }

  #[tokio::test]
  async fn test_indent_operation() {
      let mut db = TursoBackend::new_in_memory().await.unwrap();
      create_blocks_table(&db).await;

      // Create siblings: A, B, C under root
      insert_block(&db, "A", None, None).await;
      let sort_a = get_sort_key(&db, "A").await;
      insert_block(&db, "B", None, Some(&sort_a)).await;
      let sort_b = get_sort_key(&db, "B").await;
      insert_block(&db, "C", None, Some(&sort_b)).await;

      // Indent B (move under A)
      let mut params = StorageEntity::new();
      params.insert("id".to_string(), Value::String("B".to_string()));

      let op = Indent;
      let ui_state = UiState { cursor_pos: None, focused_id: None };
      op.execute(&params, &ui_state, &mut db).await.unwrap();

      // Verify B's parent is now A
      let parent = get_parent_id(&db, "B").await;
      assert_eq!(parent, Some("A".to_string()));
  }

  #[tokio::test]
  async fn test_indent_no_previous_sibling_fails() {
      let mut db = TursoBackend::new_in_memory().await.unwrap();
      create_blocks_table(&db).await;

      // Create single block at root
      insert_block(&db, "A", None, None).await;

      // Try to indent A (should fail - no previous sibling)
      let mut params = StorageEntity::new();
      params.insert("id".to_string(), Value::String("A".to_string()));

      let op = Indent;
      let ui_state = UiState { cursor_pos: None, focused_id: None };
      let result = op.execute(&params, &ui_state, &mut db).await;

      assert!(result.is_err());
      assert!(result.unwrap_err().to_string().contains("no previous sibling"));
  }

  #[tokio::test]
  async fn test_outdent_operation() {
      let mut db = TursoBackend::new_in_memory().await.unwrap();
      create_blocks_table(&db).await;

      // Create structure: A -> B -> C
      insert_block(&db, "A", None, None).await;
      insert_block(&db, "B", Some("A"), None).await;
      insert_block(&db, "C", Some("B"), None).await;

      // Outdent C (move to A's level, after B)
      let mut params = StorageEntity::new();
      params.insert("id".to_string(), Value::String("C".to_string()));

      let op = Outdent;
      let ui_state = UiState { cursor_pos: None, focused_id: None };
      op.execute(&params, &ui_state, &mut db).await.unwrap();

      // Verify C's parent is now A (same as B's parent)
      let parent = get_parent_id(&db, "C").await;
      assert_eq!(parent, Some("A".to_string()));

      // Verify C comes after B
      let sort_b = get_sort_key(&db, "B").await;
      let sort_c = get_sort_key(&db, "C").await;
      assert!(sort_b < sort_c);
  }

  #[tokio::test]
  async fn test_outdent_at_root_fails() {
      let mut db = TursoBackend::new_in_memory().await.unwrap();
      create_blocks_table(&db).await;

      // Create block at root
      insert_block(&db, "A", None, None).await;

      // Try to outdent A (should fail - already at root)
      let mut params = StorageEntity::new();
      params.insert("id".to_string(), Value::String("A".to_string()));

      let op = Outdent;
      let ui_state = UiState { cursor_pos: None, focused_id: None };
      let result = op.execute(&params, &ui_state, &mut db).await;

      assert!(result.is_err());
      assert!(result.unwrap_err().to_string().contains("no parent"));
  }

  #[tokio::test]
  async fn test_move_block_ordering_preserved() {
      let mut db = TursoBackend::new_in_memory().await.unwrap();
      create_blocks_table(&db).await;

      // Create many siblings to test ordering
      insert_block(&db, "P", None, None).await;
      let mut prev_sort = None;
      for i in 0..10 {
          let id = format!("B{}", i);
          insert_block(&db, &id, Some("P"), prev_sort.as_deref()).await;
          prev_sort = Some(get_sort_key(&db, &id).await);
      }

      // Move B5 between B2 and B3
      let mut params = StorageEntity::new();
      params.insert("id".to_string(), Value::String("B5".to_string()));
      params.insert("new_parent_id".to_string(), Value::String("P".to_string()));
      params.insert("after_block_id".to_string(), Value::String("B2".to_string()));

      let op = MoveBlock;
      let ui_state = UiState { cursor_pos: None, focused_id: None };
      op.execute(&params, &ui_state, &mut db).await.unwrap();

      // Verify B2 < B5 < B3
      let sort_2 = get_sort_key(&db, "B2").await;
      let sort_3 = get_sort_key(&db, "B3").await;
      let sort_5 = get_sort_key(&db, "B5").await;

      assert!(sort_2 < sort_5);
      assert!(sort_5 < sort_3);
  }

}
