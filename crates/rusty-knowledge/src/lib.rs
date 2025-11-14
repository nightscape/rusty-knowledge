pub mod adapter;
pub mod api;
pub mod core;
pub mod operations;
pub mod references;
pub mod storage;
pub mod sync;
pub mod tasks;
pub mod tasks_sqlite;
pub mod testing;

// Re-export query-render types for FFI
pub use query_render::types::{Arg, BinaryOperator, RenderExpr, RenderSpec};

#[cfg(test)]
pub mod examples;
