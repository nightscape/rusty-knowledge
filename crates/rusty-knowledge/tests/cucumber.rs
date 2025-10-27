use cucumber::World;
use rusty_knowledge::storage::sqlite::SqliteBackend;
use rusty_knowledge::tasks::{Task, TaskStore};

mod steps;

#[derive(Debug, World)]
#[derive(Default)]
pub struct AppWorld {
    pub task_store: TaskStore,
    pub last_task: Option<Task>,
    pub all_tasks: Vec<Task>,
    pub sqlite_backend: Option<SqliteBackend>,
    pub last_task_id: Option<String>,
    pub child_task_id: Option<String>,
}


#[tokio::main]
async fn main() {
    AppWorld::cucumber().run_and_exit("tests/features").await;
}
