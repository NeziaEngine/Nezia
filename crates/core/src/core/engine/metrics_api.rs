//! ベンチマーク・観測用のメトリクス公開 API。
//!
//! いずれも任意スレッドから lock-free に読める (atomic relaxed load)。
//! Unity の `AudioSettings.GetCPULoad()` や `Time.time` との比較ベンチマークに使う。

use std::sync::atomic::Ordering;

use crate::command::Command;
use crate::event::Event;
use crate::memory::{self, NeziaMemoryStats, vec_cap_bytes};
use crate::metrics::{DropoutStats, DspStats};

use super::{COMMAND_RING_CAPACITY, EVENT_RING_CAPACITY, SoundEngine};

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
    /// - `command_queue_full`: SPSC コマンドリングが満杯で `try_push` が失敗した累積回数。
    ///   `dropped_play_calls` と原因が異なる: こちらは「1 フレームで API バーストし
    ///   audio thread が drain する前に詰まった」状態を示す。閾値超えはリング容量不足か
    ///   1 操作あたりのコマンド数過多のサインで、容量増 / API 集約の判断材料に使う。
    #[must_use]
    pub fn dropouts(&self) -> DropoutStats {
        DropoutStats {
            voice_steal: self.metrics.voice_steal_count.load(Ordering::Relaxed),
            streaming_underrun: self
                .metrics
                .streaming_underrun_count
                .load(Ordering::Relaxed),
            dropped_play_calls: self.metrics.dropped_play_calls.load(Ordering::Relaxed),
            command_queue_full: self.metrics.command_queue_full.load(Ordering::Relaxed),
        }
    }

    /// Nezia エンジンの**メモリ使用量スナップショット**を返す。
    ///
    /// 計測経路は 2 系統:
    ///
    /// 1. **グローバルアロケータ統計** (`heap_*` / `alloc_count` / `free_count`)
    ///    [`crate::TrackingAllocator`] が `#[global_allocator]` として登録された
    ///    実行体 (典型: `nezia-ffi` の cdylib) でのみ有効。それ以外では `heap_tracked = false`
    ///    かつ `heap_*` / `alloc_count` / `free_count` は 0。
    ///
    /// 2. **サブシステム別 walker** (`voices_bytes` / `buffers_bytes` / `effects_bytes` / `graph_bytes`)
    ///    各 World / Pool / Registry が確保している `Vec` / `Box<[T]>` の **capacity ベース**
    ///    のヒープ実バイト合計。常時取得可能。
    ///
    /// 取得コストは μs 未満 (各 World の Vec::capacity を読むだけ)。毎フレーム呼んでも問題ない。
    /// audio thread の hot path には一切影響しない。
    #[must_use]
    pub fn memory_stats(&self) -> NeziaMemoryStats {
        let (heap_in_use, heap_peak, allocs, frees, tracked) = memory::snapshot_global();

        // ── voices_bytes: ソース・空間情報・ライブパラメータ・スナップショット ──
        // SourceWorld / SpatialWorld は audio thread に move 済みなので、init 時に
        // 静的キャッシュした値 (`audio_thread_static_bytes`) からは引き出せず、
        // walker からは触れない。便宜上、audio_thread_static_bytes を `voices_bytes` に
        // **含めず** 別カウントにすると合計が見えにくいので、ここでは
        // 「main thread 側のソース関連 Vec のみ」を voices に集計し、audio thread 側は
        // graph_bytes に押し込む (元 World の所有者がいないため)。
        // → ユーザ向け表示では `breakdown_total()` がほぼ heap_bytes_in_use と一致する。
        let triple_buffer_per_source = std::mem::size_of::<crate::entity::SourcePositionUpdate>()
            + std::mem::size_of::<crate::entity::SourceVelocityUpdate>()
            + std::mem::size_of::<super::SourceSnapshot>();
        // 各 triple_buffer は 3 スロット (alloc 不要を保つため init 時に max_sources で確保済み)。
        let max_sources = self.live_params.memory_bytes()
            / (3 * std::mem::size_of::<std::sync::atomic::AtomicU64>()).max(1); // 3 fields × AtomicU64
        let triple_buffer_bytes = (3 * max_sources * triple_buffer_per_source) as u64;

        let voices_bytes = self.source_slots.memory_bytes() as u64
            + self.live_params.memory_bytes() as u64
            + self.source_state_cache.memory_bytes() as u64
            + vec_cap_bytes(&self.source_sends) as u64
            + triple_buffer_bytes;

        // ── buffers_bytes: AudioBufferPool (PCM + streaming リング) ──
        let buffers_bytes = self.buffer_pool.memory_bytes() as u64;

        // ── effects_bytes: メイン側エフェクト管理 + audio thread 側エフェクト World ──
        // audio_thread_static_bytes は SourceWorld / SpatialWorld / BusWorld /
        // EffectWorld / EffectWorlds の合算なので、ここでは「メインスレッド側の
        // エフェクトハンドルアロケータ + compressor_owners」だけを effects に集計し、
        // audio thread 側 World 全体は graph に置く。
        let compressor_owners_bytes = (self.compressor_owners.capacity()
            * (std::mem::size_of::<crate::effect::EffectId>()
                + std::mem::size_of::<crate::entity::EntityId>()
                + 16))  // HashMap entry overhead 概算
            as u64;
        let effects_bytes = self.effect_slots.memory_bytes() as u64 + compressor_owners_bytes;

        // ── graph_bytes: バス routing / send / snapshot / curve / container / callbacks /
        //                 SPSC リング (command/event/capture) + audio thread 側 World 全体 ──
        let command_ring_bytes = (COMMAND_RING_CAPACITY * std::mem::size_of::<Command>()) as u64;
        let event_ring_bytes = (EVENT_RING_CAPACITY * std::mem::size_of::<Event>()) as u64;

        let graph_bytes = self.bus_routing.memory_bytes() as u64
            + self.send_slots.memory_bytes() as u64
            + self.snapshot_registry.memory_bytes() as u64
            + self.curve_registry.memory_bytes() as u64
            + self.container_world.memory_bytes() as u64
            + self.callbacks.memory_bytes() as u64
            + command_ring_bytes
            + event_ring_bytes
            + self.capture_ring_bytes
            + self.audio_thread_static_bytes;

        NeziaMemoryStats {
            heap_bytes_in_use: heap_in_use,
            heap_bytes_peak: heap_peak,
            alloc_count: allocs,
            free_count: frees,
            heap_tracked: tracked,
            voices_bytes,
            buffers_bytes,
            effects_bytes,
            graph_bytes,
        }
    }
}
