//! Stream-based TodoistSyncProvider with builder pattern
//!
//! This sync provider polls the Todoist API and emits changes on typed streams.
//! Architecture:
//! - ONE sync() call → multiple typed streams (tasks, projects)
//! - Builder pattern for registering caches
//! - Fire-and-forget operations - updates arrive via streams

use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use async_trait::async_trait;

use rusty_knowledge::core::datasource::{Change, ChangeOrigin, Result, SyncableProvider};
use rusty_knowledge::core::StreamCache as QueryableCache;

use crate::client::TodoistClient;
use crate::models::{SyncResponse, TodoistTask, TodoistProject, TodoistTaskApiResponse};

/// Stream-based TodoistSyncProvider that polls API and emits changes on typed streams
///
/// Architecture:
/// - sync() makes ONE API call → splits into task and project changes → emits on separate streams
/// - Builder pattern for registering caches
pub struct TodoistSyncProvider {
    pub(crate) client: TodoistClient,
    task_tx: broadcast::Sender<Vec<Change<TodoistTask>>>,
    project_tx: broadcast::Sender<Vec<Change<TodoistProject>>>,
    sync_token: Arc<RwLock<Option<String>>>,
}

/// Builder for TodoistSyncProvider with cache registration
pub struct TodoistSyncProviderBuilder {
    provider: TodoistSyncProvider,
    registrations: Vec<Box<dyn FnOnce(&TodoistSyncProvider) + Send>>,
}

impl TodoistSyncProvider {
    /// Create a new TodoistSyncProvider with builder pattern
    pub fn new(client: TodoistClient) -> TodoistSyncProviderBuilder {
        let (task_tx, _) = broadcast::channel(1000); // Buffer up to 1000 batches
        let (project_tx, _) = broadcast::channel(1000);

        TodoistSyncProviderBuilder {
            provider: TodoistSyncProvider {
                client,
                task_tx,
                project_tx,
                sync_token: Arc::new(RwLock::new(None)),
            },
            registrations: vec![],
        }
    }

    /// Create from API key (convenience method)
    pub fn from_api_key(api_key: &str) -> TodoistSyncProviderBuilder {
        Self::new(TodoistClient::new(api_key))
    }


    /// Get a receiver for task changes (for testing or manual wiring)
    pub fn subscribe_tasks(&self) -> broadcast::Receiver<Vec<Change<TodoistTask>>> {
        self.task_tx.subscribe()
    }

    /// Get a receiver for project changes (for testing or manual wiring)
    pub fn subscribe_projects(&self) -> broadcast::Receiver<Vec<Change<TodoistProject>>> {
        self.project_tx.subscribe()
    }

    /// Get the current sync token (for testing)
    pub async fn get_sync_token(&self) -> Option<String> {
        let sync_token = self.sync_token.read().await;
        sync_token.clone()
    }
}

#[async_trait]
impl SyncableProvider for TodoistSyncProvider {
    /// Trigger sync - ONE API call, emits on multiple streams
    ///
    /// This method:
    /// 1. Calls sync_items() API (returns tasks + projects in one response)
    /// 2. Updates sync_token
    /// 3. Splits response into task and project changes
    /// 4. Emits changes on separate typed streams
    async fn sync(&mut self) -> Result<()> {
        let token = {
            let sync_token = self.sync_token.read().await;
            sync_token.clone()
        };

        // Make ONE API call that returns both tasks and projects
        let response = self.client.sync_items(token.as_deref()).await?;

        // Update sync token
        if let Some(new_token) = response.sync_token.clone() {
            let mut sync_token = self.sync_token.write().await;
            *sync_token = Some(new_token);
        }

        // Split and emit on separate typed streams
        let task_changes = compute_task_changes(&response);
        let project_changes = compute_project_changes(&response);

        // Emit changes (fire-and-forget - ignore errors if no receivers)
        use tracing::info;
        info!("[TodoistSyncProvider] Emitting {} task changes and {} project changes", task_changes.len(), project_changes.len());
        let send_result = self.task_tx.send(task_changes);
        if send_result.is_err() {
            info!("[TodoistSyncProvider] No receivers for task changes (this is ok if no subscribers)");
        }
        let _ = self.project_tx.send(project_changes);

        Ok(())
    }
}

impl TodoistSyncProviderBuilder {
    /// Register a cache for tasks
    ///
    /// This wires up the task stream to the cache's ingest_stream method.
    pub fn with_tasks(mut self, cache: Arc<QueryableCache<TodoistTask>>) -> Self {
        let cache_clone = Arc::clone(&cache);
        self.registrations.push(Box::new(move |provider: &TodoistSyncProvider| {
            let rx = provider.subscribe_tasks();
            cache_clone.ingest_stream(rx);
        }));
        self
    }

    /// Register a cache for projects
    ///
    /// This wires up the project stream to the cache's ingest_stream method.
    pub fn with_projects(mut self, cache: Arc<QueryableCache<TodoistProject>>) -> Self {
        let cache_clone = Arc::clone(&cache);
        self.registrations.push(Box::new(move |provider: &TodoistSyncProvider| {
            let rx = provider.subscribe_projects();
            cache_clone.ingest_stream(rx);
        }));
        self
    }

    /// Build the provider and execute all registrations
    ///
    /// This wires up all registered caches to their respective streams.
    pub fn build(mut self) -> TodoistSyncProvider {
        // Execute all registrations to wire up streams
        for register in self.registrations.drain(..) {
            register(&self.provider);
        }
        self.provider
    }
}

/// Compute task changes from sync response
///
/// Converts API responses to Change<TodoistTask> enum variants.
/// Filters out deleted items (is_deleted: true).
fn compute_task_changes(response: &SyncResponse) -> Vec<Change<TodoistTask>> {
    response
        .items
        .iter()
        .filter(|api_item| !api_item.is_deleted.unwrap_or(false))
        .map(|api_item| {
            // Clone the API response to convert it
            let api_item_cloned = TodoistTaskApiResponse {
                id: api_item.id.clone(),
                content: api_item.content.clone(),
                description: api_item.description.clone(),
                project_id: api_item.project_id.clone(),
                section_id: api_item.section_id.clone(),
                parent_id: api_item.parent_id.clone(),
                checked: api_item.checked,
                priority: api_item.priority,
                due: api_item.due.as_ref().map(|d| crate::models::TodoistDue {
                    date: d.date.clone(),
                    timezone: d.timezone.clone(),
                    string: d.string.clone(),
                    is_recurring: d.is_recurring,
                }),
                labels: api_item.labels.clone(),
                added_at: api_item.added_at.clone(),
                updated_at: api_item.updated_at.clone(),
                completed_at: api_item.completed_at.clone(),
                is_deleted: api_item.is_deleted,
            };
            let task: TodoistTask = TodoistTask::from(api_item_cloned);
            // Todoist sync API doesn't distinguish create vs update, so use Updated for both
            Change::Updated {
                id: task.id.clone(),
                data: task,
                origin: ChangeOrigin::Remote,
            }
        })
        .collect()
}

/// Compute project changes from sync response
///
/// Note: The sync API returns projects in a different format (sync_projects).
/// For now, this returns empty vec - projects need separate sync call.
/// TODO: Integrate project sync into main sync() method
fn compute_project_changes(_response: &SyncResponse) -> Vec<Change<TodoistProject>> {
    // TODO: Projects are returned from sync_projects(), not sync_items()
    // Need to either:
    // 1. Call sync_projects() separately and merge
    // 2. Or modify sync() to call both APIs
    vec![]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_provider_creation() {
        let client = TodoistClient::new("test_api_key");
        let builder = TodoistSyncProvider::new(client);
        let provider = builder.build();

        // Verify provider was created
        assert!(provider.get_sync_token().await.is_none());
    }

    #[tokio::test]
    async fn test_provider_builder_with_tasks() {
        use rusty_knowledge::storage::turso::TursoBackend;
        use crate::todoist_datasource::TodoistTaskDataSource;

        let client = TodoistClient::new("test_api_key");
        let datasource = Arc::new(TodoistTaskDataSource::from_api_key("test_api_key"));
        let db = Arc::new(RwLock::new(Box::new(TursoBackend::new_in_memory().await.unwrap()) as Box<dyn rusty_knowledge::storage::backend::StorageBackend>));

        let cache = Arc::new(QueryableCache::new(
            datasource,
            db,
            "todoist_tasks".to_string(),
        ));

        let builder = TodoistSyncProvider::new(client);
        let provider = builder.with_tasks(cache).build();

        // Verify provider was created with cache registered
        assert!(provider.get_sync_token().await.is_none());
    }
}

