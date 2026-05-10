//! ベンチマーク / プロファイリング用のランタイム計測値。
//!
//! audio thread が毎コールバック末尾で書き込み、メインスレッドからは
//! `SoundEngine` 経由で lock-free に読み取れる。すべての値は monotonic
//! あるいは「最後に観測された値」のいずれかで、リセット API は提供しない
//! (差分はメインスレッド側で計算する)。
//!
//! ## 計測項目
//! - **DSP CPU stats**: 1 コールバックあたりの処理時間 / ピーク / 累積 / 予算 (ナノ秒)
//! - **Active source count**: audio thread 側 `SourceWorld::len()` のスナップショット
//! - **Dropouts**:
//!   - `voice_steal_count`: callback ごとの virtualized voice 数の累積和
//!   - `streaming_underrun_count`: ストリーミングバッファ underrun 累積
//!   - `dropped_play_calls`: `MAX_SOURCES` 上限による spawn 失敗の累積

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// audio thread が毎コールバックで更新するランタイム計測値。
///
/// すべて lock-free atomic。書き込みは audio thread 1 本のみ、
/// 読み取りは任意スレッド。`Ordering::Relaxed` で十分 (個別カウンタ
/// 単独で意味を持ち、複数カウンタ間の整合性は要求しない)。
#[derive(Debug, Default)]
pub(crate) struct EngineMetrics {
    // ── DSP CPU ──
    /// 直近 1 コールバックで `process()` に費やしたナノ秒数。
    pub last_callback_ns: AtomicU64,
    /// エンジン起動以降の最大コールバック処理時間 (ナノ秒)。
    pub peak_callback_ns: AtomicU64,
    /// `process()` を呼び出した累積回数。
    pub callback_count: AtomicU64,
    /// `process()` 累積消費時間 (ナノ秒)。`callback_count` と組で平均算出に使う。
    pub callback_total_ns: AtomicU64,
    /// 直近コールバックの「予算」(per-channel sample 数 / sample_rate を ns 換算)。
    /// 負荷率 = `last_callback_ns / last_callback_budget_ns`。
    pub last_callback_budget_ns: AtomicU64,

    // ── Source 数 ──
    /// 直近コールバック末尾の `SourceWorld::len()`。
    /// Stopped/Pausing/Playing 含むが、despawn 済みは含まない。
    pub active_source_count: AtomicU32,
    /// 直近コールバック末尾の virtualized voice 数 (gauge)。
    pub virtualized_voice_count: AtomicU32,

    // ── Dropouts ──
    /// callback ごとに `virtualized_voice_count` を加算した累積。
    /// 「voice-frame ベースで何回 mix をスキップしたか」の指標。
    pub voice_steal_count: AtomicU64,
    /// ストリーミングバッファ underrun の累積発生回数。
    pub streaming_underrun_count: AtomicU64,
    /// `MAX_SOURCES` 上限到達による Play コマンド失敗の累積回数。
    pub dropped_play_calls: AtomicU64,
    /// SPSC コマンドリングが満杯で `try_push` が失敗した累積回数。
    /// `dropped_play_calls` (= MAX_SOURCES 到達) と原因が異なる:
    /// こちらは「1 フレームで大量の API 呼び出しを行ったがリングが drain される前に
    /// 詰まった」ケースを示す。閾値超えが見えたら容量増加 or API 集約の判断材料に使う。
    pub command_queue_full: AtomicU64,
}

impl EngineMetrics {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

/// メインスレッドが取得する DSP CPU 計測値のスナップショット。
#[derive(Debug, Clone, Copy, Default)]
pub struct DspStats {
    /// 直近 1 コールバックの処理時間 (ナノ秒)。
    pub last_callback_ns: u64,
    /// 起動以降のピーク処理時間 (ナノ秒)。
    pub peak_callback_ns: u64,
    /// 累積コールバック数。
    pub callback_count: u64,
    /// 累積処理時間 (ナノ秒)。`callback_count` と組で平均を算出可能。
    pub callback_total_ns: u64,
    /// 直近コールバックの予算 (ナノ秒)。`buffer_frames / sample_rate * 1e9`。
    pub last_callback_budget_ns: u64,
}

impl DspStats {
    /// 直近コールバックの負荷率 (0.0..=1.0+)。予算ゼロ時は 0.0。
    #[must_use]
    pub fn last_load(&self) -> f32 {
        if self.last_callback_budget_ns == 0 {
            0.0
        } else {
            self.last_callback_ns as f32 / self.last_callback_budget_ns as f32
        }
    }

    /// 累積平均負荷率。`callback_count == 0` のときは 0.0。
    #[must_use]
    pub fn average_load(&self) -> f32 {
        if self.callback_count == 0 || self.last_callback_budget_ns == 0 {
            0.0
        } else {
            let avg_ns = self.callback_total_ns as f64 / self.callback_count as f64;
            (avg_ns / self.last_callback_budget_ns as f64) as f32
        }
    }
}

/// メインスレッドが取得するドロップアウトカウンタのスナップショット。
#[derive(Debug, Clone, Copy, Default)]
pub struct DropoutStats {
    /// callback ごとの virtualized voice 数を累積した値 (voice-frame 単位)。
    pub voice_steal: u64,
    /// ストリーミングバッファ underrun の累積発生回数。
    pub streaming_underrun: u64,
    /// `MAX_SOURCES` 上限到達による Play コマンド失敗の累積回数。
    pub dropped_play_calls: u64,
    /// SPSC コマンドリングが満杯で `try_push` が失敗した累積回数。
    pub command_queue_full: u64,
}

/// audio thread 側で `peak_callback_ns` を最大値に更新するヘルパ。
#[inline]
pub(crate) fn update_peak(peak: &AtomicU64, new: u64) {
    let mut cur = peak.load(Ordering::Relaxed);
    while new > cur {
        match peak.compare_exchange_weak(cur, new, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(observed) => cur = observed,
        }
    }
}
