//! オーディオバッファのロード・アンロード。

use std::slice;

use crate::engine::NeziaEngine;
use crate::panic::{guard_buffer, guard_result};
use crate::types::{NeziaBufferId, NeziaResult};

/// オーディオファイルをロードしてハンドルを返す。失敗時は `INVALID` を返す。
///
/// # 引数
/// - `path_ptr` / `path_len`: UTF-8 のファイルパス。NUL 終端は不要。
///
/// # 安全性
/// `path_ptr` は `path_len` バイト読める有効な領域を指すこと。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_buffer_load(
    engine: *mut NeziaEngine,
    path_ptr: *const u8,
    path_len: usize,
) -> NeziaBufferId {
    guard_buffer(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaBufferId::INVALID;
        };
        if path_ptr.is_null() {
            return NeziaBufferId::INVALID;
        }
        // SAFETY: 呼出側契約により path_ptr は path_len バイトの有効領域。
        let bytes = unsafe { slice::from_raw_parts(path_ptr, path_len) };
        let Ok(path) = std::str::from_utf8(bytes) else {
            return NeziaBufferId::INVALID;
        };
        match engine.inner.load(path) {
            Ok(id) => NeziaBufferId::from_core(id),
            Err(_) => NeziaBufferId::INVALID,
        }
    })
}

/// バッファをアンロードする。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_buffer_unload(
    engine: *mut NeziaEngine,
    buffer: NeziaBufferId,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine.inner.unload(buffer.to_core()) {
            NeziaResult::Ok
        } else {
            NeziaResult::InvalidHandle
        }
    })
}
