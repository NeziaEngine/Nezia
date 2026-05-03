//! DSP エフェクト関連 FFI (Phase 2-3 PR 1: バス単位 LPF / HPF)。

use crate::engine::NeziaEngine;
use crate::panic::{guard_entity, guard_result};
use crate::types::{NeziaEntityId, NeziaResult};
use nezia_core::{EffectKind, EffectPosition, EffectTarget};

/// エフェクト種別 (`core::EffectKind` の ABI ミラー)。
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeziaEffectKind {
    Lpf = 0,
    Hpf = 1,
    Reverb = 2,
}

impl NeziaEffectKind {
    fn to_core(self) -> EffectKind {
        match self {
            Self::Lpf => EffectKind::Lpf,
            Self::Hpf => EffectKind::Hpf,
            Self::Reverb => EffectKind::Reverb,
        }
    }
}

/// エフェクト挿入位置 (`core::EffectPosition` の ABI ミラー)。
/// Bus: 0=Pre-Fader, 1=Post-Fader / Source: 0=Pre-Spatial, 1=Post-Spatial
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeziaEffectPosition {
    Pre = 0,
    Post = 1,
}

impl NeziaEffectPosition {
    fn to_core(self) -> EffectPosition {
        match self {
            Self::Pre => EffectPosition::Pre,
            Self::Post => EffectPosition::Post,
        }
    }
}

/// エフェクト対象種別。`bus_or_source` の解釈を切り替える。
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeziaEffectTargetKind {
    Bus = 0,
    Source = 1,
}

/// バス / ソースのチェーン末尾にエフェクトを追加する。
///
/// 戻り値: 有効な `NeziaEntityId` (= EffectId) または `INVALID`。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_effect_add(
    engine: *mut NeziaEngine,
    target_kind: NeziaEffectTargetKind,
    bus_or_source: NeziaEntityId,
    kind: NeziaEffectKind,
    position: NeziaEffectPosition,
) -> NeziaEntityId {
    guard_entity(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaEntityId::INVALID;
        };
        let target = match target_kind {
            NeziaEffectTargetKind::Bus => EffectTarget::Bus(bus_or_source.to_core()),
            NeziaEffectTargetKind::Source => EffectTarget::Source(bus_or_source.to_core()),
        };
        engine
            .inner
            .add_effect(target, kind.to_core(), position.to_core())
            .map(NeziaEntityId::from_core)
            .unwrap_or(NeziaEntityId::INVALID)
    })
}

/// エフェクトを削除する。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_effect_remove(
    engine: *mut NeziaEngine,
    effect: NeziaEntityId,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine.inner.remove_effect(effect.to_core()) {
            NeziaResult::Ok
        } else {
            NeziaResult::QueueFull
        }
    })
}

/// エフェクトの enabled をトグルする。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_effect_set_enabled(
    engine: *mut NeziaEngine,
    effect: NeziaEntityId,
    enabled: u8,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine
            .inner
            .set_effect_enabled(effect.to_core(), enabled != 0)
        {
            NeziaResult::Ok
        } else {
            NeziaResult::QueueFull
        }
    })
}

/// エフェクトパラメータを設定する。
///
/// `param` は種別ごとに以下を意味する:
/// - LPF / HPF: 0=Cutoff (Hz), 1=Q
/// - Reverb: PR 2 で実装
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_effect_set_param(
    engine: *mut NeziaEngine,
    effect: NeziaEntityId,
    param: u8,
    value: f32,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        // core 側は EffectParamId トレイト経由で型安全だが、FFI 層では数値で透過する。
        // 内部では Command::SetEffectParam に直接 u8 を流すため小さなショートカットを使う。
        // ここでは `set_effect_param` の代わりにコマンドを直接送るためのインライン実装に近い形で扱うが、
        // SoundEngine 側に u8 直渡しエントリポイントを追加しないと到達できないため、
        // u8 を中継するラッパを SoundEngine に追加する選択肢もある。
        // PR 1 では既存 API を呼び出すためのアドホック ZST を使う。
        struct RawParam(u8);
        impl Copy for RawParam {}
        impl Clone for RawParam {
            fn clone(&self) -> Self {
                *self
            }
        }
        impl nezia_core::EffectParamId for RawParam {
            // KIND は SetEffectParam 命令上は使われないため (audio thread が EffectWorld から
            // kind を読んで分岐する) ダミーで Lpf を入れる。FFI では型安全性は呼出側の責務。
            const KIND: nezia_core::EffectKind = nezia_core::EffectKind::Lpf;
            fn as_u8(self) -> u8 {
                self.0
            }
        }
        if engine
            .inner
            .set_effect_param(effect.to_core(), RawParam(param), value)
        {
            NeziaResult::Ok
        } else {
            NeziaResult::QueueFull
        }
    })
}
