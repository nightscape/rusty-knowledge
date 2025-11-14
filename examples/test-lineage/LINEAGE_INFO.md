# PRQL Lineage Information

This document explains the lineage information returned by the `pl_to_lineage` function from the PRQL compiler.

## Overview

The `pl_to_lineage` function performs **column-level lineage tracking** for PRQL queries. It analyzes how columns flow through transformations and maintains dependencies between input and output columns.

## Running the Example

**Note**: Due to workspace configuration, this example needs to be run from outside the main workspace:

```bash
# Copy to a temporary location to avoid workspace conflicts
cp -r examples/test-lineage /tmp/
cd /tmp/test-lineage
cargo run
```

Or run from the test-lineage directory if you've excluded it from the workspace.

## Structure of Lineage Output

The lineage JSON contains three main sections:

### 1. `ast` - Abstract Syntax Tree

The parsed PRQL query as a tree structure showing:
- Pipeline stages (`from`, `select`, `sort`, etc.)
- Function calls and arguments
- Source location spans (e.g., "1:13-129")

**Use case**: Understanding the query structure and transformation order.

### 2. `frames` - Data Frames at Each Stage

An array of frames, where each frame represents the data schema at a specific pipeline stage:

```json
{
  "columns": [
    {
      "Single": {
        "name": ["blocks", "id"],
        "target_id": 123,
        "target_name": null
      }
    }
  ],
  "inputs": [
    {
      "id": 121,
      "name": "blocks",
      "table": ["default_db", "blocks"]
    }
  ]
}
```

**Key information**:
- **columns**: List of available columns at this stage
  - `name`: Fully qualified column name (table.column)
  - `target_id`: Links to the node in the lineage graph that produces this column
  - `target_name`: Alias if the column was renamed
- **inputs**: Source tables/relations feeding into this frame
  - `id`: Node identifier
  - `name`: Relation name
  - `table`: Fully qualified table reference

**Use case**: Track what columns are available after each transformation and where they come from.

### 3. `nodes` - Lineage Graph

A directed graph showing column-level lineage relationships:

```json
{
  "id": 123,
  "ident": {
    "Ident": ["this", "blocks", "id"]
  },
  "kind": "Ident",
  "parent": 131,
  "span": "1:26-28",
  "targets": [121]
}
```

**Node types**:
- **Ident**: Column reference
- **TransformCall**: Transformation operation (Select, Sort, Filter, etc.)
- **Tuple**: Group of columns

**Key fields**:
- `id`: Unique node identifier
- `kind`: Node type
- `parent`: Parent node in the transformation tree
- `children`: Child nodes (for composite nodes)
- `targets`: Source nodes this node depends on
- `span`: Source code location
- `ident`: Fully qualified identifier

**Use case**: Trace data lineage from output columns back to source columns.

## What You Can Extract

### 1. Column Dependencies

For each output column, you can trace back to its source columns:

```
output: blocks.id (node 123)
  → targets: [121] (blocks table)
```

### 2. Transformation Flow

Track how data flows through the pipeline:

```
blocks table (121)
  → Select transform (132)
    → columns: id, parent_id, depth, etc.
  → Sort transform (136)
    → sort by: parent_id, sort_key
```

### 3. Table Relationships

Identify all source tables and their usage:

```json
{
  "table": ["default_db", "blocks"],
  "columns_used": ["id", "parent_id", "depth", "sort_key", ...]
}
```

### 4. Column Lineage Graph

Build a complete lineage graph showing:
- Which source columns contribute to each output column
- How columns are transformed through the pipeline
- Dependencies between columns

### 5. Impact Analysis

Determine:
- What downstream columns are affected if a source column changes
- Which transforms reference a specific column
- Complete data flow from source to output

## Practical Applications

1. **Data Governance**: Track data provenance and lineage
2. **Impact Analysis**: Understand effects of schema changes
3. **Query Optimization**: Identify unused columns and redundant transforms
4. **Documentation**: Auto-generate data flow diagrams
5. **Debugging**: Trace where columns come from and how they're transformed
6. **Compliance**: Prove data lineage for regulatory requirements

## Example Analysis

For the query:
```prql
from blocks
select {id, parent_id, depth, sort_key, content, completed, block_type, collapsed}
sort {parent_id, sort_key}
```

**Lineage shows**:
- Source: `blocks` table (node 121)
- Select creates 8 output columns (nodes 123-130)
- Each output column directly maps to a source column (via `targets: [121]`)
- Sort operation uses `parent_id` and `sort_key` (nodes 133-134 reference 124 and 126)
- No column transformations or calculations (all are simple projections)

## Limitations

- The `internal` module is marked as unstable (API may change)
- Custom functions (like `render`) are not supported
- Only standard PRQL transformations are tracked
- Must run outside workspace due to dependency conflicts

## API Module Changes

The `pl_to_lineage` function has moved between versions:

- **prqlc 0.12.x**: Located in `prqlc::debug::pl_to_lineage`
- **prqlc 0.13.x+**: Moved to `prqlc::internal::pl_to_lineage`

This example uses **prqlc 0.13.6** with the `internal` module. The function signature and behavior remain the same:

```rust
use prqlc::prql_to_pl;
use prqlc::internal::{pl_to_lineage, json::from_lineage};
```

**Note**: Both `debug` and `internal` modules are marked as unstable and may change in future versions.
