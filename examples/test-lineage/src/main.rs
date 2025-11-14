use prqlc::prql_to_pl;
use prqlc::internal::{pl_to_lineage, json::from_lineage};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Query with function stubs for render and UI functions
    let query = r#"
# Define stub functions for rendering (these are UI-level, not SQL operations)
# Prefixed with ui_ to avoid conflicts with standard library
let ui_checkbox = checked:null -> checked
let ui_text = text_content:null -> text_content
let ui_badge = badge_content:null color:null -> badge_content
let ui_row = items:null -> items
let ui_list = rel:null hierarchical_sort:null item_template:null -> rel
let ui_render = rel:null -> rel

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
derive {
    checkbox_el = (ui_checkbox checked:this.completed),
    id_el = (ui_text text_content:this.id),
    space_el = (ui_text text_content:" "),
    content_el = (ui_text text_content:this.content),
    parent_label_el = (ui_text text_content:" parent: "),
    parent_el = (ui_text text_content:this.parent_id),
    badge_el = (ui_badge badge_content:this.block_type color:"cyan"),
    row_el = (ui_row items:[checkbox_el, id_el, space_el, content_el, parent_label_el, parent_el, badge_el]),
    list_output = (ui_list rel:this hierarchical_sort:[parent_id, sort_key] item_template:row_el),
    render_output = (ui_render rel:list_output)
}
select render_output
"#;
    let compact_query = r#"
# Define UI stub functions using s-strings for SQL generation
# These preserve lineage by referencing their parameters
let ui_checkbox = chk:null -> s"checkbox({chk})"
let ui_text = txt:null -> s"text({txt})"
let ui_badge = bdg:null clr:null -> s"badge({bdg}, {clr})"
let ui_row = itms:null -> s"row({itms})"
let ui_list = hsort:null tmpl:null -> s"list({hsort}, {tmpl})"
let ui_render = ui:null -> s"render({ui})"

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
select {
  ui = ui_render ui:(ui_list hsort:[parent_id, sort_key] tmpl:(ui_row itms:[(ui_checkbox chk:completed), (ui_text txt:id), (ui_text txt:" "), (ui_text txt:content), (ui_text txt:" parent: "), (ui_text txt:parent_id), (ui_badge bdg:block_type clr:"cyan")]))
}
"#;

    println!("Testing pl_to_lineage on PRQL query");
    println!("{}", "=".repeat(80));
    println!("{}", compact_query);
    println!("{}", "=".repeat(80));

    println!("\nStep 1: Parsing query into ModuleDef...");
    let module_def = match prql_to_pl(compact_query) {
        Ok(def) => {
            println!("✓ Successfully parsed query");
            def
        }
        Err(e) => {
            eprintln!("✗ Error parsing query: {:?}", e);
            return Err(e.into());
        }
    };

    println!("\nStep 2: Computing lineage...");
    let lineage = match pl_to_lineage(module_def) {
        Ok(l) => {
            println!("✓ Successfully computed lineage");
            l
        }
        Err(e) => {
            eprintln!("✗ Error getting lineage: {:?}", e);
            return Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("{:?}", e)
            )));
        }
    };

    println!("\nStep 3: Converting lineage to JSON...");
    let json = match from_lineage(&lineage) {
        Ok(j) => {
            println!("✓ Successfully converted to JSON");
            j
        }
        Err(e) => {
            eprintln!("✗ Error converting to JSON: {:?}", e);
            return Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("{:?}", e)
            )));
        }
    };

    println!("\n{}", "=".repeat(80));
    println!("Lineage Information (JSON):");
    println!("{}", "=".repeat(80));

    // Parse and pretty-print the JSON
    let parsed: serde_json::Value = serde_json::from_str(&json)?;
    println!("\n{}\n", serde_json::to_string_pretty(&parsed)?);

    println!("{}", "=".repeat(80));

    println!("\nKey information you can extract from lineage:");
    println!("- Column transformations and dependencies");
    println!("- Source tables and their relationships");
    println!("- Column-level data flow through the query");
    println!("- Expression lineage for each output column");

    Ok(())
}
