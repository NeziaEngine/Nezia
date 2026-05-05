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

/// 直近 audio callback の DSP CPU 計測値を取得する (ベンチマーク用)。
///
/// 任意スレッドから lock-free に呼び出せる。NULL 引数はスキップ可。
///
/// - `out_load_pct`: 直近 callback の負荷率 (0.0..=1.0+)。`last_ns / budget_ns`。
/// - `out_callback_us`: 直近 callback の処理時間 (マイクロ秒)。
/// - `out_peak_us`: 起動以降の最大 callback 処理時間 (マイクロ秒)。
/// - `out_average_us`: `callback_total_ns / callback_count` の平均処理時間 (マイクロ秒)。
/// - `out_callback_count`: 起動以降の累積 callback 回数。
///
/// Unity の `AudioSettings.GetCPULoad()` / Profiler Audio DSP CPU の対応物。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_engine_get_dsp_stats(
    engine: *mut NeziaEngine,
    out_load_pct: *mut f32,
    out_callback_us: *mut f32,
    out_peak_us: *mut f32,
    out_average_us: *mut f32,
    out_callback_count: *mut u64,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_ref() }) else {
            return NeziaResult::NullPointer;
        };
        let stats = engine.inner.dsp_stats();
        if !out_load_pct.is_null() {
            unsafe { *out_load_pct = stats.last_load() };
        }
        if !out_callback_us.is_null() {
            unsafe { *out_callback_us = stats.last_callback_ns as f32 / 1000.0 };
        }
        if !out_peak_us.is_null() {
            unsafe { *out_peak_us = stats.peak_callback_ns as f32 / 1000.0 };
        }
        if !out_average_us.is_null() {
            let avg_us = if stats.callback_count == 0 {
                0.0
            } else {
                (stats.callback_total_ns as f64 / stats.callback_count as f64 / 1000.0) as f32
            };
            unsafe { *out_average_us = avg_us };
        }
        if !out_callback_count.is_null() {
            unsafe { *out_callback_count = stats.callback_count };
        }
        NeziaResult::Ok
    })
}

/// 現在再生中 (state == Playing) のソース数を取得する。
///
/// audio thread が毎コールバック末尾に atomic store した最新値を返す
/// (`poll_events()` 不要)。Stopped / Pausing は除外される。Unity の
/// Playing Sources カウンタ相当。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_engine_get_active_source_count(
    engine: *mut NeziaEngine,
    out_count: *mut u32,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_ref() }) else {
            return NeziaResult::NullPointer;
        };
        if out_count.is_null() {
            return NeziaResult::NullPointer;
        }
        unsafe { *out_count = engine.inner.active_source_count() };
        NeziaResult::Ok
    })
}

/// ドロップアウト系カウンタを取得する (ベンチマーク用)。
///
/// すべて起動以降の cumulative カウンタ。NULL 引数はスキップ可。
///
/// - `out_voice_steal`: callback ごとの virtualized voice 数の累積和。
///   現状の Nezia は `MAX_PHYSICAL_VOICES` 超過時に「優先度下位を一時的に
///   mix スキップ」する設計のため、伝統的な voice steal とは意味が異なる。
///   ベンチマーク観点では「mix されなかった voice-frame の数」と読める。
/// - `out_underrun`: ストリーミングバッファ underrun の累積発生回数。
/// - `out_dropped_play_calls`: `MAX_SOURCES` 上限到達による Play コマンド失敗の累積回数。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_engine_get_dropouts(
    engine: *mut NeziaEngine,
    out_voice_steal: *mut u64,
    out_underrun: *mut u64,
    out_dropped_play_calls: *mut u64,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_ref() }) else {
            return NeziaResult::NullPointer;
        };
        let d = engine.inner.dropouts();
        if !out_voice_steal.is_null() {
            unsafe { *out_voice_steal = d.voice_steal };
        }
        if !out_underrun.is_null() {
            unsafe { *out_underrun = d.streaming_underrun };
        }
        if !out_dropped_play_calls.is_null() {
            unsafe { *out_dropped_play_calls = d.dropped_play_calls };
        }
        NeziaResult::Ok
    })
}
