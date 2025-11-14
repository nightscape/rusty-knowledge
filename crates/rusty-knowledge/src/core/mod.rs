pub mod datasource;
pub mod entity;
pub mod predicate;
pub mod projections;
pub mod queryable_cache;
pub mod stream_cache;
pub mod stream_registry;
pub mod traits;
pub mod unified_query;
pub mod updates;
pub mod value;

#[cfg(test)]
mod test_macro;

pub use entity::Entity;
pub use predicate::{AlwaysTrue, Eq, Gt, IsNull, Lt};
pub use projections::{Block, BlockAdapter, Blocklike};
pub use queryable_cache::QueryableCache;
pub use stream_cache::QueryableCache as StreamCache;
pub use stream_registry::StreamRegistry;
pub use datasource::{DataSource, StreamProvider};
pub use traits::{
    And, FieldSchema, HasSchema, Lens, Not, Or, Predicate, Queryable, Schema,
    SqlPredicate,
};
pub use unified_query::UnifiedQuery;
pub use updates::{FieldChange, Updates};
pub use value::Value;
