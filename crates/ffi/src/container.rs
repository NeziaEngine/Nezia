//! Container 関連 FFI (Phase 4-2)。
//!
//! 現状 Random Container のみサポート (Switch / Sequence は将来実装)。
//! 設計詳細は `docs/design/core/container.md` 参照。

use std::ffi::c_void;
use std::slice;

use nezia_core::{BufferId, ContainerId};

use crate::engine::NeziaEngine;
use crate::panic::{guard_entity, guard_result, guard_value};
use crate::types::{NeziaBufferId, NeziaEntityId, NeziaFinishCallback, NeziaResult};

// `NeziaBufferId` と `core::BufferId` のレイアウト一致を保証する
// (`nezia_container_create_random` でゼロコピー slice cast するため)。
const _: () = {
    assert!(
        size_of::<NeziaBufferId>() == size_of::<BufferId>(),
        "NeziaBufferId と core::BufferId のサイズが一致しない"
    );
    assert!(
        align_of::<NeziaBufferId>() == align_of::<BufferId>(),
        "NeziaBufferId と core::BufferId のアラインが一致しない"
    );
    assert!(size_of::<NeziaBufferId>() == 8);
};

/// Container ハンドル (`core::ContainerId` の ABI ミラー)。
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NeziaContainerId {
    pub index: u32,
    pub generation: u32,
}

impl NeziaContainerId {
    pub(crate) const INVALID: Self = Self {
        index: u32::MAX,
        generation: 0,
    };

    #[inline]
    fn from_core(id: ContainerId) -> Self {
        Self {
            index: id.index,
            generation: id.generation,
        }
    }

    #[inline]
    fn to_core(self) -> ContainerId {
        ContainerId {
            index: self.index,
            generation: self.generation,
        }
    }
}

/// Random Container を生成する。`children_ptr` は `BufferId` の配列。
/// `children_len == 0` または容量超過で `INVALID` を返す。
///
/// # 安全性
/// `children_ptr` は `children_len` 個の `NeziaBufferId` を読める有効領域を指すこと。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_container_create_random(
    engine: *mut NeziaEngine,
    children_ptr: *const NeziaBufferId,
    children_len: usize,
) -> NeziaContainerId {
    guard_value(NeziaContainerId::INVALID, || {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaContainerId::INVALID;
        };
        if children_len == 0 || children_ptr.is_null() {
            return NeziaContainerId::INVALID;
        }
        // SAFETY:
        // - 呼出側契約により `children_ptr` は `children_len` 要素分の有効領域。
        // - `NeziaBufferId` と `core::BufferId` のレイアウト一致は上の const アサートで保証済み。
        //   両者とも `#[repr(C)]` で `(u32, u32)` の組なので要素ごとの再解釈は安全。
        let children: &[BufferId] =
            unsafe { slice::from_raw_parts(children_ptr.cast::<BufferId>(), children_len) };
        engine
            .inner
            .create_random_container(children)
            .map(NeziaContainerId::from_core)
            .unwrap_or(NeziaContainerId::INVALID)
    })
}

/// Container を破棄する。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_container_destroy(
    engine: *mut NeziaEngine,
    id: NeziaContainerId,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine.inner.destroy_container(id.to_core()) {
            NeziaResult::Ok
        } else {
            NeziaResult::InvalidHandle
        }
    })
}

/// Container から子を 1 つ選んで指定バスで再生する (fire-and-forget)。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_container_play(
    engine: *mut NeziaEngine,
    container: NeziaContainerId,
    vol: f32,
    pitch: f32,
    bus: NeziaEntityId,
    looping: u8,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine
            .inner
            .play_container(container.to_core(), vol, pitch, bus.to_core(), looping != 0)
        {
            NeziaResult::Ok
        } else {
            NeziaResult::InvalidHandle
        }
    })
}

/// Container から子を 1 つ選んでハンドル付きで再生する。
/// 戻り値は **選ばれた 1 つの Source の `EntityId`** (Container ハンドルとは別)。
///
/// `callback` が `Some` のとき、自然終了時に `nezia_engine_poll_events()` 経由で
/// 1 度だけ呼ばれる (`looping != 0` の場合は呼ばれない)。`user_data` のライフタイムは
/// コールバック発火まで呼出側が保証する。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_container_play_with_handle(
    engine: *mut NeziaEngine,
    container: NeziaContainerId,
    vol: f32,
    pitch: f32,
    bus: NeziaEntityId,
    looping: u8,
    callback: NeziaFinishCallback,
    user_data: *mut c_void,
) -> NeziaEntityId {
    guard_entity(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaEntityId::INVALID;
        };
        let result = match callback {
            Some(f) => unsafe {
                // SAFETY: 呼出側契約により f / user_data は発火時まで有効。
                engine.inner.play_container_with_handle_and_callback_native(
                    container.to_core(),
                    vol,
                    pitch,
                    bus.to_core(),
                    looping != 0,
                    f,
                    user_data,
                )
            },
            None => engine.inner.play_container_with_handle(
                container.to_core(),
                vol,
                pitch,
                bus.to_core(),
                looping != 0,
            ),
        };
        result
            .map(NeziaEntityId::from_core)
            .unwrap_or(NeziaEntityId::INVALID)
    })
}
