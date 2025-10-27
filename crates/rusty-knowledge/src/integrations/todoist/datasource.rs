use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use super::client::TodoistClient;
use super::models::{CreateTaskRequest, TodoistTask, UpdateTaskRequest};
use crate::core::traits::{DataSource, Result};

pub struct TodoistDataSource {
    client: TodoistClient,
    cache: Arc<RwLock<HashMap<String, TodoistTask>>>,
}

impl TodoistDataSource {
    pub fn new(api_key: &str) -> Self {
        Self {
            client: TodoistClient::new(api_key),
            cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn refresh_cache(&self) -> Result<()> {
        let active_tasks = self.client.get_all_tasks().await?;

        let completed_tasks = self.client.get_completed_tasks(None).await?;

        let mut cache = self.cache.write().await;
        cache.clear();

        for task_api in active_tasks {
            let task = TodoistTask::from(task_api);
            cache.insert(task.id.clone(), task);
        }

        for task_api in completed_tasks {
            let task = TodoistTask::from(task_api);
            cache.insert(task.id.clone(), task);
        }

        Ok(())
    }

    async fn get_from_cache(&self, id: &str) -> Option<TodoistTask> {
        let cache = self.cache.read().await;
        cache.get(id).cloned()
    }

    async fn update_cache(&self, task: TodoistTask) {
        let mut cache = self.cache.write().await;
        cache.insert(task.id.clone(), task);
    }

    async fn remove_from_cache(&self, id: &str) {
        let mut cache = self.cache.write().await;
        cache.remove(id);
    }
}

#[async_trait]
impl DataSource<TodoistTask> for TodoistDataSource {
    async fn get_all(&self) -> Result<Vec<TodoistTask>> {
        self.refresh_cache().await?;

        let cache = self.cache.read().await;
        Ok(cache.values().cloned().collect())
    }

    async fn get_by_id(&self, id: &str) -> Result<Option<TodoistTask>> {
        if let Some(task) = self.get_from_cache(id).await {
            return Ok(Some(task));
        }

        match self.client.get_task(id).await {
            Ok(task_api) => {
                let task = TodoistTask::from(task_api);
                self.update_cache(task.clone()).await;
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

    async fn insert(&self, item: TodoistTask) -> Result<String> {
        let request = CreateTaskRequest {
            content: &item.content,
            description: item.description.as_deref(),
            project_id: Some(&item.project_id),
            due_string: item.due_date.as_deref(),
            priority: if item.priority != 1 {
                Some(item.priority)
            } else {
                None
            },
            parent_id: item.parent_id.as_deref(),
        };

        let created_task_api = self.client.create_task(&request).await?;
        let created_task = TodoistTask::from(created_task_api);
        let task_id = created_task.id.clone();

        self.update_cache(created_task).await;

        Ok(task_id)
    }

    async fn update(&self, id: &str, item: TodoistTask) -> Result<()> {
        let current_task = self.get_by_id(id).await?.ok_or_else(
            || -> Box<dyn std::error::Error + Send + Sync> {
                format!("Task {} not found", id).into()
            },
        )?;

        let mut should_update_properties = false;
        let mut should_toggle_completion = false;

        let mut content_update: Option<String> = None;
        let mut description_update: Option<Option<String>> = None;
        let mut due_string_update: Option<Option<String>> = None;
        let mut priority_update: Option<i32> = None;

        if item.content != current_task.content {
            content_update = Some(item.content.clone());
            should_update_properties = true;
        }

        if item.description != current_task.description {
            description_update = Some(item.description.clone());
            should_update_properties = true;
        }

        if item.due_date != current_task.due_date {
            due_string_update = Some(item.due_date.clone().or(Some("no date".to_string())));
            should_update_properties = true;
        }

        if item.priority != current_task.priority {
            priority_update = Some(item.priority);
            should_update_properties = true;
        }

        if item.completed != current_task.completed {
            should_toggle_completion = true;
        }

        if should_toggle_completion {
            if item.completed {
                self.client.close_task(id).await?;
            } else {
                self.client.reopen_task(id).await?;
            }
        }

        if should_update_properties {
            let request = UpdateTaskRequest {
                content: content_update.as_deref(),
                description: description_update.as_ref().and_then(|opt| opt.as_deref()),
                due_string: due_string_update.as_ref().and_then(|opt| opt.as_deref()),
                priority: priority_update,
            };

            self.client.update_task(id, &request).await?;
        }

        let updated_task_api = self.client.get_task(id).await?;
        let updated_task = TodoistTask::from(updated_task_api);
        self.update_cache(updated_task).await;

        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.client.delete_task(id).await?;

        self.remove_from_cache(id).await;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_datasource_creation() {
        let datasource = TodoistDataSource::new("test_api_key");
        assert!(datasource.cache.try_read().is_ok());
    }
}
