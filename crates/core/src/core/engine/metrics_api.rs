//! ベンチマーク・観測用のメトリクス公開 API。
//!
//! いずれも任意スレッドから lock-free に読める (atomic relaxed load)。
//! Unity の `AudioSettings.GetCPULoad()` や `Time.time` との比較ベンチマークに使う。

use std::sync::atomic::Ordering;

use crate::metrics::{DropoutStats, DspStats};

use super::SoundEngine;

impl SoundEngine {
    /// エンジン起動以降に audio thread が処理した累積フレーム数 (per-channel sample count)。
    ///
    /// Unity の `Time.time` / 動画フレーム位置と相関を取って、録音 PCM とビデオの
    /// 同期点を決めるのに使う。
    #[must_use]
    pub fn dsp_time_samples(&self) -> u64 {
        self.dsp_time_frames.load(Ordering::Relaxed)
    }

    /// `dsp_time_samples()` を秒に換算した値。
    #[must_use]
    pub fn dsp_time_seconds(&self) -> f64 {
        let frames = self.dsp_time_samples() as f64;
        let sr = self.device_sample_rate as f64;
        if sr <= 0.0 { 0.0 } else { frames / sr }
    }

    /// 直近 audio callback の DSP CPU 計測値スナップショットを返す。
    #[must_use]
    pub fn dsp_stats(&self) -> DspStats {
        DspStats {
            last_callback_ns: self.metrics.last_callback_ns.load(Ordering::Relaxed),
            peak_callback_ns: self.metrics.peak_callback_ns.load(Ordering::Relaxed),
            callback_count: self.metrics.callback_count.load(Ordering::Relaxed),
            callback_total_ns: self.metrics.callback_total_ns.load(Ordering::Relaxed),
            last_callback_budget_ns: self.metrics.last_callback_budget_ns.load(Ordering::Relaxed),
        }
    }

    /// 直近 audio callback 末尾で観測された生存ソース数 (Playing/Pausing/Stopped 含む)。
    ///
    /// `poll_events()` 経由のスナップショットではなく、audio thread が atomic に
    /// 公開した最新値。ベンチマーク時に「実際にいま鳴っているボイス本数」を
    /// 計測するのに使う。`poll_events()` 不要。
    #[must_use]
    pub fn active_source_count(&self) -> u32 {
        self.metrics.active_source_count.load(Ordering::Relaxed)
    }

    /// 直近 audio callback 末尾で virtualized (mix スキップ) 状態だったボイス数。
    #[must_use]
    pub fn virtualized_voice_count(&self) -> u32 {
        self.metrics.virtualized_voice_count.load(Ordering::Relaxed)
    }

    /// ドロップアウト系カウンタのスナップショット (cumulative)。
    ///
    /// - `voice_steal`: callback ごとの virtualized voice 数の累積和 (voice-frame 単位)。
    ///   現状の Nezia は MAX_PHYSICAL_VOICES 超過時に「古いボイスを止める」のではなく
    ///   「優先度下位を一時的に mix スキップ」する設計のため、伝統的な voice steal とは
    ///   意味が異なる点に注意。
    /// - `streaming_underrun`: ストリーミングバッファ underrun の累積発生回数。
    /// - `dropped_play_calls`: `MAX_SOURCES` 上限到達による Play コマンド失敗の累積回数。
    #[must_use]
    pub fn dropouts(&self) -> DropoutStats {
        DropoutStats {
            voice_steal: self.metrics.voice_steal_count.load(Ordering::Relaxed),
            streaming_underrun: self
                .metrics
                .streaming_underrun_count
                .load(Ordering::Relaxed),
            dropped_play_calls: self.metrics.dropped_play_calls.load(Ordering::Relaxed),
        }
    }
}
