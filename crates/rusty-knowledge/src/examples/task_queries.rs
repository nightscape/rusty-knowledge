use crate::core::predicate::Eq;
use crate::core::queryable_cache::QueryableCache;
use crate::core::traits::{DataSource, Queryable, Result};
use crate::storage::InMemoryTaskStore;
use crate::tasks::{CompletedLens, Task, TitleLens};

pub async fn setup_queryable_task_cache() -> Result<QueryableCache<InMemoryTaskStore, Task>> {
    let store = InMemoryTaskStore::new();

    let task1 = Task::new("High priority task".to_string(), None);
    let task2 = Task::new("Low priority task".to_string(), None);
    let mut task3 = Task::new("Completed task".to_string(), None);
    task3.completed = true;

    store.insert(task1).await?;
    store.insert(task2).await?;
    store.insert(task3).await?;

    let cache = QueryableCache::with_database(store, ":memory:").await?;
    cache.sync().await?;

    Ok(cache)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_query_all_tasks() {
        let cache = setup_queryable_task_cache().await.unwrap();

        let all_tasks = cache.get_all().await.unwrap();
        assert_eq!(all_tasks.len(), 3);
    }

    #[tokio::test]
    async fn test_query_completed_tasks() {
        let cache = setup_queryable_task_cache().await.unwrap();

        let completed_pred = Eq::new(CompletedLens, true);
        let completed_tasks = cache.query(completed_pred).await.unwrap();

        assert_eq!(completed_tasks.len(), 1);
        assert!(completed_tasks[0].completed);
    }

    #[tokio::test]
    async fn test_query_incomplete_tasks() {
        let cache = setup_queryable_task_cache().await.unwrap();

        let incomplete_pred = Eq::new(CompletedLens, false);
        let incomplete_tasks = cache.query(incomplete_pred).await.unwrap();

        assert_eq!(incomplete_tasks.len(), 2);
        assert!(incomplete_tasks.iter().all(|t| !t.completed));
    }

    #[tokio::test]
    async fn test_query_by_title() {
        let cache = setup_queryable_task_cache().await.unwrap();

        let title_pred = Eq::new(TitleLens, "High priority task".to_string());
        let tasks = cache.query(title_pred).await.unwrap();

        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "High priority task");
    }

    #[tokio::test]
    async fn test_combined_query() {
        let cache = setup_queryable_task_cache().await.unwrap();

        let incomplete = Eq::new(CompletedLens, false);
        let not_completed = incomplete.clone();

        let results = cache.query(not_completed).await.unwrap();
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn test_cache_insert_and_query() {
        let cache = setup_queryable_task_cache().await.unwrap();

        let new_task = Task::new("Newly added task".to_string(), None);
        cache.insert(new_task).await.unwrap();

        let title_pred = Eq::new(TitleLens, "Newly added task".to_string());
        let found = cache.query(title_pred).await.unwrap();

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].title, "Newly added task");
    }

    #[tokio::test]
    async fn test_cache_update_and_query() {
        let cache = setup_queryable_task_cache().await.unwrap();

        let all_tasks = cache.get_all().await.unwrap();
        let first_task = &all_tasks[0];
        let id = first_task.id.clone();

        let mut updated = first_task.clone();
        updated.title = "Updated title".to_string();
        cache.update(&id, updated).await.unwrap();

        let title_pred = Eq::new(TitleLens, "Updated title".to_string());
        let found = cache.query(title_pred).await.unwrap();

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, id);
    }

    #[tokio::test]
    async fn test_cache_delete_and_query() {
        let cache = setup_queryable_task_cache().await.unwrap();

        let all_tasks = cache.get_all().await.unwrap();
        let initial_count = all_tasks.len();
        let id_to_delete = all_tasks[0].id.clone();

        cache.delete(&id_to_delete).await.unwrap();

        let all_after = cache.get_all().await.unwrap();
        assert_eq!(all_after.len(), initial_count - 1);

        let found = cache.get_by_id(&id_to_delete).await.unwrap();
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn test_query_with_complex_predicate() {
        let store = InMemoryTaskStore::new();

        let mut task1 = Task::new("Task 1".to_string(), None);
        task1.completed = true;

        let task2 = Task::new("Task 2".to_string(), None);

        let mut task3 = Task::new("Task 3".to_string(), None);
        task3.completed = true;

        store.insert(task1).await.unwrap();
        store.insert(task2).await.unwrap();
        store.insert(task3).await.unwrap();

        let cache = QueryableCache::with_database(store, ":memory:")
            .await
            .unwrap();
        cache.sync().await.unwrap();

        let is_completed = Eq::new(CompletedLens, true);
        let completed_tasks = cache.query(is_completed).await.unwrap();

        assert_eq!(completed_tasks.len(), 2);
        assert!(completed_tasks.iter().all(|t| t.completed));
    }
}
