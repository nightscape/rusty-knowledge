use crate::core::traits::{Lens, Predicate, SqlPredicate};

#[derive(Clone)]
pub struct IsNull<T, U, L>
where
    L: Lens<T, U>,
{
    lens: L,
    _phantom: std::marker::PhantomData<(T, U)>,
}

impl<T, U, L> IsNull<T, U, L>
where
    L: Lens<T, U>,
{
    pub fn new(lens: L) -> Self {
        Self {
            lens,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<T, U, L> Predicate<T> for IsNull<T, U, L>
where
    L: Lens<T, U>,
    U: Send + Sync + 'static,
    T: Send + Sync + 'static,
{
    fn test(&self, item: &T) -> bool {
        self.lens.get(item).is_none()
    }

    fn to_sql(&self) -> Option<SqlPredicate> {
        let sql = format!("{} IS NULL", self.lens.sql_column());
        Some(SqlPredicate::new(sql, vec![]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestItem {
        name: Option<String>,
    }

    #[derive(Clone)]
    struct NameLens;
    impl Lens<TestItem, String> for NameLens {
        fn get(&self, item: &TestItem) -> Option<String> {
            item.name.clone()
        }

        fn set(&self, item: &mut TestItem, value: String) {
            item.name = Some(value);
        }

        fn sql_column(&self) -> &'static str {
            "name"
        }

        fn field_name(&self) -> &'static str {
            "name"
        }
    }

    #[test]
    fn test_is_null() {
        let item_with_name = TestItem {
            name: Some("Alice".to_string()),
        };
        let item_without_name = TestItem {
            name: None,
        };

        let predicate = IsNull::new(NameLens);

        assert!(!predicate.test(&item_with_name));
        assert!(predicate.test(&item_without_name));
    }

    #[test]
    fn test_is_null_to_sql() {
        let predicate = IsNull::<TestItem, String, _>::new(NameLens);
        let sql_pred = predicate.to_sql().unwrap();
        assert_eq!(sql_pred.sql, "name IS NULL");
        assert_eq!(sql_pred.params.len(), 0);
    }
}
