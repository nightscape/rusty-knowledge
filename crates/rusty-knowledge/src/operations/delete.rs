use anyhow::{Context, Result};
use async_trait::async_trait;

use super::{Operation, RowView};
use crate::storage::types::StorageEntity;
use crate::storage::turso::TursoBackend;
use crate::storage::backend::StorageBackend;
use crate::api::render_engine::UiState;

/// Delete an entity
///
/// Simple deletion operation that removes an entity from its table.
/// Future enhancements may include:
/// - Cascading deletes for hierarchical structures
/// - Soft deletes (mark as deleted instead of removing)
/// - Archive functionality
///
/// # Parameters
/// - `id` (required): Entity ID to delete
/// - `table` (optional): Table name, defaults to "blocks"
pub struct Delete;

impl Delete {
    /// Validate deletion preconditions
    ///
    /// Currently allows all deletions. Future validation may include:
    /// - Check if entity has children (prevent orphans)
    /// - Check if entity is referenced by other entities
    /// - Permission checks
    fn validate_delete(_row_data: &StorageEntity) -> Result<()> {
        // Basic validation: ensure we have an ID
        let view = RowView::new(_row_data);
        view.id()?;
        Ok(())
    }
}

#[async_trait]
impl Operation for Delete {
    fn name(&self) -> &str {
        "delete"
    }

    async fn execute(
        &self,
        row_data: &StorageEntity,
        _ui_state: &UiState,
        db: &mut TursoBackend,
    ) -> Result<()> {
        // Validate preconditions
        Self::validate_delete(row_data)?;

        let view = RowView::new(row_data);
        let id = view.id()?;

        // Get table name (default to "blocks" if not specified)
        let table = row_data
            .get("table")
            .and_then(|v| v.as_string())
            .unwrap_or("blocks");

        // Execute deletion
        db.delete(table, id).await
            .with_context(|| format!("Failed to delete entity {} from table {}", id, table))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::types::Value;

    #[test]
    fn test_validate_delete() {
        let mut entity = StorageEntity::new();
        entity.insert("id".to_string(), Value::String("block-1".to_string()));

        assert!(Delete::validate_delete(&entity).is_ok());
    }

    #[test]
    fn test_validate_delete_missing_id() {
        let entity = StorageEntity::new();
        assert!(Delete::validate_delete(&entity).is_err());
    }

    #[tokio::test]
    async fn test_delete_operation() {
        let mut db = TursoBackend::new_in_memory().await.unwrap();

        // Create test table and data
        let conn = db.get_connection().unwrap();
        conn.execute(
            "CREATE TABLE blocks (id TEXT PRIMARY KEY, content TEXT)",
            ()
        ).await.unwrap();

        conn.execute(
            "INSERT INTO blocks (id, content) VALUES ('block-1', 'Test content')",
            ()
        ).await.unwrap();
        drop(conn);

        // Verify block exists
        let conn = db.get_connection().unwrap();
        let mut stmt = conn.prepare("SELECT COUNT(*) FROM blocks WHERE id = ?").await.unwrap();
        let row = stmt.query_row(("block-1",)).await.unwrap();
        let count: i64 = row.get(0).unwrap();
        assert_eq!(count, 1);
        drop(stmt);
        drop(conn);

        // Prepare delete parameters
        let mut params = StorageEntity::new();
        params.insert("id".to_string(), Value::String("block-1".to_string()));

        let ui_state = UiState {
            cursor_pos: None,
            focused_id: None,
        };

        // Execute delete operation
        let op = Delete;
        let result = op.execute(&params, &ui_state, &mut db).await;
        assert!(result.is_ok());

        // Verify block was deleted
        let conn = db.get_connection().unwrap();
        let mut stmt = conn.prepare("SELECT COUNT(*) FROM blocks WHERE id = ?").await.unwrap();
        let row = stmt.query_row(("block-1",)).await.unwrap();
        let count: i64 = row.get(0).unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_delete_nonexistent() {
        let mut db = TursoBackend::new_in_memory().await.unwrap();

        // Create test table (but no data)
        let conn = db.get_connection().unwrap();
        conn.execute(
            "CREATE TABLE blocks (id TEXT PRIMARY KEY, content TEXT)",
            ()
        ).await.unwrap();
        drop(conn);

        let mut params = StorageEntity::new();
        params.insert("id".to_string(), Value::String("nonexistent".to_string()));

        let ui_state = UiState {
            cursor_pos: None,
            focused_id: None,
        };

        let op = Delete;
        let result = op.execute(&params, &ui_state, &mut db).await;
        // Should succeed (deleting nonexistent is idempotent)
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_delete_custom_table() {
        let mut db = TursoBackend::new_in_memory().await.unwrap();

        // Create custom table
        let conn = db.get_connection().unwrap();
        conn.execute(
            "CREATE TABLE tasks (id TEXT PRIMARY KEY, title TEXT)",
            ()
        ).await.unwrap();

        conn.execute(
            "INSERT INTO tasks (id, title) VALUES ('task-1', 'Test task')",
            ()
        ).await.unwrap();
        drop(conn);

        // Delete from custom table
        let mut params = StorageEntity::new();
        params.insert("id".to_string(), Value::String("task-1".to_string()));
        params.insert("table".to_string(), Value::String("tasks".to_string()));

        let ui_state = UiState {
            cursor_pos: None,
            focused_id: None,
        };

        let op = Delete;
        let result = op.execute(&params, &ui_state, &mut db).await;
        assert!(result.is_ok());

        // Verify deletion
        let conn = db.get_connection().unwrap();
        let mut stmt = conn.prepare("SELECT COUNT(*) FROM tasks WHERE id = ?").await.unwrap();
        let row = stmt.query_row(("task-1",)).await.unwrap();
        let count: i64 = row.get(0).unwrap();
        assert_eq!(count, 0);
    }
}
