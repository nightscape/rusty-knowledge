use super::{app_main::AppMain, state::State, config::KeyBindingConfig};
use r3bl_tui::{CommonResult, InputEvent, TerminalWindow, KeyPress, Key, KeyState, ok};
use std::sync::Arc;
use std::collections::HashMap;
use std::path::PathBuf;
use rusty_knowledge::api::backend_engine::BackendEngine;
use rusty_knowledge::di;
use ferrous_di::{ServiceCollection, ServiceCollectionModuleExt, Resolver};

pub async fn run_app(db_path: PathBuf, keybindings_path: Option<PathBuf>) -> CommonResult<()> {
    let app = AppMain::new_boxed();

    let mut services = ServiceCollection::new();

    let todoist_api_key = std::env::var("TODOIST_API_KEY").ok();
    if let Some(api_key) = &todoist_api_key {
        services.add_singleton(rusty_knowledge_todoist::di::TodoistConfig::new(Some(api_key.clone())));
    }

    // Register Todoist module BEFORE core services (so providers are registered before OperationModule collects them)
    services.add_module_mut(rusty_knowledge_todoist::di::TodoistModule)
        .map_err(|e| miette::miette!("Failed to register TodoistModule: {}", e))?;

    di::register_core_services(&mut services, db_path.clone())
        .map_err(|e| miette::miette!("Failed to register core services: {}", e))?;

    let provider = services.build();
    let engine = Resolver::get_required::<BackendEngine>(&provider);


    // TODO: Make queries user-configurable
    let prql_query = if todoist_api_key.is_some() {
        // Query Todoist tasks
        // Note: TodoistTask doesn't have an 'order' field, so we use 'id' for sorting instead
        r#"
from todoist_tasks
select {
    id,
    content,
    completed,
    priority,
    due_date,
    project_id,
    parent_id,
    created_at
}
derive sort_key = id
render (list hierarchical_sort:[parent_id, sort_key] item_template:(row (checkbox checked:this.completed) (editable_text content:this.content) (badge content:this.priority color:"cyan")))
"#.to_string()
    } else {
        // Original blocks query
        r#"
from blocks
select {
    id,
    parent_id,
    depth,
    sort_key,
    content,
    completed,
    block_type,
    collapsed
}
render (list hierarchical_sort:[parent_id, sort_key] item_template:(row (checkbox checked:this.completed) (editable_text content:this.content) (text content:" ") (badge content:block_type color:"cyan")))
"#.to_string()
    };

    let params = HashMap::new();

    // Query and set up CDC streaming
    let (render_spec, initial_data, cdc_stream) = engine.query_and_watch(prql_query, params)
        .await
        .map_err(|e| miette::miette!("Failed to query blocks: {}", e))?;

    // Load keybindings configuration
    let keybindings = if let Some(ref path) = keybindings_path {
        match KeyBindingConfig::load_from_file(path) {
            Ok(config) => {
                eprintln!("Loaded keybindings from: {}", path.display());
                Arc::new(config)
            }
            Err(e) => {
                eprintln!("Warning: Failed to load keybindings from {}: {}", path.display(), e);
                eprintln!("Using empty keybindings configuration");
                Arc::new(KeyBindingConfig::empty())
            }
        }
    } else {
        // No config file specified - use empty config
        Arc::new(KeyBindingConfig::empty())
    };

    // Create channel for CDC events
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let cdc_receiver = Arc::new(std::sync::Mutex::new(rx));

    let initial_state = State::new(engine, render_spec, initial_data, cdc_receiver, keybindings);

    // Spawn background task to forward CDC stream to channel and set pending flag
    let pending_flag = initial_state.has_pending_cdc_changes.clone();
    tokio::spawn(async move {
        use tokio_stream::StreamExt;

        let mut stream = cdc_stream;

        while let Some(change) = stream.next().await {
            if tx.send(change).is_err() {
                // Receiver dropped, exit task
                break;
            }
            // Set flag to indicate there are pending changes
            if let Ok(mut flag) = pending_flag.lock() {
                *flag = true;
            }
        }
    });

    // Spawn CDC watcher task that monitors for changes and triggers UI renders
    let pending_flag_clone = initial_state.has_pending_cdc_changes.clone();
    let main_thread_sender_channel = initial_state.main_thread_sender_channel.clone();

    tokio::spawn(async move {
        loop {
            // Check if we have a main thread sender yet
            let sender_opt = {
                let guard = main_thread_sender_channel.lock().unwrap();
                guard.clone()
            };

            if let Some(sender) = sender_opt {
                // Check if there are pending CDC changes
                let has_changes = {
                    let flag = pending_flag_clone.lock().unwrap();
                    *flag
                };

                if has_changes {
                    // Send signal to trigger render
                    use r3bl_tui::TerminalWindowMainThreadSignal;
                    if sender.send(TerminalWindowMainThreadSignal::Render(None)).await.is_err() {
                        // Main thread dropped, exit
                        break;
                    }
                }
            }

            // Poll every 50ms
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }
    });

    let exit_keys = &[InputEvent::Keyboard(KeyPress::WithModifiers {
        key: Key::Character('q'),
        mask: r3bl_tui::ModifierKeysMask {
            ctrl_key_state: KeyState::Pressed,
            shift_key_state: KeyState::NotPressed,
            alt_key_state: KeyState::NotPressed,
        },
    })];

    TerminalWindow::main_event_loop(app, exit_keys, initial_state)?.await?;

    ok!()
}
