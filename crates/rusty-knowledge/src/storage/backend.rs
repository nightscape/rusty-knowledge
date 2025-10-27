use crate::storage::schema::EntitySchema;
use crate::storage::{Entity, Filter, Result};
use async_trait::async_trait;

#[async_trait]
pub trait StorageBackend: Send + Sync {
    async fn create_entity(&mut self, schema: &EntitySchema) -> Result<()>;

    async fn get(&self, entity: &str, id: &str) -> Result<Option<Entity>>;

    async fn query(&self, entity: &str, filter: Filter) -> Result<Vec<Entity>>;

    async fn insert(&mut self, entity: &str, data: Entity) -> Result<()>;

    async fn update(&mut self, entity: &str, id: &str, data: Entity) -> Result<()>;

    async fn delete(&mut self, entity: &str, id: &str) -> Result<()>;

    async fn mark_dirty(&mut self, entity: &str, id: &str) -> Result<()>;

    async fn get_dirty(&self, entity: &str) -> Result<Vec<String>>;

    async fn mark_clean(&mut self, entity: &str, id: &str) -> Result<()>;

    async fn get_version(&self, entity: &str, id: &str) -> Result<Option<String>>;

    async fn set_version(&mut self, entity: &str, id: &str, version: String) -> Result<()>;

    async fn get_children(
        &self,
        entity: &str,
        parent_field: &str,
        parent_id: &str,
    ) -> Result<Vec<Entity>>;

    async fn get_related(
        &self,
        entity: &str,
        foreign_key: &str,
        related_id: &str,
    ) -> Result<Vec<Entity>>;
}
