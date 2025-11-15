use super::{app_main::AppMain, state::State, config::KeyBindingConfig};
use r3bl_tui::{CommonResult, InputEvent, TerminalWindow, KeyPress, Key, KeyState, ok};
use std::sync::Arc;
use tokio::sync::RwLock;
use std::collections::HashMap;
use std::path::PathBuf;
use rusty_knowledge::api::render_engine::RenderEngine;
use rusty_knowledge::di;
use ferrous_di::{ServiceCollection, Resolver, ServiceCollectionModuleExt};

pub async fn run_app(db_path: PathBuf, keybindings_path: Option<PathBuf>) -> CommonResult<()> {
    let app = AppMain::new_boxed();

    // Check if database file exists
    let db_exists = db_path.exists();

    // Set up dependency injection container
    let mut services = ServiceCollection::new();

    // Register Todoist config if API key is available
    let todoist_api_key = std::env::var("TODOIST_API_KEY").ok();
    if let Some(api_key) = &todoist_api_key {
        services.add_singleton(rusty_knowledge_todoist::di::TodoistConfig::new(Some(api_key.clone())));
    }

    // Register modules
    services.add_module_mut(rusty_knowledge_todoist::di::TodoistModule)
        .map_err(|e| miette::miette!("Failed to register TodoistModule: {}", e))?;

    di::register_core_services(&mut services, db_path.clone())
        .map_err(|e| miette::miette!("Failed to register core services: {}", e))?;

    let provider = services.build();

    // Resolve RenderEngine from DI container (as Arc<RwLock<RenderEngine>>)
    // ferrous-di wraps services in Arc, so we get Arc<Arc<RwLock<RenderEngine>>>
    let engine_arc_arc = provider.get_required::<Arc<RwLock<RenderEngine>>>();
    let engine = (*engine_arc_arc).clone(); // Extract inner Arc<RwLock<RenderEngine>>

    // Initialize database schema and sample data if needed
    if !db_exists {
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

        {
            let engine_guard = engine.write().await;
            engine_guard.execute_query(create_table_sql.to_string(), HashMap::new())
                .await
                .map_err(|e| miette::miette!("Failed to create blocks table: {}", e))?;
        }

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

        {
            let engine_guard = engine.write().await;
            engine_guard.execute_query(sample_data_sql.to_string(), HashMap::new())
                .await
                .map_err(|e| miette::miette!("Failed to insert sample data: {}", e))?;
        }
    }

    // Initialize Todoist integration (only instantiation is Todoist-specific)
    // After this, everything uses generic interfaces
    let todoist_enabled = if let Some(api_key) = todoist_api_key {
        use rusty_knowledge_todoist::{TodoistClient, TodoistSyncProvider};
        use rusty_knowledge_todoist::todoist_datasource::TodoistTaskDataSource;
        use rusty_knowledge::core::queryable_cache::QueryableCache;
        use rusty_knowledge::core::StreamRegistry;
        use rusty_knowledge_todoist::models::TodoistTask;
        use std::sync::Arc;

        // Get the syncable provider from DI (it was registered above)
        // Access it directly as Arc<dyn SyncableProvider> (no longer needs Mutex)
        let sync_provider_for_registration = {
            provider.get_required::<Arc<dyn rusty_knowledge::core::datasource::SyncableProvider>>().clone()
        };

        // Create TodoistSyncProvider instance for datasource/stream
        // We need a separate instance for datasource (not wrapped in Mutex)
        let sync_provider_for_datasource = Arc::new(TodoistSyncProvider::new(TodoistClient::new(&api_key)));

        // Create datasource that implements ChangeNotifications<TodoistTask>
        // Note: QueryableCache expects a concrete type, not Arc<dyn DataSource<T>>
        let datasource = TodoistTaskDataSource::new(sync_provider_for_datasource.clone());

        // Get RenderEngine's backend (for sharing with QueryableCache)
        let backend = {
            let engine_guard = engine.read().await;
            engine_guard.get_backend()
        };

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

        // Wire up the stream provider to the cache using StreamRegistry
        // This automatically handles all change ingestion in the background
        // Note: We register the datasource provider's stream. Manual syncs via the registered
        // provider will need to trigger cache.sync() separately, or we could register both
        // streams (but that would cause duplicate updates).
        StreamRegistry::register_stream_to_cache::<TodoistTask, _, _>(
            sync_provider_for_datasource.clone(),
            cache.clone(),
        )
        .map_err(|e| miette::miette!("Failed to register stream to cache: {}", e))?;

        tracing::info!("[TodoistIntegration] Registered TodoistTask stream to cache");

        // Syncable provider is already registered in DI, so it's automatically available in the dispatcher
        // Map table to entity
        {
            let engine_guard = engine.write().await;
            engine_guard.map_table_to_entity("todoist_tasks".to_string(), "todoist-task".to_string()).await;
        }

        // Initial sync to populate cache
        // Sync the datasource provider (this emits changes via broadcast channels)
        // Changes will be automatically ingested into cache via StreamRegistry
        {
            use rusty_knowledge::core::datasource::SyncableProvider;
            // Sync with Beginning position (full sync) - this will emit changes that are automatically ingested by StreamRegistry
            use rusty_knowledge::core::datasource::StreamPosition;
            let temp_provider = TodoistSyncProvider::new(TodoistClient::new(&api_key));
            let _new_position = temp_provider.sync(StreamPosition::Beginning).await
                .map_err(|e| miette::miette!("Failed to sync Todoist provider: {}", e))?;
            // TODO: Persist the new_position
        }

        // Also sync the registered provider so it has the same sync token
        // This ensures manual syncs via UI work correctly
        // Note: sync() now takes the token as parameter and returns the new token
        {
            use rusty_knowledge::core::datasource::{SyncableProvider, StreamPosition};
            // For initial sync, pass Beginning (full sync)
            let _new_position = sync_provider_for_registration.sync(StreamPosition::Beginning).await
                .map_err(|e| miette::miette!("Failed to sync registered provider: {}", e))?;
            // TODO: Persist the new_position to database/file
        }

        // Populate cache from datasource (calls source.get_all() which fetches from Todoist API)
        // This ensures cache has initial data, subsequent updates come via stream
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
    let (render_spec, initial_data, cdc_stream) = {
        let mut engine_guard = engine.write().await;
        engine_guard.query_and_watch(prql_query, params)
            .await
            .map_err(|e| miette::miette!("Failed to query blocks: {}", e))?
    };

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
