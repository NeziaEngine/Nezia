//! エンジン生成・破棄・グローバル制御。

use nezia_core::SoundEngine;

use crate::panic::{guard_entity, guard_result, guard_value};
use crate::types::{NeziaEntityId, NeziaResult};

/// 不透明エンジンハンドル。
///
/// `*mut NeziaEngine` は `Box<SoundEngine>` を `into_raw` したポインタとして扱う。
pub struct NeziaEngine {
    pub(crate) inner: SoundEngine,
}

/// エンジンを生成する。失敗時は NULL を返す。
///
/// # 安全性
/// 戻り値は `nezia_engine_free` で必ず解放すること。
#[unsafe(no_mangle)]
pub extern "C" fn nezia_engine_new() -> *mut NeziaEngine {
    guard_value(std::ptr::null_mut(), || match SoundEngine::new() {
        Ok(engine) => Box::into_raw(Box::new(NeziaEngine { inner: engine })),
        Err(_) => std::ptr::null_mut(),
    })
}

/// エンジンを破棄する。NULL は無視する。
///
/// # 安全性
/// `engine` は `nezia_engine_new` の戻り値かつ未解放であること。同一ポインタを
/// 二重解放してはならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_engine_free(engine: *mut NeziaEngine) {
    if engine.is_null() {
        return;
    }
    guard_value((), || {
        // SAFETY: 呼出側契約により `engine` は `Box::into_raw` 由来。
        unsafe { drop(Box::from_raw(engine)) };
    });
}

/// マスター音量（マスターバスの gain）を設定する。0.0〜1.0 にクランプされる。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_engine_set_volume(
    engine: *mut NeziaEngine,
    volume: f32,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine.inner.set_volume(volume) {
            NeziaResult::Ok
        } else {
            NeziaResult::QueueFull
        }
    })
}

/// すべてのボイスを停止する。登録済みコールバックは解放される（呼ばれない）。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_engine_stop_all(engine: *mut NeziaEngine) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine.inner.stop_all() {
            NeziaResult::Ok
        } else {
            NeziaResult::QueueFull
        }
    })
}

/// イベントをドレインし、登録済み再生終了コールバックを呼び出す。
///
/// ゲームループの毎フレーム末尾で呼ぶ想定。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_engine_poll_events(engine: *mut NeziaEngine) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        engine.inner.poll_events();
        NeziaResult::Ok
    })
}

/// マスターバスの EntityId を返す。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_engine_master_bus(engine: *mut NeziaEngine) -> NeziaEntityId {
    guard_entity(|| {
        let Some(engine) = (unsafe { engine.as_ref() }) else {
            return NeziaEntityId::INVALID;
        };
        NeziaEntityId::from_core(engine.inner.master_bus())
    })
}
