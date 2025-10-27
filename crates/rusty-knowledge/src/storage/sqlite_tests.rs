use super::*;
use chrono::Utc;

#[cfg(test)]
mod sql_injection_tests {
    use super::*;

    #[tokio::test]
    async fn test_value_to_sql_param_escapes_single_quotes() {
        let backend = SqliteBackend::new_in_memory().await.unwrap();

        let malicious = Value::String("'; DROP TABLE users; --".to_string());
        let result = backend.value_to_sql_param(&malicious);

        assert_eq!(result, "'''; DROP TABLE users; --'");
        assert!(result.contains("''"));
    }

    #[tokio::test]
    async fn test_value_to_sql_param_handles_multiple_quotes() {
        let backend = SqliteBackend::new_in_memory().await.unwrap();

        let value = Value::String("It's a test with 'quotes'".to_string());
        let result = backend.value_to_sql_param(&value);

        assert_eq!(result, "'It''s a test with ''quotes'''");
    }

    #[tokio::test]
    async fn test_value_to_sql_param_handles_empty_string() {
        let backend = SqliteBackend::new_in_memory().await.unwrap();

        let value = Value::String("".to_string());
        let result = backend.value_to_sql_param(&value);

        assert_eq!(result, "''");
    }
}

#[cfg(test)]
mod filter_building_tests {
    use super::*;

    #[tokio::test]
    async fn test_build_where_clause_simple_eq() {
        let backend = SqliteBackend::new_in_memory().await.unwrap();

        let filter = Filter::Eq("name".to_string(), Value::String("test".to_string()));
        let result = backend.build_where_clause(&filter);

        assert_eq!(result, "name = 'test'");
    }

    #[tokio::test]
    async fn test_build_where_clause_in() {
        let backend = SqliteBackend::new_in_memory().await.unwrap();

        let filter = Filter::In(
            "status".to_string(),
            vec![
                Value::String("active".to_string()),
                Value::String("pending".to_string()),
            ],
        );
        let result = backend.build_where_clause(&filter);

        assert_eq!(result, "status IN ('active', 'pending')");
    }

    #[tokio::test]
    async fn test_build_where_clause_and() {
        let backend = SqliteBackend::new_in_memory().await.unwrap();

        let filter = Filter::And(vec![
            Filter::Eq("name".to_string(), Value::String("test".to_string())),
            Filter::Eq("active".to_string(), Value::Boolean(true)),
        ]);
        let result = backend.build_where_clause(&filter);

        assert_eq!(result, "(name = 'test' AND active = 1)");
    }

    #[tokio::test]
    async fn test_build_where_clause_or() {
        let backend = SqliteBackend::new_in_memory().await.unwrap();

        let filter = Filter::Or(vec![
            Filter::Eq("priority".to_string(), Value::Integer(1)),
            Filter::Eq("priority".to_string(), Value::Integer(2)),
        ]);
        let result = backend.build_where_clause(&filter);

        assert_eq!(result, "(priority = 1 OR priority = 2)");
    }

    #[tokio::test]
    async fn test_build_where_clause_nested_and_or() {
        let backend = SqliteBackend::new_in_memory().await.unwrap();

        let filter = Filter::And(vec![
            Filter::Eq("status".to_string(), Value::String("active".to_string())),
            Filter::Or(vec![
                Filter::Eq("priority".to_string(), Value::Integer(1)),
                Filter::Eq("priority".to_string(), Value::Integer(2)),
            ]),
        ]);
        let result = backend.build_where_clause(&filter);

        assert_eq!(
            result,
            "(status = 'active' AND (priority = 1 OR priority = 2))"
        );
    }

    #[tokio::test]
    async fn test_build_where_clause_is_null() {
        let backend = SqliteBackend::new_in_memory().await.unwrap();

        let filter = Filter::IsNull("deleted_at".to_string());
        let result = backend.build_where_clause(&filter);

        assert_eq!(result, "deleted_at IS NULL");
    }

    #[tokio::test]
    async fn test_build_where_clause_is_not_null() {
        let backend = SqliteBackend::new_in_memory().await.unwrap();

        let filter = Filter::IsNotNull("created_at".to_string());
        let result = backend.build_where_clause(&filter);

        assert_eq!(result, "created_at IS NOT NULL");
    }
}

#[cfg(test)]
mod value_conversion_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn prop_integer_roundtrip(value in any::<i64>()) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let backend = SqliteBackend::new_in_memory().await.unwrap();
                let val = Value::Integer(value);
                let sql = backend.value_to_sql_param(&val);

                assert_eq!(sql, value.to_string());
                assert!(!sql.contains('\''));
            });
        }

        #[test]
        fn prop_string_escaping_prevents_injection(s in ".*") {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let backend = SqliteBackend::new_in_memory().await.unwrap();
                let val = Value::String(s.clone());
                let sql = backend.value_to_sql_param(&val);

                assert!(sql.starts_with('\''));
                assert!(sql.ends_with('\''));

                let quote_count_original = s.matches('\'').count();

                // Strip outer quotes before counting escaped quotes
                let inner_sql = &sql[1..sql.len()-1];
                let quote_count_escaped = inner_sql.matches("''").count();

                assert_eq!(quote_count_original, quote_count_escaped);
            });
        }

        #[test]
        fn prop_boolean_conversion(value in any::<bool>()) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let backend = SqliteBackend::new_in_memory().await.unwrap();
                let val = Value::Boolean(value);
                let sql = backend.value_to_sql_param(&val);

                assert_eq!(sql, if value { "1" } else { "0" });
            });
        }

        #[test]
        fn prop_reference_escaping(s in "[a-zA-Z0-9-_]*") {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let backend = SqliteBackend::new_in_memory().await.unwrap();
                let val = Value::Reference(s.clone());
                let sql = backend.value_to_sql_param(&val);

                assert!(sql.starts_with('\''));
                assert!(sql.ends_with('\''));
                assert!(sql.contains(&s));
            });
        }
    }

    #[tokio::test]
    async fn test_value_to_sql_param_datetime() {
        let backend = SqliteBackend::new_in_memory().await.unwrap();

        let dt = Utc::now();
        let value = Value::DateTime(dt);
        let result = backend.value_to_sql_param(&value);

        assert!(result.starts_with('\''));
        assert!(result.ends_with('\''));
        assert!(result.contains('T'));
    }

    #[tokio::test]
    async fn test_value_to_sql_param_json() {
        let backend = SqliteBackend::new_in_memory().await.unwrap();

        let json = serde_json::json!({"key": "value", "count": 42});
        let value = Value::Json(json);
        let result = backend.value_to_sql_param(&value);

        assert!(result.starts_with('\''));
        assert!(result.ends_with('\''));
        assert!(result.contains("key"));
        assert!(result.contains("value"));
    }

    #[tokio::test]
    async fn test_value_to_sql_param_null() {
        let backend = SqliteBackend::new_in_memory().await.unwrap();

        let value = Value::Null;
        let result = backend.value_to_sql_param(&value);

        assert_eq!(result, "NULL");
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::storage::schema::FieldSchema;

    #[tokio::test]
    async fn test_create_entity_and_insert() {
        let mut backend = SqliteBackend::new_in_memory().await.unwrap();

        let schema = EntitySchema {
            name: "test_tasks".to_string(),
            primary_key: "id".to_string(),
            fields: vec![
                FieldSchema {
                    name: "id".to_string(),
                    field_type: FieldType::String,
                    required: true,
                    indexed: true,
                },
                FieldSchema {
                    name: "title".to_string(),
                    field_type: FieldType::String,
                    required: true,
                    indexed: false,
                },
            ],
        };

        backend.create_entity(&schema).await.unwrap();

        let mut entity = Entity::new();
        entity.insert("id".to_string(), Value::String("task-1".to_string()));
        entity.insert("title".to_string(), Value::String("Test Task".to_string()));

        backend.insert("test_tasks", entity).await.unwrap();

        let retrieved = backend.get("test_tasks", "task-1").await.unwrap();
        assert!(retrieved.is_some());

        let task = retrieved.unwrap();
        assert_eq!(task.get("id").unwrap().as_string().unwrap(), "task-1");
        assert_eq!(task.get("title").unwrap().as_string().unwrap(), "Test Task");
    }

    #[tokio::test]
    async fn test_dirty_tracking() {
        let mut backend = SqliteBackend::new_in_memory().await.unwrap();

        let schema = EntitySchema {
            name: "test_items".to_string(),
            primary_key: "id".to_string(),
            fields: vec![FieldSchema {
                name: "id".to_string(),
                field_type: FieldType::String,
                required: true,
                indexed: true,
            }],
        };

        backend.create_entity(&schema).await.unwrap();

        let mut entity = Entity::new();
        entity.insert("id".to_string(), Value::String("item-1".to_string()));
        backend.insert("test_items", entity).await.unwrap();

        let dirty_before = backend.get_dirty("test_items").await.unwrap();
        assert_eq!(dirty_before.len(), 0);

        backend.mark_dirty("test_items", "item-1").await.unwrap();

        let dirty_after = backend.get_dirty("test_items").await.unwrap();
        assert_eq!(dirty_after.len(), 1);
        assert_eq!(dirty_after[0], "item-1");

        backend.mark_clean("test_items", "item-1").await.unwrap();

        let dirty_final = backend.get_dirty("test_items").await.unwrap();
        assert_eq!(dirty_final.len(), 0);
    }

    #[tokio::test]
    async fn test_version_tracking() {
        let mut backend = SqliteBackend::new_in_memory().await.unwrap();

        let schema = EntitySchema {
            name: "test_versioned".to_string(),
            primary_key: "id".to_string(),
            fields: vec![FieldSchema {
                name: "id".to_string(),
                field_type: FieldType::String,
                required: true,
                indexed: true,
            }],
        };

        backend.create_entity(&schema).await.unwrap();

        let mut entity = Entity::new();
        entity.insert("id".to_string(), Value::String("v-1".to_string()));
        backend.insert("test_versioned", entity).await.unwrap();

        let version_before = backend.get_version("test_versioned", "v-1").await.unwrap();
        assert!(version_before.is_none());

        backend
            .set_version("test_versioned", "v-1", "v1.0.0".to_string())
            .await
            .unwrap();

        let version_after = backend.get_version("test_versioned", "v-1").await.unwrap();
        assert_eq!(version_after, Some("v1.0.0".to_string()));
    }

    #[tokio::test]
    async fn test_query_with_filter() {
        let mut backend = SqliteBackend::new_in_memory().await.unwrap();

        let schema = EntitySchema {
            name: "test_query".to_string(),
            primary_key: "id".to_string(),
            fields: vec![
                FieldSchema {
                    name: "id".to_string(),
                    field_type: FieldType::String,
                    required: true,
                    indexed: true,
                },
                FieldSchema {
                    name: "status".to_string(),
                    field_type: FieldType::String,
                    required: true,
                    indexed: true,
                },
            ],
        };

        backend.create_entity(&schema).await.unwrap();

        for i in 1..=3 {
            let mut entity = Entity::new();
            entity.insert("id".to_string(), Value::String(format!("item-{}", i)));
            entity.insert(
                "status".to_string(),
                Value::String(if i % 2 == 0 { "done" } else { "pending" }.to_string()),
            );
            backend.insert("test_query", entity).await.unwrap();
        }

        let filter = Filter::Eq("status".to_string(), Value::String("pending".to_string()));
        let results = backend.query("test_query", filter).await.unwrap();

        assert_eq!(results.len(), 2);
    }
}
