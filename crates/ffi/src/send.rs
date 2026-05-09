//! Send (副ルート) 関連 FFI (Phase 3-3)。
//!
//! バス → バスの Aux Send、およびバス → Compressor sidechain Send を扱う。
//! 設計詳細は `docs/design/core/send.md` 参照。

use nezia_core::{SendId, SendPosition};

use crate::engine::NeziaEngine;
use crate::panic::{guard_result, guard_value};
use crate::types::{NeziaEntityId, NeziaResult};

/// Send 識別ハンドル (`core::SendId` の ABI ミラー)。
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NeziaSendId {
    pub index: u32,
    pub generation: u32,
}

impl NeziaSendId {
    pub(crate) const INVALID: Self = Self {
        index: u32::MAX,
        generation: 0,
    };

    #[inline]
    fn from_core(id: SendId) -> Self {
        Self {
            index: id.index,
            generation: id.generation,
        }
    }

    #[inline]
    pub(crate) fn to_core(self) -> SendId {
        SendId {
            index: self.index,
            generation: self.generation,
        }
    }
}

/// Send のタップ位置 (`core::SendPosition` の ABI ミラー)。
/// Pre = Fader 適用前で tap (本線 mute / gain 0 でも流れる)。
/// Post = Fader 適用後で tap (本線 mute なら Send もゼロ)。
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeziaSendPosition {
    Pre = 0,
    Post = 1,
}

impl NeziaSendPosition {
    fn to_core(self) -> SendPosition {
        match self {
            Self::Pre => SendPosition::Pre,
            Self::Post => SendPosition::Post,
        }
    }
}

/// バス → バスの Send を作成する。失敗時は `INVALID`。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_send_add_bus_to_bus(
    engine: *mut NeziaEngine,
    src: NeziaEntityId,
    dst: NeziaEntityId,
    position: NeziaSendPosition,
    gain: f32,
) -> NeziaSendId {
    guard_value(NeziaSendId::INVALID, || {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaSendId::INVALID;
        };
        engine
            .inner
            .add_send(src.to_core(), dst.to_core(), position.to_core(), gain)
            .map(NeziaSendId::from_core)
            .unwrap_or(NeziaSendId::INVALID)
    })
}

/// ソース → バスの Send を作成する (User-Defined Aux Send)。
/// 失敗時は `INVALID`。
///
/// Wwise / FMOD の per-event aux send 互換。同じ Reverb Bus を共有しつつ、音ごとに
/// reverb 量を独立に持たせるのに使う (`add_send_bus_to_bus` がバス全体に同一量を
/// かけるのと対比)。`src` が現在 spawn 中でない場合、audio thread 側で silently drop され、
/// `Event::SourceDespawned` 経路で SendId 自体は解放される。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_send_add_source_to_bus(
    engine: *mut NeziaEngine,
    src: NeziaEntityId,
    dst: NeziaEntityId,
    position: NeziaSendPosition,
    gain: f32,
) -> NeziaSendId {
    guard_value(NeziaSendId::INVALID, || {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaSendId::INVALID;
        };
        engine
            .inner
            .add_source_send(src.to_core(), dst.to_core(), position.to_core(), gain)
            .map(NeziaSendId::from_core)
            .unwrap_or(NeziaSendId::INVALID)
    })
}

/// ソース → Compressor sidechain 入力の Send を作成する。
/// `compressor` は `nezia_effect_add` で生成した Compressor の EffectId。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_send_add_source_to_compressor(
    engine: *mut NeziaEngine,
    src: NeziaEntityId,
    compressor: NeziaEntityId,
    position: NeziaSendPosition,
    gain: f32,
) -> NeziaSendId {
    guard_value(NeziaSendId::INVALID, || {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaSendId::INVALID;
        };
        engine
            .inner
            .add_source_send_to_compressor(
                src.to_core(),
                compressor.to_core(),
                position.to_core(),
                gain,
            )
            .map(NeziaSendId::from_core)
            .unwrap_or(NeziaSendId::INVALID)
    })
}

/// バス → Compressor sidechain 入力の Send を作成する。
/// `compressor` は `nezia_effect_add` で生成した Compressor の EffectId。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_send_add_bus_to_compressor(
    engine: *mut NeziaEngine,
    src: NeziaEntityId,
    compressor: NeziaEntityId,
    position: NeziaSendPosition,
    gain: f32,
) -> NeziaSendId {
    guard_value(NeziaSendId::INVALID, || {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaSendId::INVALID;
        };
        engine
            .inner
            .add_send_to_compressor(
                src.to_core(),
                compressor.to_core(),
                position.to_core(),
                gain,
            )
            .map(NeziaSendId::from_core)
            .unwrap_or(NeziaSendId::INVALID)
    })
}

/// Send を削除する。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_send_remove(
    engine: *mut NeziaEngine,
    send: NeziaSendId,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine.inner.remove_send(send.to_core()) {
            NeziaResult::Ok
        } else {
            NeziaResult::InvalidHandle
        }
    })
}

/// Send の gain を設定する。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_send_set_gain(
    engine: *mut NeziaEngine,
    send: NeziaSendId,
    gain: f32,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine.inner.set_send_gain(send.to_core(), gain) {
            NeziaResult::Ok
        } else {
            NeziaResult::InvalidHandle
        }
    })
}

/// Send のタップ位置 (Pre / Post-Fader) を変更する。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_send_set_position(
    engine: *mut NeziaEngine,
    send: NeziaSendId,
    position: NeziaSendPosition,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine
            .inner
            .set_send_position(send.to_core(), position.to_core())
        {
            NeziaResult::Ok
        } else {
            NeziaResult::InvalidHandle
        }
    })
}

/// Compressor の sidechain 駆動を on/off する。
/// `add_send_to_compressor` は内部で自動的に on にするため、後から off にしたいときに使う。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_compressor_bind_sidechain(
    engine: *mut NeziaEngine,
    compressor: NeziaEntityId,
    enabled: u8,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine
            .inner
            .bind_compressor_sidechain(compressor.to_core(), enabled != 0)
        {
            NeziaResult::Ok
        } else {
            NeziaResult::InvalidHandle
        }
    })
}
