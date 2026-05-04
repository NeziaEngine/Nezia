mod audio;
mod buffer_pool;
mod bus;
mod command;
mod core;
mod effect;
mod entity;
mod event;
mod source;
mod spatial;
mod streaming;

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

/// ストリーミング再生オプション (Phase 2-4)。
pub use streaming::StreamingOpts;

/// バス・ソースを識別するランタイムハンドル。
pub use entity::EntityId;

/// `SoundEngine::batch_set_source_positions()` の入力要素。
pub use entity::SourcePositionUpdate;

/// SP-10: `SoundEngine::batch_set_source_velocities()` の入力要素。
pub use entity::SourceVelocityUpdate;

/// 距離減衰モデル。
pub use spatial::AttenuationModel;

/// Phase 3-1: Custom Attenuation Curve のハンドル。
pub use spatial::AttenuationCurveId;

/// DSP エフェクト関連の公開型。
pub use effect::{
    EffectId, EffectKind, EffectParamId, EffectPosition, EffectTarget, HpfParam, LpfParam,
    ReverbParam,
};
