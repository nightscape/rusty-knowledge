use std::sync::Arc;
use std::collections::HashMap;

use crate::core::datasource::{DataSource, CrudOperationProvider};
use crate::core::traits::{Queryable, Result};
use crate::core::{AlwaysTrue, Predicate, QueryableCache, UnifiedQuery};
use crate::storage::types::Value;
use crate::integrations::todoist::TodoistTask;
use crate::storage::task_datasource::InMemoryTaskStore;
use crate::tasks::Task;

#[derive(Clone, Debug)]
pub struct UnifiedTask {
    pub id: String,
    pub title: String,
    pub completed: bool,
    pub source: TaskSource,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TaskSource {
    Internal,
    Todoist,
}

impl From<&Task> for UnifiedTask {
    fn from(task: &Task) -> Self {
        UnifiedTask {
            id: task.id.clone(),
            title: task.title.clone(),
            completed: task.completed,
            source: TaskSource::Internal,
        }
    }
}

impl From<&TodoistTask> for UnifiedTask {
    fn from(task: &TodoistTask) -> Self {
        UnifiedTask {
            id: task.id.clone(),
            title: task.content.clone(),
            completed: task.completed,
            source: TaskSource::Todoist,
        }
    }
}

#[derive(Clone)]
struct UnifiedCompletedPredicate {
    completed: bool,
}

impl Predicate<UnifiedTask> for UnifiedCompletedPredicate {
    fn test(&self, item: &UnifiedTask) -> bool {
        item.completed == self.completed
    }

    fn to_sql(&self) -> Option<crate::core::traits::SqlPredicate> {
        None
    }
}

#[derive(Clone)]
struct UnifiedTitlePredicate {
    title: String,
}

impl Predicate<UnifiedTask> for UnifiedTitlePredicate {
    fn test(&self, item: &UnifiedTask) -> bool {
        item.title.contains(&self.title)
    }

    fn to_sql(&self) -> Option<crate::core::traits::SqlPredicate> {
        None
    }
}

struct TaskProjection<Q> {
    queryable: Arc<Q>,
}

impl<Q> TaskProjection<Q>
where
    Q: crate::core::Queryable<Task> + Send + Sync + 'static,
{
    fn new(queryable: Arc<Q>) -> Self {
        Self { queryable }
    }
}

#[async_trait::async_trait]
impl<Q> Queryable<UnifiedTask> for TaskProjection<Q>
where
    Q: Queryable<Task> + Send + Sync + 'static,
{
    async fn query<P>(&self, predicate: P) -> Result<Vec<UnifiedTask>>
    where
        P: Predicate<UnifiedTask> + Send + 'static,
    {
        let all_tasks = self.queryable.query(AlwaysTrue).await?;

        Ok(all_tasks
            .iter()
            .map(UnifiedTask::from)
            .filter(|t| predicate.test(t))
            .collect())
    }
}

struct TodoistProjection<Q> {
    queryable: Arc<Q>,
}

impl<Q> TodoistProjection<Q>
where
    Q: crate::core::Queryable<TodoistTask> + Send + Sync + 'static,
{
    fn new(queryable: Arc<Q>) -> Self {
        Self { queryable }
    }
}

#[async_trait::async_trait]
impl<Q> Queryable<UnifiedTask> for TodoistProjection<Q>
where
    Q: Queryable<TodoistTask> + Send + Sync + 'static,
{
    async fn query<P>(&self, predicate: P) -> Result<Vec<UnifiedTask>>
    where
        P: Predicate<UnifiedTask> + Send + 'static,
    {
        let all_tasks = self.queryable.query(AlwaysTrue).await?;

        Ok(all_tasks
            .iter()
            .map(UnifiedTask::from)
            .filter(|t| predicate.test(t))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_unified_query_across_sources() {
        let internal_store = InMemoryTaskStore::new();

        let mut fields1 = HashMap::new();
        fields1.insert("id".to_string(), Value::String("1".to_string()));
        fields1.insert("title".to_string(), Value::String("Internal Task 1".to_string()));
        fields1.insert("completed".to_string(), Value::Boolean(false));
        internal_store.create(fields1).await.unwrap();

        let mut fields2 = HashMap::new();
        fields2.insert("id".to_string(), Value::String("2".to_string()));
        fields2.insert("title".to_string(), Value::String("Internal Task 2".to_string()));
        fields2.insert("completed".to_string(), Value::Boolean(true));
        internal_store.create(fields2).await.unwrap();

        let internal_cache = Arc::new(
            QueryableCache::with_database(internal_store, ":memory:")
                .await
                .unwrap(),
        );
        internal_cache.sync().await.unwrap();

        let task_projection = TaskProjection::new(internal_cache);

        let unified: UnifiedQuery<UnifiedTask> = UnifiedQuery::new().add_source(task_projection);

        let completed_tasks = unified
            .query(UnifiedCompletedPredicate { completed: true })
            .await
            .unwrap();

        assert_eq!(completed_tasks.len(), 1);
        assert_eq!(completed_tasks[0].id, "2");
        assert_eq!(completed_tasks[0].source, TaskSource::Internal);
    }

    #[tokio::test]
    async fn test_unified_query_with_title_search() {
        let internal_store = InMemoryTaskStore::new();

        let mut fields1 = HashMap::new();
        fields1.insert("id".to_string(), Value::String("1".to_string()));
        fields1.insert("title".to_string(), Value::String("Buy groceries".to_string()));
        fields1.insert("completed".to_string(), Value::Boolean(false));
        internal_store.create(fields1).await.unwrap();

        let mut fields2 = HashMap::new();
        fields2.insert("id".to_string(), Value::String("2".to_string()));
        fields2.insert("title".to_string(), Value::String("Write report".to_string()));
        fields2.insert("completed".to_string(), Value::Boolean(false));
        internal_store.create(fields2).await.unwrap();

        let internal_cache = Arc::new(
            QueryableCache::with_database(internal_store, ":memory:")
                .await
                .unwrap(),
        );
        internal_cache.sync().await.unwrap();

        let task_projection = TaskProjection::new(internal_cache);

        let unified: UnifiedQuery<UnifiedTask> = UnifiedQuery::new().add_source(task_projection);

        let tasks = unified
            .query(UnifiedTitlePredicate {
                title: "Buy".to_string(),
            })
            .await
            .unwrap();

        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Buy groceries");
    }

    #[tokio::test]
    async fn test_unified_query_dedup() {
        let internal_store1 = InMemoryTaskStore::new();

        let mut fields1 = HashMap::new();
        fields1.insert("id".to_string(), Value::String("shared-1".to_string()));
        fields1.insert("title".to_string(), Value::String("Shared Task".to_string()));
        fields1.insert("completed".to_string(), Value::Boolean(false));
        internal_store1.create(fields1).await.unwrap();

        let internal_store2 = InMemoryTaskStore::new();

        let mut fields2 = HashMap::new();
        fields2.insert("id".to_string(), Value::String("shared-1".to_string()));
        fields2.insert("title".to_string(), Value::String("Shared Task (duplicate)".to_string()));
        fields2.insert("completed".to_string(), Value::Boolean(false));
        internal_store2.create(fields2).await.unwrap();

        let mut fields3 = HashMap::new();
        fields3.insert("id".to_string(), Value::String("unique-2".to_string()));
        fields3.insert("title".to_string(), Value::String("Unique Task".to_string()));
        fields3.insert("completed".to_string(), Value::Boolean(false));
        internal_store2.create(fields3).await.unwrap();

        let cache1 = Arc::new(
            QueryableCache::with_database(internal_store1, ":memory:")
                .await
                .unwrap(),
        );
        cache1.sync().await.unwrap();

        let cache2 = Arc::new(
            QueryableCache::with_database(internal_store2, ":memory:")
                .await
                .unwrap(),
        );
        cache2.sync().await.unwrap();

        let projection1 = TaskProjection::new(cache1);
        let projection2 = TaskProjection::new(cache2);

        let unified: UnifiedQuery<UnifiedTask> = UnifiedQuery::new()
            .add_source(projection1)
            .add_source(projection2)
            .with_dedup(|task| task.id.clone());

        let tasks = unified
            .query(UnifiedCompletedPredicate { completed: false })
            .await
            .unwrap();

        assert_eq!(tasks.len(), 2);
        let ids: Vec<String> = tasks.iter().map(|t| t.id.clone()).collect();
        assert!(ids.contains(&"shared-1".to_string()));
        assert!(ids.contains(&"unique-2".to_string()));
    }
}
