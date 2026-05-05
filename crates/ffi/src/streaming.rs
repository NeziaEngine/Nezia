//! ストリーミング再生関連 FFI (Phase 2-4)。
//!
//! 静的バッファ ID と同じ ID 空間を返すので、`nezia_play_*` 系の既存 API がそのまま
//! 使える。設計詳細は `docs/design/core/streaming.md` 参照。

use std::slice;

use nezia_core::StreamingOpts;

use crate::engine::NeziaEngine;
use crate::panic::{guard_buffer, guard_value};
use crate::types::NeziaBufferId;

/// ストリーミングオプション (`core::StreamingOpts` の ABI ミラー)。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NeziaStreamingOpts {
    /// リング容量の目安 (秒)。default 1.0。
    pub buffer_seconds: f32,
}

impl NeziaStreamingOpts {
    fn to_core(self) -> StreamingOpts {
        StreamingOpts {
            buffer_seconds: self.buffer_seconds,
        }
    }
}

/// オーディオファイルをストリーミング再生用にロードしてハンドルを返す。失敗時は `INVALID`。
///
/// # 引数
/// - `path_ptr` / `path_len`: UTF-8 のファイルパス。NUL 終端は不要。
/// - `opts`: ストリーミングオプション。`{ buffer_seconds: 1.0 }` を渡せばデフォルト相当。
///
/// # 安全性
/// `path_ptr` は `path_len` バイト読める有効領域を指すこと。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_buffer_load_streaming(
    engine: *mut NeziaEngine,
    path_ptr: *const u8,
    path_len: usize,
    opts: NeziaStreamingOpts,
) -> NeziaBufferId {
    guard_buffer(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaBufferId::INVALID;
        };
        if path_ptr.is_null() {
            return NeziaBufferId::INVALID;
        }
        // SAFETY: 呼出側契約。
        let bytes = unsafe { slice::from_raw_parts(path_ptr, path_len) };
        let Ok(path) = std::str::from_utf8(bytes) else {
            return NeziaBufferId::INVALID;
        };
        match engine.inner.load_streaming(path, opts.to_core()) {
            Ok(id) => NeziaBufferId::from_core(id),
            Err(_) => NeziaBufferId::INVALID,
        }
    })
}

/// ストリーミングバッファをシークする。静的バッファに対しては no-op。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_buffer_seek_streaming(
    engine: *const NeziaEngine,
    buffer: NeziaBufferId,
    frame_offset: u64,
) {
    guard_value((), || {
        let Some(engine) = (unsafe { engine.as_ref() }) else {
            return;
        };
        engine.inner.seek_streaming(buffer.to_core(), frame_offset);
    })
}

/// ストリーミングバッファのループフラグを設定する。静的バッファに対しては no-op。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_buffer_set_streaming_loop(
    engine: *const NeziaEngine,
    buffer: NeziaBufferId,
    looping: u8,
) {
    guard_value((), || {
        let Some(engine) = (unsafe { engine.as_ref() }) else {
            return;
        };
        engine
            .inner
            .set_streaming_loop(buffer.to_core(), looping != 0);
    })
}
