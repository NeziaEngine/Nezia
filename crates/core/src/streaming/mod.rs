//! ストリーミング再生モジュール (Phase 2-4)。
//!
//! 詳細は `docs/design/core/streaming.md` を参照。

mod mirror_ring;
mod worker;

pub use worker::StreamingOpts;
pub(crate) use worker::{
    LoopRegion, StreamCmd, StreamingHandle, StreamingState, spawn_streaming_worker, status,
};
