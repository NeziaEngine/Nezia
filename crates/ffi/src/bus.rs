//! バスの生成・破棄・ゲイン/ミュート/ルーティング。

use crate::engine::NeziaEngine;
use crate::panic::{guard_entity, guard_result};
use crate::types::{NeziaEntityId, NeziaResult};

/// マスターバス直下に新しいバスを生成する。失敗時は INVALID。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_bus_create(engine: *mut NeziaEngine, gain: f32) -> NeziaEntityId {
    guard_entity(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaEntityId::INVALID;
        };
        engine
            .inner
            .create_bus(gain)
            .map(NeziaEntityId::from_core)
            .unwrap_or(NeziaEntityId::INVALID)
    })
}

/// 指定した親バス配下に新しいバスを生成する。失敗時は INVALID。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_bus_create_routed(
    engine: *mut NeziaEngine,
    gain: f32,
    parent: NeziaEntityId,
) -> NeziaEntityId {
    guard_entity(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaEntityId::INVALID;
        };
        engine
            .inner
            .create_bus_routed(gain, parent.to_core())
            .map(NeziaEntityId::from_core)
            .unwrap_or(NeziaEntityId::INVALID)
    })
}

/// バスを削除する。マスターバスは削除できない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_bus_destroy(
    engine: *mut NeziaEngine,
    bus: NeziaEntityId,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine.inner.destroy_bus(bus.to_core()) {
            NeziaResult::Ok
        } else {
            NeziaResult::InvalidHandle
        }
    })
}

/// バスのゲインを設定する。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_bus_set_gain(
    engine: *mut NeziaEngine,
    bus: NeziaEntityId,
    gain: f32,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine.inner.set_bus_gain(bus.to_core(), gain) {
            NeziaResult::Ok
        } else {
            NeziaResult::QueueFull
        }
    })
}

/// バスのミュートを設定する。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_bus_set_muted(
    engine: *mut NeziaEngine,
    bus: NeziaEntityId,
    muted: u8,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine.inner.set_bus_muted(bus.to_core(), muted != 0) {
            NeziaResult::Ok
        } else {
            NeziaResult::QueueFull
        }
    })
}

/// バスの出力先を変更する。ループが検出された場合は `BusLoopDetected` を返す。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_bus_set_output(
    engine: *mut NeziaEngine,
    bus: NeziaEntityId,
    parent: NeziaEntityId,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine.inner.set_bus_output(bus.to_core(), parent.to_core()) {
            NeziaResult::Ok
        } else {
            NeziaResult::BusLoopDetected
        }
    })
}
