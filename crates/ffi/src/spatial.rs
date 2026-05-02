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
