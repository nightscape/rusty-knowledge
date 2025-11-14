# Compact PRQL Query with Lineage Tracking

## Solution

The compact query syntax works by defining UI stub functions using s-strings that preserve lineage while allowing a more compact syntax:

```prql
# Define UI stub functions using s-strings for SQL generation
let ui_checkbox = chk:null -> s"checkbox({chk})"
let ui_text = txt:null -> s"text({txt})"
let ui_badge = bdg:null clr:null -> s"badge({bdg}, {clr})"
let ui_row = itms:null -> s"row({itms})"
let ui_list = hsort:null tmpl:null -> s"list({hsort}, {tmpl})"
let ui_render = ui:null -> s"render({ui})"

from blocks
select {
    id, parent_id, depth, sort_key, content, completed, block_type, collapsed
}
select {
  ui = ui_render ui:(ui_list hsort:[parent_id, sort_key] tmpl:(ui_row itms:[(ui_checkbox chk:completed), (ui_text txt:id), (ui_text txt:" "), (ui_text txt:content), (ui_text txt:" parent: "), (ui_text txt:parent_id), (ui_badge bdg:block_type clr:"cyan")]))
}
```

## Key Changes from Original Desired Query

1. **Prefixed function names**: Used `ui_` prefix to avoid conflicts with PRQL standard library (`text` conflicts with `std.text`)
2. **Abbreviated parameter names**: Used short names (`chk`, `txt`, `bdg`, `clr`, `itms`, `hsort`, `tmpl`) to avoid conflicts with column names
3. **Removed `rel` parameter**: The `list` and `row` functions don't need explicit relation parameters - they work on the implicit context

## Lineage Information Captured

The lineage JSON shows exactly which fields are used in which widgets:

- `ui_checkbox` uses `completed` (node 144 → 131)
- `ui_text` instances use: `id`, `content`, `parent_id` (nodes 146, 148, 152)
- `ui_badge` uses: `block_type` (node 154 → 132)
- `ui_list` uses: `parent_id`, `sort_key` for sorting (nodes 138, 140)

## Generated SQL

```sql
SELECT
  render(
    list(
      [parent_id, sort_key],
      row(
        [checkbox(completed), text(id), text(' '), text(content), text(' parent: '), text(parent_id), badge(block_type, 'cyan')]
      )
    )
  ) AS ui
FROM blocks
```

The SQL mirrors the render structure from the compact query while preserving field dependencies.
