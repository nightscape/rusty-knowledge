use crate::core::traits::{DataSource, Result};
use crate::tasks::Task;
use async_trait::async_trait;
use std::sync::{Arc, RwLock};

#[derive(Clone)]
pub struct InMemoryTaskStore {
    tasks: Arc<RwLock<Vec<Task>>>,
}

impl InMemoryTaskStore {
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn with_tasks(tasks: Vec<Task>) -> Self {
        Self {
            tasks: Arc::new(RwLock::new(tasks)),
        }
    }

    fn flatten_tasks(tasks: &[Task]) -> Vec<Task> {
        let mut result = Vec::new();
        for task in tasks {
            let mut task_copy = task.clone();
            task_copy.children = Vec::new();
            result.push(task_copy);

            if !task.children.is_empty() {
                result.extend(Self::flatten_tasks(&task.children));
            }
        }
        result
    }

    fn rebuild_hierarchy(flat_tasks: Vec<Task>) -> Vec<Task> {
        let mut task_map: std::collections::HashMap<String, Task> =
            flat_tasks.into_iter().map(|t| (t.id.clone(), t)).collect();

        let mut root_tasks = Vec::new();
        let mut child_tasks: Vec<(String, Task)> = Vec::new();

        for (_id, task) in task_map.drain() {
            if let Some(parent_id) = &task.parent_id {
                child_tasks.push((parent_id.clone(), task));
            } else {
                root_tasks.push(task);
            }
        }

        for (parent_id, child) in child_tasks {
            Self::add_child_to_hierarchy(&mut root_tasks, &parent_id, child);
        }

        root_tasks
    }

    fn add_child_to_hierarchy(tasks: &mut [Task], parent_id: &str, child: Task) -> bool {
        for task in tasks {
            if task.id == parent_id {
                task.children.push(child);
                return true;
            }
            if Self::add_child_to_hierarchy(&mut task.children, parent_id, child.clone()) {
                return true;
            }
        }
        false
    }
}

impl Default for InMemoryTaskStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DataSource<Task> for InMemoryTaskStore {
    async fn get_all(&self) -> Result<Vec<Task>> {
        let tasks = self
            .tasks
            .read()
            .map_err(|e| format!("Failed to read tasks: {}", e))?;
        let flat = Self::flatten_tasks(&tasks);
        Ok(flat)
    }

    async fn get_by_id(&self, id: &str) -> Result<Option<Task>> {
        let tasks = self
            .tasks
            .read()
            .map_err(|e| format!("Failed to read tasks: {}", e))?;
        let flat = Self::flatten_tasks(&tasks);
        Ok(flat.into_iter().find(|t| t.id == id))
    }

    async fn insert(&self, mut item: Task) -> Result<String> {
        item.children = Vec::new();
        let id = item.id.clone();

        let mut tasks = self
            .tasks
            .write()
            .map_err(|e| format!("Failed to write tasks: {}", e))?;

        let mut flat = Self::flatten_tasks(&tasks);
        flat.push(item);
        *tasks = Self::rebuild_hierarchy(flat);

        Ok(id)
    }

    async fn update(&self, id: &str, mut item: Task) -> Result<()> {
        item.children = Vec::new();

        let mut tasks = self
            .tasks
            .write()
            .map_err(|e| format!("Failed to write tasks: {}", e))?;

        let mut flat = Self::flatten_tasks(&tasks);

        if let Some(pos) = flat.iter().position(|t| t.id == id) {
            flat[pos] = item;
            *tasks = Self::rebuild_hierarchy(flat);
            Ok(())
        } else {
            Err(format!("Task not found: {}", id).into())
        }
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let mut tasks = self
            .tasks
            .write()
            .map_err(|e| format!("Failed to write tasks: {}", e))?;

        let mut flat = Self::flatten_tasks(&tasks);

        if let Some(pos) = flat.iter().position(|t| t.id == id) {
            flat.remove(pos);
            *tasks = Self::rebuild_hierarchy(flat);
            Ok(())
        } else {
            Err(format!("Task not found: {}", id).into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_insert_and_get_all() {
        let store = InMemoryTaskStore::new();
        let task = Task::new("Test Task".to_string(), None);
        let id = task.id.clone();

        store.insert(task).await.unwrap();
        let all_tasks = store.get_all().await.unwrap();

        assert_eq!(all_tasks.len(), 1);
        assert_eq!(all_tasks[0].id, id);
        assert_eq!(all_tasks[0].title, "Test Task");
    }

    #[tokio::test]
    async fn test_get_by_id() {
        let store = InMemoryTaskStore::new();
        let task = Task::new("Find Me".to_string(), None);
        let id = task.id.clone();

        store.insert(task).await.unwrap();
        let found = store.get_by_id(&id).await.unwrap();

        assert!(found.is_some());
        assert_eq!(found.unwrap().title, "Find Me");
    }

    #[tokio::test]
    async fn test_update() {
        let store = InMemoryTaskStore::new();
        let task = Task::new("Original".to_string(), None);
        let id = task.id.clone();

        store.insert(task).await.unwrap();

        let mut updated = Task::new("Updated".to_string(), None);
        updated.id = id.clone();
        updated.completed = true;

        store.update(&id, updated).await.unwrap();

        let found = store.get_by_id(&id).await.unwrap().unwrap();
        assert_eq!(found.title, "Updated");
        assert!(found.completed);
    }

    #[tokio::test]
    async fn test_delete() {
        let store = InMemoryTaskStore::new();
        let task = Task::new("Delete Me".to_string(), None);
        let id = task.id.clone();

        store.insert(task).await.unwrap();
        assert_eq!(store.get_all().await.unwrap().len(), 1);

        store.delete(&id).await.unwrap();
        assert_eq!(store.get_all().await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_hierarchical_tasks() {
        let store = InMemoryTaskStore::new();

        let parent = Task::new("Parent".to_string(), None);
        let parent_id = parent.id.clone();
        store.insert(parent).await.unwrap();

        let child = Task::new("Child".to_string(), Some(parent_id.clone()));
        store.insert(child).await.unwrap();

        let all = store.get_all().await.unwrap();
        assert_eq!(all.len(), 2);

        let parent_task = all.iter().find(|t| t.id == parent_id).unwrap();
        assert!(parent_task.parent_id.is_none());

        let child_task = all.iter().find(|t| t.id != parent_id).unwrap();
        assert_eq!(child_task.parent_id, Some(parent_id));
    }
}
