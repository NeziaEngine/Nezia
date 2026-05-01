mod system;
mod world;

pub use system::BusSystem;
pub use world::{BusComponent, BusWorld};

/// 最大バス数。
pub const MAX_BUSES: usize = 64;

/// バスの mix_buffer のサイズ上限（バスあたり）。
/// 4096 フレーム × 2ch = 8192 サンプル。
pub const MAX_MIX_BUFFER_SIZE: usize = 8192;
