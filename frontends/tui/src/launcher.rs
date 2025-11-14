use super::{app_main::AppMain, state::State, config::KeyBindingConfig};
use r3bl_tui::{CommonResult, InputEvent, TerminalWindow, KeyPress, Key, KeyState, ok};
use std::sync::Arc;
use tokio::sync::RwLock;
use std::collections::HashMap;
use std::path::PathBuf;
use rusty_knowledge::api::render_engine::RenderEngine;

pub async fn run_app(db_path: PathBuf, keybindings_path: Option<PathBuf>) -> CommonResult<()> {
    let app = AppMain::new_boxed();

    // Check if database file exists
    let db_exists = db_path.exists();

    // Initialize RenderEngine with file-based database
    let mut engine = if db_exists {
        // File exists - use it as-is without running DDLs or inserting data
        RenderEngine::new(db_path.clone())
            .await
            .map_err(|e| miette::miette!("Failed to initialize RenderEngine with existing database: {}", e))?
    } else {
        // File doesn't exist - create it and populate with example data
        let engine = RenderEngine::new(db_path.clone())
            .await
            .map_err(|e| miette::miette!("Failed to initialize RenderEngine: {}", e))?;

        // Create blocks table schema
        let create_table_sql = r#"
            CREATE TABLE IF NOT EXISTS blocks (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                depth INTEGER NOT NULL DEFAULT 0,
                sort_key TEXT NOT NULL,
                content TEXT NOT NULL,
                collapsed INTEGER NOT NULL DEFAULT 0,
                completed INTEGER NOT NULL DEFAULT 0,
                block_type TEXT NOT NULL DEFAULT 'text',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            )
        "#;

        engine.execute_query(create_table_sql.to_string(), HashMap::new())
            .await
            .map_err(|e| miette::miette!("Failed to create blocks table: {}", e))?;

        // Generate proper fractional index keys for sample data
        // Using loro_fractional_index to generate valid keys
        use rusty_knowledge::storage::fractional_index::gen_key_between;

        let root_1_key = gen_key_between(None, None)
            .map_err(|e| miette::miette!("Failed to generate root-1 key: {}", e))?;
        let root_2_key = gen_key_between(Some(&root_1_key), None)
            .map_err(|e| miette::miette!("Failed to generate root-2 key: {}", e))?;

        let child_1_key = gen_key_between(None, None)
            .map_err(|e| miette::miette!("Failed to generate child-1 key: {}", e))?;
        let child_2_key = gen_key_between(Some(&child_1_key), None)
            .map_err(|e| miette::miette!("Failed to generate child-2 key: {}", e))?;

        let grandchild_1_key = gen_key_between(None, None)
            .map_err(|e| miette::miette!("Failed to generate grandchild-1 key: {}", e))?;

        // Insert sample data for testing with fractional indexing sort_keys
        let sample_data_sql = format!(r#"
            INSERT OR IGNORE INTO blocks (id, parent_id, depth, sort_key, content, block_type, completed)
            VALUES
                ('root-1', NULL, 0, '{}', 'Welcome to Block Outliner', 'heading', 0),
                ('child-1', 'root-1', 1, '{}', 'This is a child block', 'text', 0),
                ('child-2', 'root-1', 1, '{}', 'Another child block', 'text', 1),
                ('grandchild-1', 'child-1', 2, '{}', 'A nested grandchild', 'text', 0),
                ('root-2', NULL, 0, '{}', 'Second top-level block', 'heading', 0)
        "#, root_1_key, child_1_key, child_2_key, grandchild_1_key, root_2_key);

        engine.execute_query(sample_data_sql.to_string(), HashMap::new())
            .await
            .map_err(|e| miette::miette!("Failed to insert sample data: {}", e))?;

        engine
    };

    // Initialize Todoist integration (only instantiation is Todoist-specific)
    // After this, everything uses generic interfaces
    let todoist_enabled = if let Ok(api_key) = std::env::var("TODOIST_API_KEY") {
        use rusty_knowledge_todoist::{TodoistClient, TodoistSyncProvider};
        use rusty_knowledge_todoist::todoist_datasource::TodoistTaskDataSource;
        use rusty_knowledge::core::queryable_cache::QueryableCache;
        use rusty_knowledge::core::datasource::{DataSource, OperationProvider};
        use std::sync::Arc;
        use tokio::sync::Mutex;
        
        // Create Todoist-specific components
        // For MVP: Create two instances - one for datasource, one for sync registration
        // TODO: Refactor to share a single instance properly (requires TodoistTaskDataSource changes)
        let client1 = TodoistClient::new(&api_key);
        let sync_provider_for_datasource = Arc::new(TodoistSyncProvider::new(client1).build());
        
        let client2 = TodoistClient::new(&api_key);
        let sync_provider_concrete = TodoistSyncProvider::new(client2).build();
        
        // CRITICAL: Subscribe to the registered provider's stream BEFORE wrapping it
        // This allows us to wire up the stream to update the cache
        let rx_registered = sync_provider_concrete.subscribe_tasks();
        let sync_provider_for_registration = Arc::new(Mutex::new(sync_provider_concrete));
        
        // Create datasource that implements ChangeNotifications<TodoistTask>
        // Note: QueryableCache expects a concrete type, not Arc<dyn DataSource<T>>
        let datasource = TodoistTaskDataSource::new(sync_provider_for_datasource);
        
        // Get RenderEngine's backend (for sharing with QueryableCache)
        let backend = engine.get_backend();
        
        // Create QueryableCache using RenderEngine's backend
        // Pass the concrete datasource directly, not wrapped in Arc
        let cache = Arc::new(
            QueryableCache::new_with_backend(
                datasource,
                backend.clone()
            )
            .await
            .map_err(|e| miette::miette!("Failed to create Todoist cache: {}", e))?
        );
        
        // CRITICAL: Wire up the registered provider's stream to update the cache
        // When sync is triggered via 'r', the registered provider emits changes that need to be written to the database
        // We spawn a background task that watches the registered provider's stream and updates the cache
        {
            use rusty_knowledge_todoist::models::TodoistTask;
            use rusty_knowledge::api::streaming::Change;
            use rusty_knowledge::core::Entity;
            use rusty_knowledge::core::HasSchema;
            use tokio_stream::wrappers::BroadcastStream;
            use tokio_stream::StreamExt;
            
            let cache_clone = cache.clone();
            let backend_clone = backend.clone();
            
            tokio::spawn(async move {
                tracing::info!("[StreamHandler] Starting stream handler for registered provider");
                // Convert broadcast receiver to stream
                let mut stream = BroadcastStream::new(rx_registered);
                
                while let Some(result) = stream.next().await {
                    match result {
                        Ok(changes) => {
                            tracing::info!("[StreamHandler] Received {} changes from registered provider", changes.len());
                            // Process each change and write to database via cache
                            for change in changes {
                                match change {
                                    Change::Created { data, .. } | Change::Updated { data, .. } => {
                                        tracing::debug!("[StreamHandler] Upserting change: id={}", data.id.clone());
                                        // Write to database using cache's upsert method
                                        if let Err(e) = cache_clone.upsert_to_cache(&data).await {
                                            tracing::error!("[StreamHandler] Failed to ingest change into cache: {}", e);
                                        } else {
                                            tracing::debug!("[StreamHandler] Successfully upserted change");
                                        }
                                    }
                                    Change::Deleted { id, .. } => {
                                        tracing::debug!("[StreamHandler] Deleting change: id={}", id);
                                        // Delete from database
                                        if let Err(e) = cache_clone.delete_from_cache(&id).await {
                                            tracing::error!("[StreamHandler] Failed to delete from cache: {}", e);
                                        } else {
                                            tracing::debug!("[StreamHandler] Successfully deleted change");
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("[StreamHandler] Stream error: {:?}", e);
                            // Continue processing - don't break on errors
                        }
                    }
                }
                tracing::warn!("[StreamHandler] Stream handler exited (stream closed)");
            });
        }
        
        // Register syncable provider (wrapped in Arc<Mutex<...>>)
        engine.register_syncable_provider("todoist".to_string(), sync_provider_for_registration.clone() as Arc<Mutex<dyn rusty_knowledge::core::datasource::SyncableProvider>>).await;
        
        // Map table to entity
        engine.map_table_to_entity("todoist_tasks".to_string(), "todoist-task".to_string()).await;
        
        // Initial sync to populate cache
        // NOTE: We sync the datasource's provider first, then call cache.sync() to populate the cache
        // The cache.sync() calls source.get_all() which fetches tasks from Todoist API
        // TODO: Wire up ongoing updates from sync provider's stream to cache
        {
            use rusty_knowledge::core::datasource::SyncableProvider;
            // Sync the datasource's provider (this emits changes via broadcast channels)
            // We need mutable access, so we create a temporary mutable copy
            // This is a workaround - ideally TodoistSyncProvider would use interior mutability
            let mut temp_provider = TodoistSyncProvider::new(TodoistClient::new(&api_key)).build();
            temp_provider.sync().await
                .map_err(|e| miette::miette!("Failed to initial Todoist sync: {}", e))?;
            
            // Also sync the registered provider so manual syncs work
            let mut provider_mut = sync_provider_for_registration.lock().await;
            provider_mut.sync().await
                .map_err(|e| miette::miette!("Failed to sync registered provider: {}", e))?;
        }
        
        // Populate cache from datasource (calls source.get_all() which fetches from Todoist API)
        cache.sync().await
            .map_err(|e| miette::miette!("Failed to sync cache: {}", e))?;
        
        // TODO: QueryableCache needs to implement OperationProvider
        // For now, skip registration until we implement OperationProvider for QueryableCache
        // engine.register_provider("todoist-task".to_string(), cache.clone() as Arc<dyn OperationProvider>).await
        //     .map_err(|e| miette::miette!("Failed to register Todoist provider: {}", e))?;
        
        true
    } else {
        false
    };

    // PRQL query - temporary: use Todoist if enabled, otherwise use blocks
    // TODO: Make queries user-configurable
    let prql_query = if todoist_enabled {
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
render (list hierarchical_sort:[parent_id, sort_key] item_template:(row (checkbox checked:this.completed) (text content:this.content) (badge content:this.priority color:"cyan")))
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
    let (render_spec, initial_data, cdc_stream) = engine
        .query_and_watch(prql_query, params)
        .await
        .map_err(|e| miette::miette!("Failed to query blocks: {}", e))?;

    let engine = Arc::new(RwLock::new(engine));

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
