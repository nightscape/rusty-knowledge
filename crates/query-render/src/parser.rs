use anyhow::{Result, bail};
use prqlc::pr::*;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct QueryRenderSplit {
    pub query_module: ModuleDef,
    pub render_ast: Expr,
}

/// Parse PRQL source using PRQL's native parser and extract render() call
pub fn split_prql_at_render(source: &str) -> Result<QueryRenderSplit> {
    // Parse using PRQL's parser
    let mut module = prqlc::prql_to_pl(source)?;

    // Find and extract the render() call from the last statement
    let mut render_ast = extract_render_from_module(&mut module)?;

    // Expand user-defined functions in render() expression
    expand_functions_in_expr(&mut render_ast, &module)?;

    Ok(QueryRenderSplit {
        query_module: module,
        render_ast,
    })
}

/// Expand user-defined function calls in an expression
fn expand_functions_in_expr(expr: &mut Expr, module: &ModuleDef) -> Result<()> {
    match &mut expr.kind {
        ExprKind::FuncCall(func_call) => {
            // Recursively expand in function arguments
            for arg in &mut func_call.args {
                expand_functions_in_expr(arg, module)?;
            }
            for (_name, arg) in &mut func_call.named_args {
                expand_functions_in_expr(arg, module)?;
            }

            // Check if the function name is a user-defined function
            if let ExprKind::Ident(ident) = &func_call.name.kind {
                if ident.path.is_empty() {
                    if let Some(func_def) = find_function_in_module(&ident.name, module) {
                        // Expand the function call inline
                        *expr = expand_function_call(func_def, &func_call.args, &func_call.named_args)?;
                        // Recursively expand in the expanded result
                        expand_functions_in_expr(expr, module)?;
                    }
                }
            }
        }
        ExprKind::Ident(_) => {
            // Check if this identifier is a zero-argument function call
            if let ExprKind::Ident(ident) = &expr.kind {
                if ident.path.is_empty() {
                    if let Some(func_def) = find_function_in_module(&ident.name, module) {
                        // Expand zero-arg function
                        *expr = expand_function_call(func_def, &[], &std::collections::HashMap::new())?;
                        expand_functions_in_expr(expr, module)?;
                    }
                }
            }
        }
        ExprKind::Pipeline(pipeline) => {
            for e in &mut pipeline.exprs {
                expand_functions_in_expr(e, module)?;
            }
        }
        ExprKind::Array(items) | ExprKind::Tuple(items) => {
            for item in items {
                expand_functions_in_expr(item, module)?;
            }
        }
        ExprKind::Binary(binary) => {
            expand_functions_in_expr(&mut binary.left, module)?;
            expand_functions_in_expr(&mut binary.right, module)?;
        }
        _ => {}
    }
    Ok(())
}

/// Find a function definition in the module by name
fn find_function_in_module<'a>(name: &str, module: &'a ModuleDef) -> Option<&'a Func> {
    for stmt in &module.stmts {
        if let StmtKind::VarDef(var_def) = &stmt.kind {
            if var_def.name == name {
                if let Some(value) = &var_def.value {
                    if let ExprKind::Func(func) = &value.kind {
                        return Some(func);
                    }
                }
            }
        }
    }
    None
}

/// Expand a function call by substituting parameters
fn expand_function_call(
    func: &Func,
    args: &[Expr],
    named_args: &std::collections::HashMap<String, Expr>,
) -> Result<Expr> {
    // Build parameter substitution map
    let mut substitutions = std::collections::HashMap::new();

    // Map positional arguments to parameters
    for (i, arg) in args.iter().enumerate() {
        if i < func.params.len() {
            substitutions.insert(func.params[i].name.clone(), arg.clone());
        }
    }

    // Map named arguments to parameters
    for (name, arg) in named_args {
        substitutions.insert(name.clone(), arg.clone());
    }

    // Clone the function body and substitute parameters
    let mut expanded_body = (*func.body).clone();
    substitute_params(&mut expanded_body, &substitutions)?;

    Ok(expanded_body)
}

/// Substitute parameter references with actual arguments
fn substitute_params(expr: &mut Expr, substitutions: &std::collections::HashMap<String, Expr>) -> Result<()> {
    match &mut expr.kind {
        ExprKind::Ident(ident) => {
            if ident.path.is_empty() {
                if let Some(replacement) = substitutions.get(&ident.name) {
                    *expr = replacement.clone();
                }
            }
        }
        ExprKind::FuncCall(func_call) => {
            substitute_params(&mut func_call.name, substitutions)?;
            for arg in &mut func_call.args {
                substitute_params(arg, substitutions)?;
            }
            for (_name, arg) in &mut func_call.named_args {
                substitute_params(arg, substitutions)?;
            }
        }
        ExprKind::Pipeline(pipeline) => {
            for e in &mut pipeline.exprs {
                substitute_params(e, substitutions)?;
            }
        }
        ExprKind::Array(items) | ExprKind::Tuple(items) => {
            for item in items {
                substitute_params(item, substitutions)?;
            }
        }
        ExprKind::Binary(binary) => {
            substitute_params(&mut binary.left, substitutions)?;
            substitute_params(&mut binary.right, substitutions)?;
        }
        _ => {}
    }
    Ok(())
}

/// Extract render() call from the PRQL module
/// Modifies the module in-place to remove the render() call
fn extract_render_from_module(module: &mut ModuleDef) -> Result<Expr> {
    // PRQL queries are typically in a VarDef with kind Main
    for stmt in &mut module.stmts {
        match &mut stmt.kind {
            StmtKind::VarDef(var_def) => {
                // Check if this is the main query
                if matches!(var_def.kind, VarDefKind::Main) {
                    if let Some(value) = &mut var_def.value {
                        if let Some(render) = extract_render_from_expr(value) {
                            return Ok(render);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    bail!("No render() call found in PRQL source")
}

/// Extract render() from an expression if it's a pipeline ending with render()
/// Returns the render expression and modifies the input to remove it
fn extract_render_from_expr(expr: &mut Expr) -> Option<Expr> {
    match &mut expr.kind {
        ExprKind::Pipeline(pipeline) => {
            // Check if last element is render()
            if let Some(last) = pipeline.exprs.last() {
                if is_render_call(last) {
                    // Remove and return the render call
                    return pipeline.exprs.pop();
                }
            }
        }
        _ => {}
    }
    None
}

/// Check if an expression is a render() function call
fn is_render_call(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::FuncCall(func_call) => {
            // Check if the function name is "render"
            if let ExprKind::Ident(ident) = &func_call.name.kind {
                return ident.name == "render";
            }
            false
        }
        _ => false,
    }
}

/// Convert PRQL PR AST expression to JSON for easier processing
pub fn prql_ast_to_json(expr: &Expr) -> Result<Value> {
    match &expr.kind {
        ExprKind::Literal(lit) => {
            Ok(literal_to_json(lit))
        }
        ExprKind::Ident(ident) => {
            // Column reference - join path parts
            let full_name = if ident.path.is_empty() {
                ident.name.clone()
            } else {
                let mut parts = ident.path.clone();
                parts.push(ident.name.clone());
                parts.join(".")
            };
            Ok(Value::String(format!("$col:{}", full_name)))
        }
        ExprKind::FuncCall(func_call) => {
            let mut obj = serde_json::Map::new();

            // Get function name
            if let ExprKind::Ident(ident) = &func_call.name.kind {
                let func_name = if ident.path.is_empty() {
                    ident.name.clone()
                } else {
                    let mut parts = ident.path.clone();
                    parts.push(ident.name.clone());
                    parts.join(".")
                };
                obj.insert("__fn".to_string(), Value::String(func_name));
            } else {
                bail!("Expected identifier for function name");
            }

            // Process positional arguments
            for (idx, arg) in func_call.args.iter().enumerate() {
                obj.insert(format!("arg{}", idx), prql_ast_to_json(arg)?);
            }

            // Process named arguments
            for (name, value) in &func_call.named_args {
                obj.insert(name.clone(), prql_ast_to_json(value)?);
            }

            Ok(Value::Object(obj))
        }
        ExprKind::Tuple(items) | ExprKind::Array(items) => {
            let values: Result<Vec<_>> = items.iter().map(prql_ast_to_json).collect();
            Ok(Value::Array(values?))
        }
        ExprKind::Binary(binary) => {
            let mut obj = serde_json::Map::new();
            obj.insert("__op".to_string(), Value::String(format!("{:?}", binary.op)));
            obj.insert("left".to_string(), prql_ast_to_json(&binary.left)?);
            obj.insert("right".to_string(), prql_ast_to_json(&binary.right)?);
            Ok(Value::Object(obj))
        }
        _ => bail!("Unsupported expression type for render: {:?}", expr.kind),
    }
}

fn literal_to_json(lit: &Literal) -> Value {
    match lit {
        Literal::Null => Value::Null,
        Literal::Boolean(b) => Value::Bool(*b),
        Literal::Integer(n) => Value::Number((*n).into()),
        Literal::Float(f) => serde_json::Number::from_f64(*f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Literal::String(s) | Literal::RawString(s) => Value::String(s.clone()),
        Literal::Date(d) => Value::String(d.to_string()),
        Literal::Time(t) => Value::String(t.to_string()),
        Literal::Timestamp(ts) => Value::String(ts.to_string()),
        Literal::ValueAndUnit(v) => {
            let mut obj = serde_json::Map::new();
            obj.insert("value".to_string(), Value::Number(v.n.into()));
            obj.insert("unit".to_string(), Value::String(format!("{:?}", v.unit)));
            Value::Object(obj)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple() {
        let source = r#"
from tasks
filter priority > 2
render (list item_template:(block indent:10))
        "#;

        let split = split_prql_at_render(source).unwrap();

        // Check that render was extracted
        assert!(is_render_call(&split.render_ast));

        // Convert to JSON for inspection
        let json = prql_ast_to_json(&split.render_ast).unwrap();
        assert_eq!(json["__fn"], "render");
    }

    #[test]
    fn test_render_with_nested_calls() {
        let source = r#"
from tasks
render (row (checkbox checked:status) (text content))
        "#;

        let split = split_prql_at_render(source).unwrap();
        let json = prql_ast_to_json(&split.render_ast).unwrap();

        assert_eq!(json["__fn"], "render");
        // First arg should be row(...)
        assert_eq!(json["arg0"]["__fn"], "row");
    }

    #[test]
    fn test_function_expansion() {
        let source = r#"
let make_row = func -> (row (text "A") (text "B"))
from tasks
render (make_row)
        "#;

        let split = split_prql_at_render(source).unwrap();
        let json = prql_ast_to_json(&split.render_ast).unwrap();

        // Function should be expanded
        assert_eq!(json["__fn"], "render");
        assert_eq!(json["arg0"]["__fn"], "row");
        // Should have 2 children (text "A" and text "B")
        assert_eq!(json["arg0"]["arg0"]["__fn"], "text");
        assert_eq!(json["arg0"]["arg0"]["arg0"], "A");
        assert_eq!(json["arg0"]["arg1"]["__fn"], "text");
        assert_eq!(json["arg0"]["arg1"]["arg0"], "B");
    }

    #[test]
    fn test_function_expansion_with_params() {
        let source = r#"
let make_text = func content -> (text content)
from tasks
render (make_text "Hello")
        "#;

        let split = split_prql_at_render(source).unwrap();
        let json = prql_ast_to_json(&split.render_ast).unwrap();

        // Function should be expanded with parameter substitution
        assert_eq!(json["__fn"], "render");
        assert_eq!(json["arg0"]["__fn"], "text");
        assert_eq!(json["arg0"]["arg0"], "Hello");
    }
}
