//! マスター出力 PCM キャプチャ FFI (Unity Recorder 等の外部録音向け)。
//!
//! 詳細設計は `docs/design/core/capture.md` 参照。
//!
//! 典型的な利用フロー (Unity 側 C#):
//! 1. メインスレッドで `nezia_engine_enable_master_capture` を 1 度だけ呼んでハンドル取得。
//! 2. デバイスフォーマットを `nezia_engine_output_format` で取得して muxer 設定に渡す。
//! 3. 任意スレッドで `nezia_capture_reader_read` を周期 drain し PCM を Recorder へ供給。
//! 4. 終了時に `nezia_engine_disable_master_capture` を呼び、最終 drain 後 `_close` で解放。

use std::slice;

use nezia_core::CaptureReader as CoreCaptureReader;

use crate::engine::NeziaEngine;
use crate::panic::guard_value;

/// `NeziaCaptureReader` 不透明ハンドル。
#[allow(non_camel_case_types)]
pub struct NeziaCaptureReader {
    inner: CoreCaptureReader,
}

/// マスター出力キャプチャを有効化し、リーダーハンドルを返す。
///
/// 戻り値: 成功時はハンドル、二重 enable / engine NULL 時は NULL。
/// ハンドルは最終的に `nezia_capture_reader_close` で解放すること。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_engine_enable_master_capture(
    engine: *mut NeziaEngine,
) -> *mut NeziaCaptureReader {
    guard_value(std::ptr::null_mut(), || {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return std::ptr::null_mut();
        };
        let Some(reader) = engine.inner.enable_master_capture() else {
            return std::ptr::null_mut();
        };
        Box::into_raw(Box::new(NeziaCaptureReader { inner: reader }))
    })
}

/// マスター出力キャプチャを無効化する。リーダーは引き続き残量 drain に使ってよい。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_engine_disable_master_capture(engine: *mut NeziaEngine) {
    guard_value((), || {
        let Some(engine) = (unsafe { engine.as_ref() }) else {
            return;
        };
        engine.inner.disable_master_capture();
    })
}

/// 出力フォーマットを取得する。引数ポインタは NULL 可 (NULL は無視)。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_engine_output_format(
    engine: *const NeziaEngine,
    out_sample_rate: *mut u32,
    out_channels: *mut u16,
) {
    guard_value((), || {
        let Some(engine) = (unsafe { engine.as_ref() }) else {
            return;
        };
        let (sr, ch) = engine.inner.output_format();
        if !out_sample_rate.is_null() {
            // SAFETY: 呼出側契約により書き込み可能領域。
            unsafe { *out_sample_rate = sr };
        }
        if !out_channels.is_null() {
            // SAFETY: 呼出側契約により書き込み可能領域。
            unsafe { *out_channels = ch };
        }
    })
}

/// エンジン起動以降の累積処理フレーム数 (per-channel sample count) を返す。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_engine_dsp_time_samples(engine: *const NeziaEngine) -> u64 {
    guard_value(0u64, || {
        let Some(engine) = (unsafe { engine.as_ref() }) else {
            return 0;
        };
        engine.inner.dsp_time_samples()
    })
}

/// `nezia_engine_dsp_time_samples` を秒に換算した値。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_engine_dsp_time_seconds(engine: *const NeziaEngine) -> f64 {
    guard_value(0.0_f64, || {
        let Some(engine) = (unsafe { engine.as_ref() }) else {
            return 0.0;
        };
        engine.inner.dsp_time_seconds()
    })
}

/// キャプチャリーダーを閉じる。NULL は無視。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_capture_reader_close(reader: *mut NeziaCaptureReader) {
    if reader.is_null() {
        return;
    }
    guard_value((), || {
        // SAFETY: 呼出側契約。
        unsafe { drop(Box::from_raw(reader)) };
    });
}

/// サンプルレート (Hz) を返す。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_capture_reader_sample_rate(
    reader: *const NeziaCaptureReader,
) -> u32 {
    guard_value(0, || {
        let Some(r) = (unsafe { reader.as_ref() }) else {
            return 0;
        };
        r.inner.sample_rate()
    })
}

/// チャンネル数を返す。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_capture_reader_channels(reader: *const NeziaCaptureReader) -> u16 {
    guard_value(0, || {
        let Some(r) = (unsafe { reader.as_ref() }) else {
            return 0;
        };
        r.inner.channels()
    })
}

/// 起動以降の累積ドロップサンプル数を返す。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_capture_reader_dropped_samples(
    reader: *const NeziaCaptureReader,
) -> u64 {
    guard_value(0u64, || {
        let Some(r) = (unsafe { reader.as_ref() }) else {
            return 0;
        };
        r.inner.dropped_samples()
    })
}

/// インターリーブ PCM を `dst` に最大 `dst_len` サンプル書き込む。戻り値: 実書き込みサンプル数。
///
/// 任意スレッドから呼んでよい (lock-free SPSC)。`dst_len` は `channels` の倍数を想定するが、
/// リング側は要素単位で動作するため端数が出ることがある (フレーム揃えしたい場合は呼出側で
/// `dst_len` を `channels` の倍数にしておくこと)。
///
/// # 安全性
/// `dst_ptr` は `dst_len` 個の `f32` を書ける有効領域を指すこと。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_capture_reader_read(
    reader: *mut NeziaCaptureReader,
    dst_ptr: *mut f32,
    dst_len: usize,
) -> u64 {
    guard_value(0u64, || {
        let Some(r) = (unsafe { reader.as_mut() }) else {
            return 0;
        };
        if dst_len == 0 || dst_ptr.is_null() {
            return 0;
        }
        // SAFETY: 呼出側契約により `dst_ptr` は `dst_len` 個の f32 領域。
        let dst = unsafe { slice::from_raw_parts_mut(dst_ptr, dst_len) };
        r.inner.read_interleaved(dst) as u64
    })
}
