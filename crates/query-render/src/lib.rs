pub mod types;
pub mod parser;
pub mod compiler;
pub mod lineage;

pub use types::{RenderSpec, RenderExpr, Arg, BinaryOperator, OperationWiring, OperationDescriptor, OperationParam, TypeHint, Number, Value, PreconditionChecker};
pub use parser::QueryRenderSplit;
pub use compiler::compile_render_spec;
pub use lineage::{LineagePreprocessor, WidgetOperationMapping};

use anyhow::Result;

/// Main entry point: Parse PRQL with render(), split into SQL query + UI instructions
pub fn parse_query_render(prql_source: &str) -> Result<(String, RenderSpec)> {
    let split = parser::split_prql_at_render(prql_source)?;

    let rq = prqlc::pl_to_rq(split.query_module)?;
    let sql = prqlc::rq_to_sql(rq, &prqlc::Options::default())?;

    let render_json = parser::prql_ast_to_json(&split.render_ast)?;

    let render_spec = compiler::compile_render_spec(&render_json)?;

    Ok((sql, render_spec))
}

/// Parse PRQL with automatic operation inference from lineage
///
/// This version extracts the table name from the query and annotates the render tree with
/// auto-operations based on direct column references.
pub fn parse_query_render_with_operations(prql_source: &str) -> Result<(String, RenderSpec)> {
    // Step 1: Split query and render
    let split = parser::split_prql_at_render(prql_source)?;

    // Step 2: Extract table name from the main query
    let table_name = extract_table_name(&split.query_module)?;

    // Step 3: Compile SQL and render spec
    let rq = prqlc::pl_to_rq(split.query_module)?;
    let sql = prqlc::rq_to_sql(rq, &prqlc::Options::default())?;
    let render_json = parser::prql_ast_to_json(&split.render_ast)?;
    let mut render_spec = compiler::compile_render_spec(&render_json)?;

    // Step 4: Annotate tree with auto-operations
    annotate_tree_with_operations(&mut render_spec.root, &table_name);

    Ok((sql, render_spec))
}

/// Extract the table name from the main query pipeline
fn extract_table_name(module: &prqlc::pr::ModuleDef) -> Result<String> {
    use prqlc::pr::*;

    // Find the main query
    for stmt in &module.stmts {
        if let StmtKind::VarDef(var_def) = &stmt.kind {
            if matches!(var_def.kind, VarDefKind::Main) {
                if let Some(value) = &var_def.value {
                    // Look for 'from' in the pipeline
                    return find_from_in_expr(value);
                }
            }
        }
    }
    anyhow::bail!("No main query found in module")
}

/// Find the 'from' transform in an expression and extract the table name
fn find_from_in_expr(expr: &prqlc::pr::Expr) -> Result<String> {
    use prqlc::pr::*;

    match &expr.kind {
        ExprKind::Pipeline(pipeline) => {
            // Check each expression in the pipeline
            for e in &pipeline.exprs {
                if let ExprKind::FuncCall(func_call) = &e.kind {
                    // Check if this is a 'from' call
                    if let ExprKind::Ident(ident) = &func_call.name.kind {
                        if ident.name == "from" {
                            // The first argument should be the table name (an identifier)
                            if let Some(arg) = func_call.args.first() {
                                if let ExprKind::Ident(table_ident) = &arg.kind {
                                    return Ok(table_ident.name.clone());
                                }
                            }
                        }
                    }
                }
            }
            anyhow::bail!("No 'from' transform found in pipeline")
        }
        _ => anyhow::bail!("Expected pipeline expression")
    }
}

/// Walk the RenderExpr tree and attach auto-operations based on table name
///
/// For each FunctionCall with ColumnRef parameters that reference "this.",
/// attach an auto-operation for updating that column.
fn annotate_tree_with_operations(expr: &mut RenderExpr, table_name: &str) {

    match expr {
        RenderExpr::FunctionCall { name, args, operations } => {

            // Check each argument for direct column references
            for arg in args.iter() {
                if let (Some(param_name), RenderExpr::ColumnRef { name: col_name }) =
                    (&arg.name, &arg.value) {
                    // Strip "this." prefix if present
                    let field_name = col_name.strip_prefix("this.")
                        .unwrap_or(col_name);

                    // If the column reference uses "this." prefix, it's a direct column reference
                    // NOTE: This is legacy auto-operation code. In the new architecture, operations
                    // are discovered via OperationProvider during compile_query() in RenderEngine.
                    // This creates a placeholder descriptor - real operations should come from OperationProvider.
                    if col_name.starts_with("this.") {
                        operations.push(OperationWiring {
                            widget_type: name.clone(),
                            modified_param: param_name.clone(),
                            descriptor: OperationDescriptor {
                                entity_name: String::new(), // Will be filled by OperationProvider
                                table: table_name.to_string(),
                                id_column: "id".to_string(),
                                name: "set_field".to_string(),
                                display_name: format!("Set {}", field_name),
                                description: format!("Update {} field", field_name),
                                required_params: vec![
                                    OperationParam {
                                        name: "id".to_string(),
                                        type_hint: TypeHint::String,
                                        description: "Entity ID".to_string(),
                                    },
                                    OperationParam {
                                        name: "field".to_string(),
                                        type_hint: TypeHint::String,
                                        description: format!("Field name: {}", field_name),
                                    },
                                    OperationParam {
                                        name: "value".to_string(),
                                        type_hint: TypeHint::String, // "any" not supported, use String
                                        description: format!("New value for {}", field_name),
                                    },
                                ],
                                precondition: None,
                            },
                        });
                    }
                }
            }

            // Recurse into nested expressions
            for arg in args.iter_mut() {
                annotate_tree_with_operations(&mut arg.value, table_name);
            }
        }
        RenderExpr::Array { items } => {
            for item in items.iter_mut() {
                annotate_tree_with_operations(item, table_name);
            }
        }
        RenderExpr::BinaryOp { left, right, .. } => {
            annotate_tree_with_operations(left, table_name);
            annotate_tree_with_operations(right, table_name);
        }
        RenderExpr::Object { fields } => {
            for value in fields.values_mut() {
                annotate_tree_with_operations(value, table_name);
            }
        }
        _ => {}  // ColumnRef, Literal - no recursion needed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_function_call() {
        let prql = r#"
from blocks
render (text "Hello World")
        "#;

        let result = parse_query_render(prql);
        assert!(result.is_ok(), "Failed to parse: {:?}", result.err());

        let (_sql, spec) = result.unwrap();
        match spec.root {
            RenderExpr::FunctionCall { name, args, .. } => {
                assert_eq!(name, "text");
                assert_eq!(args.len(), 1);
            }
            _ => panic!("Expected function call"),
        }
    }

    #[test]
    fn test_with_column_reference() {
        let prql = r#"
from blocks
render (text content)
        "#;

        let result = parse_query_render(prql);
        assert!(result.is_ok(), "Failed to parse: {:?}", result.err());

        let (_sql, spec) = result.unwrap();
        match spec.root {
            RenderExpr::FunctionCall { name, args, .. } => {
                assert_eq!(name, "text");
                assert_eq!(args.len(), 1);
                match &args[0].value {
                    RenderExpr::ColumnRef { name } => {
                        assert_eq!(name, "content");
                    }
                    _ => panic!("Expected column ref"),
                }
            }
            _ => panic!("Expected function call"),
        }
    }

    #[test]
    fn test_nested_function_calls() {
        let prql = r#"
from blocks
render (row (text "A") (text "B"))
        "#;

        let result = parse_query_render(prql);
        assert!(result.is_ok(), "Failed to parse: {:?}", result.err());

        let (_sql, spec) = result.unwrap();
        match spec.root {
            RenderExpr::FunctionCall { name, args, .. } => {
                assert_eq!(name, "row");
                assert_eq!(args.len(), 2);
                for arg in &args {
                    match &arg.value {
                        RenderExpr::FunctionCall { name, .. } => {
                            assert_eq!(name, "text");
                        }
                        _ => panic!("Expected nested function call"),
                    }
                }
            }
            _ => panic!("Expected function call"),
        }
    }

    #[test]
    fn test_named_arguments() {
        let prql = r#"
from blocks
render (block indent:depth content:(text title))
        "#;

        let result = parse_query_render(prql);
        assert!(result.is_ok(), "Failed to parse: {:?}", result.err());

        let (_sql, spec) = result.unwrap();
        match spec.root {
            RenderExpr::FunctionCall { name, args, .. } => {
                assert_eq!(name, "block");
                assert!(args.iter().any(|a| a.name == Some("indent".to_string())));
                assert!(args.iter().any(|a| a.name == Some("content".to_string())));
            }
            _ => panic!("Expected function call"),
        }
    }

    #[test]
    fn test_helper_function_expansion() {
        let prql = r#"
let make_row = func -> (row (text "A") (text "B"))
from blocks
render (make_row)
        "#;

        let result = parse_query_render(prql);
        assert!(result.is_ok(), "Failed to expand helper function: {:?}", result.err());

        let (_sql, spec) = result.unwrap();
        match spec.root {
            RenderExpr::FunctionCall { name, args, .. } => {
                assert_eq!(name, "row");
                assert_eq!(args.len(), 2);
            }
            _ => panic!("Expected row function call from expansion"),
        }
    }

    #[test]
    fn test_helper_function_with_params() {
        let prql = r#"
let make_text = func content -> (text content)
from blocks
render (make_text "Hello")
        "#;

        let result = parse_query_render(prql);
        assert!(result.is_ok(), "Failed to expand parameterized function: {:?}", result.err());

        let (_sql, spec) = result.unwrap();
        match spec.root {
            RenderExpr::FunctionCall { name, args, .. } => {
                assert_eq!(name, "text");
                match &args[0].value {
                    RenderExpr::Literal { value } => {
                        assert_eq!(value, "Hello");
                    }
                    _ => panic!("Expected literal value after expansion"),
                }
            }
            _ => panic!("Expected text function call"),
        }
    }

    #[test]
    fn test_sql_generation() {
        let prql = r#"
from blocks
filter depth > 0
select [id, title, depth]
render (text title)
        "#;

        let result = parse_query_render(prql);
        assert!(result.is_ok(), "Failed to parse: {:?}", result.err());

        let (sql, _spec) = result.unwrap();
        assert!(sql.contains("SELECT"));
        assert!(sql.contains("FROM"));
        assert!(sql.contains("WHERE"));
        assert!(sql.to_lowercase().contains("depth"));
    }
}
