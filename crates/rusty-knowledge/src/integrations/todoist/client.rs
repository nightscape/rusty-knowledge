use super::models::{CreateTaskRequest, PagedResponse, TodoistTaskApiResponse, UpdateTaskRequest};
use reqwest::header::HeaderMap;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

const BASE_URL: &str = "https://todoist.com/api/v1";

pub struct TodoistClient {
    default_headers: HeaderMap,
    client: reqwest::Client,
}

impl TodoistClient {
    pub fn new(api_key: &str) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            format!("Bearer {}", api_key)
                .parse()
                .expect("Invalid API key format"),
        );
        headers.insert(
            "Content-Type",
            "application/json".parse().expect("Invalid content type"),
        );

        Self {
            default_headers: headers,
            client: reqwest::Client::new(),
        }
    }

    pub async fn get_all_tasks(&self) -> Result<Vec<TodoistTaskApiResponse>> {
        let mut all_tasks = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            let mut url = format!("{}/tasks?limit=200", BASE_URL);
            if let Some(c) = &cursor {
                url.push_str(&format!("&cursor={}", c));
            }

            let response: PagedResponse<TodoistTaskApiResponse> = self
                .client
                .get(&url)
                .headers(self.default_headers.clone())
                .send()
                .await?
                .json()
                .await?;

            all_tasks.extend(response.data);

            if response.next_cursor.is_none() {
                break;
            }
            cursor = response.next_cursor;
        }

        Ok(all_tasks)
    }

    pub async fn get_task(&self, task_id: &str) -> Result<TodoistTaskApiResponse> {
        let url = format!("{}/tasks/{}", BASE_URL, task_id);

        let response = self
            .client
            .get(&url)
            .headers(self.default_headers.clone())
            .send()
            .await?
            .json::<TodoistTaskApiResponse>()
            .await?;

        Ok(response)
    }

    pub async fn get_completed_tasks(
        &self,
        project_id: Option<&str>,
    ) -> Result<Vec<TodoistTaskApiResponse>> {
        let mut all_tasks = Vec::new();
        let mut cursor: Option<String> = None;

        let since = chrono::Utc::now()
            .checked_sub_days(chrono::Days::new(30))
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%SZ");
        let until = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");

        loop {
            let mut url = format!(
                "{}/tasks/completed?limit=200&since={}&until={}",
                BASE_URL, since, until
            );

            if let Some(project_id) = project_id {
                url.push_str(&format!("&project_id={}", project_id));
            }

            if let Some(c) = &cursor {
                url.push_str(&format!("&cursor={}", c));
            }

            let response: PagedResponse<TodoistTaskApiResponse> = self
                .client
                .get(&url)
                .headers(self.default_headers.clone())
                .send()
                .await?
                .json()
                .await?;

            all_tasks.extend(response.data);

            if response.next_cursor.is_none() {
                break;
            }
            cursor = response.next_cursor;
        }

        Ok(all_tasks)
    }

    pub async fn create_task(
        &self,
        request: &CreateTaskRequest<'_>,
    ) -> Result<TodoistTaskApiResponse> {
        let url = format!("{}/tasks", BASE_URL);

        let response = self
            .client
            .post(&url)
            .headers(self.default_headers.clone())
            .json(request)
            .send()
            .await?
            .json::<TodoistTaskApiResponse>()
            .await?;

        Ok(response)
    }

    pub async fn update_task(&self, task_id: &str, request: &UpdateTaskRequest<'_>) -> Result<()> {
        let url = format!("{}/tasks/{}", BASE_URL, task_id);

        self.client
            .post(&url)
            .headers(self.default_headers.clone())
            .json(request)
            .send()
            .await?
            .error_for_status()?;

        Ok(())
    }

    pub async fn close_task(&self, task_id: &str) -> Result<()> {
        let url = format!("{}/tasks/{}/close", BASE_URL, task_id);

        self.client
            .post(&url)
            .headers(self.default_headers.clone())
            .send()
            .await?
            .error_for_status()?;

        Ok(())
    }

    pub async fn reopen_task(&self, task_id: &str) -> Result<()> {
        let url = format!("{}/tasks/{}/reopen", BASE_URL, task_id);

        self.client
            .post(&url)
            .headers(self.default_headers.clone())
            .send()
            .await?
            .error_for_status()?;

        Ok(())
    }

    pub async fn delete_task(&self, task_id: &str) -> Result<()> {
        let url = format!("{}/tasks/{}", BASE_URL, task_id);

        self.client
            .delete(&url)
            .headers(self.default_headers.clone())
            .send()
            .await?
            .error_for_status()?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let client = TodoistClient::new("test_api_key_12345");
        assert_eq!(
            client.default_headers.get("Authorization").unwrap(),
            "Bearer test_api_key_12345"
        );
    }
}
