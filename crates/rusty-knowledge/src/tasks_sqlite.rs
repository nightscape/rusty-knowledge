use crate::storage::backend::StorageBackend;
use crate::storage::turso::TursoBackend;
use crate::storage::types::{StorageEntity, Filter, Value};
use crate::tasks::Task;
use std::collections::HashMap;

pub struct SqliteTaskStore {
    backend: TursoBackend,
}

impl SqliteTaskStore {
    pub async fn new_in_memory() -> anyhow::Result<Self> {
        let mut backend = TursoBackend::new_in_memory().await?;
        let schema = Task::entity_schema();
        backend.create_entity(&schema).await?;

        Ok(Self { backend })
    }

    pub async fn new(db_path: &str) -> anyhow::Result<Self> {
        let mut backend = TursoBackend::new(db_path).await?;
        let schema = Task::entity_schema();
        backend.create_entity(&schema).await?;

        Ok(Self { backend })
    }

    pub async fn with_default_tasks() -> anyhow::Result<Self> {
        let mut store = Self::new_in_memory().await?;
        store.insert_default_tasks().await?;
        Ok(store)
    }

    async fn insert_default_tasks(&mut self) -> anyhow::Result<()> {
        let default_tasks = Self::default_tasks();
        self.insert_tasks_recursive(&default_tasks).await?;
        Ok(())
    }

    fn default_tasks() -> Vec<Task> {
        vec![
            Task {
                id: "1".to_string(),
                title: "Build Rusty Knowledge MVP".to_string(),
                completed: false,
                parent_id: None,
                children: vec![
                    Task {
                        id: "1-1".to_string(),
                        title: "Set up Tauri".to_string(),
                        completed: true,
                        parent_id: Some("1".to_string()),
                        children: vec![],
                    },
                    Task {
                        id: "1-2".to_string(),
                        title: "Create task UI".to_string(),
                        completed: false,
                        parent_id: Some("1".to_string()),
                        children: vec![],
                    },
                ],
            },
            Task {
                id: "2".to_string(),
                title: "Add Loro integration".to_string(),
                completed: false,
                parent_id: None,
                children: vec![],
            },
        ]
    }

    fn insert_tasks_recursive<'a>(
        &'a mut self,
        tasks: &'a [Task],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + 'a>> {
        Box::pin(async move {
            for task in tasks {
                let entity = task_to_entity(task);
                self.backend.insert("tasks", entity).await?;

                if !task.children.is_empty() {
                    self.insert_tasks_recursive(&task.children).await?;
                }
            }
            Ok(())
        })
    }

    pub async fn get_all_tasks(&self) -> anyhow::Result<Vec<Task>> {
        let filter = Filter::IsNull("parent_id".to_string());
        let entities = self.backend.query("tasks", filter).await?;

        let mut tasks = Vec::new();
        for entity in entities {
            let mut task = entity_to_task(&entity)?;
            task.children = self.get_children(&task.id).await?;
            tasks.push(task);
        }

        Ok(tasks)
    }

    fn get_children<'a>(
        &'a self,
        parent_id: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Vec<Task>>> + 'a>> {
        Box::pin(async move {
            let entities = self
                .backend
                .get_children("tasks", "parent_id", parent_id)
                .await?;

            let mut children = Vec::new();
            for entity in entities {
                let mut child = entity_to_task(&entity)?;
                let child_id = child.id.clone();
                child.children = self.get_children(&child_id).await?;
                children.push(child);
            }

            Ok(children)
        })
    }

    pub async fn add_task(
        &mut self,
        title: String,
        parent_id: Option<String>,
    ) -> anyhow::Result<Task> {
        let task = Task::new(title, parent_id);
        let entity = task_to_entity(&task);
        self.backend.insert("tasks", entity).await?;
        Ok(task)
    }

    pub async fn toggle_task(&mut self, task_id: &str) -> anyhow::Result<()> {
        if let Some(entity) = self.backend.get("tasks", task_id).await? {
            let mut task = entity_to_task(&entity)?;
            task.completed = !task.completed;
            let updated_entity = task_to_entity(&task);
            self.backend
                .update("tasks", task_id, updated_entity)
                .await?;
        }
        Ok(())
    }

    pub async fn update_task(&mut self, task_id: &str, title: String) -> anyhow::Result<()> {
        if let Some(entity) = self.backend.get("tasks", task_id).await? {
            let mut task = entity_to_task(&entity)?;
            task.title = title;
            let updated_entity = task_to_entity(&task);
            self.backend
                .update("tasks", task_id, updated_entity)
                .await?;
        }
        Ok(())
    }

    pub fn delete_task<'a>(
        &'a mut self,
        task_id: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + 'a>> {
        Box::pin(async move {
            // First delete all children recursively
            let children = self
                .backend
                .get_children("tasks", "parent_id", task_id)
                .await?;
            for child in children {
                if let Some(Value::String(child_id)) = child.get("id") {
                    self.delete_task(child_id).await?;
                }
            }

            // Then delete the task itself
            self.backend.delete("tasks", task_id).await?;
            Ok(())
        })
    }

    pub async fn move_task(
        &mut self,
        task_id: &str,
        new_parent_id: Option<String>,
        _index: usize,
    ) -> anyhow::Result<()> {
        if let Some(entity) = self.backend.get("tasks", task_id).await? {
            let mut task = entity_to_task(&entity)?;
            task.parent_id = new_parent_id;
            let updated_entity = task_to_entity(&task);
            self.backend
                .update("tasks", task_id, updated_entity)
                .await?;
        }
        Ok(())
    }
}

fn task_to_entity(task: &Task) -> StorageEntity {
    let mut entity = HashMap::new();
    entity.insert("id".to_string(), Value::String(task.id.clone()));
    entity.insert("title".to_string(), Value::String(task.title.clone()));
    entity.insert("completed".to_string(), Value::Boolean(task.completed));

    if let Some(ref parent_id) = task.parent_id {
        entity.insert("parent_id".to_string(), Value::String(parent_id.clone()));
    }

    entity
}

fn entity_to_task(entity: &StorageEntity) -> anyhow::Result<Task> {
    let id = match entity.get("id") {
        Some(Value::String(s)) => s.clone(),
        _ => anyhow::bail!("Missing or invalid id"),
    };

    let title = match entity.get("title") {
        Some(Value::String(s)) => s.clone(),
        _ => anyhow::bail!("Missing or invalid title"),
    };

    let completed = match entity.get("completed") {
        Some(Value::Boolean(b)) => *b,
        Some(Value::String(s)) => s == "1",
        _ => false,
    };

    let parent_id = match entity.get("parent_id") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Null) => None,
        _ => None,
    };

    Ok(Task {
        id,
        title,
        completed,
        parent_id,
        children: Vec::new(),
    })
}
