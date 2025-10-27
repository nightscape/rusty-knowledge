use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::adapter::sync_stats::SyncStats;
use crate::storage::{
    backend::StorageBackend,
    schema::EntitySchema,
    types::{Entity, Result, StorageError},
};

pub trait ApiEntity {
    fn get_id(&self) -> String;
    fn get_version(&self) -> String;
}

#[async_trait]
pub trait ApiClient<T: ApiEntity>: Send + Sync {
    async fn fetch_all(&self) -> Result<Vec<T>>;

    async fn fetch_one(&self, id: &str) -> Result<T>;

    async fn update(&self, id: &str, item: T) -> Result<T>;

    async fn create(&self, item: T) -> Result<T>;

    async fn delete(&self, id: &str) -> Result<()>;

    fn is_conflict_error(&self, error: &StorageError) -> bool {
        matches!(error, StorageError::BackendError(msg) if msg.contains("conflict"))
    }
}

pub struct ExternalSystemAdapter<B, T>
where
    B: StorageBackend + Send + Sync,
    T: ApiEntity + Send + Sync,
{
    storage: Arc<Mutex<B>>,
    api_client: Arc<dyn ApiClient<T>>,
    #[allow(dead_code)]
    schema: EntitySchema,
    entity_name: String,
    api_to_entity: Box<dyn Fn(T) -> Result<Entity> + Send + Sync>,
    entity_to_api: Box<dyn Fn(Entity) -> Result<T> + Send + Sync>,
}

impl<B, T> ExternalSystemAdapter<B, T>
where
    B: StorageBackend + Send + Sync + 'static,
    T: ApiEntity + Send + Sync + 'static,
{
    pub fn new(
        storage: Arc<Mutex<B>>,
        api_client: Arc<dyn ApiClient<T>>,
        schema: EntitySchema,
        api_to_entity: Box<dyn Fn(T) -> Result<Entity> + Send + Sync>,
        entity_to_api: Box<dyn Fn(Entity) -> Result<T> + Send + Sync>,
    ) -> Self {
        let entity_name = schema.name.clone();
        Self {
            storage,
            api_client,
            schema,
            entity_name,
            api_to_entity,
            entity_to_api,
        }
    }

    pub async fn sync_from_remote(&mut self) -> Result<SyncStats> {
        let mut stats = SyncStats::default();
        let remote_items = self.api_client.fetch_all().await?;
        let mut storage = self.storage.lock().await;

        for remote_item in remote_items {
            let id = remote_item.get_id();
            let version = remote_item.get_version();

            match storage.get(&self.entity_name, &id).await? {
                Some(_existing) => {
                    let entity = (self.api_to_entity)(remote_item)?;
                    storage.update(&self.entity_name, &id, entity).await?;
                    storage.set_version(&self.entity_name, &id, version).await?;
                    stats.updated += 1;
                }
                None => {
                    let entity = (self.api_to_entity)(remote_item)?;
                    storage.insert(&self.entity_name, entity).await?;
                    storage.set_version(&self.entity_name, &id, version).await?;
                    stats.inserted += 1;
                }
            }
        }

        Ok(stats)
    }

    pub async fn sync_to_remote(&mut self) -> Result<SyncStats> {
        let mut stats = SyncStats::default();
        let mut storage = self.storage.lock().await;
        let dirty_ids = storage.get_dirty(&self.entity_name).await?;

        for id in dirty_ids {
            if let Some(entity) = storage.get(&self.entity_name, &id).await? {
                let api_item = (self.entity_to_api)(entity)?;

                match self.api_client.update(&id, api_item).await {
                    Ok(updated) => {
                        storage
                            .set_version(&self.entity_name, &id, updated.get_version())
                            .await?;
                        storage.mark_clean(&self.entity_name, &id).await?;
                        stats.pushed += 1;
                    }
                    Err(e) if self.api_client.is_conflict_error(&e) => {
                        stats
                            .errors
                            .push((id.clone(), "Conflict detected".to_string()));
                    }
                    Err(e) => {
                        stats.errors.push((id.clone(), e.to_string()));
                    }
                }
            }
        }

        Ok(stats)
    }

    pub async fn sync(&mut self) -> Result<(SyncStats, SyncStats)> {
        let from_stats = self.sync_from_remote().await?;
        let to_stats = self.sync_to_remote().await?;
        Ok((from_stats, to_stats))
    }
}
