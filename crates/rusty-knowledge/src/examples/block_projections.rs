use crate::core::{
    AlwaysTrue, Block, BlockAdapter, Blocklike, DataSource, Eq, Queryable, QueryableCache,
};
use crate::storage::task_datasource::InMemoryTaskStore;
use crate::tasks::Task;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_task_block_projection() {
        let store = InMemoryTaskStore::new();
        let task1 = Task::new("Write docs".to_string(), None);
        let mut task2 = Task::new("Review PR".to_string(), None);
        task2.completed = true;

        store.insert(task1.clone()).await.unwrap();
        store.insert(task2.clone()).await.unwrap();

        let cache = QueryableCache::with_database(store, ":memory:")
            .await
            .expect("Failed to create cache");
        cache.sync().await.expect("Failed to sync");

        let adapter = BlockAdapter::new(cache);

        let all_blocks = adapter.query(AlwaysTrue).await.unwrap();
        assert_eq!(all_blocks.len(), 2);
        assert_eq!(all_blocks[0].source, "internal");
        assert_eq!(all_blocks[1].source, "internal");
    }

    #[tokio::test]
    async fn test_block_query_completed() {
        let store = InMemoryTaskStore::new();
        let mut task1 = Task::new("Task 1".to_string(), None);
        let mut task2 = Task::new("Task 2".to_string(), None);
        let task3 = Task::new("Task 3".to_string(), None);

        task1.completed = true;
        task2.completed = true;

        store.insert(task1).await.unwrap();
        store.insert(task2).await.unwrap();
        store.insert(task3).await.unwrap();

        let cache = QueryableCache::with_database(store, ":memory:")
            .await
            .expect("Failed to create cache");
        cache.sync().await.expect("Failed to sync");

        let adapter = BlockAdapter::new(cache);

        let completed_predicate = Eq::new(crate::core::projections::block::CompletedLens, true);
        let completed_blocks = adapter.query(completed_predicate).await.unwrap();

        assert_eq!(completed_blocks.len(), 2);
        assert!(completed_blocks.iter().all(|b| b.completed));
    }

    #[tokio::test]
    async fn test_block_roundtrip_conversion() {
        let original_task = Task::new("Original Task".to_string(), None);
        let block = original_task.to_block();

        assert_eq!(block.title, "Original Task");
        assert!(!block.completed);
        assert_eq!(block.source, "internal");

        let reconstructed = Task::from_block(&block).unwrap();
        assert_eq!(reconstructed.title, "Original Task");
        assert!(!reconstructed.completed);
    }

    #[test]
    fn test_block_rejects_wrong_source() {
        let block = Block {
            id: "1".to_string(),
            title: "External".to_string(),
            content: None,
            completed: false,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            source: "todoist".to_string(),
            source_id: "ext-123".to_string(),
            tags: None,
        };

        let task = Task::from_block(&block);
        assert!(task.is_none());
    }

    #[tokio::test]
    async fn test_unified_block_view_tasks_only() {
        let store = InMemoryTaskStore::new();
        let task1 = Task::new("Local Task 1".to_string(), None);
        let task2 = Task::new("Local Task 2".to_string(), None);

        store.insert(task1).await.unwrap();
        store.insert(task2).await.unwrap();

        let cache = QueryableCache::with_database(store, ":memory:")
            .await
            .expect("Failed to create cache");
        cache.sync().await.expect("Failed to sync");

        let adapter = BlockAdapter::new(cache);

        let all_blocks = adapter.query(AlwaysTrue).await.unwrap();
        assert_eq!(all_blocks.len(), 2);
        assert!(all_blocks.iter().all(|b| b.source == "internal"));
    }

    #[test]
    fn test_block_schema() {
        use crate::core::HasSchema;

        let schema = Block::schema();
        assert_eq!(schema.table_name, "blocks");

        let field_names: Vec<&str> = schema.fields.iter().map(|f| f.name.as_str()).collect();
        assert!(field_names.contains(&"id"));
        assert!(field_names.contains(&"title"));
        assert!(field_names.contains(&"content"));
        assert!(field_names.contains(&"completed"));
        assert!(field_names.contains(&"source"));
        assert!(field_names.contains(&"source_id"));
    }

    #[test]
    fn test_block_lenses() {
        use crate::core::Lens;

        let block = Block {
            id: "test-1".to_string(),
            title: "Test Block".to_string(),
            content: Some("Content here".to_string()),
            completed: false,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            source: "internal".to_string(),
            source_id: "task-123".to_string(),
            tags: Some("tag1,tag2".to_string()),
        };

        let id_lens = crate::core::projections::block::IdLens;
        assert_eq!(id_lens.get(&block), Some("test-1".to_string()));

        let title_lens = crate::core::projections::block::TitleLens;
        assert_eq!(title_lens.get(&block), Some("Test Block".to_string()));

        let source_lens = crate::core::projections::block::SourceLens;
        assert_eq!(source_lens.get(&block), Some("internal".to_string()));
    }

    #[tokio::test]
    async fn test_block_predicate_translation() {
        let store = InMemoryTaskStore::new();
        let mut task = Task::new("Completed Task".to_string(), None);
        task.completed = true;

        store.insert(task).await.unwrap();

        let cache = QueryableCache::with_database(store, ":memory:")
            .await
            .expect("Failed to create cache");
        cache.sync().await.expect("Failed to sync");

        let adapter = BlockAdapter::new(cache);

        let predicate = Eq::new(crate::core::projections::block::CompletedLens, true);

        let results = adapter.query(predicate).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Completed Task");
        assert!(results[0].completed);
    }
}
