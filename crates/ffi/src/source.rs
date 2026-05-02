//! Source の生成・再生・空間パラメータ・バッチ更新。

use core::ffi::c_void;
use std::slice;

use crate::engine::NeziaEngine;
use crate::panic::{guard_entity, guard_result, guard_value};
use crate::types::{
    NeziaAttenuationModel, NeziaBufferId, NeziaEntityId, NeziaFinishCallback, NeziaResult,
    NeziaSourcePositionUpdate,
};

/// 生ポインタを `Send` なクロージャに渡すための変換。
///
/// `*mut c_void` 自体は `!Send` であり、edition 2024 の disjoint capture では
/// ラッパ構造体に対する `unsafe impl Send` も効かない（フィールドアクセスで
/// ポインタ単体が捕捉されるため）。アドレスを `usize` として渡し、コールバック
/// 直前で復元することで Send を満たす。呼出側が「コールバック発火時まで
/// `user_data` を有効に保つ」契約を守ることが前提。
#[inline]
fn pack(ptr: *mut c_void) -> usize {
    ptr as usize
}
#[inline]
fn unpack(addr: usize) -> *mut c_void {
    addr as *mut c_void
}

/// マスターバスにボイスを再生する（fire-and-forget）。
///
/// 戻り値: 1 = 受理、0 = 失敗（無効バッファ / コマンドキュー満杯）。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_play(
    engine: *mut NeziaEngine,
    buffer: NeziaBufferId,
    volume: f32,
    pitch: f32,
) -> u8 {
    guard_value(0, || {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return 0;
        };
        engine.inner.play(buffer.to_core(), volume, pitch) as u8
    })
}

/// マスターバスにボイスを再生し、自然終了時にコールバックを呼ぶ。
///
/// `user_data` のライフタイムはコールバック発火まで呼出側が保証する。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_play_with_callback(
    engine: *mut NeziaEngine,
    buffer: NeziaBufferId,
    volume: f32,
    pitch: f32,
    callback: NeziaFinishCallback,
    user_data: *mut c_void,
) -> u8 {
    guard_value(0, || {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return 0;
        };
        let ud = pack(user_data);
        let cb = callback;
        engine
            .inner
            .play_with_callback(buffer.to_core(), volume, pitch, move || {
                if let Some(f) = cb {
                    // SAFETY: 呼出側契約により f / user_data は有効。
                    unsafe { f(unpack(ud)) };
                }
            }) as u8
    })
}

/// 指定バスにボイスを再生する。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_play_to_bus(
    engine: *mut NeziaEngine,
    buffer: NeziaBufferId,
    volume: f32,
    pitch: f32,
    bus: NeziaEntityId,
) -> u8 {
    guard_value(0, || {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return 0;
        };
        engine
            .inner
            .play_to_bus(buffer.to_core(), volume, pitch, bus.to_core()) as u8
    })
}

/// 指定バスにボイスを再生し、自然終了時にコールバックを呼ぶ。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_play_to_bus_with_callback(
    engine: *mut NeziaEngine,
    buffer: NeziaBufferId,
    volume: f32,
    pitch: f32,
    bus: NeziaEntityId,
    callback: NeziaFinishCallback,
    user_data: *mut c_void,
) -> u8 {
    guard_value(0, || {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return 0;
        };
        let ud = pack(user_data);
        let cb = callback;
        engine.inner.play_to_bus_with_callback(
            buffer.to_core(),
            volume,
            pitch,
            bus.to_core(),
            move || {
                if let Some(f) = cb {
                    // SAFETY: 呼出側契約により f / user_data は有効。
                    unsafe { f(unpack(ud)) };
                }
            },
        ) as u8
    })
}

/// 3D ソースをスポーンし、EntityId を返す。失敗時は INVALID。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_spawn(
    engine: *mut NeziaEngine,
    buffer: NeziaBufferId,
    volume: f32,
    pitch: f32,
    bus: NeziaEntityId,
) -> NeziaEntityId {
    guard_entity(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaEntityId::INVALID;
        };
        engine
            .inner
            .spawn_source(buffer.to_core(), volume, pitch, bus.to_core())
            .map(NeziaEntityId::from_core)
            .unwrap_or(NeziaEntityId::INVALID)
    })
}

/// ソースの距離減衰パラメータを設定する（初期化・変更時のみ）。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_set_spatial_params(
    engine: *mut NeziaEngine,
    source: NeziaEntityId,
    model: NeziaAttenuationModel,
    min_distance: f32,
    max_distance: f32,
    rolloff: f32,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine.inner.set_source_spatial_params(
            source.to_core(),
            model.to_core(),
            min_distance,
            max_distance,
            rolloff,
        ) {
            NeziaResult::Ok
        } else {
            NeziaResult::QueueFull
        }
    })
}

/// ソースの空間演算を有効化・無効化する。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_set_spatial_enabled(
    engine: *mut NeziaEngine,
    source: NeziaEntityId,
    enabled: u8,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine
            .inner
            .set_source_spatial_enabled(source.to_core(), enabled != 0)
        {
            NeziaResult::Ok
        } else {
            NeziaResult::QueueFull
        }
    })
}

/// 複数ソースの位置を一括更新する（毎フレーム想定）。
///
/// # 安全性
/// `updates_ptr` は `updates_len` 要素分の有効領域を指すこと。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_batch_set_positions(
    engine: *mut NeziaEngine,
    updates_ptr: *const NeziaSourcePositionUpdate,
    updates_len: usize,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if updates_len == 0 {
            engine.inner.batch_set_source_positions(&[]);
            return NeziaResult::Ok;
        }
        if updates_ptr.is_null() {
            return NeziaResult::NullPointer;
        }
        // SAFETY: 呼出側契約。
        let raw = unsafe { slice::from_raw_parts(updates_ptr, updates_len) };
        // ABI 型から core 型へ変換した一時配列を作る。
        // MAX_SOURCES でクランプはコア側がやるが、alloc 削減のため小さい入力ならスタック相当でも
        // よい。現状は素直に Vec で渡す。
        let converted: Vec<(nezia_core::EntityId, [f32; 3])> = raw
            .iter()
            .map(|u| (u.source.to_core(), u.position.to_array()))
            .collect();
        engine.inner.batch_set_source_positions(&converted);
        NeziaResult::Ok
    })
}
