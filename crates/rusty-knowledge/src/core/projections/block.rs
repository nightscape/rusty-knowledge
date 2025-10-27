use chrono::{DateTime, Utc};
use rusty_knowledge_macros::Entity;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Entity)]
#[entity(name = "blocks")]
pub struct Block {
    pub id: String,
    pub title: String,
    pub content: Option<String>,
    pub completed: bool,
    #[serde(skip)]
    pub created_at: DateTime<Utc>,
    #[serde(skip)]
    pub updated_at: DateTime<Utc>,
    pub source: String,
    pub source_id: String,
    pub tags: Option<String>,
}

pub trait Blocklike: Sized + Send + Sync {
    fn to_block(&self) -> Block;

    fn from_block(block: &Block) -> Option<Self>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::traits::Predicate;
    use crate::core::{Eq, HasSchema, Lens, Value};

    #[test]
    fn test_block_has_schema() {
        let schema = Block::schema();
        assert_eq!(schema.table_name, "blocks");
        assert!(schema.fields.iter().any(|f| f.name == "id"));
        assert!(schema.fields.iter().any(|f| f.name == "title"));
        assert!(schema.fields.iter().any(|f| f.name == "completed"));
    }

    #[test]
    fn test_block_lenses() {
        let block = Block {
            id: "1".to_string(),
            title: "Test Block".to_string(),
            content: Some("Content".to_string()),
            completed: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            source: "internal".to_string(),
            source_id: "1".to_string(),
            tags: None,
        };

        assert_eq!(IdLens.get(&block), Some("1".to_string()));
        assert_eq!(TitleLens.get(&block), Some("Test Block".to_string()));
        assert_eq!(CompletedLens.get(&block), Some(false));
        assert_eq!(SourceLens.get(&block), Some("internal".to_string()));
    }

    #[test]
    fn test_block_to_entity() {
        let block = Block {
            id: "1".to_string(),
            title: "Test".to_string(),
            content: None,
            completed: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            source: "test".to_string(),
            source_id: "1".to_string(),
            tags: None,
        };

        let entity = block.to_entity();

        if let Value::String(id) = entity.get("id").unwrap() {
            assert_eq!(id, "1");
        } else {
            panic!("Expected string id");
        }

        if let Value::String(title) = entity.get("title").unwrap() {
            assert_eq!(title, "Test");
        } else {
            panic!("Expected string title");
        }
    }

    #[test]
    fn test_block_predicates() {
        let block = Block {
            id: "1".to_string(),
            title: "Test Block".to_string(),
            content: Some("Content".to_string()),
            completed: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            source: "internal".to_string(),
            source_id: "1".to_string(),
            tags: Some("tag1,tag2".to_string()),
        };

        let pred = Eq::new(CompletedLens, true);
        assert!(pred.test(&block));

        let pred2 = Eq::new(SourceLens, "internal".to_string());
        assert!(pred2.test(&block));
    }
}
