# PRQL Lineage Test Script

This script demonstrates how to use PRQL's `pl_to_lineage` function to extract column-level lineage information from PRQL queries.

## Quick Start

```bash
# Copy to avoid workspace conflicts
cp -r examples/test-lineage /tmp/
cd /tmp/test-lineage
cargo run
```

## What is Lineage?

The `pl_to_lineage` function traces column-level data flow through PRQL transformations:

- **Where does each output column come from?** → Trace back to source tables
- **How do columns transform?** → See each pipeline stage
- **What depends on what?** → Understand column dependencies

## API Location

The function is in the `internal` module (marked as unstable):

```rust
use prqlc::internal::{pl_to_lineage, json::from_lineage};
```

**Version History:**
- prqlc 0.12.x: `prqlc::debug::pl_to_lineage`
- prqlc 0.13.x+: `prqlc::internal::pl_to_lineage` (current)

## Output Structure

The lineage JSON contains:

1. **`ast`**: Parsed query structure (pipeline stages, function calls)
2. **`frames`**: Data schema at each transformation stage (available columns, source tables)
3. **`nodes`**: Directed graph showing column dependencies (use `targets` field to trace sources)

## Key Use Cases

- **Data Governance**: Track data provenance
- **Impact Analysis**: Understand effects of schema changes
- **Query Optimization**: Identify unused columns
- **Documentation**: Auto-generate lineage diagrams
- **Debugging**: Trace column sources and transformations

## Example Query

```prql
from blocks
select {id, parent_id, depth, sort_key, content, completed, block_type, collapsed}
sort {parent_id, sort_key}
```

**Lineage shows:**
- Source: `blocks` table (node 118)
- 8 columns selected (nodes 120-127), each targeting the source table
- Sort uses `parent_id` (node 130 → 121) and `sort_key` (node 131 → 123)

## Full Documentation

See [LINEAGE_INFO.md](./LINEAGE_INFO.md) for detailed documentation.

## Limitations

- `internal` module API is unstable
- Only standard PRQL operations supported (no custom functions)
- Must run outside main workspace

## Dependencies

- prqlc 0.13.6
- serde_json 1.0
