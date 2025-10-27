use super::value::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub struct Entity {
    pub type_name: String,
    pub fields: HashMap<String, Value>,
}

impl Entity {
    pub fn new(type_name: impl Into<String>) -> Self {
        Self {
            type_name: type_name.into(),
            fields: HashMap::new(),
        }
    }

    pub fn with_field(mut self, name: impl Into<String>, value: impl Into<Value>) -> Self {
        self.fields.insert(name.into(), value.into());
        self
    }

    pub fn get(&self, field: &str) -> Option<&Value> {
        self.fields.get(field)
    }

    pub fn get_mut(&mut self, field: &str) -> Option<&mut Value> {
        self.fields.get_mut(field)
    }

    pub fn set(&mut self, field: impl Into<String>, value: impl Into<Value>) {
        self.fields.insert(field.into(), value.into());
    }

    pub fn remove(&mut self, field: &str) -> Option<Value> {
        self.fields.remove(field)
    }

    pub fn has_field(&self, field: &str) -> bool {
        self.fields.contains_key(field)
    }

    pub fn get_string(&self, field: &str) -> Option<String> {
        self.get(field).and_then(|v| v.as_str().map(String::from))
    }

    pub fn get_i64(&self, field: &str) -> Option<i64> {
        self.get(field).and_then(|v| v.as_i64())
    }

    pub fn get_bool(&self, field: &str) -> Option<bool> {
        self.get(field).and_then(|v| v.as_bool())
    }

    pub fn get_f64(&self, field: &str) -> Option<f64> {
        self.get(field).and_then(|v| v.as_f64())
    }
}

impl Default for Entity {
    fn default() -> Self {
        Self::new("unknown")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entity_creation() {
        let entity = Entity::new("Task")
            .with_field("id", "123")
            .with_field("title", "Test Task")
            .with_field("priority", 5i64);

        assert_eq!(entity.type_name, "Task");
        assert_eq!(entity.get_string("id"), Some("123".to_string()));
        assert_eq!(entity.get_string("title"), Some("Test Task".to_string()));
        assert_eq!(entity.get_i64("priority"), Some(5));
    }

    #[test]
    fn test_entity_mutation() {
        let mut entity = Entity::new("Task");
        entity.set("title", "New Title");
        assert_eq!(entity.get_string("title"), Some("New Title".to_string()));

        entity.remove("title");
        assert!(!entity.has_field("title"));
    }

    #[test]
    fn test_entity_type_accessors() {
        let entity = Entity::new("Test")
            .with_field("bool_field", true)
            .with_field("int_field", 42i64)
            .with_field("float_field", 3.5)
            .with_field("string_field", "hello");

        assert_eq!(entity.get_bool("bool_field"), Some(true));
        assert_eq!(entity.get_i64("int_field"), Some(42));
        assert_eq!(entity.get_f64("float_field"), Some(3.5));
        assert_eq!(entity.get_string("string_field"), Some("hello".to_string()));
    }

    #[test]
    fn test_entity_missing_fields() {
        let entity = Entity::new("Test");
        assert_eq!(entity.get_string("missing"), None);
        assert!(!entity.has_field("missing"));
    }
}
