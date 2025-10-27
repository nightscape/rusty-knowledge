use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields, Meta, parse_macro_input};

#[proc_macro_derive(Entity, attributes(entity, primary_key, indexed, reference, lens))]
pub fn derive_entity(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    let name = &input.ident;
    let entity_name = extract_entity_name(&input.attrs);

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
            quote! { crate::storage::schema::FieldType::Reference(#ref_entity.to_string()) }
        } else {
            type_to_field_type(field_type)
        };

        let is_required = !is_option_type(field_type);

        field_schemas.push(quote! {
            crate::storage::schema::FieldSchema {
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

                impl crate::core::traits::Lens<#name, #inner_type> for #lens_name {
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
                crate::core::traits::FieldSchema::new(#field_name_str, #sql_type)
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
            pub fn entity_schema() -> crate::storage::schema::EntitySchema {
                crate::storage::schema::EntitySchema {
                    name: #entity_name.to_string(),
                    primary_key: #primary_key.to_string(),
                    fields: vec![
                        #(#field_schemas),*
                    ],
                }
            }
        }

        #(#lens_definitions)*

        impl crate::core::traits::HasSchema for #name {
            fn schema() -> crate::core::traits::Schema {
                crate::core::traits::Schema::new(
                    #entity_name,
                    vec![
                        #(#schema_fields),*
                    ]
                )
            }

            fn to_entity(&self) -> crate::core::entity::Entity {
                let mut entity = crate::core::entity::Entity::new(#entity_name);
                #(#to_entity_fields;)*
                entity
            }

            fn from_entity(entity: crate::core::entity::Entity) -> crate::core::traits::Result<Self> {
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

fn type_to_field_type(ty: &syn::Type) -> proc_macro2::TokenStream {
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
        "String" => quote! { crate::storage::schema::FieldType::String },
        "i64" | "i32" | "u64" | "u32" | "usize" => {
            quote! { crate::storage::schema::FieldType::Integer }
        }
        "bool" => quote! { crate::storage::schema::FieldType::Boolean },
        t if t.contains("DateTime") => quote! { crate::storage::schema::FieldType::DateTime },
        _ => quote! { crate::storage::schema::FieldType::Json },
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
