//! 基本 Source 再生 API (Rust クロージャ版、即時発音)。

use ringbuf::traits::Producer;

use crate::buffer_pool::BufferId;
use crate::command::Command;
use crate::entity::EntityId;

use super::super::SoundEngine;

impl SoundEngine {
    /// ボイスをマスターバスに再生する（fire-and-forget）。
    #[must_use]
    pub fn play(&mut self, buffer: BufferId, vol: f32, pitch: f32, looping: bool) -> bool {
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
                start_dsp_frame: 0,
            })
            .is_ok()
    }

    /// ボイスをマスターバスにコールバック付きで再生する。
    ///
    /// 再生が自然終了したとき、次の `poll_events()` で `callback` が呼ばれる。
    /// `MAX_SOURCES` 上限に達していた場合はコールバックは呼ばれない。
    #[must_use]
    pub fn play_with_callback(
        &mut self,
        buffer: BufferId,
        vol: f32,
        pitch: f32,
        looping: bool,
        callback: impl FnOnce() + Send + 'static,
    ) -> bool {
        let Some(index) = self.buffer_pool.resolve(buffer) else {
            return false;
        };
        let Some(token) = self.callbacks.register_rust(Box::new(callback)) else {
            return false;
        };
        let ok = self
            .command_producer
            .try_push(Command::Play {
                audio_buffer_index: index,
                vol,
                pitch,
                token,
                looping,
                start_dsp_frame: 0,
            })
            .is_ok();
        if !ok {
            self.callbacks.cancel(token);
        }
        ok
    }

    /// ボイスを指定バスに再生する（fire-and-forget）。
    #[must_use]
    pub fn play_to_bus(
        &mut self,
        buffer: BufferId,
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
                start_dsp_frame: 0,
            })
            .is_ok()
    }

    /// ボイスを指定バスにコールバック付きで再生する。
    ///
    /// 再生が自然終了したとき、次の `poll_events()` で `callback` が呼ばれる。
    /// `MAX_SOURCES` 上限に達していた場合はコールバックは呼ばれない。
    #[must_use]
    pub fn play_to_bus_with_callback(
        &mut self,
        buffer: BufferId,
        vol: f32,
        pitch: f32,
        bus: EntityId,
        looping: bool,
        callback: impl FnOnce() + Send + 'static,
    ) -> bool {
        let Some(index) = self.buffer_pool.resolve(buffer) else {
            return false;
        };
        let Some(output_bus_dense) = self.bus_routing.resolve_dense(bus) else {
            return false;
        };
        let Some(token) = self.callbacks.register_rust(Box::new(callback)) else {
            return false;
        };
        let ok = self
            .command_producer
            .try_push(Command::PlayToBus {
                audio_buffer_index: index,
                vol,
                pitch,
                output_bus_dense,
                token,
                looping,
                start_dsp_frame: 0,
            })
            .is_ok();
        if !ok {
            self.callbacks.cancel(token);
        }
        ok
    }

    /// 再生を開始し、制御用ハンドル（EntityId）を返す。
    ///
    /// Source は 1 回の発音インスタンスを表す。バッファ末尾到達 (`looping=false`) または
    /// `stop_source()` で despawn され、その時点で EntityId は無効化される。
    /// 再生し直したい場合は再度この関数を呼んで新しい EntityId を取り直す。
    ///
    /// 返った EntityId は `set_source_volume()` / `set_source_pitch()` / `seek_source()` /
    /// `pause_source()` / `resume_source()` / `stop_source()` および
    /// `set_source_spatial_params()` / `set_source_spatial_enabled()` /
    /// `batch_set_source_positions()` の引数として使う。
    #[must_use]
    pub fn play_with_handle(
        &mut self,
        buffer: BufferId,
        vol: f32,
        pitch: f32,
        bus: EntityId,
        looping: bool,
    ) -> Option<EntityId> {
        let index = self.buffer_pool.resolve(buffer)?;
        let output_bus_dense = self.bus_routing.resolve_dense(bus)?;

        let id = self.source_slots.alloc()?;
        // ライブパラメータスロットを priming（古い値・古い generation を上書き）。
        // setter 経路がここに直接 atomic store するため、初期値も同じ場所に書く。
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
                start_dsp_frame: 0,
            })
            .is_err()
        {
            // command 送信失敗 → スロットを即返却
            self.source_slots.free(id);
            return None;
        }
        Some(id)
    }

    /// 再生を開始し、自然終了時にコールバックを呼ぶ版。EntityId を返す。
    ///
    /// セマンティクスは `play_with_handle()` と同じ（Source = 1 回の発音インスタンス）。
    /// `looping = true` の場合は終了通知が発火しないため、コールバックは呼ばれずに
    /// `stop_source()` などで明示的に終わらせるまで保持される。
    /// `MAX_SOURCES` 上限などでコマンド送信に失敗した場合はコールバックは呼ばれず破棄される。
    #[must_use]
    pub fn play_with_handle_and_callback(
        &mut self,
        buffer: BufferId,
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
                start_dsp_frame: 0,
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
