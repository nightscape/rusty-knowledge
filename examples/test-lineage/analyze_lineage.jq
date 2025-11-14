# PRQL Lineage Analyzer for Auto-Wiring Operations
# This query analyzes PRQL column lineage to determine which operations can be auto-wired

# Build a map of nodes by id for fast lookup
(.nodes | map({key: (.id | tostring), value: .}) | from_entries) as $nodes_map |

# Extract ident array from node (handles {"Ident": [...]} wrapper)
def get_ident:
  if .ident == null then null
  elif (.ident | type) == "object" then .ident.Ident
  else .ident
  end;

# Recursively trace column lineage to source table
def trace_to_source(node_id; max_depth):
  if node_id == null then
    {kind: "null", updatable: false}
  elif max_depth <= 0 then
    {kind: "max_depth_reached", updatable: false}
  else
    $nodes_map[(node_id | tostring)] as $node |
    if $node == null then
      {kind: "missing_node", updatable: false, missing_id: node_id}
    else
      ($node | get_ident) as $ident |
      if ($node.targets | type) == "null" or ($node.targets | length) == 0 then
        # Leaf node - check if it is a source table
        if ($ident | type) == "array" and ($ident | length) == 2 then
          {kind: "source_table", table: $ident[1], updatable: false}
        else
          {kind: ($node.kind // "unknown"), updatable: false}
        end
      elif ($ident | type) == "array" and ($ident | length) == 3 and $ident[0] == "this" then
        # Direct column reference like ["this", "blocks", "id"]
        trace_to_source($node.targets[0]; max_depth - 1) as $source |
        {
          kind: "direct_column",
          table: $ident[1],
          column: $ident[2],
          updatable: true,
          source_table: $source.table
        }
      elif ($ident | type) == "array" and ($ident | length) == 2 and $ident[0] == "this" then
        # Reference to a derived column like ["this", "render_output"]
        trace_to_source($node.targets[0]; max_depth - 1)
      else
        # Computed/derived - not directly updatable
        {kind: ($node.kind // "unknown"), updatable: false}
      end
    end
  end;

# Analyze the second frame (after derive) - has both direct and derived columns
.frames[1][1] as $derive_frame |

# Process all columns
($derive_frame.columns | to_entries | map(
  .value.Single as $col |
  .key as $index |
  {
    index: $index,
    name: $col.name,
    target_id: $col.target_id,
    lineage: trace_to_source($col.target_id; 20)
  }
)) as $all_columns |

{
  # Summary of updatable columns (direct columns from source tables)
  updatable_columns: (
    $all_columns |
    map(select(.lineage.updatable == true)) |
    map({
      name: (.name | if type == "array" then join(".") else . end),
      table: .lineage.table,
      column: .lineage.column,
      target_id: .target_id
    })
  ),

  # Summary of computed columns (not updatable)
  computed_columns: (
    $all_columns |
    map(select(.lineage.updatable != true and .name != null)) |
    map({
      name: (.name | if type == "array" then join(".") else . end),
      kind: .lineage.kind,
      target_id: .target_id
    })
  ),

  # Source tables
  source_tables: $derive_frame.inputs | map({
    name: .name,
    table: .table
  }),

  # Statistics
  stats: {
    total_columns: ($all_columns | length),
    updatable_columns: ($all_columns | map(select(.lineage.updatable == true)) | length),
    computed_columns: ($all_columns | map(select(.lineage.updatable != true)) | length)
  }
}
