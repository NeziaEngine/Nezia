mod audio;
mod buffer_pool;
mod bus;
mod capture;
mod command;
mod container;
mod core;
mod effect;
mod entity;
mod event;
mod snapshot;
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

/// マスター出力 PCM のキャプチャリーダー (Unity Recorder 等の外部録音向け)。
pub use capture::CaptureReader;

/// ロード済みオーディオバッファへのハンドル。
pub use buffer_pool::BufferId;

/// ストリーミング再生オプション (Phase 2-4)。
pub use streaming::StreamingOpts;

/// Mixer Snapshot のハンドル (Phase 3-2)。
pub use snapshot::SnapshotId;

/// Phase 3-3: Send (副ルート) のハンドルとタップ位置。
pub use bus::{SendId, SendPosition};

/// Phase 4-2: Random Container のハンドル。
pub use container::ContainerId;

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
    CompressorParam, EffectId, EffectKind, EffectParamId, EffectPosition, EffectTarget, HpfParam,
    LpfParam, ReverbParam,
};
