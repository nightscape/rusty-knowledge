use crate::core::{Blocklike, Predicate, Queryable};
use async_trait::async_trait;

pub struct BlockAdapter<T, C>
where
    T: Blocklike + Send + Sync + 'static,
    C: Queryable<T> + Send + Sync,
{
    cache: C,
    _phantom: std::marker::PhantomData<T>,
}

impl<T, C> BlockAdapter<T, C>
where
    T: Blocklike + Send + Sync + 'static,
    C: Queryable<T> + Send + Sync,
{
    pub fn new(cache: C) -> Self {
        Self {
            cache,
            _phantom: std::marker::PhantomData,
        }
    }
}

#[async_trait]
impl<T, C> Queryable<super::Block> for BlockAdapter<T, C>
where
    T: Blocklike + Send + Sync + 'static,
    C: Queryable<T> + Send + Sync,
{
    async fn query<P>(
        &self,
        predicate: P,
    ) -> Result<Vec<super::Block>, Box<dyn std::error::Error + Send + Sync>>
    where
        P: Predicate<super::Block> + Send + 'static,
    {
        struct AdaptedPredicate<P, T> {
            predicate: P,
            _phantom: std::marker::PhantomData<T>,
        }

        impl<P, T> Predicate<T> for AdaptedPredicate<P, T>
        where
            P: Predicate<super::Block>,
            T: Blocklike,
        {
            fn test(&self, item: &T) -> bool {
                let block = item.to_block();
                self.predicate.test(&block)
            }

            fn to_sql(&self) -> Option<crate::core::SqlPredicate> {
                None
            }
        }

        let adapted = AdaptedPredicate {
            predicate,
            _phantom: std::marker::PhantomData,
        };

        let items = self.cache.query(adapted).await?;
        Ok(items.iter().map(|item| item.to_block()).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{AlwaysTrue, Blocklike, DataSource, Eq, Queryable, QueryableCache};
    use crate::storage::task_datasource::InMemoryTaskStore;
    use crate::tasks::Task;

    #[tokio::test]
    async fn test_block_adapter_query() {
        let store = InMemoryTaskStore::new();
        let task1 = Task::new("Test Task 1".to_string(), None);
        let task2 = Task::new("Test Task 2".to_string(), None);

        store.insert(task1.clone()).await.unwrap();
        store.insert(task2.clone()).await.unwrap();

        let cache = QueryableCache::with_database(store, ":memory:")
            .await
            .expect("Failed to create cache");
        cache.sync().await.expect("Failed to sync");

        let adapter = BlockAdapter::new(cache);

        let results = adapter.query(AlwaysTrue).await.unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().any(|b| b.title == "Test Task 1"));
        assert!(results.iter().any(|b| b.title == "Test Task 2"));
        assert!(results.iter().all(|b| b.source == "internal"));
    }

    #[tokio::test]
    async fn test_block_adapter_with_predicate() {
        let store = InMemoryTaskStore::new();
        let task1 = Task::new("Incomplete Task".to_string(), None);
        let mut task2 = Task::new("Complete Task".to_string(), None);
        task2.completed = true;

        store.insert(task1.clone()).await.unwrap();
        store.insert(task2.clone()).await.unwrap();

        let cache = QueryableCache::with_database(store, ":memory:")
            .await
            .expect("Failed to create cache");
        cache.sync().await.expect("Failed to sync");

        let adapter = BlockAdapter::new(cache);

        let predicate = Eq::new(crate::core::projections::block::CompletedLens, true);
        let results = adapter.query(predicate).await.unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Complete Task");
        assert!(results[0].completed);
    }

    #[test]
    fn test_task_blocklike_conversion() {
        let task = Task::new("Test".to_string(), None);
        let block = task.to_block();

        assert_eq!(block.title, "Test");
        assert!(!block.completed);
        assert_eq!(block.source, "internal");

        let task_back = Task::from_block(&block).unwrap();
        assert_eq!(task_back.title, "Test");
        assert!(!task_back.completed);
    }
}
