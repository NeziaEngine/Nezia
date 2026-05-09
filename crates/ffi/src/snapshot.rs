//! Mixer Snapshot 関連 FFI (Phase 3-2)。
//!
//! core 側はビルダーパターン (`engine.snapshot_builder().set_*().commit()`) だが、
//! C ABI では複数呼び出しを跨いだエンジンの可変借用を保持できないので、FFI 側で
//! 中間状態を独立した `NeziaSnapshotBuilder` に貯めておき、`commit` で初めて
//! engine と合流させる。設計詳細は `docs/design/core/snapshot.md` 参照。

use std::ptr::null_mut;

use nezia_core::{EffectKind, EffectParamId, SendId, SnapshotId};

use crate::engine::NeziaEngine;
use crate::panic::{guard_result, guard_value};
use crate::send::NeziaSendId;
use crate::types::{NeziaEntityId, NeziaResult};

/// Snapshot ハンドル (`core::SnapshotId` の ABI ミラー)。
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NeziaSnapshotId {
    pub index: u32,
    pub generation: u32,
}

impl NeziaSnapshotId {
    pub(crate) const INVALID: Self = Self {
        index: u32::MAX,
        generation: 0,
    };

    #[inline]
    fn from_core(id: SnapshotId) -> Self {
        Self {
            index: id.index,
            generation: id.generation,
        }
    }

    #[inline]
    fn to_core(self) -> SnapshotId {
        SnapshotId {
            index: self.index,
            generation: self.generation,
        }
    }
}

/// FFI 側で `commit` まで貯める中間バッファ。`Box::into_raw` で C 側に渡す不透明型。
///
/// core の `SnapshotBuilder<'a>` は engine への mut 借用を保持するが、FFI ABI 越しに
/// その借用を引き回すのは難しい。代わりに値をすべて FFI 側で貯め、`commit` 時に
/// engine の builder を立ち上げてフィールドを流し込む。意味論は同一。
#[allow(non_camel_case_types)]
pub struct NeziaSnapshotBuilder {
    bus_gains: Vec<(NeziaEntityId, f32)>,
    bus_muted: Vec<(NeziaEntityId, bool)>,
    send_gains: Vec<(NeziaSendId, f32)>,
    /// (effect, kind, param, value)。kind は `core::EffectKind` を `u8` で保持。
    effect_params: Vec<(NeziaEntityId, u8, u8, f32)>,
}

/// 新しい Snapshot ビルダーを開始する。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_snapshot_builder_begin() -> *mut NeziaSnapshotBuilder {
    guard_value(null_mut(), || {
        Box::into_raw(Box::new(NeziaSnapshotBuilder {
            bus_gains: Vec::new(),
            bus_muted: Vec::new(),
            send_gains: Vec::new(),
            effect_params: Vec::new(),
        }))
    })
}

/// バスの gain を Snapshot に追加する。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_snapshot_builder_set_bus_gain(
    builder: *mut NeziaSnapshotBuilder,
    bus: NeziaEntityId,
    gain: f32,
) {
    guard_value((), || {
        let Some(b) = (unsafe { builder.as_mut() }) else {
            return;
        };
        b.bus_gains.push((bus, gain));
    })
}

/// バスのミュート状態を Snapshot に追加する。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_snapshot_builder_set_bus_muted(
    builder: *mut NeziaSnapshotBuilder,
    bus: NeziaEntityId,
    muted: u8,
) {
    guard_value((), || {
        let Some(b) = (unsafe { builder.as_mut() }) else {
            return;
        };
        b.bus_muted.push((bus, muted != 0));
    })
}

/// Send の gain を Snapshot に追加する。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_snapshot_builder_set_send_gain(
    builder: *mut NeziaSnapshotBuilder,
    send: NeziaSendId,
    gain: f32,
) {
    guard_value((), || {
        let Some(b) = (unsafe { builder.as_mut() }) else {
            return;
        };
        b.send_gains.push((send, gain));
    })
}

/// エフェクトパラメータを Snapshot に追加する。
///
/// `kind` は `NeziaEffectKind` (Lpf=0 / Hpf=1 / Reverb=2 / Compressor=3 / PeakingEq=4 / Limiter=5)、
/// `param` は種別ごとのパラメータインデックス (`nezia_effect_set_param` と同じ意味)。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_snapshot_builder_set_effect_param(
    builder: *mut NeziaSnapshotBuilder,
    effect: NeziaEntityId,
    kind: u8,
    param: u8,
    value: f32,
) {
    guard_value((), || {
        let Some(b) = (unsafe { builder.as_mut() }) else {
            return;
        };
        b.effect_params.push((effect, kind, param, value));
    })
}

/// Snapshot を commit してハンドルを返す。失敗時は `INVALID`。
/// `builder` は呼出後に解放されるので二度と使わないこと (NULL も書き戻されないので
/// 呼出側は自前でポインタをクリアすること)。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_snapshot_builder_commit(
    engine: *mut NeziaEngine,
    builder: *mut NeziaSnapshotBuilder,
) -> NeziaSnapshotId {
    guard_value(NeziaSnapshotId::INVALID, || {
        if builder.is_null() {
            return NeziaSnapshotId::INVALID;
        }
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            // builder は呼出側責任で `_cancel` してもらう (engine NULL は致命的なので
            // ここで free しない)。
            return NeziaSnapshotId::INVALID;
        };
        // SAFETY: 呼出側契約により `builder` は `_begin` の戻り値かつ未解放。
        let b = unsafe { Box::from_raw(builder) };

        let mut sb = engine.inner.snapshot_builder();
        for (bus, gain) in b.bus_gains {
            sb = sb.set_bus_gain(bus.to_core(), gain);
        }
        for (bus, muted) in b.bus_muted {
            sb = sb.set_bus_muted(bus.to_core(), muted);
        }
        for (send, gain) in b.send_gains {
            sb = sb.set_send_gain(
                SendId {
                    index: send.index,
                    generation: send.generation,
                },
                gain,
            );
        }
        for (effect, kind, param, value) in b.effect_params {
            sb = match kind {
                0 => sb.set_effect_param(effect.to_core(), RawLpf(param), value),
                1 => sb.set_effect_param(effect.to_core(), RawHpf(param), value),
                2 => sb.set_effect_param(effect.to_core(), RawReverb(param), value),
                3 => sb.set_effect_param(effect.to_core(), RawCompressor(param), value),
                4 => sb.set_effect_param(effect.to_core(), RawPeakingEq(param), value),
                5 => sb.set_effect_param(effect.to_core(), RawLimiter(param), value),
                _ => sb,
            };
        }
        sb.commit()
            .map(NeziaSnapshotId::from_core)
            .unwrap_or(NeziaSnapshotId::INVALID)
    })
}

/// `_begin` で取得した builder を commit せずに解放する。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_snapshot_builder_cancel(builder: *mut NeziaSnapshotBuilder) {
    if builder.is_null() {
        return;
    }
    guard_value((), || {
        // SAFETY: 呼出側契約により `builder` は `_begin` の戻り値かつ未解放。
        unsafe { drop(Box::from_raw(builder)) };
    })
}

/// Snapshot を破棄する (進行中の補間には影響しない)。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_snapshot_destroy(
    engine: *mut NeziaEngine,
    id: NeziaSnapshotId,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine.inner.destroy_snapshot(id.to_core()) {
            NeziaResult::Ok
        } else {
            NeziaResult::InvalidHandle
        }
    })
}

/// Snapshot を `fade_seconds` かけて適用する (0.0 で即時)。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_snapshot_apply(
    engine: *mut NeziaEngine,
    id: NeziaSnapshotId,
    fade_seconds: f32,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine.inner.apply_snapshot(id.to_core(), fade_seconds) {
            NeziaResult::Ok
        } else {
            NeziaResult::InvalidHandle
        }
    })
}

// ── core::SnapshotBuilder::set_effect_param が `P: EffectParamId` を要求するため、
//     KIND だけが違う 4 種の dummy ZST 風型を用意する (effect.rs の RawParam と同思想)。

#[derive(Clone, Copy)]
struct RawLpf(u8);
impl EffectParamId for RawLpf {
    const KIND: EffectKind = EffectKind::Lpf;
    fn as_u8(self) -> u8 {
        self.0
    }
}
#[derive(Clone, Copy)]
struct RawHpf(u8);
impl EffectParamId for RawHpf {
    const KIND: EffectKind = EffectKind::Hpf;
    fn as_u8(self) -> u8 {
        self.0
    }
}
#[derive(Clone, Copy)]
struct RawReverb(u8);
impl EffectParamId for RawReverb {
    const KIND: EffectKind = EffectKind::Reverb;
    fn as_u8(self) -> u8 {
        self.0
    }
}
#[derive(Clone, Copy)]
struct RawCompressor(u8);
impl EffectParamId for RawCompressor {
    const KIND: EffectKind = EffectKind::Compressor;
    fn as_u8(self) -> u8 {
        self.0
    }
}
#[derive(Clone, Copy)]
struct RawPeakingEq(u8);
impl EffectParamId for RawPeakingEq {
    const KIND: EffectKind = EffectKind::PeakingEq;
    fn as_u8(self) -> u8 {
        self.0
    }
}
#[derive(Clone, Copy)]
struct RawLimiter(u8);
impl EffectParamId for RawLimiter {
    const KIND: EffectKind = EffectKind::Limiter;
    fn as_u8(self) -> u8 {
        self.0
    }
}
