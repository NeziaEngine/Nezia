//! ランタイムプロファイラ (M3 可視化基盤) の FFI。
//!
//! Editor の可視化ウィンドウが数 Hz〜数十 Hz でポーリングする想定。
//! `nezia_profiler_update` で最新フレームを取り込み、`copy_*` / getter で読む。
//! フレームは triple buffer の newest-wins 読みなので、どの頻度で呼んでも
//! サウンドスレッドを妨げない。

use std::slice;

use crate::engine::NeziaEngine;
use crate::panic::{guard_result, guard_value};
use crate::types::NeziaResult;

/// バス 1 本のプロファイル (core `ProfilerBus` と同内容の C ABI 形)。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NeziaProfilerBus {
    pub index: u32,
    pub generation: u32,
    /// 現在の線形ゲイン (Snapshot 補間中はその瞬間値)。
    pub gain: f32,
    /// 0 = false / 1 = true。
    pub muted: u8,
    pub _pad: [u8; 3],
}

/// アクティブソース 1 本のプロファイル (core `ProfilerSource` の C ABI 形)。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NeziaProfilerSource {
    pub index: u32,
    pub generation: u32,
    /// 出力先バスの EntityId。無効時は index = u32::MAX。
    pub bus_index: u32,
    pub bus_generation: u32,
    pub volume: f32,
    pub pitch: f32,
    /// 再生位置 (ソースフレーム、ピッチ換算前)。
    pub sample_offset: f32,
    /// 0 = Stopped / 1 = Playing / 2 = Scheduled / 3 = Pausing。
    pub state: u8,
    /// 0 = false / 1 = true。
    pub is_virtual: u8,
    pub _pad: [u8; 2],
}

/// プロファイラ publish の有効/無効を切り替える。
///
/// 無効時のサウンドスレッド追加コストは atomic load 1 回のみ。
/// 可視化ウィンドウを開いている間だけ有効にする運用を想定する。
///
/// # 安全性
/// - `engine` は `nezia_engine_new` が返した有効なポインタであること。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_profiler_set_enabled(
    engine: *mut NeziaEngine,
    enabled: bool,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        engine.inner.set_profiling_enabled(enabled);
        NeziaResult::Ok
    })
}

/// 最新のプロファイラフレームを取り込む (triple buffer update)。
///
/// 以降の getter / copy はこの呼び出しで取り込んだフレームの値を返す。
/// ポーリングごとに 1 回呼ぶこと。
///
/// # 安全性
/// - `engine` は有効なポインタであること。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_profiler_update(engine: *mut NeziaEngine) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        let _ = engine.inner.profiler_frame();
        NeziaResult::Ok
    })
}

/// 直近フレームのマスター出力 peak (callback 内 max |sample|、soft limiter 後)。
///
/// # 安全性
/// - `engine` は有効なポインタ、`out_left` / `out_right` は f32 を 1 個
///   書ける有効な領域を指すこと。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_profiler_master_peak(
    engine: *mut NeziaEngine,
    out_left: *mut f32,
    out_right: *mut f32,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if out_left.is_null() || out_right.is_null() {
            return NeziaResult::NullPointer;
        }
        let peak = engine.inner.profiler_frame().master_peak;
        // SAFETY: 呼出側契約により out_* は書き込み可能。
        unsafe {
            out_left.write(peak[0]);
            out_right.write(peak[1]);
        }
        NeziaResult::Ok
    })
}

/// 直近フレームの Mixer Snapshot フェード進行 (サンプル)。
/// `out_total` が 0 のときフェードは進行していない。
///
/// # 安全性
/// - `engine` は有効なポインタ、`out_total` / `out_remaining` は u64 を 1 個
///   書ける有効な領域を指すこと。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_profiler_snapshot_progress(
    engine: *mut NeziaEngine,
    out_total: *mut u64,
    out_remaining: *mut u64,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if out_total.is_null() || out_remaining.is_null() {
            return NeziaResult::NullPointer;
        }
        let frame = engine.inner.profiler_frame();
        // SAFETY: 呼出側契約により out_* は書き込み可能。
        unsafe {
            out_total.write(frame.snapshot_fade_total);
            out_remaining.write(frame.snapshot_fade_remaining);
        }
        NeziaResult::Ok
    })
}

/// 直近フレームの生存バスを呼び出し側配列へコピーする。
/// 戻り値は書き込んだ個数 (capacity 超過分は切り捨て)。
///
/// # 安全性
/// - `engine` は有効なポインタ、`out_ptr` は `NeziaProfilerBus` を
///   `capacity` 個書ける有効な領域を指すこと。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_profiler_copy_buses(
    engine: *mut NeziaEngine,
    out_ptr: *mut NeziaProfilerBus,
    capacity: usize,
) -> usize {
    guard_value(0, || {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return 0;
        };
        if out_ptr.is_null() || capacity == 0 {
            return 0;
        }
        let frame = engine.inner.profiler_frame();
        let count = frame.buses.len().min(capacity);
        // SAFETY: 呼出側契約により out_ptr は capacity 個の書き込み可能領域。
        let out = unsafe { slice::from_raw_parts_mut(out_ptr, count) };
        for (dst, src) in out.iter_mut().zip(frame.buses.iter()) {
            *dst = NeziaProfilerBus {
                index: src.index,
                generation: src.generation,
                gain: src.gain,
                muted: u8::from(src.muted),
                _pad: [0; 3],
            };
        }
        count
    })
}

/// 直近フレームの生存ソースを呼び出し側配列へコピーする。
/// 戻り値は書き込んだ個数 (capacity 超過分は切り捨て)。
///
/// # 安全性
/// - `engine` は有効なポインタ、`out_ptr` は `NeziaProfilerSource` を
///   `capacity` 個書ける有効な領域を指すこと。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_profiler_copy_sources(
    engine: *mut NeziaEngine,
    out_ptr: *mut NeziaProfilerSource,
    capacity: usize,
) -> usize {
    guard_value(0, || {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return 0;
        };
        if out_ptr.is_null() || capacity == 0 {
            return 0;
        }
        let frame = engine.inner.profiler_frame();
        let count = frame.sources.len().min(capacity);
        // SAFETY: 呼出側契約により out_ptr は capacity 個の書き込み可能領域。
        let out = unsafe { slice::from_raw_parts_mut(out_ptr, count) };
        for (dst, src) in out.iter_mut().zip(frame.sources.iter()) {
            *dst = NeziaProfilerSource {
                index: src.index,
                generation: src.generation,
                bus_index: src.bus_index,
                bus_generation: src.bus_generation,
                volume: src.volume,
                pitch: src.pitch,
                sample_offset: src.sample_offset,
                state: src.state,
                is_virtual: u8::from(src.is_virtual),
                _pad: [0; 2],
            };
        }
        count
    })
}
