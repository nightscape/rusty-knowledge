mod comparison;
mod eq;
mod null;

pub use comparison::{Gt, Lt};
pub use eq::{AlwaysTrue, Eq};
pub use null::IsNull;
