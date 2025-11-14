//! Real Todoist datasource implementation for stream-based architecture
//!
//! TodoistTaskDataSource implements ChangeNotifications<TodoistTask> and CrudOperationProvider<TodoistTask>:
//! - Stateless (no cache)
//! - Makes HTTP calls to Todoist API
//! - Returns immediately (fire-and-forget)
//! - Changes arrive via TodoistSyncProvider stream

use async_trait::async_trait;
use rusty_knowledge::api::streaming::{ChangeNotifications, Change, ChangeOrigin, StreamPosition};
use rusty_knowledge::api::types::ApiError;
use rusty_knowledge::core::datasource::{CrudOperationProvider, Result};
use rusty_knowledge::storage::types::Value;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use crate::client::TodoistClient;
use crate::models::{CreateTaskRequest, TodoistTask, TodoistProject, TodoistProjectApiResponse, UpdateTaskRequest};

use super::todoist_sync_provider::TodoistSyncProvider;
use tokio_stream::{wrappers::ReceiverStream, Stream, StreamExt};
use futures::stream;
use tokio::sync::broadcast;

/// DataSource implementation for TodoistTask
///
/// This wraps TodoistSyncProvider and implements ChangeNotifications<TodoistTask>.
/// Changes come from the sync provider's stream.
pub struct TodoistTaskDataSource {
    provider: Arc<TodoistSyncProvider>,
}

impl TodoistTaskDataSource {
    pub fn new(provider: Arc<TodoistSyncProvider>) -> Self {
        Self { provider }
    }
}


#[async_trait]
impl ChangeNotifications<TodoistTask> for TodoistTaskDataSource {
    async fn watch_changes_since(
        &self,
        position: StreamPosition,
    ) -> Pin<Box<dyn Stream<Item = std::result::Result<Vec<Change<TodoistTask>>, ApiError>> + Send>> {
        let rx = self.provider.subscribe_tasks();

        match position {
            StreamPosition::Beginning => {
                // Trigger initial sync to get current state
                // Note: This requires mutable access, but we have Arc.
                // For now, we'll just subscribe to the stream and let the caller trigger sync separately.
                // In a production system, we might want to add a method to trigger sync.

                // Convert broadcast receiver to stream
                // Note: ReceiverStream only works with mpsc::Receiver, so we need to manually create a stream
                let change_stream = futures::stream::unfold(rx, |mut rx| async move {
                    match rx.recv().await {
                        Ok(changes) => Some((Ok(changes), rx)),
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            eprintln!("Stream lagged by {} messages", n);
                            Some((Err(ApiError::InternalError {
                                message: format!("Stream lagged by {} messages", n),
                            }), rx))
                        }
                        Err(broadcast::error::RecvError::Closed) => None,
                    }
                });

                Box::pin(change_stream)
            }
            StreamPosition::Version(_version_bytes) => {
                // For version-based sync, just forward future changes
                // The sync provider handles version tracking via sync tokens
                let change_stream = futures::stream::unfold(rx, |mut rx| async move {
                    match rx.recv().await {
                        Ok(changes) => Some((Ok(changes), rx)),
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            eprintln!("Stream lagged by {} messages", n);
                            Some((Err(ApiError::InternalError {
                                message: format!("Stream lagged by {} messages", n),
                            }), rx))
                        }
                        Err(broadcast::error::RecvError::Closed) => None,
                    }
                });
                Box::pin(change_stream)
            }
        }
    }

    async fn get_current_version(&self) -> std::result::Result<Vec<u8>, ApiError> {
        // Note: Sync tokens are now managed externally (by OperationDispatcher or caller)
        // This method should return the current version from the dispatcher or database
        // For now, return empty vec - the version should be retrieved from OperationDispatcher
        // TODO: Get sync token from OperationDispatcher or database
        Ok(Vec::new())
    }
}

// Keep DataSource implementation for backward compatibility during migration
// This will be removed once all consumers migrate to ChangeNotifications
#[async_trait]
impl rusty_knowledge::core::datasource::DataSource<TodoistTask> for TodoistTaskDataSource {
    async fn get_all(&self) -> Result<Vec<TodoistTask>> {
        match self.provider.client.get_all_tasks().await {
            Ok(tasks) => {
                Ok(tasks.into_iter().map(|task| TodoistTask::from(task)).collect())
            }
            Err(e) => {
                Err(e)
            }
        }
    }

    async fn get_by_id(&self, id: &str) -> Result<Option<TodoistTask>> {
        match self.provider.client.get_task(id).await {
            Ok(task_api) => {
                let task = TodoistTask::from(task_api);
                Ok(Some(task))
            }
            Err(e) => {
                if e.to_string().contains("404") {
                    Ok(None)
                } else {
                    Err(e)
                }
            }
        }
    }
}

#[async_trait]
impl CrudOperationProvider<TodoistTask> for TodoistTaskDataSource {
    async fn set_field(&self, id: &str, field: &str, value: Value) -> Result<()> {

        match field {
            "content" => {
                if let Value::String(s) = value {
                    let request = UpdateTaskRequest {
                        content: Some(&s),
                        description: None,
                        due_string: None,
                        priority: None,
                    };
                    self.provider.client.update_task(id, &request).await?;
                }
            }
            "description" => {
                let desc = match value {
                    Value::String(s) => Some(s),
                    Value::Null => Some("no description".to_string()),
                    _ => None,
                };
                if let Some(d) = desc {
                    let request = UpdateTaskRequest {
                        content: None,
                        description: Some(&d),
                        due_string: None,
                        priority: None,
                    };
                    self.provider.client.update_task(id, &request).await?;
                }
            }
            "completed" => {
                if let Value::Boolean(b) = value {
                    if b {
                        self.provider.client.close_task(id).await?;
                    } else {
                        self.provider.client.reopen_task(id).await?;
                    }
                }
            }
            "priority" => {
                if let Value::Integer(i) = value {
                    let request = UpdateTaskRequest {
                        content: None,
                        description: None,
                        due_string: None,
                        priority: Some(i as i32),
                    };
                    self.provider.client.update_task(id, &request).await?;
                }
            }
            "due_date" => {
                let due_str = match value {
                    Value::String(s) => Some(s),
                    Value::Null => Some("no date".to_string()),
                    _ => None,
                };
                if let Some(d) = due_str {
                    let request = UpdateTaskRequest {
                        content: None,
                        description: None,
                        due_string: Some(&d),
                        priority: None,
                    };
                    self.provider.client.update_task(id, &request).await?;
                }
            }
            _ => {
                return Err(format!("Field {} not supported", field).into());
            }
        }

        Ok(())
    }

    async fn create(&self, fields: HashMap<String, Value>) -> Result<String> {
        let content = fields.get("content")
            .and_then(|v| v.as_string().map(|s| s.to_string()))
            .ok_or_else(|| "Missing content field".to_string())?;
        let description = fields.get("description")
            .and_then(|v| v.as_string().map(|s| s.to_string()));
        let project_id = fields.get("project_id")
            .and_then(|v| v.as_string().map(|s| s.to_string()))
            .ok_or_else(|| "Missing project_id field".to_string())?;
        let due_string = fields.get("due_date")
            .and_then(|v| v.as_string().map(|s| s.to_string()));
        let priority = fields.get("priority")
            .and_then(|v| v.as_i64().map(|i| i as i32));
        let parent_id = fields.get("parent_id")
            .and_then(|v| v.as_string().map(|s| s.to_string()));

        let request = CreateTaskRequest {
            content: &content,
            description: description.as_deref(),
            project_id: Some(&project_id),
            due_string: due_string.as_deref(),
            priority,
            parent_id: parent_id.as_deref(),
        };

        let created_task_api = self.provider.client.create_task(&request).await?;
        let created_task = TodoistTask::from(created_task_api);
        let task_id = created_task.id.clone();

        Ok(task_id)
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.provider.client.delete_task(id).await?;
        Ok(())
    }
}

/// DataSource implementation for TodoistProject
///
/// This wraps TodoistSyncProvider and implements ChangeNotifications<TodoistProject>.
/// Changes come from the sync provider's stream.
pub struct TodoistProjectDataSource {
    provider: Arc<TodoistSyncProvider>,
}

impl TodoistProjectDataSource {
    pub fn new(provider: Arc<TodoistSyncProvider>) -> Self {
        Self { provider }
    }
}


#[async_trait]
impl ChangeNotifications<TodoistProject> for TodoistProjectDataSource {
    async fn watch_changes_since(
        &self,
        position: StreamPosition,
    ) -> Pin<Box<dyn Stream<Item = std::result::Result<Vec<Change<TodoistProject>>, ApiError>> + Send>> {
        let rx = self.provider.subscribe_projects();

        match position {
            StreamPosition::Beginning => {
                // Convert broadcast receiver to stream
                // Note: ReceiverStream only works with mpsc::Receiver, so we need to manually create a stream
                let change_stream = futures::stream::unfold(rx, |mut rx| async move {
                    match rx.recv().await {
                        Ok(changes) => Some((Ok(changes), rx)),
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            eprintln!("Stream lagged by {} messages", n);
                            Some((Err(ApiError::InternalError {
                                message: format!("Stream lagged by {} messages", n),
                            }), rx))
                        }
                        Err(broadcast::error::RecvError::Closed) => None,
                    }
                });

                Box::pin(change_stream)
            }
            StreamPosition::Version(_version_bytes) => {
                // For version-based sync, just forward future changes
                let change_stream = futures::stream::unfold(rx, |mut rx| async move {
                    match rx.recv().await {
                        Ok(changes) => Some((Ok(changes), rx)),
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            eprintln!("Stream lagged by {} messages", n);
                            Some((Err(ApiError::InternalError {
                                message: format!("Stream lagged by {} messages", n),
                            }), rx))
                        }
                        Err(broadcast::error::RecvError::Closed) => None,
                    }
                });
                Box::pin(change_stream)
            }
        }
    }

    async fn get_current_version(&self) -> std::result::Result<Vec<u8>, ApiError> {
        // Note: Sync tokens are now managed externally (by OperationDispatcher or caller)
        // This method should return the current version from the dispatcher or database
        // For now, return empty vec - the version should be retrieved from OperationDispatcher
        // TODO: Get sync token from OperationDispatcher or database
        Ok(Vec::new())
    }
}

// Keep DataSource implementation for backward compatibility during migration
#[async_trait]
impl rusty_knowledge::core::datasource::DataSource<TodoistProject> for TodoistProjectDataSource {
    async fn get_all(&self) -> Result<Vec<TodoistProject>> {
        let sync_resp = self.provider.client.sync_projects(None).await?;

        // Extract projects from response
        let projects_array = sync_resp.get("projects")
            .and_then(|p| p.as_array())
            .ok_or_else(|| "No projects array in response".to_string())?;

        // Parse projects
        let projects: Vec<TodoistProject> = projects_array
            .iter()
            .filter_map(|p| {
                serde_json::from_value::<TodoistProjectApiResponse>(p.clone())
                    .ok()
                    .filter(|api: &TodoistProjectApiResponse| !api.is_deleted.unwrap_or(false))
                    .map(|api| TodoistProject::from(api))
            })
            .collect();

        // Update sync token
        let sync_token = sync_resp.get("sync_token")
            .and_then(|t| t.as_str())
            .map(|s| s.to_string());

        // Note: We can't update the provider's sync_token directly since it's private.
        // The sync provider manages its own token via sync() calls.
        // This is fine - the token will be updated when sync() is called.

        Ok(projects)
    }

    async fn get_by_id(&self, id: &str) -> Result<Option<TodoistProject>> {
        // For projects, we need to sync to get a specific project
        // Since there's no direct "get project by ID" endpoint, we sync all projects
        let all_projects = self.get_all().await?;
        Ok(all_projects.into_iter().find(|p| p.id == id))
    }
}

#[async_trait]
impl CrudOperationProvider<TodoistProject> for TodoistProjectDataSource {
    async fn set_field(&self, id: &str, field: &str, value: Value) -> Result<()> {
        match field {
            "name" => {
                if let Value::String(_name) = value {
                    // TODO: Implement project_update command in client
                    // For now, just sync to refresh cache
                    use rusty_knowledge::core::datasource::DataSource;
                    let _ = <Self as DataSource<TodoistProject>>::get_all(self).await?;
                }
            }
            _ => {
                return Err(format!("Field {} not supported for projects", field).into());
            }
        }
        Ok(())
    }

    async fn create(&self, fields: HashMap<String, Value>) -> Result<String> {
        let name = fields.get("name")
            .and_then(|v| v.as_string().map(|s| s.to_string()))
            .ok_or_else(|| "Missing name field".to_string())?;

        // Create project via Sync API
        let project_id = self.provider.client.create_project(&name).await?;

        // Sync to get the full project details
        let sync_resp = self.provider.client.sync_projects(None).await?;
        let projects_array = sync_resp.get("projects")
            .and_then(|p| p.as_array())
            .ok_or_else(|| "No projects array in response".to_string())?;

        // Find the created project (no need to cache it)
        if let Some(project_json) = projects_array.iter().find(|p| {
            p.get("id").and_then(|id| id.as_str()) == Some(&project_id)
        }) {
            // Verify project was created successfully
            if serde_json::from_value::<TodoistProjectApiResponse>(project_json.clone()).is_err() {
                return Err("Failed to parse created project".to_string().into());
            }
        }

        Ok(project_id)
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.provider.client.delete_project(id).await?;
        Ok(())
    }
}

