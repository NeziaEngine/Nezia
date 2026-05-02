mod audio;
mod buffer_pool;
mod bus;
mod command;
mod core;
mod entity;
mod event;
mod source;
mod spatial;

// ── 公開 API ──────────────────────────────────────────────────────────────────
// 外部クレートが必要とする型だけをここで再エクスポートする。
// 内部モジュール（audio, bus, command, source 等）は公開しない。

/// サウンドエンジン本体。
pub use core::engine::SoundEngine;

/// ロード済みオーディオバッファへのハンドル。
pub use buffer_pool::BufferId;

/// バス・ソースを識別するランタイムハンドル。
pub use entity::EntityId;

/// 距離減衰モデル。
pub use spatial::AttenuationModel;
