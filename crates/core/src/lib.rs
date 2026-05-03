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

/// メモリ上のバイト列からオーディオメタデータを取得する。
pub use audio::{AudioMetadata, peek_metadata};

/// バッファリーダー（任意スレッドから PCM を読める読み取りハンドル）。
pub use core::engine::BufferReader;

/// ロード済みオーディオバッファへのハンドル。
pub use buffer_pool::BufferId;

/// バス・ソースを識別するランタイムハンドル。
pub use entity::EntityId;

/// `SoundEngine::batch_set_source_positions()` の入力要素。
pub use entity::SourcePositionUpdate;

/// 距離減衰モデル。
pub use spatial::AttenuationModel;
