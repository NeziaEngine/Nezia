//! 予約再生 (Phase 3-4 PlayScheduled) の公開 API。
//!
//! 主 API は `_in(seconds)` 形式で「今から N 秒後に発音」を表現する (Unity の
//! `AudioSource.PlayScheduled` 互換の使い心地)。低レベル `_at_frame(u64)` は絶対
//! DSP frame 指定で、リズムゲーム等で 1 frame 単位の精度が要る場面で使う。
//!
//! タイミングのセマンティクスは `docs/design/core/scheduling.md` 参照。要約:
//! - `delay_seconds <= 0.0` または `start_dsp_frame` が過去 → 即時再生 (silent fallback)
//! - サブ callback 精度: 1 sample 単位で発音開始位置を制御
//! - 予約中の `stop_source` でキャンセル可、`pause_source` は no-op (予定通り fire)

use ringbuf::traits::Producer;

use crate::buffer_pool::BufferId;
use crate::command::Command;
use crate::entity::EntityId;

use super::super::SoundEngine;

impl SoundEngine {
    /// `delay_seconds` を絶対 DSP frame に変換する (内部用)。
    /// `delay_seconds <= 0.0` で sentinel `0` (= 即時) を返す。計算結果が 0 になった場合は
    /// sentinel と衝突するため `1` に繰り上げる。
    #[inline]
    fn delay_seconds_to_dsp_frame(&self, delay_seconds: f64) -> u64 {
        if delay_seconds <= 0.0 {
            return 0;
        }
        let delta = (delay_seconds * self.device_sample_rate as f64) as u64;
        let target = self.dsp_time_samples().saturating_add(delta);
        target.max(1)
    }

    /// 現在から `delay_seconds` 後にマスターバスで再生する (sample 精度)。
    /// `delay_seconds <= 0.0` は即時再生として扱う。
    #[must_use]
    pub fn play_scheduled_in(
        &mut self,
        buffer: BufferId,
        delay_seconds: f64,
        vol: f32,
        pitch: f32,
        looping: bool,
    ) -> bool {
        let start_dsp_frame = self.delay_seconds_to_dsp_frame(delay_seconds);
        self.play_scheduled_at_frame(buffer, start_dsp_frame, vol, pitch, looping)
    }

    /// 絶対 DSP frame でマスターバスに予約再生する (`engine.dsp_time_samples()` 基準)。
    /// `start_dsp_frame = 0` または過去時刻は即時再生にフォールバックする。
    #[must_use]
    pub fn play_scheduled_at_frame(
        &mut self,
        buffer: BufferId,
        start_dsp_frame: u64,
        vol: f32,
        pitch: f32,
        looping: bool,
    ) -> bool {
        let Some(index) = self.buffer_pool.resolve(buffer) else {
            return false;
        };
        self.command_producer
            .try_push(Command::Play {
                audio_buffer_index: index,
                vol,
                pitch,
                token: 0,
                looping,
                start_dsp_frame,
            })
            .is_ok()
    }

    /// 現在から `delay_seconds` 後に指定バスで再生する (sample 精度)。
    #[must_use]
    pub fn play_to_bus_scheduled_in(
        &mut self,
        buffer: BufferId,
        delay_seconds: f64,
        vol: f32,
        pitch: f32,
        bus: EntityId,
        looping: bool,
    ) -> bool {
        let start_dsp_frame = self.delay_seconds_to_dsp_frame(delay_seconds);
        self.play_to_bus_scheduled_at_frame(buffer, start_dsp_frame, vol, pitch, bus, looping)
    }

    /// 絶対 DSP frame で指定バスに予約再生する。
    #[must_use]
    pub fn play_to_bus_scheduled_at_frame(
        &mut self,
        buffer: BufferId,
        start_dsp_frame: u64,
        vol: f32,
        pitch: f32,
        bus: EntityId,
        looping: bool,
    ) -> bool {
        let Some(index) = self.buffer_pool.resolve(buffer) else {
            return false;
        };
        let Some(output_bus_dense) = self.bus_routing.resolve_dense(bus) else {
            return false;
        };
        self.command_producer
            .try_push(Command::PlayToBus {
                audio_buffer_index: index,
                vol,
                pitch,
                output_bus_dense,
                token: 0,
                looping,
                start_dsp_frame,
            })
            .is_ok()
    }

    /// 現在から `delay_seconds` 後にハンドル付きで再生する (`stop_source` でキャンセル可)。
    /// 予約中も `set_source_volume` / `set_source_pitch` / `seek_source` が反映される。
    #[must_use]
    pub fn play_with_handle_scheduled_in(
        &mut self,
        buffer: BufferId,
        delay_seconds: f64,
        vol: f32,
        pitch: f32,
        bus: EntityId,
        looping: bool,
    ) -> Option<EntityId> {
        let start_dsp_frame = self.delay_seconds_to_dsp_frame(delay_seconds);
        self.play_with_handle_scheduled_at_frame(buffer, start_dsp_frame, vol, pitch, bus, looping)
    }

    /// 絶対 DSP frame でハンドル付きの予約再生を行う。
    #[must_use]
    pub fn play_with_handle_scheduled_at_frame(
        &mut self,
        buffer: BufferId,
        start_dsp_frame: u64,
        vol: f32,
        pitch: f32,
        bus: EntityId,
        looping: bool,
    ) -> Option<EntityId> {
        let index = self.buffer_pool.resolve(buffer)?;
        let output_bus_dense = self.bus_routing.resolve_dense(bus)?;
        let id = self.source_slots.alloc()?;
        self.live_params.prime(id, vol, pitch);
        if self
            .command_producer
            .try_push(Command::SpawnSource {
                id,
                audio_buffer_index: index,
                vol,
                pitch,
                output_bus_dense,
                token: 0,
                looping,
                start_dsp_frame,
            })
            .is_err()
        {
            self.source_slots.free(id);
            return None;
        }
        Some(id)
    }

    /// `_in` 形式 + 終了コールバック付きハンドル予約再生。`looping = true` だとコールバックは
    /// 発火しないため `stop_source` で能動的に終わらせる。
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn play_with_handle_scheduled_in_and_callback(
        &mut self,
        buffer: BufferId,
        delay_seconds: f64,
        vol: f32,
        pitch: f32,
        bus: EntityId,
        looping: bool,
        callback: impl FnOnce() + Send + 'static,
    ) -> Option<EntityId> {
        let start_dsp_frame = self.delay_seconds_to_dsp_frame(delay_seconds);
        self.play_with_handle_scheduled_at_frame_and_callback(
            buffer,
            start_dsp_frame,
            vol,
            pitch,
            bus,
            looping,
            callback,
        )
    }

    /// `_at_frame` 形式 + 終了コールバック付きハンドル予約再生。
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn play_with_handle_scheduled_at_frame_and_callback(
        &mut self,
        buffer: BufferId,
        start_dsp_frame: u64,
        vol: f32,
        pitch: f32,
        bus: EntityId,
        looping: bool,
        callback: impl FnOnce() + Send + 'static,
    ) -> Option<EntityId> {
        let index = self.buffer_pool.resolve(buffer)?;
        let output_bus_dense = self.bus_routing.resolve_dense(bus)?;
        let id = self.source_slots.alloc()?;
        self.live_params.prime(id, vol, pitch);
        let Some(token) = self.callbacks.register_rust(Box::new(callback)) else {
            self.source_slots.free(id);
            return None;
        };
        let ok = self
            .command_producer
            .try_push(Command::SpawnSource {
                id,
                audio_buffer_index: index,
                vol,
                pitch,
                output_bus_dense,
                token,
                looping,
                start_dsp_frame,
            })
            .is_ok();
        if !ok {
            self.callbacks.cancel(token);
            self.source_slots.free(id);
            return None;
        }
        Some(id)
    }
}
