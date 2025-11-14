# Architecture: Personal Knowledge & Task Management System

## Vision

A unified system combining Personal Knowledge Management (PKM) with task/project management that:
- Integrates data from external systems (Todoist, Gmail, Jira, Linear, Calendar)
- Supports filtering (location, time, energy, people), Kanban views, block transclusion
- Can export internal data in plain text (Markdown/YAML)
- Enables offline-first operation with eventual sync
- Separates UI from backend

## Core Technology Decisions

### CRDT: Loro
**Why:** Superior data structure match for task management
- Native MovableList for drag-drop task ordering
- Tree structures for project hierarchies
- Time-travel/version control capabilities
- Rust-native with good ProseMirror bindings
- Better suited than Yjs for our use case despite smaller ecosystem

### Rich Text: TipTap
**Why:** Developer-friendly wrapper over ProseMirror with Loro integration
- Official Loro-ProseMirror bindings available
- Headless design provides full UI control
- Less complex than raw ProseMirror

### Backend: Rust
**Why:** Performance, type safety, and native Loro support
- Traits provide typeclass-like abstractions
- Cross-platform (Tauri for desktop)
- Prevents integration bugs through strong typing
- Framework: `axum` or `actix-web`
- Search: `tantivy`

### Desktop UI: Tauri
**Why:** Native performance with web frontend flexibility
- Rust backend + modern JS frontend (SolidJS or Svelte preferred over React)
- Shared business logic across platforms

## Architecture Patterns

### 1. Hybrid Storage Model

**Decision:** Use different storage strategies for different data types

```
┌─────────────────────────────────────────┐
│         Application Layer               │
└─────────────────────────────────────────┘
                    │
        ┌───────────┴───────────┐
        ▼                       ▼
┌───────────────┐      ┌──────────────────┐
│ Internal      │      │  External        │
│ Content       │      │  Systems         │
│               │      │                  │
│ Loro (full    │      │ Turso cache +   │
│ CRDT power)   │      │ adapters         │
│               │      │                  │
│ ↓             │      │ ↓                │
│ Markdown      │      │ Simple sync      │
│ (export only) │      │                  │
└───────────────┘      └──────────────────┘
```

**Why:**
- Loro where it shines: your notes, relationships, collaborative editing
- Simple Turso cache for external read-mostly data
- 50% less code for external integrations
- Easier debugging and testing

### 2. Block-Based Editor Architecture

**Decision:** Don't force ProseMirror to be an outliner; use separate concerns

```
Outliner/Tree Component (manages block hierarchy)
        ↓
Block Storage (Single root block + normalized adjacency list)
        ↓
Individual Block Editors (TipTap per block)
```

**Why:**
- ProseMirror is document-centric, not block-centric
- Each block gets own editor instance
- Normalized adjacency list with single root block enables:
  - O(1) block lookups
  - Efficient hierarchical operations
  - Drill-down/zoom (any block can become view root)
  - Uniform API (every block has a parent)
- Loro.Text for each block's content
- Loro.Map for block metadata

### 3. External System Integration

**Decision:** Sync/cache pattern with adapters, not direct pass-through

**Why direct pass-through fails:**
- Requires constant internet connection
- High latency (every operation = API call)
- Can't add local metadata or relationships
- Can't search/filter across systems efficiently
- API rate limits break UI

**Sync/cache approach:**
```
External System (Todoist/Jira/Gmail)
        ↓ periodic sync
    Local Cache (Turso - libSQL embedded)
        ↓ read/write
    Application
        ↓ push updates
External System
```

**Benefits:**
- Works offline
- Fast reads
- Add local metadata (tags, contexts, relationships)
- Handle rate limits gracefully
- Search across all systems

### 4. Unified Storage Backend Abstraction

**Decision:** Common interface for Loro and SQLite backends

**Why:** Enables easy migration and per-integration backend choice

```rust
trait StorageBackend {
    // Schema management
    async fn create_entity(&mut self, schema: &EntitySchema) -> Result<()>;

    // CRUD operations
    async fn get(&self, entity: &str, id: &str) -> Result<Option<Entity>>;
    async fn query(&self, entity: &str, filter: Filter) -> Result<Vec<Entity>>;
    async fn insert(&mut self, entity: &str, data: Entity) -> Result<()>;
    async fn update(&mut self, entity: &str, id: &str, data: Entity) -> Result<()>;

    // Sync tracking
    async fn mark_dirty(&mut self, entity: &str, id: &str) -> Result<()>;
    async fn get_dirty(&self, entity: &str) -> Result<Vec<String>>;

    // Versioning (for conflict detection)
    async fn get_version(&self, entity: &str, id: &str) -> Result<Option<String>>;
    async fn set_version(&mut self, entity: &str, id: &str, version: String) -> Result<()>;
}
```

**Benefits:**
- Start with SQLite for external systems
- Migrate specific integrations to Loro when CRDT benefits justify complexity
- Mix and match per integration
- Clear performance comparison

### 5. Schema Definition with Derive Macros

**Decision:** Define schemas using Rust structs with derive macros

```rust
#[derive(Debug, Clone, Entity)]
#[entity(name = "todoist_tasks")]
struct TodoistTask {
    #[primary_key]
    #[indexed]
    id: String,

    content: String,

    #[indexed]
    priority: Option<i32>,

    #[indexed]
    due_date: Option<DateTime<Utc>>,

    #[reference(entity = "todoist_projects")]
    project_id: Option<String>,
}
```

**Why:**
- Type safety at compile time
- Less boilerplate
- IDE support (autocomplete, refactoring)
- Self-documenting
- Automatic schema generation for both backends

### 6. Adapter Pattern for External Systems

**Decision:** Use adapters over strict typeclasses

**Why:**
- Task models too heterogeneous across systems (Todoist flat, Jira hierarchical, Linear team-based)
- Each system has different capabilities
- Allows system-specific optimizations
- Easier to maintain than forced unified interface

```rust
trait TaskProvider {
    async fn fetch_tasks(&self) -> Result<Vec<Task>>;
    async fn update_task(&self, id: &str, updates: TaskUpdates) -> Result<Task>;
    fn capabilities(&self) -> ProviderCapabilities;
}
```

## File Structure

```
projects/
  personal-website/
    project.yaml          # Loro Tree/Map → YAML export
    notes/
      architecture.md     # Loro Text → Markdown export
      .architecture.crdt  # CRDT metadata (Loro state)
    tasks/
      tasks.yaml          # Loro MovableList → YAML export
```

**Key point:** Markdown/YAML are export-only, not source of truth. CRDT files maintain actual state.

**Why:** Two-way Markdown sync with CRDTs leads to broken state when external editors modify files.

## Data Storage Strategy

### Internal Content (Your Data)
- **Storage:** Pure Loro CRDT
- **Content types:** Notes, projects, internal tasks, relationships, metadata
- **Features:** Full CRDT guarantees, rich text editing, collaborative features
- **Sync:** Loro peer-to-peer or via sync server

### External Content (External Systems)
- **Storage:** Turso cache
- **Content types:** Todoist tasks, Jira issues, Gmail messages, calendar events
- **Features:** Offline access, fast queries, eventual consistency
- **Sync:** Adapter-managed bidirectional sync with conflict resolution

### Your Metadata on External Content
- **Storage:** Loro (never synced to external systems)
- **Content types:** Tags, contexts, relationships, custom fields
- **Example:** Link Todoist task to project note, add location/energy filters

```rust
// Structure
doc.get_map("todoist_tasks")      // Cached external data
doc.get_map("task_metadata")      // Your metadata on external tasks
doc.get_map("task_relationships") // Links between tasks and notes
```

## Conflict Resolution

### For Internal Content (Loro)
- Automatic CRDT merging
- Guaranteed convergence between devices

### For External Systems
- Last-write-wins (by timestamp) or server-wins strategy
- Field-level conflict detection
- Optional user prompt for important conflicts
- Version tracking using API etags/versions

## Implementation Phases

### Phase 1: MVP (3-6 months)
- Rust backend + Tauri desktop app
- Local Markdown/YAML files with Loro CRDT
- Basic task management (internal only)
- Kanban + list views
- Block references within system

### Phase 2: First Integration (2-3 months)
- Add Todoist integration
- Prove adapter pattern works
- Turso cache implementation
- Sync conflict handling

### Phase 3: Expansion (ongoing)
- Additional integrations (2-3 months each)
- Start with SQLite backend
- Migrate to Loro backend if offline editing becomes heavy

### Phase 4: Collaboration (3-4 months)
- CRDT sync server
- Multi-device sync
- Optional: mobile app

## Code Estimates

### Infrastructure (write once)
- StorageBackend trait + types: 350 lines
- Generic adapter implementation: 500 lines
- SQLite backend: 950 lines
- Loro backend: 750 lines
- Derive macro for schemas: 300 lines
- **Total: ~2,850 lines**

### Per Integration (with abstraction)
- Schema definition (struct): 50 lines
- API client: 300 lines
- Type conversions: 100 lines
- Tests: 200 lines
- **Total per integration: ~650 lines**

### Full System (3 integrations)
- Infrastructure: 2,850 lines
- 3 integrations × 650: 1,950 lines
- **Total: ~4,800 lines**

## Key Risks & Mitigations

### Risk: Loro is Newer
- **Mitigation:** Open source, can fork if needed. Local-first design means no vendor lock-in.

### Risk: External API Maintenance Burden
- **Mitigation:** Start with 1-2 integrations. Build robust adapter layer from day one. Add slowly.

### Risk: Scope Creep
- **Mitigation:** Build MVP with internal data only. Validate workflow before adding integrations.

### Risk: SQLite vs Loro Performance Differences
- **Mitigation:** Storage backend abstraction allows easy benchmarking and migration per integration.

## Core Implementation Details

### StorageBackend Trait

```rust
// Generic storage operations abstraction
#[async_trait]
trait StorageBackend: Send + Sync {
    // Schema management
    async fn create_entity(&mut self, schema: &EntitySchema) -> Result<()>;

    // CRUD operations
    async fn get(&self, entity: &str, id: &str) -> Result<Option<Entity>>;
    async fn query(&self, entity: &str, filter: Filter) -> Result<Vec<Entity>>;
    async fn insert(&mut self, entity: &str, data: Entity) -> Result<()>;
    async fn update(&mut self, entity: &str, id: &str, data: Entity) -> Result<()>;
    async fn delete(&mut self, entity: &str, id: &str) -> Result<()>;

    // Sync tracking
    async fn mark_dirty(&mut self, entity: &str, id: &str) -> Result<()>;
    async fn get_dirty(&self, entity: &str) -> Result<Vec<String>>;
    async fn mark_clean(&mut self, entity: &str, id: &str) -> Result<()>;

    // Versioning (for conflict detection)
    async fn get_version(&self, entity: &str, id: &str) -> Result<Option<String>>;
    async fn set_version(&mut self, entity: &str, id: &str, version: String) -> Result<()>;

    // Relationship queries
    async fn get_children(
        &self,
        entity: &str,
        parent_field: &str,
        parent_id: &str,
    ) -> Result<Vec<Entity>>;

    async fn get_related(
        &self,
        entity: &str,
        foreign_key: &str,
        related_id: &str,
    ) -> Result<Vec<Entity>>;
}

// Generic entity representation
type Entity = HashMap<String, Value>;

#[derive(Debug, Clone, Serialize, Deserialize)]
enum Value {
    String(String),
    Integer(i64),
    Boolean(bool),
    DateTime(DateTime<Utc>),
    Json(serde_json::Value),
    Reference(String),
    Null,
}

// Query filters
#[derive(Debug, Clone)]
enum Filter {
    Eq(String, Value),
    In(String, Vec<Value>),
    And(Vec<Filter>),
    Or(Vec<Filter>),
    IsNull(String),
    IsNotNull(String),
}
```

### Schema Definition

```rust
// Backend-agnostic schema
#[derive(Debug, Clone)]
struct EntitySchema {
    name: String,
    fields: Vec<FieldSchema>,
    primary_key: String,
}

#[derive(Debug, Clone)]
struct FieldSchema {
    name: String,
    field_type: FieldType,
    required: bool,
    indexed: bool,
}

#[derive(Debug, Clone)]
enum FieldType {
    String,
    Integer,
    Boolean,
    DateTime,
    Json,
    Reference(String),  // Reference to another entity
}
```

### Derive Macro for Type-Safe Schemas

```rust
// Define schemas using Rust structs with derive macros
#[derive(Debug, Clone, Entity)]
#[entity(name = "todoist_tasks")]
#[relationships(
    section = (belongs_to = "todoist_sections", field = "section_id"),
    project = (belongs_to = "todoist_projects", field = "project_id"),
    subtasks = (has_many = "todoist_tasks", field = "parent_id"),
)]
struct TodoistTask {
    #[primary_key]
    #[indexed]
    id: String,

    content: String,

    #[indexed]
    priority: Option<i32>,

    #[indexed]
    due_date: Option<DateTime<Utc>>,

    #[reference(entity = "todoist_sections")]
    section_id: Option<String>,

    #[reference(entity = "todoist_projects")]
    project_id: Option<String>,

    #[reference(entity = "todoist_tasks")]
    parent_id: Option<String>,

    completed: bool,
    order: i32,
}

// The macro generates:
// - EntitySchema implementation
// - to_entity() / from_entity() conversions
// - Relationship helper methods (section(), subtasks(), parent())
```

### Generic External System Adapter

```rust
struct ExternalSystemAdapter<B: StorageBackend, T: Entity> {
    storage: Arc<Mutex<B>>,
    api_client: Box<dyn ApiClient<T>>,
    schema: EntitySchema,
    entity_name: String,
}

impl<B: StorageBackend, T: Entity> ExternalSystemAdapter<B, T> {
    async fn sync_from_remote(&mut self) -> Result<SyncStats> {
        let mut stats = SyncStats::default();
        let remote_items = self.api_client.fetch_all().await?;
        let mut storage = self.storage.lock().await;

        for remote_item in remote_items {
            let id = remote_item.get_id();
            match storage.get(&self.entity_name, &id).await? {
                Some(_) => {
                    let entity = self.api_to_entity(remote_item)?;
                    storage.update(&self.entity_name, &id, entity).await?;
                    stats.updated += 1;
                }
                None => {
                    let entity = self.api_to_entity(remote_item)?;
                    storage.insert(&self.entity_name, entity).await?;
                    stats.inserted += 1;
                }
            }
        }
        Ok(stats)
    }

    async fn sync_to_remote(&mut self) -> Result<SyncStats> {
        let mut stats = SyncStats::default();
        let mut storage = self.storage.lock().await;
        let dirty_ids = storage.get_dirty(&self.entity_name).await?;

        for id in dirty_ids {
            if let Some(entity) = storage.get(&self.entity_name, &id).await? {
                let api_item = self.entity_to_api(entity)?;
                match self.api_client.update(&id, api_item).await {
                    Ok(updated) => {
                        storage.set_version(&self.entity_name, &id,
                            updated.get_version().to_string()).await?;
                        storage.mark_clean(&self.entity_name, &id).await?;
                        stats.pushed += 1;
                    }
                    Err(e) if e.is_conflict() => {
                        stats.conflicts.push(/* conflict info */);
                    }
                    Err(e) => stats.errors.push((id.clone(), e)),
                }
            }
        }
        Ok(stats)
    }
}
```

### Block Reference System

```rust
// Reference to external entities in internal notes
#[derive(Debug, Clone, Serialize, Deserialize)]
enum BlockReference {
    // Internal content (stored in Loro)
    Internal { block_id: String },

    // External entities
    External {
        system: String,      // "todoist"
        entity_type: String, // "section"
        entity_id: String,   // "sec_456"
        view: Option<ViewConfig>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ViewConfig {
    show_completed: bool,
    group_by: Option<String>,
    sort_by: Vec<(String, Order)>,
    max_depth: Option<usize>,  // For hierarchies
}

// Example usage in notes
impl BlockReference {
    fn todoist_section(id: impl Into<String>) -> Self {
        Self::External {
            system: "todoist".into(),
            entity_type: "section".into(),
            entity_id: id.into(),
            view: Some(ViewConfig {
                show_completed: false,
                sort_by: vec![("priority".into(), Order::Desc)],
                max_depth: Some(2),
            }),
        }
    }
}
```

### Reference Resolver

```rust
struct ReferenceResolver {
    storage: Arc<Storage>,
}

impl ReferenceResolver {
    async fn resolve(&self, reference: &BlockReference) -> Result<ResolvedBlock> {
        match reference {
            BlockReference::Internal { block_id } => {
                self.resolve_internal(block_id).await
            }
            BlockReference::External { system, entity_type, entity_id, view } => {
                self.resolve_external(system, entity_type, entity_id, view).await
            }
        }
    }

    async fn resolve_todoist_section(
        &self,
        section_id: &str,
        view: &Option<ViewConfig>,
    ) -> Result<ResolvedBlock> {
        let storage = &self.storage.todoist;

        // Load section
        let section: TodoistSection = storage.get(section_id).await?
            .ok_or(Error::NotFound)?;

        // Load all tasks in section
        let mut tasks: Vec<TodoistTask> = QueryBuilder::new(&storage)
            .filter(Filter::Eq("section_id".into(), Value::String(section_id.into())))
            .all()
            .await?;

        // Apply view configuration
        if let Some(view) = view {
            if !view.show_completed {
                tasks.retain(|t| !t.completed);
            }
            // Apply sorting...
        }

        // Build hierarchy (tasks with subtasks)
        let tree = build_task_tree(tasks, view.as_ref().and_then(|v| v.max_depth));

        Ok(ResolvedBlock::TodoistSection { section, tasks: tree })
    }
}
```

### Example Integration Usage

```rust
// Define Todoist schema
let todoist_schema = EntitySchema {
    name: "todoist_tasks".to_string(),
    primary_key: "id".to_string(),
    fields: vec![
        FieldSchema {
            name: "id".to_string(),
            field_type: FieldType::String,
            required: true,
            indexed: true,
        },
        FieldSchema {
            name: "content".to_string(),
            field_type: FieldType::String,
            required: true,
            indexed: false,
        },
        // ... more fields
    ],
};

// Use with SQLite backend
let sqlite_storage = SqliteBackend::new("tasks.db").await?;
let todoist_adapter = ExternalSystemAdapter::new(
    Arc::new(Mutex::new(sqlite_storage)),
    Box::new(TodoistClient::new(api_token)),
    todoist_schema.clone(),
).await?;

// OR use with Loro backend - same code!
let loro_storage = LoroBackend::new();
let todoist_adapter = ExternalSystemAdapter::new(
    Arc::new(Mutex::new(loro_storage)),
    Box::new(TodoistClient::new(api_token)),
    todoist_schema,
).await?;

// Sync works the same regardless of backend
todoist_adapter.sync_from_remote().await?;
todoist_adapter.sync_to_remote().await?;
```

## Design Principles

### Type Safety Over Flexibility
- Strong schemas with compile-time validation
- Prefer catching errors at compile time
- Derive macros for schema generation from structs

### Separation of Concerns
- Internal content (full control) vs external content (cached)
- Structure (Loro Tree) vs rich text (TipTap)
- Storage backend vs sync logic
- Relationships managed separately from entity data

### Local-First Architecture
- Offline operation is primary mode
- Sync is enhancement, not requirement
- Fast local operations, eventual consistency with external systems
- Block references resolved from cached data

### Pragmatic Technology Choices
- Use CRDTs where they provide value (internal collaborative content)
- Use simple caching where sufficient (external read-mostly data)
- Don't over-engineer; migrate to complex solutions only when needed
- Common abstraction allows easy migration between backends

## Next Steps

1. Set up Rust project structure with Tauri
2. Implement StorageBackend trait and SQLite implementation
3. Create basic Loro document structure for internal notes
4. Build simple block-based editor with TipTap
5. Implement derive macro for Entity trait
6. Implement first external integration (Todoist) with SQLite backend
7. Add Loro backend implementation
8. Build migration tooling between backends
9. Implement block reference resolver system
