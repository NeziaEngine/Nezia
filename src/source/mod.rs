mod lifecycle;
mod system;
mod world;

pub use lifecycle::SourceLifecycleSystem;
pub use system::SourceMixingSystem;
pub use world::{SourceComponent, SourceState, SourceWorld};

/// 最大同時発音数。
pub const MAX_SOURCES: usize = 256;
