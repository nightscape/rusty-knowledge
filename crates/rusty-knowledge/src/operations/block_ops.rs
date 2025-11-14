use anyhow::{Context, Result, bail};
use async_trait::async_trait;

use super::{Operation, RowView};
use crate::storage::types::{StorageEntity, Value};
use crate::storage::turso::TursoBackend;
use crate::storage::backend::StorageBackend;
use crate::storage::fractional_index::gen_key_between;
use crate::api::render_engine::UiState;
use uuid::Uuid;
use turso_core as turso;

/// Update a single field on an entity
///
/// This is the primary operation for simple field updates. It validates
/// field types and constraints but does NOT check against current values
/// (important for offline-first + eventual consistency).
///
/// # Examples
/// - Set completed = true
/// - Set content = "new text"
/// - Set priority = 5
/// - Set parent_id = "block-123"
pub struct UpdateField;

impl UpdateField {
    /// Validate that the field name is allowed and value type is correct
    fn validate_field(field: &str, value: &Value) -> Result<()> {
        match field {
            // String fields
            "content" | "parent_id" | "block_type" | "sort_key" => {
                match value {
                    Value::String(_) | Value::Null => Ok(()),
                    _ => bail!("Field '{}' must be a string or null", field),
                }
            }

            // Integer fields
            "depth" | "priority" => {
                match value {
                    Value::Integer(v) => {
                        if field == "depth" && *v < 0 {
                            bail!("Field 'depth' must be >= 0");
                        }
                        Ok(())
                    }
                    _ => bail!("Field '{}' must be an integer", field),
                }
            }

            // Boolean fields
            "collapsed" | "completed" | "visible" => {
                match value {
                    Value::Boolean(_) => Ok(()),
                    _ => bail!("Field '{}' must be a boolean", field),
                }
            }

            // DateTime fields
            "created_at" | "updated_at" | "due_date" => {
                match value {
                    Value::DateTime(_) | Value::Null => Ok(()),
                    _ => bail!("Field '{}' must be a datetime or null", field),
                }
            }

            // JSON fields (flexible)
            "metadata" | "tags" => {
                match value {
                    Value::Json(_) | Value::Null => Ok(()),
                    _ => bail!("Field '{}' must be JSON or null", field),
                }
            }

            // Reserved fields (cannot be updated directly)
            "id" => bail!("Field 'id' cannot be updated"),

            // Unknown field - allow it (for extension flexibility)
            // but warn in logs
            _ => {
                eprintln!("WARNING: Updating unknown field '{}' - may not be in schema", field);
                Ok(())
            }
        }
    }
}

#[async_trait]
impl Operation for UpdateField {
    fn name(&self) -> &str {
        "update_field"
    }

    async fn execute(
        &self,
        row_data: &StorageEntity,
        _ui_state: &UiState,
        db: &mut TursoBackend,
    ) -> Result<()> {
        let view = RowView::new(row_data);
        let id = view.id()?;

        // Extract field name and value from row_data params
        let field = row_data
            .get("field")
            .context("Missing 'field' parameter")?
            .as_string()
            .context("Parameter 'field' must be a string")?;

        let value = row_data
            .get("value")
            .context("Missing 'value' parameter")?
            .clone();

        // Validate field and value
        Self::validate_field(field, &value)?;

        // Get table name (default to "blocks" if not specified)
        let table = row_data
            .get("table")
            .and_then(|v| v.as_string())
            .unwrap_or("blocks");

        // Execute update
        let mut updates = StorageEntity::new();
        updates.insert(field.to_string(), value);

        db.update(table, id, updates)
            .await
            .context("Failed to update field in database")?;

        Ok(())
    }
}

/// Split a block at the cursor position
///
/// Creates a new block with content after the cursor and truncates
/// the original block to content before the cursor. The new block
/// appears directly below the original block using fractional indexing.
///
/// # Requirements
/// - `ui_state.cursor_pos` must be set with `block_id` and `offset`
/// - The block must exist in the database
/// - The cursor offset must be within the content length
pub struct SplitBlock;

impl SplitBlock {
    /// Get current timestamp in milliseconds
    fn now_millis() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    }

    /// Query block data from database
    async fn get_block_data(
        db: &TursoBackend,
        block_id: &str,
    ) -> Result<(String, Option<String>, i64, String)> {
        let conn = db.get_connection()?;
        let mut stmt = conn
            .prepare("SELECT content, parent_id, depth, sort_key FROM blocks WHERE id = ?")
            .await?;
        let row = stmt.query_row((block_id,)).await?;

        // Helper to convert turso::Value to String
        let turso_value_to_string = |value: turso::Value| -> String {
            match value {
                turso::Value::Null => String::new(),
                turso::Value::Text(s) => s.to_string(),
                turso::Value::Integer(i) => i.to_string(),
                turso::Value::Float(f) => f.to_string(),
                turso::Value::Blob(_) => String::new(),
            }
        };

        // Helper to convert turso::Value to Option<String>
        let turso_value_to_option_string = |value: turso::Value| -> Option<String> {
            match value {
                turso::Value::Null => None,
                turso::Value::Text(s) => Some(s.to_string()),
                turso::Value::Integer(i) => Some(i.to_string()),
                turso::Value::Float(f) => Some(f.to_string()),
                turso::Value::Blob(_) => None,
            }
        };

        let content = turso_value_to_string(row.get_value(0)?.into());
        let parent_id = turso_value_to_option_string(row.get_value(1)?.into());

        let depth = match row.get_value(2)?.into() {
            turso::Value::Integer(i) => i,
            _ => bail!("Depth is not an integer"),
        };

        let sort_key = turso_value_to_string(row.get_value(3)?.into());

        Ok((content, parent_id, depth, sort_key))
    }

    /// Query the sort_key of the next sibling block (if any)
    ///
    /// Finds the sibling that comes immediately after the current block in sort order.
    /// Excludes the current block by ID to ensure we don't accidentally include it.
    async fn get_next_sibling_sort_key(
        db: &TursoBackend,
        parent_id: Option<&str>,
        current_block_id: &str,
        current_sort_key: &str,
    ) -> Result<Option<String>> {
        let conn = db.get_connection()?;

        // Helper to convert turso::Value to String
        let turso_value_to_string = |value: turso::Value| -> String {
            match value {
                turso::Value::Null => String::new(),
                turso::Value::Text(s) => s.to_string(),
                turso::Value::Integer(i) => i.to_string(),
                turso::Value::Float(f) => f.to_string(),
                turso::Value::Blob(_) => String::new(),
            }
        };

        // Handle NULL parent_id properly by using separate queries
        // Also exclude the current block by ID to be safe
        let result = match parent_id {
            Some(parent) => {
                let mut stmt = conn.prepare(
                    "SELECT sort_key FROM blocks
                     WHERE parent_id = ? AND id != ? AND sort_key > ?
                     ORDER BY sort_key LIMIT 1"
                ).await?;
                stmt.query_row((parent, current_block_id, current_sort_key)).await
            }
            None => {
                let mut stmt = conn.prepare(
                    "SELECT sort_key FROM blocks
                     WHERE parent_id IS NULL AND id != ? AND sort_key > ?
                     ORDER BY sort_key LIMIT 1"
                ).await?;
                stmt.query_row((current_block_id, current_sort_key)).await
            }
        };

        match result {
            Ok(row) => {
                let value = row.get_value(0)?;
                let sort_key = turso_value_to_string(value.into());
                Ok(Some(sort_key))
            }
            Err(_) => Ok(None), // No next sibling found
        }
    }
}

#[async_trait]
impl Operation for SplitBlock {
    fn name(&self) -> &str {
        "split_block"
    }

    async fn execute(
        &self,
        _row_data: &StorageEntity,
        ui_state: &UiState,
        db: &mut TursoBackend,
    ) -> Result<()> {
        // Get cursor position
        let cursor_pos = ui_state
            .cursor_pos
            .as_ref()
            .context("Cursor position not set in UI state")?;

        let block_id = &cursor_pos.block_id;
        let offset = cursor_pos.offset as usize;

        // Query current block data
        let (content, parent_id, depth, sort_key) = Self::get_block_data(db, block_id).await?;

        // Validate offset is within bounds
        if offset > content.len() {
            bail!(
                "Cursor offset {} exceeds content length {}",
                offset,
                content.len()
            );
        }

        // Split content at cursor
        let mut content_before = content[..offset].to_string();
        let mut content_after = content[offset..].to_string();

        // Strip trailing whitespace from the old block
        content_before = content_before.trim_end().to_string();

        // Strip leading whitespace from the new block
        content_after = content_after.trim_start().to_string();

        // Generate new block ID
        let new_block_id = Uuid::new_v4().to_string();

        // Generate sort_key for new block (between current block and next sibling)
        // Find the next sibling's sort_key, then generate a key between current and next
        let next_sibling_sort_key = Self::get_next_sibling_sort_key(db, parent_id.as_deref(), block_id, &sort_key).await?;
        let new_sort_key = gen_key_between(Some(&sort_key), next_sibling_sort_key.as_deref())
            .context("Failed to generate sort_key for new block")?;

        // Get current timestamp
        let now = Self::now_millis();

        // Create new block
        let mut new_block = StorageEntity::new();
        new_block.insert("id".to_string(), Value::String(new_block_id.clone()));
        new_block.insert("content".to_string(), Value::String(content_after));
        new_block.insert("parent_id".to_string(), {
            if let Some(ref pid) = parent_id {
                Value::String(pid.clone())
            } else {
                Value::Null
            }
        });
        new_block.insert("depth".to_string(), Value::Integer(depth));
        new_block.insert("sort_key".to_string(), Value::String(new_sort_key));
        new_block.insert("created_at".to_string(), Value::Integer(now));
        new_block.insert("updated_at".to_string(), Value::Integer(now));
        new_block.insert("collapsed".to_string(), Value::Boolean(false));
        new_block.insert("completed".to_string(), Value::Boolean(false));
        new_block.insert("block_type".to_string(), Value::String("text".to_string()));

        db.insert("blocks", new_block)
            .await
            .context("Failed to insert new block")?;

        // Update current block with truncated content
        let mut updates = StorageEntity::new();
        updates.insert("content".to_string(), Value::String(content_before));
        updates.insert("updated_at".to_string(), Value::Integer(now));

        db.update("blocks", block_id, updates)
            .await
            .context("Failed to update original block")?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_field_string() {
        assert!(UpdateField::validate_field("content", &Value::String("test".to_string())).is_ok());
        assert!(UpdateField::validate_field("content", &Value::Null).is_ok());
        assert!(UpdateField::validate_field("content", &Value::Integer(42)).is_err());
    }

    #[test]
    fn test_validate_field_integer() {
        assert!(UpdateField::validate_field("depth", &Value::Integer(0)).is_ok());
        assert!(UpdateField::validate_field("depth", &Value::Integer(5)).is_ok());
        assert!(UpdateField::validate_field("depth", &Value::Integer(-1)).is_err());
        assert!(UpdateField::validate_field("priority", &Value::Integer(-1)).is_ok());
    }

    #[test]
    fn test_validate_field_boolean() {
        assert!(UpdateField::validate_field("completed", &Value::Boolean(true)).is_ok());
        assert!(UpdateField::validate_field("collapsed", &Value::Boolean(false)).is_ok());
        assert!(UpdateField::validate_field("completed", &Value::String("true".to_string())).is_err());
    }

    #[test]
    fn test_validate_field_id_protected() {
        assert!(UpdateField::validate_field("id", &Value::String("new-id".to_string())).is_err());
    }

    #[test]
    fn test_validate_field_unknown_allowed() {
        // Unknown fields are allowed (with warning) for extension flexibility
        assert!(UpdateField::validate_field("custom_field", &Value::String("value".to_string())).is_ok());
    }

    #[tokio::test]
    async fn test_update_field_operation() {
        let mut db = TursoBackend::new_in_memory().await.unwrap();

        // Create test table and data
        let conn = db.get_connection().unwrap();
        conn.execute(
            "CREATE TABLE blocks (id TEXT PRIMARY KEY, content TEXT, completed BOOLEAN)",
            ()
        ).await.unwrap();

        conn.execute(
            "INSERT INTO blocks (id, content, completed) VALUES ('block-1', 'Old content', false)",
            ()
        ).await.unwrap();
        drop(conn);

        // Prepare operation parameters
        let mut params = StorageEntity::new();
        params.insert("id".to_string(), Value::String("block-1".to_string()));
        params.insert("field".to_string(), Value::String("completed".to_string()));
        params.insert("value".to_string(), Value::Boolean(true));

        let ui_state = UiState {
            cursor_pos: None,
            focused_id: None,
        };

        // Execute operation
        let op = UpdateField;
        let result = op.execute(&params, &ui_state, &mut db).await;
        assert!(result.is_ok());

        // Verify update
        let conn = db.get_connection().unwrap();
        let mut stmt = conn.prepare("SELECT completed FROM blocks WHERE id = ?").await.unwrap();
        let row = stmt.query_row(("block-1",)).await.unwrap();
        let completed: bool = row.get(0).unwrap();
        assert!(completed);
    }

    #[tokio::test]
    async fn test_update_field_validation_error() {
        let mut db = TursoBackend::new_in_memory().await.unwrap();

        let mut params = StorageEntity::new();
        params.insert("id".to_string(), Value::String("block-1".to_string()));
        params.insert("field".to_string(), Value::String("completed".to_string()));
        params.insert("value".to_string(), Value::String("not a boolean".to_string()));

        let ui_state = UiState {
            cursor_pos: None,
            focused_id: None,
        };

        let op = UpdateField;
        let result = op.execute(&params, &ui_state, &mut db).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must be a boolean"));
    }

    #[tokio::test]
    async fn test_update_field_missing_params() {
        let mut db = TursoBackend::new_in_memory().await.unwrap();

        let mut params = StorageEntity::new();
        params.insert("id".to_string(), Value::String("block-1".to_string()));
        // Missing "field" and "value"

        let ui_state = UiState {
            cursor_pos: None,
            focused_id: None,
        };

        let op = UpdateField;
        let result = op.execute(&params, &ui_state, &mut db).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Missing 'field'"));
    }

    #[tokio::test]
    async fn test_split_block_middle() {
        let mut db = TursoBackend::new_in_memory().await.unwrap();

        // Create test table and data
        let conn = db.get_connection().unwrap();
        conn.execute(
            "CREATE TABLE blocks (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                depth INTEGER NOT NULL DEFAULT 0,
                sort_key TEXT NOT NULL,
                content TEXT NOT NULL,
                collapsed INTEGER NOT NULL DEFAULT 0,
                completed INTEGER NOT NULL DEFAULT 0,
                block_type TEXT NOT NULL DEFAULT 'text',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            )",
            ()
        ).await.unwrap();

        use crate::storage::fractional_index::gen_key_between;
        let sort_key = gen_key_between(None, None).unwrap();

        conn.execute(
            "INSERT INTO blocks (id, content, depth, sort_key, created_at, updated_at)
             VALUES ('block-1', 'Hello World', 0, ?, ?, ?)",
            (sort_key.as_str(), 1000i64, 1000i64)
        ).await.unwrap();
        drop(conn);

        // Prepare UI state with cursor at position 6 (after "Hello ")
        let ui_state = UiState {
            cursor_pos: Some(crate::api::render_engine::CursorPosition {
                block_id: "block-1".to_string(),
                offset: 6,
            }),
            focused_id: Some("block-1".to_string()),
        };

        // Execute split operation
        let op = SplitBlock;
        let result = op.execute(&StorageEntity::new(), &ui_state, &mut db).await;
        assert!(result.is_ok());

        // Verify original block was truncated and trailing whitespace stripped
        let conn = db.get_connection().unwrap();
        let mut stmt = conn.prepare("SELECT content FROM blocks WHERE id = ?").await.unwrap();
        let row = stmt.query_row(("block-1",)).await.unwrap();
        let content: String = row.get(0).unwrap();
        assert_eq!(content, "Hello"); // Trailing space should be stripped

        // Verify new block was created with remaining content (leading whitespace stripped)
        let mut stmt = conn.prepare("SELECT content, sort_key FROM blocks WHERE id != 'block-1' ORDER BY sort_key").await.unwrap();
        let mut rows = stmt.query(()).await.unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let new_content: String = row.get(0).unwrap();
        assert_eq!(new_content, "World"); // Leading space should be stripped
    }

    #[tokio::test]
    async fn test_split_block_at_start() {
        let mut db = TursoBackend::new_in_memory().await.unwrap();

        let conn = db.get_connection().unwrap();
        conn.execute(
            "CREATE TABLE blocks (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                depth INTEGER NOT NULL DEFAULT 0,
                sort_key TEXT NOT NULL,
                content TEXT NOT NULL,
                collapsed INTEGER NOT NULL DEFAULT 0,
                completed INTEGER NOT NULL DEFAULT 0,
                block_type TEXT NOT NULL DEFAULT 'text',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            )",
            ()
        ).await.unwrap();

        use crate::storage::fractional_index::gen_key_between;
        let sort_key = gen_key_between(None, None).unwrap();

        conn.execute(
            "INSERT INTO blocks (id, content, depth, sort_key, created_at, updated_at)
             VALUES ('block-1', 'Hello', 0, ?, ?, ?)",
            (sort_key.as_str(), 1000i64, 1000i64)
        ).await.unwrap();
        drop(conn);

        let ui_state = UiState {
            cursor_pos: Some(crate::api::render_engine::CursorPosition {
                block_id: "block-1".to_string(),
                offset: 0,
            }),
            focused_id: Some("block-1".to_string()),
        };

        let op = SplitBlock;
        let result = op.execute(&StorageEntity::new(), &ui_state, &mut db).await;
        assert!(result.is_ok());

        // Original block should be empty
        let conn = db.get_connection().unwrap();
        let mut stmt = conn.prepare("SELECT content FROM blocks WHERE id = ?").await.unwrap();
        let row = stmt.query_row(("block-1",)).await.unwrap();
        let content: String = row.get(0).unwrap();
        assert_eq!(content, "");

        // New block should have all content
        let mut stmt = conn.prepare("SELECT content FROM blocks WHERE id != 'block-1'").await.unwrap();
        let row = stmt.query_row(()).await.unwrap();
        let new_content: String = row.get(0).unwrap();
        assert_eq!(new_content, "Hello");
    }

    #[tokio::test]
    async fn test_split_block_at_end() {
        let mut db = TursoBackend::new_in_memory().await.unwrap();

        let conn = db.get_connection().unwrap();
        conn.execute(
            "CREATE TABLE blocks (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                depth INTEGER NOT NULL DEFAULT 0,
                sort_key TEXT NOT NULL,
                content TEXT NOT NULL,
                collapsed INTEGER NOT NULL DEFAULT 0,
                completed INTEGER NOT NULL DEFAULT 0,
                block_type TEXT NOT NULL DEFAULT 'text',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            )",
            ()
        ).await.unwrap();

        use crate::storage::fractional_index::gen_key_between;
        let sort_key = gen_key_between(None, None).unwrap();

        conn.execute(
            "INSERT INTO blocks (id, content, depth, sort_key, created_at, updated_at)
             VALUES ('block-1', 'Hello', 0, ?, ?, ?)",
            (sort_key.as_str(), 1000i64, 1000i64)
        ).await.unwrap();
        drop(conn);

        let ui_state = UiState {
            cursor_pos: Some(crate::api::render_engine::CursorPosition {
                block_id: "block-1".to_string(),
                offset: 5, // At end of "Hello"
            }),
            focused_id: Some("block-1".to_string()),
        };

        let op = SplitBlock;
        let result = op.execute(&StorageEntity::new(), &ui_state, &mut db).await;
        assert!(result.is_ok());

        // Original block should keep all content
        let conn = db.get_connection().unwrap();
        let mut stmt = conn.prepare("SELECT content FROM blocks WHERE id = ?").await.unwrap();
        let row = stmt.query_row(("block-1",)).await.unwrap();
        let content: String = row.get(0).unwrap();
        assert_eq!(content, "Hello");

        // New block should be empty
        let mut stmt = conn.prepare("SELECT content FROM blocks WHERE id != 'block-1'").await.unwrap();
        let row = stmt.query_row(()).await.unwrap();
        let new_content: String = row.get(0).unwrap();
        assert_eq!(new_content, "");
    }

    #[tokio::test]
    async fn test_split_block_missing_cursor() {
        let mut db = TursoBackend::new_in_memory().await.unwrap();

        let ui_state = UiState {
            cursor_pos: None,
            focused_id: None,
        };

        let op = SplitBlock;
        let result = op.execute(&StorageEntity::new(), &ui_state, &mut db).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Cursor position not set"));
    }

    #[tokio::test]
    async fn test_split_block_invalid_offset() {
        let mut db = TursoBackend::new_in_memory().await.unwrap();

        let conn = db.get_connection().unwrap();
        conn.execute(
            "CREATE TABLE blocks (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                depth INTEGER NOT NULL DEFAULT 0,
                sort_key TEXT NOT NULL,
                content TEXT NOT NULL,
                collapsed INTEGER NOT NULL DEFAULT 0,
                completed INTEGER NOT NULL DEFAULT 0,
                block_type TEXT NOT NULL DEFAULT 'text',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            )",
            ()
        ).await.unwrap();

        use crate::storage::fractional_index::gen_key_between;
        let sort_key = gen_key_between(None, None).unwrap();

        conn.execute(
            "INSERT INTO blocks (id, content, depth, sort_key, created_at, updated_at)
             VALUES ('block-1', 'Hello', 0, ?, ?, ?)",
            (sort_key.as_str(), 1000i64, 1000i64)
        ).await.unwrap();
        drop(conn);

        let ui_state = UiState {
            cursor_pos: Some(crate::api::render_engine::CursorPosition {
                block_id: "block-1".to_string(),
                offset: 100, // Out of bounds
            }),
            focused_id: Some("block-1".to_string()),
        };

        let op = SplitBlock;
        let result = op.execute(&StorageEntity::new(), &ui_state, &mut db).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("exceeds content length"));
    }

    #[tokio::test]
    async fn test_split_block_strips_whitespace() {
        let mut db = TursoBackend::new_in_memory().await.unwrap();

        let conn = db.get_connection().unwrap();
        conn.execute(
            "CREATE TABLE blocks (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                depth INTEGER NOT NULL DEFAULT 0,
                sort_key TEXT NOT NULL,
                content TEXT NOT NULL,
                collapsed INTEGER NOT NULL DEFAULT 0,
                completed INTEGER NOT NULL DEFAULT 0,
                block_type TEXT NOT NULL DEFAULT 'text',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            )",
            ()
        ).await.unwrap();

        use crate::storage::fractional_index::gen_key_between;
        let sort_key = gen_key_between(None, None).unwrap();

        // Test with content that has whitespace around the split point
        conn.execute(
            "INSERT INTO blocks (id, content, depth, sort_key, created_at, updated_at)
             VALUES ('block-1', 'Hello   World', 0, ?, ?, ?)",
            (sort_key.as_str(), 1000i64, 1000i64)
        ).await.unwrap();
        drop(conn);

        // Split at position 8 (after "Hello  " - 2 spaces)
        let ui_state = UiState {
            cursor_pos: Some(crate::api::render_engine::CursorPosition {
                block_id: "block-1".to_string(),
                offset: 8,
            }),
            focused_id: Some("block-1".to_string()),
        };

        let op = SplitBlock;
        let result = op.execute(&StorageEntity::new(), &ui_state, &mut db).await;
        assert!(result.is_ok());

        // Verify original block has trailing whitespace stripped
        let conn = db.get_connection().unwrap();
        let mut stmt = conn.prepare("SELECT content FROM blocks WHERE id = ?").await.unwrap();
        let row = stmt.query_row(("block-1",)).await.unwrap();
        let content: String = row.get(0).unwrap();
        assert_eq!(content, "Hello"); // Trailing spaces should be stripped

        // Verify new block has leading whitespace stripped
        let mut stmt = conn.prepare("SELECT content FROM blocks WHERE id != 'block-1'").await.unwrap();
        let row = stmt.query_row(()).await.unwrap();
        let new_content: String = row.get(0).unwrap();
        assert_eq!(new_content, "World"); // Leading spaces should be stripped
    }

    #[tokio::test]
    async fn test_split_block_with_multiple_siblings() {
        let mut db = TursoBackend::new_in_memory().await.unwrap();

        let conn = db.get_connection().unwrap();
        conn.execute(
            "CREATE TABLE blocks (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                depth INTEGER NOT NULL DEFAULT 0,
                sort_key TEXT NOT NULL,
                content TEXT NOT NULL,
                collapsed INTEGER NOT NULL DEFAULT 0,
                completed INTEGER NOT NULL DEFAULT 0,
                block_type TEXT NOT NULL DEFAULT 'text',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            )",
            ()
        ).await.unwrap();

        use crate::storage::fractional_index::gen_key_between;

        // Create three blocks: A, B, C
        let key_a = gen_key_between(None, None).unwrap();
        let key_c = gen_key_between(Some(&key_a), None).unwrap();
        let key_b = gen_key_between(Some(&key_a), Some(&key_c)).unwrap();

        conn.execute(
            "INSERT INTO blocks (id, content, depth, sort_key, created_at, updated_at)
             VALUES ('block-a', 'Split me', 0, ?, ?, ?)",
            (key_a.as_str(), 1000i64, 1000i64)
        ).await.unwrap();

        conn.execute(
            "INSERT INTO blocks (id, content, depth, sort_key, created_at, updated_at)
             VALUES ('block-b', 'Next sibling', 0, ?, ?, ?)",
            (key_b.as_str(), 1000i64, 1000i64)
        ).await.unwrap();

        conn.execute(
            "INSERT INTO blocks (id, content, depth, sort_key, created_at, updated_at)
             VALUES ('block-c', 'After next', 0, ?, ?, ?)",
            (key_c.as_str(), 1000i64, 1000i64)
        ).await.unwrap();
        drop(conn);

        // Split block-a at position 5 (after "Split")
        let ui_state = UiState {
            cursor_pos: Some(crate::api::render_engine::CursorPosition {
                block_id: "block-a".to_string(),
                offset: 5,
            }),
            focused_id: Some("block-a".to_string()),
        };

        let op = SplitBlock;
        let result = op.execute(&StorageEntity::new(), &ui_state, &mut db).await;
        assert!(result.is_ok());

        // Verify the new block appears between block-a and block-b
        let conn = db.get_connection().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, sort_key FROM blocks ORDER BY sort_key"
        ).await.unwrap();
        let mut rows = stmt.query(()).await.unwrap();

        let mut blocks_with_keys: Vec<(String, String)> = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            let id: String = row.get(0).unwrap();
            let sort_key: String = row.get(1).unwrap();
            blocks_with_keys.push((id, sort_key));
        }

        // Should be: block-a (truncated), new block, block-b, block-c
        // Find the new block (not block-a, block-b, or block-c)
        let (new_block_id, key_new) = blocks_with_keys.iter()
            .find(|(id, _)| *id != "block-a" && *id != "block-b" && *id != "block-c")
            .unwrap();

        // Get sort keys for known blocks
        let (_, key_a_after) = blocks_with_keys.iter()
            .find(|(id, _)| *id == "block-a")
            .unwrap();

        let (_, key_b) = blocks_with_keys.iter()
            .find(|(id, _)| *id == "block-b")
            .unwrap();

        // Verify ordering: block-a < new block < block-b
        assert!(key_a_after < key_new, "New block should come after block-a (sort_key comparison)");
        assert!(key_new < key_b, "New block should come before block-b (sort_key comparison)");

        // Also verify the new block is not block-c (which should be after block-b)
        let (_, key_c) = blocks_with_keys.iter()
            .find(|(id, _)| *id == "block-c")
            .unwrap();
        assert!(key_new < key_c, "New block should come before block-c");

        // Verify the new block content
        let mut stmt = conn.prepare("SELECT content FROM blocks WHERE id = ?").await.unwrap();
        let row = stmt.query_row((new_block_id.as_str(),)).await.unwrap();
        let new_content: String = row.get(0).unwrap();
        assert_eq!(new_content, "me"); // Content after "Split" (position 5)
    }

}
