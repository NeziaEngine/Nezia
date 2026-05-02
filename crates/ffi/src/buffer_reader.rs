//! 任意スレッドから PCM を読める buffer reader ハンドル。
//!
//! Unity の `AudioClip.Create(stream: true, pcmReadCallback)` のように、
//! Unity audio thread 等から PCM を都度ストリーム供給する用途で使う。
//!
//! `nezia_buffer_reader_open` は main thread から呼ぶ（`SoundEngine` を参照するため）。
//! 戻り値の `*mut NeziaBufferReader` を保持しておけば、以降 `read_frames` /
//! `read_interleaved` は **任意のスレッドから呼んでよい**（内部で `Arc<AudioBuffer>` を
//! 保持しているため、main thread が `nezia_buffer_unload()` してもメモリは解放されない）。

use std::slice;

use nezia_core::BufferReader as CoreBufferReader;

use crate::engine::NeziaEngine;
use crate::panic::guard_value;
use crate::types::NeziaBufferId;

/// `NeziaBufferReader` 不透明ハンドル。
#[allow(non_camel_case_types)]
pub struct NeziaBufferReader {
    inner: CoreBufferReader,
}

/// バッファに対する読み取りハンドルを開く。
///
/// 失敗時（バッファ ID 無効など）は NULL を返す。戻り値は
/// `nezia_buffer_reader_close` で必ず解放すること。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_buffer_reader_open(
    engine: *mut NeziaEngine,
    buffer: NeziaBufferId,
) -> *mut NeziaBufferReader {
    guard_value(std::ptr::null_mut(), || {
        let Some(engine) = (unsafe { engine.as_ref() }) else {
            return std::ptr::null_mut();
        };
        let Some(reader) = engine.inner.open_buffer_reader(buffer.to_core()) else {
            return std::ptr::null_mut();
        };
        Box::into_raw(Box::new(NeziaBufferReader { inner: reader }))
    })
}

/// バッファリーダーを閉じる。NULL は無視。
///
/// # 安全性
/// `reader` は `nezia_buffer_reader_open` の戻り値かつ未解放であること。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_buffer_reader_close(reader: *mut NeziaBufferReader) {
    if reader.is_null() {
        return;
    }
    guard_value((), || {
        // SAFETY: 呼出側契約。
        unsafe { drop(Box::from_raw(reader)) };
    });
}

/// チャンネル数を返す。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_buffer_reader_channels(reader: *const NeziaBufferReader) -> u16 {
    guard_value(0, || {
        let Some(r) = (unsafe { reader.as_ref() }) else {
            return 0;
        };
        r.inner.channels()
    })
}

/// サンプルレート（Hz）を返す。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_buffer_reader_sample_rate(reader: *const NeziaBufferReader) -> u32 {
    guard_value(0, || {
        let Some(r) = (unsafe { reader.as_ref() }) else {
            return 0;
        };
        r.inner.sample_rate()
    })
}

/// 総フレーム数（チャンネルあたり）を返す。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_buffer_reader_total_frames(reader: *const NeziaBufferReader) -> u64 {
    guard_value(0, || {
        let Some(r) = (unsafe { reader.as_ref() }) else {
            return 0;
        };
        r.inner.total_frames() as u64
    })
}

/// `frame_offset` から `dst` をインターリーブ PCM で埋める。
///
/// 戻り値: 実際に書き込んだ **フレーム数**（要求より少ないことがある = EOF 到達）。
/// `dst_len` は `channels` の倍数を期待するが、端数があれば切り捨てる。
/// EOF 到達後の dst 末尾は呼出側で 0 埋めすること（Unity のコールバックは無音を期待）。
///
/// この関数は **任意スレッドから呼んでよい**（lock-free）。
///
/// # 安全性
/// `dst_ptr` は `dst_len` 個の `f32` を書ける有効領域を指すこと。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_buffer_reader_read(
    reader: *const NeziaBufferReader,
    frame_offset: u64,
    dst_ptr: *mut f32,
    dst_len: usize,
) -> u64 {
    guard_value(0u64, || {
        let Some(r) = (unsafe { reader.as_ref() }) else {
            return 0;
        };
        if dst_len == 0 || dst_ptr.is_null() {
            return 0;
        }
        // SAFETY: 呼出側契約により dst_ptr は dst_len 個の f32 領域。
        let dst = unsafe { slice::from_raw_parts_mut(dst_ptr, dst_len) };
        r.inner.read_frames(frame_offset as usize, dst) as u64
    })
}
