mod audio;
mod buffer_pool;
mod bus;
mod capture;
mod command;
mod config;
mod container;
mod core;
mod effect;
mod entity;
mod event;
mod limiter;
mod memory;
mod metrics;
mod snapshot;
mod source;
mod spatial;
mod streaming;

// ── 公開 API ──────────────────────────────────────────────────────────────────
// 外部クレートが必要とする型だけをここで再エクスポートする。
// 内部モジュール（audio, bus, command, source 等）は公開しない。

/// サウンドエンジン本体。
pub use core::engine::SoundEngine;

/// エンジン初期化時のキャパシティ設定 (`SoundEngine::with_config` に渡す)。
pub use config::EngineConfig;

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

/// ベンチマーク / プロファイリング用の DSP CPU 計測値とドロップアウトカウンタ。
pub use metrics::{DropoutStats, DspStats};

/// メモリ使用量計測の公開 API。
/// `TrackingAllocator` を `#[global_allocator]` として登録した cdylib (`nezia-ffi`) で
/// グローバル統計が有効になる。breakdown は常時取得可能。
pub use memory::{NeziaMemoryStats, TrackingAllocator};

/// `SoundEngine::batch_set_source_positions()` の入力要素。
pub use entity::SourcePositionUpdate;

/// SP-10: `SoundEngine::batch_set_source_velocities()` の入力要素。
pub use entity::SourceVelocityUpdate;

/// 距離減衰モデル。
pub use spatial::AttenuationModel;

/// `play_with_handle()` に渡す spawn 時の spatial 一括初期化パラメータ。
pub use command::SpawnSpatialInit;

/// Phase 3-1: Custom Attenuation Curve のハンドル。
pub use spatial::AttenuationCurveId;

/// DSP エフェクト関連の公開型。
pub use effect::{
    CompressorParam, EffectId, EffectKind, EffectParamId, EffectPosition, EffectTarget, HpfParam,
    LimiterParam, LpfParam, PeakingEqParam, ReverbParam,
};
