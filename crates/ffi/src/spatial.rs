//! リスナー位置・姿勢の設定。

use crate::engine::NeziaEngine;
use crate::panic::guard_result;
use crate::types::{NeziaResult, NeziaVec3};

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
