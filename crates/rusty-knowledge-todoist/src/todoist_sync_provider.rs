//! Stream-based TodoistSyncProvider with builder pattern
//!
//! This sync provider polls the Todoist API and emits changes on typed streams.
//! Architecture:
//! - ONE sync() call → multiple typed streams (tasks, projects)
//! - Builder pattern for registering caches
//! - Fire-and-forget operations - updates arrive via streams

use std::sync::Arc;
use tokio::sync::broadcast;
use async_trait::async_trait;

use rusty_knowledge::core::datasource::{Change, ChangeOrigin, Result, SyncableProvider, StreamProvider, StreamPosition};

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
}

impl TodoistSyncProvider {
    pub fn new(client: TodoistClient) -> Self {
        Self {
            client,
            task_tx: broadcast::channel(1000).0,
            project_tx: broadcast::channel(1000).0,
        }
    }

    /// Get a receiver for task changes (for testing or manual wiring)
    pub fn subscribe_tasks(&self) -> broadcast::Receiver<Vec<Change<TodoistTask>>> {
        self.task_tx.subscribe()
    }

    /// Get a receiver for project changes (for testing or manual wiring)
    pub fn subscribe_projects(&self) -> broadcast::Receiver<Vec<Change<TodoistProject>>> {
        self.project_tx.subscribe()
    }
}

#[async_trait]
impl SyncableProvider for TodoistSyncProvider {
    fn provider_name(&self) -> &str {
        "todoist"
    }

    /// Trigger sync - ONE API call, emits on multiple streams
    ///
    /// This method:
    /// 1. Calls sync_items() API (returns tasks + projects in one response)
    /// 2. Splits response into task and project changes
    /// 3. Emits changes on separate typed streams
    /// 4. Returns the new stream position for persistence
    async fn sync(&self, position: StreamPosition) -> Result<StreamPosition> {
        // Extract sync token from StreamPosition
        let token_str = match &position {
            StreamPosition::Beginning => None, // Full sync
            StreamPosition::Version(bytes) => {
                std::str::from_utf8(bytes).ok()
            }
        };

        // Make ONE API call that returns both tasks and projects
        let response = self.client.sync_items(token_str).await?;

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

        // Return the new sync token as StreamPosition::Version
        // If no token is returned, return Beginning (though this shouldn't happen in practice)
        match response.sync_token {
            Some(token) => Ok(StreamPosition::Version(token.as_bytes().to_vec())),
            None => Ok(StreamPosition::Beginning), // Fallback - shouldn't happen
        }
    }
}

// ExternalServiceDiscovery
impl StreamProvider<TodoistTask> for TodoistSyncProvider {
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Vec<Change<TodoistTask>>> {
        self.subscribe_tasks()
    }
}

// ExternalServiceDiscovery
impl StreamProvider<TodoistProject> for TodoistSyncProvider {
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Vec<Change<TodoistProject>>> {
        self.subscribe_projects()
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
