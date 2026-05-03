//! リスナー位置・姿勢の設定。

use core::slice;

use crate::engine::NeziaEngine;
use crate::panic::guard_result;
use crate::types::{NeziaResult, NeziaSourceVelocityUpdate, NeziaVec3};
use nezia_core::SourceVelocityUpdate;

// `NeziaSourceVelocityUpdate` と `core::SourceVelocityUpdate` のレイアウト一致を保証する
// （`batch_set_velocities` でゼロコピーキャストするため）。
const _: () = {
    assert!(
        size_of::<NeziaSourceVelocityUpdate>() == size_of::<SourceVelocityUpdate>(),
        "NeziaSourceVelocityUpdate と core::SourceVelocityUpdate のサイズが一致しない"
    );
    assert!(
        align_of::<NeziaSourceVelocityUpdate>() == align_of::<SourceVelocityUpdate>(),
        "NeziaSourceVelocityUpdate と core::SourceVelocityUpdate のアラインが一致しない"
    );
    assert!(size_of::<NeziaSourceVelocityUpdate>() == 20);
};

/// リスナーの位置・向きを更新する（毎フレーム呼び出し可）。
///
/// `forward` / `up` は内部で正規化される。triple buffer 経由で publish するため
/// キュー詰まりは発生しない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_listener_set(
    engine: *mut NeziaEngine,
    position: NeziaVec3,
    forward: NeziaVec3,
    up: NeziaVec3,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        engine
            .inner
            .set_listener(position.to_array(), forward.to_array(), up.to_array());
        NeziaResult::Ok
    })
}

/// SP-10: リスナーの速度ベクトル (m/s) を設定する。Doppler 計算に使用。
///
/// `nezia_listener_set` と同じ triple buffer に乗るため、両者は順序を問わず
/// 同フレーム内で呼んで構わない。既定値 `(0,0,0)` では Doppler 効果は発生しない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_listener_set_velocity(
    engine: *mut NeziaEngine,
    velocity: NeziaVec3,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        engine.inner.set_listener_velocity(velocity.to_array());
        NeziaResult::Ok
    })
}

/// SP-10: 媒質中の音速 (m/s) を設定する。0 以下は無視される。既定値 343.0（Unity 互換）。
///
/// 用途例: 水中シーンで 1480.0 等に変更。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_set_sound_speed(
    engine: *mut NeziaEngine,
    speed: f32,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine.inner.set_sound_speed(speed) {
            NeziaResult::Ok
        } else {
            NeziaResult::QueueFull
        }
    })
}

/// SP-10: 複数ソースの速度を一括更新する（毎フレーム想定）。
///
/// # 安全性
/// `updates_ptr` は `updates_len` 要素分の有効領域を指すこと。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_batch_set_velocities(
    engine: *mut NeziaEngine,
    updates_ptr: *const NeziaSourceVelocityUpdate,
    updates_len: usize,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if updates_len == 0 {
            engine.inner.batch_set_source_velocities(&[]);
            return NeziaResult::Ok;
        }
        if updates_ptr.is_null() {
            return NeziaResult::NullPointer;
        }
        // SAFETY:
        // - `updates_ptr` は呼出側契約により `updates_len` 要素分の有効領域。
        // - `NeziaSourceVelocityUpdate` と `SourceVelocityUpdate` は上の const アサーションで
        //   レイアウト一致を保証しているので、要素ごとの再解釈はトリビアルに安全。
        let updates: &[SourceVelocityUpdate] = unsafe {
            slice::from_raw_parts(updates_ptr.cast::<SourceVelocityUpdate>(), updates_len)
        };
        engine.inner.batch_set_source_velocities(updates);
        NeziaResult::Ok
    })
}

/// SP-10: ソースの Doppler 効果レベル `[0.0, 1.0]` を設定する。
///
/// 0.0 で Doppler 完全無効、1.0 で物理計算を完全適用。値域外は内部でクランプされる。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_set_doppler_level(
    engine: *mut NeziaEngine,
    source: crate::types::NeziaEntityId,
    level: f32,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine
            .inner
            .set_source_doppler_level(source.to_core(), level)
        {
            NeziaResult::Ok
        } else {
            NeziaResult::QueueFull
        }
    })
}

/// SP-06: リスナーフォーカスを設定する。
///
/// 距離減衰用とパンニング用で独立した補間係数を取り、空間演算では
/// `lerp(listener_position, focus_point, level)` で導出した仮想リスナー位置を使う。
/// `*_focus_level = 0.0` でフォーカス無効。値域外 `[0.0, 1.0]` は内部でクランプされる。
///
/// コマンドキューが満杯の場合は `QueueFull` を返す。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_listener_set_focus(
    engine: *mut NeziaEngine,
    focus_point: NeziaVec3,
    distance_focus_level: f32,
    direction_focus_level: f32,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine.inner.set_listener_focus(
            focus_point.to_array(),
            distance_focus_level,
            direction_focus_level,
        ) {
            NeziaResult::Ok
        } else {
            NeziaResult::QueueFull
        }
    })
}
