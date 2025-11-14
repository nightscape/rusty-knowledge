use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields, Meta, parse_macro_input, ItemTrait, ItemFn, FnArg, Pat, Type};

#[proc_macro_derive(Entity, attributes(entity, primary_key, indexed, reference, lens))]
pub fn derive_entity(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    let name = &input.ident;
    let entity_name = extract_entity_name(&input.attrs);

    // Detect if we're in the rusty-knowledge crate itself or an external crate
    // We check the CARGO_PKG_NAME environment variable
    let pkg_name = std::env::var("CARGO_PKG_NAME").unwrap_or_default();
    let is_internal = pkg_name == "rusty-knowledge";

    // Use crate:: for internal use, rusty_knowledge:: for external
    let crate_path = if is_internal {
        quote! { crate }
    } else {
        quote! { rusty_knowledge }
    };

    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => &fields.named,
            _ => panic!("Entity can only be derived for structs with named fields"),
        },
        _ => panic!("Entity can only be derived for structs"),
    };

    let mut primary_key_field = None;
    let mut field_schemas = Vec::new();
    let mut lens_definitions = Vec::new();
    let mut to_entity_fields = Vec::new();
    let mut from_entity_fields = Vec::new();
    let mut schema_fields = Vec::new();

    for field in fields {
        let field_name = field.ident.as_ref().unwrap();
        let field_name_str = field_name.to_string();
        let field_type = &field.ty;

        let is_primary_key = field
            .attrs
            .iter()
            .any(|attr| attr.path().is_ident("primary_key"));

        let is_indexed = field
            .attrs
            .iter()
            .any(|attr| attr.path().is_ident("indexed"));

        let skip_lens = field.attrs.iter().any(|attr| {
            if attr.path().is_ident("lens")
                && let Meta::List(meta_list) = &attr.meta
            {
                let tokens_str = meta_list.tokens.to_string();
                return tokens_str == "skip";
            }
            false
        });

        let skip_serialization = field.attrs.iter().any(|attr| {
            if attr.path().is_ident("serde")
                && let Meta::List(meta_list) = &attr.meta
            {
                let tokens_str = meta_list.tokens.to_string();
                return tokens_str.contains("skip");
            }
            false
        });

        let reference_entity = field
            .attrs
            .iter()
            .find(|attr| attr.path().is_ident("reference"))
            .and_then(|attr| {
                if let Meta::List(meta_list) = &attr.meta {
                    let tokens = &meta_list.tokens;
                    Some(quote! { #tokens }.to_string())
                } else {
                    None
                }
            });

        if is_primary_key {
            primary_key_field = Some(field_name_str.clone());
        }

        let field_type_enum = if let Some(ref_entity) = reference_entity {
            quote! { #crate_path::storage::schema::FieldType::Reference(#ref_entity.to_string()) }
        } else {
            type_to_field_type(field_type, &crate_path)
        };

        let is_required = !is_option_type(field_type);

        field_schemas.push(quote! {
            #crate_path::storage::schema::FieldSchema {
                name: #field_name_str.to_string(),
                field_type: #field_type_enum,
                required: #is_required,
                indexed: #is_indexed,
            }
        });

        if !skip_lens {
            let lens_name = format_ident!("{}Lens", to_camel_case(&field_name_str));
            let inner_type = extract_inner_type(field_type);

            let get_impl = if is_option_type(field_type) {
                quote! { source.#field_name.clone() }
            } else {
                quote! { Some(source.#field_name.clone()) }
            };

            let set_impl = if is_option_type(field_type) {
                quote! { source.#field_name = Some(value); }
            } else {
                quote! { source.#field_name = value; }
            };

            lens_definitions.push(quote! {
                #[derive(Clone)]
                pub struct #lens_name;

                impl #crate_path::core::traits::Lens<#name, #inner_type> for #lens_name {
                    fn get(&self, source: &#name) -> Option<#inner_type> {
                        #get_impl
                    }

                    fn set(&self, source: &mut #name, value: #inner_type) {
                        #set_impl
                    }

                    fn field_name(&self) -> &'static str {
                        #field_name_str
                    }

                    fn sql_column(&self) -> &'static str {
                        #field_name_str
                    }
                }
            });
        }

        if !skip_serialization {
            let sql_type = rust_type_to_sql_type(field_type);
            let nullable = is_option_type(field_type);

            let mut field_schema_builder = quote! {
                #crate_path::core::traits::FieldSchema::new(#field_name_str, #sql_type)
            };

            if is_primary_key {
                field_schema_builder = quote! { #field_schema_builder.primary_key() };
            }

            if is_indexed {
                field_schema_builder = quote! { #field_schema_builder.indexed() };
            }

            if nullable {
                field_schema_builder = quote! { #field_schema_builder.nullable() };
            }

            schema_fields.push(field_schema_builder);
        }

        if !skip_serialization {
            to_entity_fields.push(quote! {
                entity.set(#field_name_str, self.#field_name.clone())
            });

            let from_entity_conversion = if is_option_type(field_type) {
                quote! {
                    #field_name: entity.get(#field_name_str).and_then(|v| v.clone().try_into().ok())
                }
            } else {
                quote! {
                    #field_name: entity.get(#field_name_str)
                        .and_then(|v| v.clone().try_into().ok())
                        .ok_or_else(|| format!("Missing or invalid field: {}", #field_name_str))?
                }
            };

            from_entity_fields.push(from_entity_conversion);
        } else {
            let default_value = if is_option_type(field_type) {
                quote! { #field_name: None }
            } else if is_vec_type(field_type) {
                quote! { #field_name: Vec::new() }
            } else {
                quote! { #field_name: Default::default() }
            };
            from_entity_fields.push(default_value);
        }
    }

    let primary_key = primary_key_field.unwrap_or_else(|| "id".to_string());

    let expanded = quote! {
        impl #name {
            pub fn entity_schema() -> #crate_path::storage::schema::EntitySchema {
                #crate_path::storage::schema::EntitySchema {
                    name: #entity_name.to_string(),
                    primary_key: #primary_key.to_string(),
                    fields: vec![
                        #(#field_schemas),*
                    ],
                }
            }
        }

        #(#lens_definitions)*

        impl #crate_path::core::traits::HasSchema for #name {
            fn schema() -> #crate_path::core::traits::Schema {
                #crate_path::core::traits::Schema::new(
                    #entity_name,
                    vec![
                        #(#schema_fields),*
                    ]
                )
            }

            fn to_entity(&self) -> #crate_path::core::entity::Entity {
                let mut entity = #crate_path::core::entity::Entity::new(#entity_name);
                #(#to_entity_fields;)*
                entity
            }

            fn from_entity(entity: #crate_path::core::entity::Entity) -> #crate_path::core::traits::Result<Self> {
                Ok(Self {
                    #(#from_entity_fields),*
                })
            }
        }
    };

    TokenStream::from(expanded)
}

fn extract_entity_name(attrs: &[syn::Attribute]) -> String {
    for attr in attrs {
        if attr.path().is_ident("entity")
            && let Meta::List(meta_list) = &attr.meta
        {
            let tokens_str = meta_list.tokens.to_string();
            if let Some(name) = tokens_str
                .strip_prefix("name = \"")
                .and_then(|s| s.strip_suffix("\""))
            {
                return name.to_string();
            }
        }
    }
    panic!("Entity derive macro requires #[entity(name = \"...\")]");
}

/// Parse provider_name from macro attribute: #[operations_trait(provider_name = "todoist")]
fn parse_provider_name(attr: &TokenStream) -> Option<String> {
    if attr.is_empty() {
        return None;
    }

    let attr_str = attr.to_string();
    // Look for provider_name = "value" pattern
    if let Some(start) = attr_str.find("provider_name") {
        if let Some(equals) = attr_str[start..].find('=') {
            let value_start = attr_str[start + equals + 1..].find('"')? + start + equals + 1;
            let value_end = attr_str[value_start + 1..].find('"')? + value_start + 1;
            return Some(attr_str[value_start + 1..value_end].to_string());
        }
    }
    None
}

fn is_option_type(ty: &syn::Type) -> bool {
    if let syn::Type::Path(type_path) = ty
        && let Some(segment) = type_path.path.segments.last()
    {
        return segment.ident == "Option";
    }
    false
}

fn is_vec_type(ty: &syn::Type) -> bool {
    if let syn::Type::Path(type_path) = ty
        && let Some(segment) = type_path.path.segments.last()
    {
        return segment.ident == "Vec";
    }
    false
}

fn type_to_field_type(ty: &syn::Type, crate_path: &proc_macro2::TokenStream) -> proc_macro2::TokenStream {
    let type_str = quote! { #ty }.to_string();

    let inner_type = if type_str.starts_with("Option <") {
        type_str
            .trim_start_matches("Option <")
            .trim_end_matches('>')
            .trim()
    } else {
        type_str.as_str()
    };

    match inner_type {
        "String" => quote! { #crate_path::storage::schema::FieldType::String },
        "i64" | "i32" | "u64" | "u32" | "usize" => {
            quote! { #crate_path::storage::schema::FieldType::Integer }
        }
        "bool" => quote! { #crate_path::storage::schema::FieldType::Boolean },
        t if t.contains("DateTime") => quote! { #crate_path::storage::schema::FieldType::DateTime },
        _ => quote! { #crate_path::storage::schema::FieldType::Json },
    }
}

fn rust_type_to_sql_type(ty: &syn::Type) -> String {
    let type_str = quote! { #ty }.to_string();

    let inner_type = if type_str.starts_with("Option <") {
        type_str
            .trim_start_matches("Option <")
            .trim_end_matches('>')
            .trim()
    } else {
        type_str.as_str()
    };

    match inner_type {
        "String" => "TEXT".to_string(),
        "i64" | "i32" | "u64" | "u32" | "usize" => "INTEGER".to_string(),
        "bool" => "INTEGER".to_string(),
        "f64" | "f32" => "REAL".to_string(),
        t if t.contains("DateTime") => "TEXT".to_string(),
        _ => "TEXT".to_string(),
    }
}

fn extract_inner_type(ty: &syn::Type) -> proc_macro2::TokenStream {
    let type_str = quote! { #ty }.to_string();

    if type_str.starts_with("Option <") {
        let inner = type_str
            .trim_start_matches("Option <")
            .trim_end_matches('>')
            .trim();
        let inner_ident = syn::Ident::new(inner, proc_macro2::Span::call_site());
        quote! { #inner_ident }
    } else {
        quote! { #ty }
    }
}

fn to_camel_case(s: &str) -> String {
    s.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().chain(chars).collect(),
            }
        })
        .collect()
}

fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            result.push('_');
        }
        result.push(c.to_lowercase().next().unwrap_or(c));
    }
    result
}

fn to_display_name(s: &str) -> String {
    // Convert snake_case or camelCase to Title Case
    // e.g., "set_completion" -> "Set Completion", "indentBlock" -> "Indent Block"
    let mut result = String::new();
    let mut capitalize_next = true;

    for c in s.chars() {
        if c == '_' {
            result.push(' ');
            capitalize_next = true;
        } else if c.is_uppercase() && !result.is_empty() {
            result.push(' ');
            result.push(c);
            capitalize_next = false;
        } else if capitalize_next {
            result.push(c.to_uppercase().next().unwrap_or(c));
            capitalize_next = false;
        } else {
            result.push(c);
        }
    }

    result
}

/// Generate operation descriptors for all async methods in a trait
///
/// This macro generates:
/// - One function `fn OPERATION_NAME_OP() -> OperationDescriptor` per async method
/// - One function `fn TRAIT_NAME_operations() -> Vec<OperationDescriptor>` returning all operations
/// - A module `__operations_trait_name` (snake_case) containing all operations
///
/// Usage:
/// ```rust
/// #[operations_trait]
/// #[async_trait]
/// pub trait CrudOperationProvider<T>: Send + Sync {
///     /// Set single field
///     async fn set_field(&self, id: &str, field: &str, value: Value) -> Result<()>;
/// }
/// ```
///
/// The generated operations can be accessed via:
/// ```rust
/// use crate::core::datasource::mutable_data_source_operations;
/// let ops = mutable_data_source_operations();
/// ```
#[proc_macro_attribute]
pub fn operations_trait(attr: TokenStream, item: TokenStream) -> TokenStream {
    let trait_def = parse_macro_input!(item as ItemTrait);

    // Parse provider_name from attribute: #[operations_trait(provider_name = "todoist")]
    let provider_name = parse_provider_name(&attr);

    let trait_name = &trait_def.ident;
    let operations_fn_name = format_ident!("{}_operations", to_snake_case(&trait_name.to_string()));
    let operations_module_name = format_ident!("__operations_{}", to_snake_case(&trait_name.to_string()));

    // Extract where clause constraints for the entity type parameter
    // Look for constraints on the generic parameter (usually T or E)
    // We need to map T -> E in the constraints
    let entity_constraints: Vec<_> = trait_def.generics.where_clause.as_ref()
        .map(|where_clause| {
            where_clause.predicates.iter()
                .filter_map(|pred| {
                    // Look for type bounds like `T: BlockEntity + Send + Sync`
                    if let syn::WherePredicate::Type(pred_type) = pred {
                        // Replace the type parameter name (T) with E in the predicate
                        // This is a simplified approach - we assume the first generic param is the entity type
                        let mut new_pred = pred_type.clone();
                        // Replace T with E in the type path
                        if let syn::Type::Path(type_path) = &mut new_pred.bounded_ty {
                            if let Some(segment) = type_path.path.segments.first_mut() {
                                if segment.ident == "T" {
                                    segment.ident = syn::Ident::new("E", segment.ident.span());
                                }
                            }
                        }
                        Some(quote! { #new_pred })
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    // Detect crate path for Result type and Value types (needed for dispatch function generation)
    let pkg_name = std::env::var("CARGO_PKG_NAME").unwrap_or_default();
    let is_internal = pkg_name == "rusty-knowledge";
    let crate_path = if is_internal {
        quote! { crate }
    } else {
        quote! { rusty_knowledge }
    };

    // Extract all async fn methods (skip associated types, consts, etc.)
    let methods: Vec<_> = trait_def.items.iter()
        .filter_map(|item| {
            // In syn 2.0, methods are TraitItem::Fn
            if let syn::TraitItem::Fn(method) = item {
                // Check if method is async (has asyncness)
                if method.sig.asyncness.is_some() {
                    Some(method)
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    // Generate OperationDescriptor function for each method
    let operation_fns: Vec<_> = methods.iter()
        .map(|method| {
            let method_name = &method.sig.ident;
            let fn_name = format_ident!("{}_OP", method_name.to_string().to_uppercase());

            // Extract doc comments for description
            let description = extract_doc_comments(&method.attrs);

            // Extract parameters (skip &self)
            let params: Vec<_> = method.sig.inputs.iter()
                .skip(1)  // Skip &self
                .filter_map(|arg| match arg {
                    FnArg::Typed(pat_type) => {
                        let param_name = extract_param_name(&pat_type.pat);
                        let (type_str, _required) = infer_type(&pat_type.ty);
                        let param_name_lit = param_name.clone();
                        let type_str_lit = type_str.clone();

                        // Parse type hint with entity ID detection
                        let type_hint_expr = parse_param_type_hint(
                            &param_name,
                            &pat_type.attrs,
                            &type_str_lit,
                        );

                        Some(quote! {
                            query_render::OperationParam {
                                name: #param_name_lit.to_string(),
                                type_hint: #type_hint_expr,
                                description: String::new(), // TODO: Extract from doc comments
                            }
                        })
                    }
                    _ => None,
                })
                .collect();

            // Use stringify! for name and description (compile-time strings)
            let name_lit = method_name.to_string();
            let display_name = to_display_name(&name_lit);
            let desc_lit = if description.is_empty() {
                format!("Execute {}", display_name)
            } else {
                description.clone()
            };

            // Extract and generate precondition if present
            let precondition_field = if let Some(precondition_tokens) = extract_require_precondition(&method.attrs) {
                let precondition_closure = generate_precondition_closure(method, &precondition_tokens, &crate_path);
                quote! {
                    precondition: Some(#precondition_closure),
                }
            } else {
                quote! {
                    precondition: None,
                }
            };

            // Construct entity_name: if provider_name is set, use "{provider_name}.{operation_name}", otherwise use passed entity_name
            let entity_name_expr = if let Some(ref provider) = provider_name {
                let provider_lit = provider.clone();
                let operation_name_lit = name_lit.clone();
                quote! {
                    format!("{}.{}", #provider_lit, #operation_name_lit)
                }
            } else {
                quote! {
                    entity_name.to_string()
                }
            };

            quote! {
                /// Generate operation descriptor for this method
                ///
                /// Parameters:
                /// - entity_name: Entity identifier (e.g., "todoist-task", "logseq-block")
                ///   Note: If provider_name is set in macro, entity_name will be "{provider_name}.{operation_name}"
                /// - table: Database table name (e.g., "todoist_tasks", "logseq_blocks")
                /// - id_column: Primary key column name (default: "id")
                pub fn #fn_name(
                    entity_name: &str,
                    table: &str,
                    id_column: &str
                ) -> query_render::OperationDescriptor {
                    query_render::OperationDescriptor {
                        entity_name: #entity_name_expr,
                        table: table.to_string(),
                        id_column: id_column.to_string(),
                        name: #name_lit.to_string(),
                        display_name: #display_name.to_string(),
                        description: #desc_lit.to_string(),
                        required_params: vec![
                            #(#params),*
                        ],
                        #precondition_field
                    }
                }
            }
        })
        .collect();

    // Generate dispatch function code for each method
    let dispatch_cases: Vec<_> = methods.iter()
        .map(|method| {
            let method_name = &method.sig.ident;
            let method_name_str = method_name.to_string();

            // Extract parameters and generate extraction code, building both lists together
            let mut param_extractions_code = Vec::new();
            let mut param_names_for_call = Vec::new();

            for arg in method.sig.inputs.iter().skip(1) {  // Skip &self
                if let FnArg::Typed(pat_type) = arg {
                    let param_name_ident = match &*pat_type.pat {
                        Pat::Ident(pat_ident) => pat_ident.ident.clone(),
                        _ => {
                            // Fallback: try to extract from string
                            let name_str = extract_param_name(&pat_type.pat);
                            syn::Ident::new(&name_str, proc_macro2::Span::call_site())
                        }
                    };
                    let param_name_str = param_name_ident.to_string();
                    let (type_str, is_required) = infer_type(&pat_type.ty);
                    let is_optional = !is_required;  // Convert required flag to optional flag
                    let type_str_cleaned = type_str.replace(" ", "");

                    // Check if original type was a reference (for &str handling)
                    // Check the actual type structure, not stringified version
                    let is_ref_type = matches!(&*pat_type.ty, syn::Type::Reference(_));

                    // For Option<&str>, check if inner type is a reference
                    let is_option_ref_str = if is_optional {
                        if let syn::Type::Path(type_path) = &*pat_type.ty {
                            if let Some(segment) = type_path.path.segments.last() {
                                if segment.ident == "Option" {
                                    if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                                        if let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() {
                                            matches!(inner_ty, syn::Type::Reference(_))
                                        } else {
                                            false
                                        }
                                    } else {
                                        false
                                    }
                                } else {
                                    false
                                }
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    } else {
                        false
                    };

                    // Generate extraction code based on type
                    let extraction = if type_str_cleaned == "String" || type_str_cleaned == "&str" {
                        if is_optional {
                            quote! {
                                let #param_name_ident: Option<String> = params.get(#param_name_str)
                                    .and_then(|v| v.as_string().map(|s| s.to_string()));
                            }
                        } else {
                            quote! {
                                let #param_name_ident: String = params.get(#param_name_str)
                                    .and_then(|v| v.as_string().map(|s| s.to_string()))
                                    .ok_or_else(|| format!("Missing or invalid parameter: {}", #param_name_str))?;
                            }
                        }
                    } else if type_str_cleaned == "bool" {
                        if is_optional {
                            quote! {
                                let #param_name_ident: Option<bool> = params.get(#param_name_str)
                                    .and_then(|v| v.as_bool());
                            }
                        } else {
                            quote! {
                                let #param_name_ident: bool = params.get(#param_name_str)
                                    .and_then(|v| v.as_bool())
                                    .ok_or_else(|| format!("Missing or invalid parameter: {}", #param_name_str))?;
                            }
                        }
                    } else if type_str_cleaned.starts_with("i64") {
                        if is_optional {
                            quote! {
                                let #param_name_ident: Option<i64> = params.get(#param_name_str)
                                    .and_then(|v| v.as_i64());
                            }
                        } else {
                            quote! {
                                let #param_name_ident: i64 = params.get(#param_name_str)
                                    .and_then(|v| v.as_i64())
                                    .ok_or_else(|| format!("Missing or invalid parameter: {}", #param_name_str))?;
                            }
                        }
                    } else if type_str_cleaned.starts_with("i32") {
                        if is_optional {
                            quote! {
                                let #param_name_ident: Option<i32> = params.get(#param_name_str)
                                    .and_then(|v| v.as_i64().map(|i| i as i32));
                            }
                        } else {
                            quote! {
                                let #param_name_ident: i32 = params.get(#param_name_str)
                                    .and_then(|v| v.as_i64().map(|i| i as i32))
                                    .ok_or_else(|| format!("Missing or invalid parameter: {}", #param_name_str))?;
                            }
                        }
                    } else if type_str_cleaned == "HashMap" {
                        // For HashMap<String, Value>, extract the whole StorageEntity
                        // Check original type to confirm it's HashMap<String, Value>
                        let original_type_str = quote! { #pat_type.ty }.to_string();
                        let original_type_contains_value = original_type_str.contains("Value");
                        if original_type_contains_value {
                            let crate_path_clone = crate_path.clone();
                            quote! {
                                let #param_name_ident: std::collections::HashMap<String, #crate_path_clone::storage::types::Value> = params.clone();
                            }
                        } else {
                            // Fallback for other HashMap types
                            let crate_path_clone = crate_path.clone();
                            quote! {
                                let #param_name_ident: #crate_path_clone::storage::types::Value = params.get(#param_name_str)
                                    .cloned()
                                    .ok_or_else(|| format!("Missing parameter: {}", #param_name_str))?;
                            }
                        }
                    } else if is_optional && type_str_cleaned.contains("DateTime") {
                        quote! {
                            let #param_name_ident: Option<chrono::DateTime<chrono::Utc>> = params.get(#param_name_str)
                                .and_then(|v| v.as_datetime().cloned());
                        }
                    } else if type_str_cleaned == "Value" {
                        // For Value type, clone directly
                        let crate_path_clone = crate_path.clone();
                        if is_optional {
                            quote! {
                                let #param_name_ident: Option<#crate_path_clone::storage::types::Value> = params.get(#param_name_str).cloned();
                            }
                        } else {
                            quote! {
                                let #param_name_ident: #crate_path_clone::storage::types::Value = params.get(#param_name_str)
                                    .cloned()
                                    .ok_or_else(|| format!("Missing parameter: {}", #param_name_str))?;
                            }
                        }
                    } else {
                        // For other types, try to clone Value and let the trait method handle conversion
                        let crate_path_clone = crate_path.clone();
                        quote! {
                            let #param_name_ident: #crate_path_clone::storage::types::Value = params.get(#param_name_str)
                                .cloned()
                                .ok_or_else(|| format!("Missing parameter: {}", #param_name_str))?;
                        }
                    };

                    param_extractions_code.push(extraction);

                    // If parameter type is &str, we need to borrow the String
                    // Also handle Option<&str> specially
                    if (is_ref_type && type_str_cleaned == "String") || is_option_ref_str {
                        if is_optional {
                            // For Option<&str>, extract as Option<String> and borrow
                            param_names_for_call.push(quote! { #param_name_ident.as_ref().map(|s| s.as_str()) });
                        } else {
                            param_names_for_call.push(quote! { &*#param_name_ident });
                        }
                    } else {
                        param_names_for_call.push(quote! { #param_name_ident });
                    }
                }
            }

            // Check return type - if it's Result<String>, map to Result<()>
            let return_type_str = quote! { #method.sig.output }.to_string();
            let return_handling = if return_type_str.contains("Result<String") ||
                                      return_type_str.contains("Result< String") ||
                                      return_type_str.contains("Result < String") ||
                                      (return_type_str.contains("Result") && return_type_str.contains("String") && method_name_str == "create") {
                quote! {
                    target.#method_name(#(#param_names_for_call),*).await.map(|_| ())
                }
            } else {
                quote! {
                    target.#method_name(#(#param_names_for_call),*).await
                }
            };

            quote! {
                #method_name_str => {
                    #(#param_extractions_code)*
                    #return_handling
                }
            }
        })
        .collect();

    // Generate function calls for the operations array
    let operation_calls: Vec<_> = methods.iter()
        .map(|method| {
            let method_name = &method.sig.ident;
            let fn_name = format_ident!("{}_OP", method_name.to_string().to_uppercase());
            quote! { #fn_name(entity_name, table, id_column) }
        })
        .collect();

    let expanded = quote! {
        // Original trait (unchanged)
        #trait_def

        // Generated operations module
        #[doc(hidden)]
        pub mod #operations_module_name {
            use super::*;
            use #crate_path::storage::types::{StorageEntity, Value};
            use #crate_path::core::datasource::Result;

            #(#operation_fns)*

            /// All operations for this trait
            ///
            /// Parameters:
            /// - entity_name: Entity identifier (e.g., "todoist-task", "logseq-block")
            /// - table: Database table name (e.g., "todoist_tasks", "logseq_blocks")
            /// - id_column: Primary key column name (default: "id")
            pub fn #operations_fn_name(
                entity_name: &str,
                table: &str,
                id_column: &str
            ) -> Vec<query_render::OperationDescriptor> {
                vec![
                    #(#operation_calls),*
                ]
            }

            /// Dispatch operation to appropriate trait method
            ///
            /// Extracts parameters from StorageEntity and calls the appropriate trait method.
            /// Returns an error if the operation name is not recognized or parameters are invalid.
            ///
            /// Note: The entity type `E` must satisfy all constraints required by the trait.
            /// For example, `MutableBlockDataSource<E>` requires `E: BlockEntity`.
            pub async fn dispatch_operation<DS, E>(
                target: &DS,
                op_name: &str,
                params: &StorageEntity
            ) -> Result<()>
            where
                DS: #trait_name<E> + Send + Sync,
                E: Send + Sync + 'static,
                #(#entity_constraints),*
            {
                match op_name {
                    #(#dispatch_cases),*
                    _ => Err(format!("Unknown operation: {} for trait {}", op_name, stringify!(#trait_name)).into())
                }
            }

            // Helper: Extract entity type constraints from trait where clause
            // This is a workaround - the dispatch function should ideally extract these automatically
            // For now, callers must ensure E satisfies all trait constraints
        }
    };

    TokenStream::from(expanded)
}

/// Extract doc comments from attributes
fn extract_doc_comments(attrs: &[syn::Attribute]) -> String {
    let mut docs = Vec::new();
    for attr in attrs {
        if attr.path().is_ident("doc") {
            // Handle both NameValue (/// doc) and List (/// doc) formats
            match &attr.meta {
                Meta::NameValue(meta) => {
                    if let syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) = &meta.value {
                        let doc = s.value();
                        let cleaned = doc.trim();
                        if !cleaned.is_empty() {
                            docs.push(cleaned.to_string());
                        }
                    }
                }
                Meta::List(meta_list) => {
                    // Try to parse as a string literal
                    let tokens = &meta_list.tokens;
                    let token_str = quote! { #tokens }.to_string();
                    // Remove quotes if present
                    let cleaned = token_str
                        .strip_prefix('"')
                        .and_then(|s| s.strip_suffix('"'))
                        .unwrap_or(&token_str)
                        .trim();
                    if !cleaned.is_empty() {
                        docs.push(cleaned.to_string());
                    }
                }
                _ => {}
            }
        }
    }
    docs.join(" ")
}

/// Extract require precondition tokens from attributes
///
/// Returns the combined tokens from all #[require(...)] attributes,
/// combined with && operator if multiple exist.
fn extract_require_precondition(attrs: &[syn::Attribute]) -> Option<proc_macro2::TokenStream> {
    let mut preconditions = Vec::new();

    for attr in attrs {
        // Check if this is a require attribute (either #[require(...)] or #[rusty_knowledge_macros::require(...)])
        let is_require = attr.path().is_ident("require") ||
            (attr.path().segments.len() == 2 &&
             attr.path().segments[0].ident == "rusty_knowledge_macros" &&
             attr.path().segments[1].ident == "require");

        if is_require {
            if let Meta::List(meta_list) = &attr.meta {
                preconditions.push(meta_list.tokens.clone());
            }
        }
    }

    if preconditions.is_empty() {
        None
    } else if preconditions.len() == 1 {
        Some(preconditions[0].clone())
    } else {
        // Combine multiple preconditions with &&
        let mut combined = preconditions[0].clone();
        for prec in preconditions.iter().skip(1) {
            combined = quote! { (#combined) && (#prec) };
        }
        Some(combined)
    }
}

/// Generate precondition closure code for a method
///
/// Creates a closure that extracts parameters from HashMap<String, Box<dyn Any>>,
/// converts them to the appropriate types, and evaluates the precondition expression.
fn generate_precondition_closure(
    method: &syn::TraitItemFn,
    precondition_tokens: &proc_macro2::TokenStream,
    crate_path: &proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    // Generate parameter extraction and type conversion code
    let mut param_declarations = Vec::new();

    for arg in method.sig.inputs.iter().skip(1) {  // Skip &self
        if let FnArg::Typed(pat_type) = arg {
            let param_name_ident = match &*pat_type.pat {
                Pat::Ident(pat_ident) => pat_ident.ident.clone(),
                _ => {
                    let name_str = extract_param_name(&pat_type.pat);
                    syn::Ident::new(&name_str, proc_macro2::Span::call_site())
                }
            };
            let param_name_str = param_name_ident.to_string();
            let (type_str, is_required) = infer_type(&pat_type.ty);
            let is_optional = !is_required;
            let type_str_cleaned = type_str.replace(" ", "");

            let is_ref_type = matches!(&*pat_type.ty, syn::Type::Reference(_));

            // Generate code to extract and convert the parameter
            // Chain the operations: downcast from Any to Value, then convert to target type
            let type_conversion = if type_str_cleaned == "String" || type_str_cleaned == "&str" {
                if is_optional {
                    quote! {
                        let #param_name_ident: Option<String> = params.get(#param_name_str)
                            .and_then(|any_val| {
                                any_val.downcast_ref::<#crate_path::storage::types::Value>()
                                    .and_then(|v| v.as_string().map(|s| s.to_string()))
                            });
                    }
                } else {
                    quote! {
                        let #param_name_ident: String = params.get(#param_name_str)
                            .and_then(|any_val| {
                                any_val.downcast_ref::<#crate_path::storage::types::Value>()
                                    .and_then(|v| v.as_string().map(|s| s.to_string()))
                            })
                            .ok_or_else(|| format!("Missing or invalid parameter: {}", #param_name_str))?;
                    }
                }
            } else if type_str_cleaned == "bool" {
                if is_optional {
                    quote! {
                        let #param_name_ident: Option<bool> = params.get(#param_name_str)
                            .and_then(|any_val| {
                                any_val.downcast_ref::<#crate_path::storage::types::Value>()
                                    .and_then(|v| v.as_bool())
                            });
                    }
                } else {
                    quote! {
                        let #param_name_ident: bool = params.get(#param_name_str)
                            .and_then(|any_val| {
                                any_val.downcast_ref::<#crate_path::storage::types::Value>()
                                    .and_then(|v| v.as_bool())
                            })
                            .ok_or_else(|| format!("Missing or invalid parameter: {}", #param_name_str))?;
                    }
                }
            } else if type_str_cleaned.starts_with("i64") {
                if is_optional {
                    quote! {
                        let #param_name_ident: Option<i64> = params.get(#param_name_str)
                            .and_then(|any_val| {
                                any_val.downcast_ref::<#crate_path::storage::types::Value>()
                                    .and_then(|v| v.as_i64())
                            });
                    }
                } else {
                    quote! {
                        let #param_name_ident: i64 = params.get(#param_name_str)
                            .and_then(|any_val| {
                                any_val.downcast_ref::<#crate_path::storage::types::Value>()
                                    .and_then(|v| v.as_i64())
                            })
                            .ok_or_else(|| format!("Missing or invalid parameter: {}", #param_name_str))?;
                    }
                }
            } else if type_str_cleaned.starts_with("i32") {
                if is_optional {
                    quote! {
                        let #param_name_ident: Option<i32> = params.get(#param_name_str)
                            .and_then(|any_val| {
                                any_val.downcast_ref::<#crate_path::storage::types::Value>()
                                    .and_then(|v| v.as_i64().map(|i| i as i32))
                            });
                    }
                } else {
                    quote! {
                        let #param_name_ident: i32 = params.get(#param_name_str)
                            .and_then(|any_val| {
                                any_val.downcast_ref::<#crate_path::storage::types::Value>()
                                    .and_then(|v| v.as_i64().map(|i| i as i32))
                            })
                            .ok_or_else(|| format!("Missing or invalid parameter: {}", #param_name_str))?;
                    }
                }
            } else if is_optional && type_str_cleaned.contains("DateTime") {
                quote! {
                    let #param_name_ident: Option<chrono::DateTime<chrono::Utc>> = params.get(#param_name_str)
                        .and_then(|any_val| {
                            any_val.downcast_ref::<#crate_path::storage::types::Value>()
                                .and_then(|v| v.as_datetime().cloned())
                        });
                }
            } else {
                // For other types, try to use Value directly or return error
                let crate_path_clone = crate_path.clone();
                if is_optional {
                    quote! {
                        let #param_name_ident: Option<#crate_path_clone::storage::types::Value> = params.get(#param_name_str)
                            .and_then(|any_val| {
                                any_val.downcast_ref::<#crate_path_clone::storage::types::Value>().cloned()
                            });
                    }
                } else {
                    quote! {
                        let #param_name_ident: #crate_path_clone::storage::types::Value = params.get(#param_name_str)
                            .and_then(|any_val| {
                                any_val.downcast_ref::<#crate_path_clone::storage::types::Value>().cloned()
                            })
                            .ok_or_else(|| format!("Missing parameter: {}", #param_name_str))?;
                    }
                }
            };

            param_declarations.push(type_conversion);

            // For reference types, we need to handle borrowing
            if is_ref_type && (type_str_cleaned == "String" || type_str_cleaned == "&str") {
                // Store as String, will borrow in precondition expression if needed
                // The precondition code can use &param_name_ident to get &str
            }
        }
    }

    // Generate the closure that wraps everything
    quote! {
        {
            use std::sync::Arc;
            use std::any::Any;
            use std::collections::HashMap;

            Arc::new(Box::new(move |params: &HashMap<String, Box<dyn Any + Send + Sync>>| -> std::result::Result<bool, String> {
                #(#param_declarations)*
                Ok(#precondition_tokens)
            }) as Box<query_render::PreconditionChecker>)
        }
    }
}

/// Extract parameter name from pattern
fn extract_param_name(pat: &Pat) -> String {
    match pat {
        Pat::Ident(pat_ident) => pat_ident.ident.to_string(),
        Pat::Wild(_) => "_".to_string(),
        _ => {
            // Fallback: try to stringify the pattern
            quote! { #pat }.to_string()
        }
    }
}

/// Infer type string and required flag from Rust type
fn infer_type(ty: &Type) -> (String, bool) {
    let type_str = quote! { #ty }.to_string();
    let cleaned = type_str.replace(" ", "");

    // Check if it's an Option type
    if cleaned.starts_with("Option<") {
        let inner = cleaned
            .strip_prefix("Option<")
            .and_then(|s| s.strip_suffix(">"))
            .unwrap_or(&cleaned);
        let inner_type = infer_type_string(inner);
        return (inner_type, false);
    }

    // Check for reference types (strip & but don't affect required flag)
    let inner = if cleaned.starts_with("&") {
        cleaned.strip_prefix("&").unwrap_or(&cleaned)
    } else {
        cleaned.as_str()
    };

    let type_str = infer_type_string(inner);
    (type_str, true)
}

/// Infer type string from cleaned type name
fn infer_type_string(type_str: &str) -> String {
    // Remove lifetime parameters
    let without_lifetime = type_str.split('<').next().unwrap_or(type_str);

    match without_lifetime {
        "str" => "String".to_string(),
        "String" => "String".to_string(),
        "i64" => "i64".to_string(),
        "i32" => "i32".to_string(),
        "u64" => "u64".to_string(),
        "u32" => "u32".to_string(),
        "usize" => "usize".to_string(),
        "bool" => "bool".to_string(),
        "f64" => "f64".to_string(),
        "f32" => "f32".to_string(),
        s if s.contains("HashMap") => "HashMap".to_string(),
        s if s.contains("Vec") => "Vec".to_string(),
        s if s.contains("DateTime") => "DateTime".to_string(),
        s if s.contains("Value") => "Value".to_string(),
        _ => type_str.to_string(),
    }
}

/// Parse parameter type hint with entity ID detection
///
/// Detects entity references based on parameter name convention ({entity_name}_id)
/// and supports attribute overrides (#[entity_ref("name")] and #[not_entity]).
fn parse_param_type_hint(
    param_name: &str,
    attrs: &[syn::Attribute],
    rust_type_str: &str,
) -> proc_macro2::TokenStream {
    // Check for explicit override attributes
    let mut entity_ref_override: Option<String> = None;
    let mut not_entity = false;

    for attr in attrs {
        // Check for #[entity_ref("name")]
        if attr.path().is_ident("entity_ref") {
            if let Meta::List(meta_list) = &attr.meta {
                let tokens = &meta_list.tokens;
                // Try to extract string literal from tokens
                let token_str = quote! { #tokens }.to_string();
                // Remove quotes if present
                if let Some(stripped) = token_str.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                    entity_ref_override = Some(stripped.to_string());
                }
            }
        }

        // Check for #[not_entity]
        if attr.path().is_ident("not_entity") {
            not_entity = true;
        }
    }

    // Generate TypeHint enum expression
    if let Some(entity_name) = entity_ref_override {
        // Explicit override: use provided entity name
        quote! {
            query_render::TypeHint::EntityId {
                entity_name: #entity_name.to_string(),
            }
        }
    } else if not_entity {
        // Explicitly not an entity - infer from Rust type
        infer_type_hint_from_rust_type(rust_type_str)
    } else if param_name.ends_with("_id") {
        // Convention: {entity_name}_id → EntityId
        let entity_name = param_name.strip_suffix("_id").unwrap();
        let entity_name_lit = entity_name.to_string();
        quote! {
            query_render::TypeHint::EntityId {
                entity_name: #entity_name_lit.to_string(),
            }
        }
    } else {
        // Infer from Rust type
        infer_type_hint_from_rust_type(rust_type_str)
    }
}

/// Infer TypeHint from Rust type string
fn infer_type_hint_from_rust_type(rust_type_str: &str) -> proc_macro2::TokenStream {
    match rust_type_str {
        "String" | "&str" | "str" => {
            quote! { query_render::TypeHint::String }
        }
        "bool" => {
            quote! { query_render::TypeHint::Bool }
        }
        "i64" | "i32" | "u64" | "u32" | "usize" | "integer" => {
            quote! { query_render::TypeHint::Number }
        }
        s if s.contains("DateTime") => {
            // DateTime is still a string in our type system
            quote! { query_render::TypeHint::String }
        }
        _ => {
            // Default fallback to String
            quote! { query_render::TypeHint::String }
        }
    }
}

/// Generate an OperationDescriptor for a standalone async function
///
/// This macro generates a const `OPERATION_NAME_OP: OperationDescriptor` for a single function.
/// Useful for operations that aren't part of a trait.
///
/// Usage:
/// ```rust
/// #[operation]
/// /// Delete a block by ID
/// async fn delete_block(id: &str) -> Result<()> {
///     // Implementation
/// }
/// ```
///
/// The generated descriptor can be accessed via:
/// ```rust
/// use crate::operations::DELETE_BLOCK_OP;
/// let op = DELETE_BLOCK_OP();
/// ```
#[proc_macro_attribute]
pub fn operation(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let fn_item = parse_macro_input!(item as ItemFn);

    // Detect crate path (same logic as Entity macro)
    let pkg_name = std::env::var("CARGO_PKG_NAME").unwrap_or_default();
    let is_internal = pkg_name == "rusty-knowledge";
    let crate_path = if is_internal {
        quote! { crate }
    } else {
        quote! { rusty_knowledge }
    };

    let fn_name = &fn_item.sig.ident;
    let const_name = format_ident!("{}_OP", fn_name.to_string().to_uppercase());

    // Extract doc comments for description
    let description = extract_doc_comments(&fn_item.attrs);

    // Extract parameters (skip &self if present)
    let params: Vec<_> = fn_item.sig.inputs.iter()
        .filter_map(|arg| match arg {
            FnArg::Receiver(_) => None, // Skip &self
            FnArg::Typed(pat_type) => {
                let param_name = extract_param_name(&pat_type.pat);
                let (type_str, required) = infer_type(&pat_type.ty);
                let param_name_lit = param_name.clone();
                let type_str_lit = type_str.clone();
                Some(quote! {
                    #crate_path::core::datasource::ParamDescriptor {
                        name: #param_name_lit.to_string(),
                        param_type: #type_str_lit.to_string(),
                        required: #required,
                        default: None,
                    }
                })
            }
        })
        .collect();

    let name_lit = fn_name.to_string();
    let desc_lit = if description.is_empty() {
        String::new()
    } else {
        description.clone()
    };

    let expanded = quote! {
        // Original function (unchanged)
        #fn_item

        // Generated operation descriptor
        pub fn #const_name() -> #crate_path::core::datasource::OperationDescriptor {
            #crate_path::core::datasource::OperationDescriptor {
                name: #name_lit.to_string(),
                description: #desc_lit.to_string(),
                params: vec![
                    #(#params),*
                ],
            }
        }
    };

    TokenStream::from(expanded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::{parse_quote, TraitItemFn};

    #[test]
    fn test_extract_require_precondition_single() {
        // Create a method with a single require attribute
        let method: TraitItemFn = parse_quote! {
            #[require(id.len() > 0)]
            async fn delete(&self, id: &str) -> Result<()>;
        };

        let result = extract_require_precondition(&method.attrs);
        assert!(result.is_some(), "Should extract precondition");
        let tokens = result.unwrap();
        let code = quote! { #tokens }.to_string();
        // The code might have extra formatting, so check for key parts
        assert!(code.contains("id") && code.contains("len"), "Should contain the precondition code");
    }

    #[test]
    fn test_extract_require_precondition_multiple() {
        // Create a method with multiple require attributes
        let method: TraitItemFn = parse_quote! {
            #[require(priority >= 1)]
            #[require(priority <= 5)]
            async fn set_priority(&self, id: &str, priority: i64) -> Result<()>;
        };

        let result = extract_require_precondition(&method.attrs);
        assert!(result.is_some(), "Should extract combined preconditions");
        let tokens = result.unwrap();
        let code = quote! { #tokens }.to_string();
        assert!(code.contains("priority >= 1"), "Should contain first precondition");
        assert!(code.contains("priority <= 5"), "Should contain second precondition");
        assert!(code.contains("&&"), "Should combine with &&");
    }

    #[test]
    fn test_extract_require_precondition_none() {
        // Create a method without require attributes
        let method: TraitItemFn = parse_quote! {
            async fn no_precondition(&self, id: &str) -> Result<()>;
        };

        let result = extract_require_precondition(&method.attrs);
        assert!(result.is_none(), "Should return None when no precondition");
    }

    #[test]
    fn test_generate_precondition_closure_basic() {
        // Test that generate_precondition_closure produces valid code
        let method: TraitItemFn = parse_quote! {
            #[require(id.len() > 0)]
            async fn delete(&self, id: &str) -> Result<()>;
        };

        let precondition_tokens = extract_require_precondition(&method.attrs).unwrap();
        let crate_path = quote! { crate };
        let closure_code = generate_precondition_closure(&method, &precondition_tokens, &crate_path);

        // Verify the generated code compiles (by checking it has expected structure)
        let code_str = quote! { #closure_code }.to_string();
        // Check for key components - the structure might vary
        assert!(code_str.contains("Arc") || code_str.contains("arc"), "Should wrap in Arc");
        assert!(code_str.contains("Box") || code_str.contains("box"), "Should wrap in Box");
        assert!(code_str.contains("params"), "Should extract from params");
        assert!(code_str.contains("id"), "Should reference parameter name");
    }

    #[test]
    fn test_generate_precondition_closure_with_bool() {
        // Test precondition generation for bool parameter
        let method: TraitItemFn = parse_quote! {
            #[require(value == true || value == false)]
            async fn set_flag(&self, id: &str, value: bool) -> Result<()>;
        };

        let precondition_tokens = extract_require_precondition(&method.attrs).unwrap();
        let crate_path = quote! { crate };
        let closure_code = generate_precondition_closure(&method, &precondition_tokens, &crate_path);

        let code_str = quote! { #closure_code }.to_string();
        assert!(code_str.contains("as_bool"), "Should convert to bool");
        assert!(code_str.contains("value"), "Should reference value parameter");
    }

    #[test]
    fn test_generate_precondition_closure_with_i64() {
        // Test precondition generation for i64 parameter
        let method: TraitItemFn = parse_quote! {
            #[require(priority >= 1)]
            async fn set_priority(&self, id: &str, priority: i64) -> Result<()>;
        };

        let precondition_tokens = extract_require_precondition(&method.attrs).unwrap();
        let crate_path = quote! { crate };
        let closure_code = generate_precondition_closure(&method, &precondition_tokens, &crate_path);

        let code_str = quote! { #closure_code }.to_string();
        assert!(code_str.contains("as_i64"), "Should convert to i64");
        assert!(code_str.contains("priority"), "Should reference priority parameter");
    }
}

/// No-op proc macro for #[require(...)] attribute
/// This allows the attribute to be recognized by Rust's parser
/// The actual processing is done by the operations_trait macro
#[proc_macro_attribute]
pub fn require(_attr: TokenStream, item: TokenStream) -> TokenStream {
    // Just return the item unchanged - the operations_trait macro will process the require attributes
    // This is a no-op macro that just passes through the item
    // We clone the token stream to ensure proper span preservation for rust-analyzer
    item
}
