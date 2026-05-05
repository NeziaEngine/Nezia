//! Mixer Snapshot (Phase 3-2)。
//!
//! 「ミキサー全パラメータの状態を名前付きで保存し、ゲーム内で滑らかに切り替える」仕組み。
//! 詳細: `docs/design/core/snapshot.md`。
//!
//! ## API 形 (宣言的ビルダー)
//!
//! ```ignore
//! let battle = engine
//!     .snapshot_builder()
//!     .set_bus_gain(bgm_bus, 0.3)
//!     .set_bus_gain(sfx_bus, 1.0)
//!     .set_effect_param(reverb1, ReverbParam::Wet, 0.5)
//!     .commit()?;
//!
//! engine.apply_snapshot(battle, 2.0); // 2 秒クロスフェード
//! ```
//!
//! ## 含む / 含まない
//!
//! 含む: `BusWorld.gain` / `muted` + `LpfWorld` (cutoff/Q) + `HpfWorld` (cutoff/Q) +
//!       `ReverbWorld` (room_size/damping/wet/dry/width)。
//! 含まない: Source 個別パラメータ (vol/pitch)、3D 位置・速度、リスナー姿勢、
//!           AudioBuffer / AttenuationCurve のロード状態。
//!           これらは「ミキサー設定」ではなく毎フレーム動的状態のため。

mod registry;
mod world;

pub use registry::{SnapshotId, SnapshotRegistry};
pub(crate) use world::{
    ActiveSnapshot, BusGainEntry, BusMutedEntry, EffectParamEntry, SendGainEntry, Snapshot,
    SnapshotEffectKind,
};
