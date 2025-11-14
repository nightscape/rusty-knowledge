use query_render::*;

fn main() -> anyhow::Result<()> {
    let prql_source = r#"
from todoist_tasks
filter priority > 2
select [id, status, priority, content, parent_id, num_parents]
render (
  list item_template:(
    block indent:num_parents content:(
      row (checkbox checked:status) (editable_text content)
    )
  )
)
    "#;

    println!("=== PRQL Source ===\n{}\n", prql_source);

    println!("=== Parsing and splitting ===");
    let (sql, ui_spec) = parse_query_render(prql_source)?;

    println!("\n=== Generated SQL ===");
    println!("{}\n", sql);

    println!("=== UI Specification (JSON) ===");
    println!("{}\n", serde_json::to_string_pretty(&ui_spec)?);

    println!("=== UI Specification (Debug) ===");
    println!("{:#?}\n", ui_spec);

    Ok(())
}
