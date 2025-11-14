# Rusty Knowledge: Vision & Roadmap

## Overview

Rusty Knowledge aims to be an offline-first, high-performance knowledge management system that treats third-party services (Todoist, JIRA, Linear, Gmail, calendars) as first-class citizens within a unified outliner interface. Think LogSeq meets Notion, but with deep integrations and local data ownership.

## Core Vision

### 1. Third-Party Systems as First-Class Citizens

You can embed blocks from external systems anywhere in the graph. For a "Build Carport" project, see:
- All outliner blocks with your custom notes
- All Todoist tasks from the corresponding project
- All Google Drive files from the project folder
- All calendar entries with the project tag
- All JIRA issues linked to the project

Everything in one unified view. Perform actions directly in RK that you'd normally do in the external system (mark done, update priority, change status).

For operations with identical semantics across systems (task status, priority), use unified shortcuts and menus regardless of the underlying system.

#### 1.0 Unified Item Types

Define item types with unified representations across third-party systems. A "task" type represents Todoist tasks, JIRA issues, Linear issues, etc. with:
- **Common properties**: title, description, status, priority, due date
- **System-specific extensions**: JIRA sprints/story points, Todoist sections, Linear cycles

Implementation uses Rust traits for common interfaces and extension structs for system-specific features. Macros generate serialization/deserialization boilerplate.

#### 1.1 Custom Mappings

Define mappings between your knowledge graph and third-party systems:
- Blocks with tag `#work-task` sync to Todoist project "Work"
- Blocks under "Q1 Roadmap" sync to JIRA project as issues

#### 1.2 Embedding

Embed third-party items anywhere:
- Todoist task as a block inside another block
- JIRA issue embedded in a project block
- Calendar event in a meeting notes block

#### 1.3 Bi-Directional Sync

Changes in external systems reflect in RK, and vice versa. Update a Todoist task in either place, see changes everywhere.

#### 1.4 Unified Search

Search across all systems from RK's search bar. Find tasks in Todoist, issues in JIRA, emails in Gmail, all from one interface.

### 2. Custom Visualizations

Different item types get appropriate visualizations:
- **Tasks**: Round bullet showing status icon
- **Tables**: Spreadsheet-like views
- **Kanban boards**: Drag-and-drop task management
- **Inline content**: HTML, images, embeds

Customization via declarative descriptions (where reasonable) to avoid abstraction leakage.

### 3. Strong Automation

Define rules for automatic actions:
- Create Todoist tasks when certain blocks are created
- Update JIRA status when corresponding RK block marked done
- Auto-tag blocks based on content analysis

### 4. Offline-First with Strong Sync

Work on notes and tasks across devices without internet connection. Everything syncs automatically when back online.

**Architecture**:
- **CRDTs (Loro)** for owned data (blocks, links, properties)
- **Local cache + operation queue** for third-party systems
- **Conflict resolution** when offline edits clash with remote changes

### 5. Sharing

Share parts of your knowledge graph with others:
- Read-only sharing for documentation
- Collaborative editing for team projects

### 6. AI Integration

Advanced knowledge management capabilities:
- Automatic task proposals
- AI-assisted research (delegate to AI agents)
- Automatic summarization
- Intelligent linking of related blocks

### 7. Flexible Customization

- **UI themes**: Easy customization
- **Logic extensions**: Choose best abstraction (scripting languages, Rust plugins)
- **Strong typing**: Rust structs for item types to minimize runtime errors
- **Plugin sandboxing**: WASM for security

### 8. Cross-Platform

Windows, macOS, Linux, iOS, Android. Built with Flutter and Tauri.

### 9. Performance & Scalability

Handle large knowledge graphs (100k+ blocks) with many third-party integrations without slowdowns.

**Strategies**:
- Virtual scrolling and lazy loading
- Full-text search indexing (Tantivy)
- Selective loading (only visible + recently accessed)
- Multi-layer caching (memory → Turso embedded → API)

### 10. Modularity

Each component does one thing well. Makes maintenance and extension easier.

---

## Critical Architectural Challenges

### Challenge 1: Offline-First + Third-Party Sync

**The Paradox**: CRDTs work for multi-master data (your outline), but third-party APIs are server-authoritative. You can't "merge" with JIRA; you must send requests that can be rejected.

**Solution: Hybrid Architecture**

```
┌─────────────────────────────────────┐
│       UNIFIED VIEW LAYER (UI)       │
│   Merged view + sync indicators     │
└────────────┬────────────────────────┘
             │
     ┌───────┴────────┐
     │                │
┌────▼─────┐   ┌──────▼───────────┐
│  OWNED   │   │   THIRD-PARTY    │
│  DATA    │   │   SHADOW LAYER   │
├──────────┤   ├──────────────────┤
│ Loro     │   │ • Local cache    │
│ CRDT     │   │ • Operation log  │
│          │   │ • Reconciliation │
│ Source   │   │ Eventually       │
│ of truth │   │ consistent       │
└──────────┘   └──────────────────┘
```

**Key Principles**:
1. Never store third-party data in Loro (it's a cached view)
2. Queue offline changes as operations to replay
3. Reconciliation engine handles conflicts
4. Clear UI indicators (synced ✓, pending ⏳, conflict ⚠️)

### Challenge 2: API Rate Limits & Costs

With 1000s of third-party items, naive polling exceeds rate limits.

**Solution: Intelligent Sync**
- Webhook-first (JIRA, Linear, Google APIs)
- Smart caching (ETags, conditional requests)
- Batch operations (Gmail batch API, JIRA bulk)
- Selective sync (active items real-time, others background)
- Cost monitoring (track usage, alert on limits)

### Challenge 3: Type Unification vs. System Diversity

JIRA has sprints, Todoist has sections, Linear has cycles. How to unify without losing features?

**Solution: Trait-Based Protocol + Extensions**

```rust
pub trait Task {
    fn id(&self) -> TaskId;
    fn title(&self) -> &str;
    fn status(&self) -> TaskStatus;
    fn priority(&self) -> Option<Priority>;
    fn extensions(&self) -> &dyn Any;  // System-specific
}

struct JiraExtensions {
    story_points: Option<f32>,
    sprint: Option<SprintId>,
}

#[derive(UnifiedTask)]
struct JiraTask {
    #[common] id: TaskId,
    #[common] title: String,
    #[extension] jira: JiraExtensions,
}
```

Common operations work uniformly; system-specific features accessible when needed.

---

## Phased Implementation Roadmap

### Phase 1: Core Outliner (3-6 months) ← START HERE

**Goal**: Usable as LogSeq alternative

**Deliverables**:
- Loro-based outliner with blocks, links, backlinks
- Local-only (no third-party integrations yet)
- Basic task visualization (checkbox bullets with status icons)
- Full-text search (Tantivy)
- Flutter desktop + mobile
- Cross-device sync via Loro CRDT

**Validates**:
- Loro performance at scale
- Core UX and outliner interactions
- CRDT sync reliability

### Phase 2: First Integration (2-3 months)

**Goal**: Prove hybrid architecture

**Deliverables**:
- Todoist integration only (simplest API)
- Operation log + local cache
- Reconciliation engine
- Conflict resolution UI
- Offline queue with retry logic
- OAuth credential management (OS keychain)

**Validates**:
- Hybrid sync architecture works
- Conflict resolution UX is acceptable
- API rate limits are manageable

### Phase 3: Unified Types (2-3 months)

**Goal**: Validate type unification scales

**Deliverables**:
- Add JIRA + Linear integrations
- Implement trait system for tasks
- Common task interface in UI
- System-specific extension panels
- Macro-generated boilerplate

**Validates**:
- Type abstraction approach scales
- UI can handle multiple systems elegantly
- Users can access system-specific features

### Phase 4: Advanced Features (3-6 months)

**Goal**: Feature parity with vision

**Deliverables**:
- Custom visualizations (kanban, tables)
- Automation rules engine (trigger → action)
- Sharing (read-only first)
- Declarative UI DSL
- More integrations (Gmail, Calendar, GitHub)

**Validates**:
- Declarative UI is expressive enough
- Automation engine is useful
- Sharing model is secure

### Phase 5: Polish & Scale (ongoing)

**Deliverables**:
- AI integration (summarization, linking, task proposals)
- Plugin system (WASM sandboxing)
- Performance optimization (100k+ blocks)
- Additional integrations
- Mobile feature parity

---

## Trade-Offs to Accept

1. **Eventually Consistent**: Third-party sync is NOT real-time CRDT. Expect 5-30 second delays.
2. **API Limits**: Power users may hit rate limits. Document this; offer throttling controls.
3. **Platform Differences**: Some features won't work on mobile (complex plugins). That's OK.
4. **Complexity**: This is an advanced tool. Not targeting complete beginners.

---

## Success Criteria

**Phase 1 Success**: Daily-drivable outliner, better performance than LogSeq, reliable sync
**Phase 2 Success**: Todoist integration feels native, offline mode works reliably
**Phase 3 Success**: Working across JIRA/Todoist/Linear feels unified
**Full Vision Success**: All features implemented, 10k+ active users, thriving plugin ecosystem

---

## Related Documents

- `docs/adr/0001-hybrid-sync-architecture.md` - Architectural decision record for sync strategy
- `docs/architecture/trait-system.md` - Design for unified type system (TBD)
- `docs/performance/indexing-strategy.md` - Full-text search and caching (TBD)
