use crate::core::traits::Lens;
use rusty_knowledge_macros::Entity;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Entity)]
#[entity(name = "test_items")]
pub struct TestItem {
    #[primary_key]
    #[indexed]
    pub id: String,
    pub title: String,
    #[indexed]
    pub priority: i64,
    pub completed: bool,
    pub optional_field: Option<String>,
    #[lens(skip)]
    #[serde(skip)]
    pub derived_field: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::entity::Entity;
    use crate::core::traits::HasSchema;

    #[test]
    fn test_lens_generation() {
        let mut item = TestItem {
            id: "1".to_string(),
            title: "Test".to_string(),
            priority: 5,
            completed: false,
            optional_field: Some("optional".to_string()),
            derived_field: vec![],
        };

        let id_lens = IdLens;
        assert_eq!(id_lens.get(&item), Some("1".to_string()));
        assert_eq!(id_lens.field_name(), "id");

        let title_lens = TitleLens;
        assert_eq!(title_lens.get(&item), Some("Test".to_string()));
        title_lens.set(&mut item, "New Title".to_string());
        assert_eq!(item.title, "New Title");

        let priority_lens = PriorityLens;
        assert_eq!(priority_lens.get(&item), Some(5));
        priority_lens.set(&mut item, 10);
        assert_eq!(item.priority, 10);

        let completed_lens = CompletedLens;
        assert_eq!(completed_lens.get(&item), Some(false));
        completed_lens.set(&mut item, true);
        assert!(item.completed);

        let optional_lens = OptionalFieldLens;
        assert_eq!(optional_lens.get(&item), Some("optional".to_string()));
        optional_lens.set(&mut item, "changed".to_string());
        assert_eq!(item.optional_field, Some("changed".to_string()));
    }

    #[test]
    fn test_has_schema_implementation() {
        let schema = TestItem::schema();
        assert_eq!(schema.table_name, "test_items");
        assert!(schema.fields.len() >= 5);

        let id_field = schema.fields.iter().find(|f| f.name == "id").unwrap();
        assert!(id_field.primary_key);
        assert!(id_field.indexed);

        let priority_field = schema.fields.iter().find(|f| f.name == "priority").unwrap();
        assert!(priority_field.indexed);

        let optional_field = schema
            .fields
            .iter()
            .find(|f| f.name == "optional_field")
            .unwrap();
        assert!(optional_field.nullable);
    }

    #[test]
    fn test_to_entity_conversion() {
        let item = TestItem {
            id: "1".to_string(),
            title: "Test".to_string(),
            priority: 5,
            completed: false,
            optional_field: Some("optional".to_string()),
            derived_field: vec!["ignored".to_string()],
        };

        let entity = item.to_entity();
        assert_eq!(entity.type_name, "test_items");
        assert_eq!(entity.get_string("id"), Some("1".to_string()));
        assert_eq!(entity.get_string("title"), Some("Test".to_string()));
        assert_eq!(entity.get_i64("priority"), Some(5));
        assert_eq!(entity.get_bool("completed"), Some(false));
        assert_eq!(
            entity.get_string("optional_field"),
            Some("optional".to_string())
        );
        assert!(!entity.has_field("derived_field"));
    }

    #[test]
    fn test_from_entity_conversion() {
        let mut entity = Entity::new("test_items");
        entity.set("id", "1");
        entity.set("title", "Test");
        entity.set("priority", 5i64);
        entity.set("completed", false);
        entity.set("optional_field", "optional");

        let item = TestItem::from_entity(entity).unwrap();
        assert_eq!(item.id, "1");
        assert_eq!(item.title, "Test");
        assert_eq!(item.priority, 5);
        assert!(!item.completed);
        assert_eq!(item.optional_field, Some("optional".to_string()));
        assert_eq!(item.derived_field, Vec::<String>::new());
    }

    #[test]
    fn test_from_entity_with_none() {
        let mut entity = Entity::new("test_items");
        entity.set("id", "1");
        entity.set("title", "Test");
        entity.set("priority", 5i64);
        entity.set("completed", false);

        let item = TestItem::from_entity(entity).unwrap();
        assert_eq!(item.optional_field, None);
    }

    #[test]
    fn test_roundtrip_conversion() {
        let original = TestItem {
            id: "1".to_string(),
            title: "Test".to_string(),
            priority: 5,
            completed: false,
            optional_field: Some("optional".to_string()),
            derived_field: vec![],
        };

        let entity = original.to_entity();
        let restored = TestItem::from_entity(entity).unwrap();

        assert_eq!(original.id, restored.id);
        assert_eq!(original.title, restored.title);
        assert_eq!(original.priority, restored.priority);
        assert_eq!(original.completed, restored.completed);
        assert_eq!(original.optional_field, restored.optional_field);
    }
}
