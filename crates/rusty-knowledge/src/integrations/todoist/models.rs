use rusty_knowledge_macros::Entity;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Entity)]
#[entity(name = "todoist_tasks")]
pub struct TodoistTask {
    #[primary_key]
    #[indexed]
    pub id: String,

    pub content: String,

    pub description: Option<String>,

    #[indexed]
    pub project_id: String,

    pub section_id: Option<String>,

    pub parent_id: Option<String>,

    #[indexed]
    pub completed: bool,

    pub priority: i32,

    pub due_date: Option<String>,

    pub labels: Option<String>,

    pub created_at: Option<String>,

    pub updated_at: Option<String>,

    pub completed_at: Option<String>,

    pub url: String,
}

impl TodoistTask {
    pub fn new(id: String, content: String, project_id: String) -> Self {
        Self {
            id: id.clone(),
            content,
            description: None,
            project_id,
            section_id: None,
            parent_id: None,
            completed: false,
            priority: 1,
            due_date: None,
            labels: None,
            created_at: None,
            updated_at: None,
            completed_at: None,
            url: format!("https://app.todoist.com/app/task/{}", id),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct TodoistTaskApiResponse {
    pub id: String,
    pub content: String,
    pub description: Option<String>,
    pub project_id: String,
    pub section_id: Option<String>,
    pub parent_id: Option<String>,
    pub checked: Option<bool>,
    pub priority: Option<i32>,
    pub due: Option<TodoistDue>,
    pub labels: Option<Vec<String>>,
    pub added_at: Option<String>,
    pub updated_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TodoistDue {
    pub date: String,
    pub timezone: Option<String>,
    pub string: String,
    pub is_recurring: bool,
}

#[derive(Debug, Serialize)]
pub struct CreateTaskRequest<'a> {
    pub content: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_string: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<&'a str>,
}

#[derive(Debug, Serialize)]
pub struct UpdateTaskRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_string: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct PagedResponse<T> {
    #[serde(alias = "items")]
    #[serde(alias = "results")]
    pub data: Vec<T>,
    pub next_cursor: Option<String>,
}

impl From<TodoistTaskApiResponse> for TodoistTask {
    fn from(api: TodoistTaskApiResponse) -> Self {
        TodoistTask {
            id: api.id.clone(),
            content: api.content,
            description: api.description,
            project_id: api.project_id,
            section_id: api.section_id,
            parent_id: api.parent_id,
            completed: api.checked.unwrap_or(false),
            priority: api.priority.unwrap_or(1),
            due_date: api.due.map(|d| d.date),
            labels: api.labels.map(|labels| labels.join(",")),
            created_at: api.added_at,
            updated_at: api.updated_at,
            completed_at: api.completed_at,
            url: format!("https://app.todoist.com/app/task/{}", api.id),
        }
    }
}
