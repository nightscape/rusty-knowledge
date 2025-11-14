use query_render::*;

fn main() -> anyhow::Result<()> {
    let prql_source = r#"
from blocks
filter workspace_id == @current_workspace
derive [
    depth = 0,
    has_children = true,
    is_collapsed = false
]
sort [parent_id, sort_order]
select [id, content, parent_id, depth, has_children, is_collapsed, completed]
render (list item_template:(block
    indent:depth
    draggable:true
    content:(row
        (collapse_button visible:has_children collapsed:is_collapsed)
        (checkbox checked:completed)
        (editable_text content)
    )
))
    "#;

    println!("=== Complex Outliner Example ===\n");
    println!("PRQL Source:\n{}\n", prql_source);

    let (sql, ui_spec) = parse_query_render(prql_source)?;

    println!("Generated SQL:\n{}\n", sql);
    println!("UI Spec:\n{}\n", serde_json::to_string_pretty(&ui_spec)?);

    Ok(())
}
