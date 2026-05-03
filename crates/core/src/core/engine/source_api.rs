use ringbuf::traits::Producer;

use crate::buffer_pool::BufferId;
use crate::command::Command;
use crate::entity::EntityId;

use super::SoundEngine;

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
        let token = self.callbacks.register(Box::new(callback));
        let ok = self
            .command_producer
            .try_push(Command::Play {
                audio_buffer_index: index,
                vol,
                pitch,
                token,
                looping,
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
        let token = self.callbacks.register(Box::new(callback));
        let ok = self
            .command_producer
            .try_push(Command::PlayToBus {
                audio_buffer_index: index,
                vol,
                pitch,
                output_bus_dense,
                token,
                looping,
            })
            .is_ok();
        if !ok {
            self.callbacks.cancel(token);
        }
        ok
    }

    /// 3D ソースをスポーンし、EntityId を返す。
    ///
    /// 返った EntityId を使って `set_source_spatial_params()` / `set_source_spatial_enabled()` /
    /// `batch_set_source_positions()` で空間パラメータを更新する。
    #[must_use]
    pub fn spawn_source(
        &mut self,
        buffer: BufferId,
        vol: f32,
        pitch: f32,
        bus: EntityId,
        looping: bool,
    ) -> Option<EntityId> {
        let index = self.buffer_pool.resolve(buffer)?;
        let output_bus_dense = self.bus_routing.resolve_dense(bus)?;

        let id = EntityId {
            index: self.next_source_index,
            generation: 0,
        };

        self.command_producer
            .try_push(Command::SpawnSource {
                id,
                audio_buffer_index: index,
                vol,
                pitch,
                output_bus_dense,
                token: 0,
                looping,
            })
            .ok()?;

        self.next_source_index += 1;
        Some(id)
    }

    /// 3D ソースをスポーンし、自然終了時にコールバックを呼ぶ。
    ///
    /// `looping = true` の場合は終了通知が発火しないため、コールバックは呼ばれずに
    /// `stop_source()` などで明示的に終わらせるまで保持される。
    /// `MAX_SOURCES` 上限などでコマンド送信に失敗した場合はコールバックは呼ばれず破棄される。
    #[must_use]
    pub fn spawn_source_with_callback(
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

        let id = EntityId {
            index: self.next_source_index,
            generation: 0,
        };

        let token = self.callbacks.register(Box::new(callback));
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
            })
            .is_ok();
        if !ok {
            self.callbacks.cancel(token);
            return None;
        }

        self.next_source_index += 1;
        Some(id)
    }

    /// すべてのボイスを停止する。
    ///
    /// 登録済みのコールバックは解放されるが呼び出されない。
    #[must_use]
    pub fn stop_all(&mut self) -> bool {
        self.callbacks.clear();
        self.command_producer.try_push(Command::StopAll).is_ok()
    }

    // ── ライブソース制御 ──

    /// ソースの音量を設定する（spawn 後の動的変更）。
    #[must_use]
    pub fn set_source_volume(&mut self, id: EntityId, vol: f32) -> bool {
        self.command_producer
            .try_push(Command::SetSourceVolume { id, vol })
            .is_ok()
    }

    /// ソースのピッチを設定する（spawn 後の動的変更）。
    #[must_use]
    pub fn set_source_pitch(&mut self, id: EntityId, pitch: f32) -> bool {
        self.command_producer
            .try_push(Command::SetSourcePitch { id, pitch })
            .is_ok()
    }

    /// ソースの再生位置（フレーム単位）をシークする。
    #[must_use]
    pub fn seek_source(&mut self, id: EntityId, frame_offset: f32) -> bool {
        self.command_producer
            .try_push(Command::SeekSource { id, frame_offset })
            .is_ok()
    }

    /// ソースを一時停止する。再生位置は保持される。
    #[must_use]
    pub fn pause_source(&mut self, id: EntityId) -> bool {
        self.command_producer
            .try_push(Command::PauseSource { id })
            .is_ok()
    }

    /// 一時停止中のソースを再開する。
    #[must_use]
    pub fn resume_source(&mut self, id: EntityId) -> bool {
        self.command_producer
            .try_push(Command::ResumeSource { id })
            .is_ok()
    }

    /// ソースを停止する。次の audio callback で despawn される。
    #[must_use]
    pub fn stop_source(&mut self, id: EntityId) -> bool {
        self.command_producer
            .try_push(Command::StopSource { id })
            .is_ok()
    }

    /// ソースのループフラグを動的に変更する。
    #[must_use]
    pub fn set_source_loop(&mut self, id: EntityId, looping: bool) -> bool {
        self.command_producer
            .try_push(Command::SetSourceLoop { id, looping })
            .is_ok()
    }
}
