use crate::core::traits::{Lens, Predicate, SqlPredicate};
use crate::core::value::Value;

#[derive(Clone)]
pub struct Lt<T, U, L>
where
    L: Lens<T, U>,
    U: PartialOrd + Clone + Into<Value>,
{
    lens: L,
    value: U,
    _phantom: std::marker::PhantomData<T>,
}

impl<T, U, L> Lt<T, U, L>
where
    L: Lens<T, U>,
    U: PartialOrd + Clone + Into<Value>,
{
    pub fn new(lens: L, value: U) -> Self {
        Self {
            lens,
            value,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<T, U, L> Predicate<T> for Lt<T, U, L>
where
    L: Lens<T, U>,
    U: PartialOrd + Clone + Into<Value> + Send + Sync + 'static,
    T: Send + Sync + 'static,
{
    fn test(&self, item: &T) -> bool {
        if let Some(field_value) = self.lens.get(item) {
            field_value < self.value
        } else {
            false
        }
    }

    fn to_sql(&self) -> Option<SqlPredicate> {
        let sql = format!("{} < ?", self.lens.sql_column());
        let params = vec![self.value.clone().into()];
        Some(SqlPredicate::new(sql, params))
    }
}

#[derive(Clone)]
pub struct Gt<T, U, L>
where
    L: Lens<T, U>,
    U: PartialOrd + Clone + Into<Value>,
{
    lens: L,
    value: U,
    _phantom: std::marker::PhantomData<T>,
}

impl<T, U, L> Gt<T, U, L>
where
    L: Lens<T, U>,
    U: PartialOrd + Clone + Into<Value>,
{
    pub fn new(lens: L, value: U) -> Self {
        Self {
            lens,
            value,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<T, U, L> Predicate<T> for Gt<T, U, L>
where
    L: Lens<T, U>,
    U: PartialOrd + Clone + Into<Value> + Send + Sync + 'static,
    T: Send + Sync + 'static,
{
    fn test(&self, item: &T) -> bool {
        if let Some(field_value) = self.lens.get(item) {
            field_value > self.value
        } else {
            false
        }
    }

    fn to_sql(&self) -> Option<SqlPredicate> {
        let sql = format!("{} > ?", self.lens.sql_column());
        let params = vec![self.value.clone().into()];
        Some(SqlPredicate::new(sql, params))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestItem {
        age: i64,
        score: f64,
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

    #[derive(Clone)]
    struct ScoreLens;
    impl Lens<TestItem, f64> for ScoreLens {
        fn get(&self, item: &TestItem) -> Option<f64> {
            Some(item.score)
        }

        fn set(&self, item: &mut TestItem, value: f64) {
            item.score = value;
        }

        fn sql_column(&self) -> &'static str {
            "score"
        }

        fn field_name(&self) -> &'static str {
            "score"
        }
    }

    #[test]
    fn test_lt_integer() {
        let item = TestItem {
            age: 30,
            score: 85.5,
        };

        let predicate = Lt::new(AgeLens, 40i64);
        assert!(predicate.test(&item));

        let predicate = Lt::new(AgeLens, 20i64);
        assert!(!predicate.test(&item));

        let predicate = Lt::new(AgeLens, 30i64);
        assert!(!predicate.test(&item));
    }

    #[test]
    fn test_gt_integer() {
        let item = TestItem {
            age: 30,
            score: 85.5,
        };

        let predicate = Gt::new(AgeLens, 20i64);
        assert!(predicate.test(&item));

        let predicate = Gt::new(AgeLens, 40i64);
        assert!(!predicate.test(&item));

        let predicate = Gt::new(AgeLens, 30i64);
        assert!(!predicate.test(&item));
    }

    #[test]
    fn test_lt_float() {
        let item = TestItem {
            age: 30,
            score: 85.5,
        };

        let predicate = Lt::new(ScoreLens, 90.0);
        assert!(predicate.test(&item));

        let predicate = Lt::new(ScoreLens, 80.0);
        assert!(!predicate.test(&item));
    }

    #[test]
    fn test_gt_float() {
        let item = TestItem {
            age: 30,
            score: 85.5,
        };

        let predicate = Gt::new(ScoreLens, 80.0);
        assert!(predicate.test(&item));

        let predicate = Gt::new(ScoreLens, 90.0);
        assert!(!predicate.test(&item));
    }

    #[test]
    fn test_lt_to_sql() {
        let predicate = Lt::new(AgeLens, 40i64);
        let sql_pred = predicate.to_sql().unwrap();
        assert_eq!(sql_pred.sql, "age < ?");
        assert_eq!(sql_pred.params.len(), 1);
    }

    #[test]
    fn test_gt_to_sql() {
        let predicate = Gt::new(AgeLens, 20i64);
        let sql_pred = predicate.to_sql().unwrap();
        assert_eq!(sql_pred.sql, "age > ?");
        assert_eq!(sql_pred.params.len(), 1);
    }
}
