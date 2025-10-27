use crate::core::traits::{Lens, Predicate, SqlPredicate};
use crate::core::value::Value;

#[derive(Clone, Copy)]
pub struct AlwaysTrue;

impl<T> Predicate<T> for AlwaysTrue
where
    T: Send + Sync + 'static,
{
    fn test(&self, _item: &T) -> bool {
        true
    }

    fn to_sql(&self) -> Option<SqlPredicate> {
        Some(SqlPredicate::new("1 = 1".to_string(), vec![]))
    }
}

#[derive(Clone)]
pub struct Eq<T, U, L>
where
    L: Lens<T, U>,
    U: PartialEq + Clone + Into<Value>,
{
    lens: L,
    value: U,
    _phantom: std::marker::PhantomData<T>,
}

impl<T, U, L> Eq<T, U, L>
where
    L: Lens<T, U>,
    U: PartialEq + Clone + Into<Value>,
{
    pub fn new(lens: L, value: U) -> Self {
        Self {
            lens,
            value,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<T, U, L> Predicate<T> for Eq<T, U, L>
where
    L: Lens<T, U>,
    U: PartialEq + Clone + Into<Value> + Send + Sync + 'static,
    T: Send + Sync + 'static,
{
    fn test(&self, item: &T) -> bool {
        if let Some(field_value) = self.lens.get(item) {
            field_value == self.value
        } else {
            false
        }
    }

    fn to_sql(&self) -> Option<SqlPredicate> {
        let sql = format!("{} = ?", self.lens.sql_column());
        let params = vec![self.value.clone().into()];
        Some(SqlPredicate::new(sql, params))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestItem {
        name: String,
        age: i64,
    }

    #[derive(Clone)]
    struct NameLens;
    impl Lens<TestItem, String> for NameLens {
        fn get(&self, item: &TestItem) -> Option<String> {
            Some(item.name.clone())
        }

        fn set(&self, item: &mut TestItem, value: String) {
            item.name = value;
        }

        fn sql_column(&self) -> &'static str {
            "name"
        }

        fn field_name(&self) -> &'static str {
            "name"
        }
    }

    #[derive(Clone)]
    struct AgeLens;
    impl Lens<TestItem, i64> for AgeLens {
        fn get(&self, item: &TestItem) -> Option<i64> {
            Some(item.age)
        }

        fn set(&self, item: &mut TestItem, value: i64) {
            item.age = value;
        }

        fn sql_column(&self) -> &'static str {
            "age"
        }

        fn field_name(&self) -> &'static str {
            "age"
        }
    }

    #[test]
    fn test_eq_string() {
        let item = TestItem {
            name: "Alice".to_string(),
            age: 30,
        };

        let predicate = Eq::new(NameLens, "Alice".to_string());
        assert!(predicate.test(&item));

        let predicate = Eq::new(NameLens, "Bob".to_string());
        assert!(!predicate.test(&item));
    }

    #[test]
    fn test_eq_integer() {
        let item = TestItem {
            name: "Alice".to_string(),
            age: 30,
        };

        let predicate = Eq::new(AgeLens, 30i64);
        assert!(predicate.test(&item));

        let predicate = Eq::new(AgeLens, 25i64);
        assert!(!predicate.test(&item));
    }

    #[test]
    fn test_eq_to_sql() {
        let predicate = Eq::new(NameLens, "Alice".to_string());
        let sql_pred = predicate.to_sql().unwrap();
        assert_eq!(sql_pred.sql, "name = ?");
        assert_eq!(sql_pred.params.len(), 1);

        let predicate = Eq::new(AgeLens, 30i64);
        let sql_pred = predicate.to_sql().unwrap();
        assert_eq!(sql_pred.sql, "age = ?");
        assert_eq!(sql_pred.params.len(), 1);
    }
}
