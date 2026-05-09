//! Phase 4-2: Random Container の公開 API。
//!
//! Container はメインスレッド完結型 (audio thread に流れない)。
//! `play_container_*` はメインスレッドで子を 1 つ選び、既存の Source 再生
//! コマンドに変換して発行する。設計詳細は `docs/design/core/container.md` 参照。

use std::ffi::c_void;

use ringbuf::traits::Producer;

use crate::buffer_pool::BufferId;
use crate::command::Command;
use crate::container::{ContainerId, RandomPick};
use crate::entity::EntityId;

use super::SoundEngine;
use super::source_api::NativeFinishFn;

impl SoundEngine {
    /// Random Container を生成する。
    ///
    /// `children` は 1 個以上必須 (空配列なら `None`)。容量超過時も `None`。
    /// 戻った `ContainerId` は `play_container_*` / `destroy_container` に渡せる。
    #[must_use]
    pub fn create_random_container(&mut self, children: &[BufferId]) -> Option<ContainerId> {
        self.container_world.create_random(children)
    }

    /// Container を破棄する。
    ///
    /// 既に再生開始した Source には影響しない (Container を解決した時点で
    /// 通常の Source として独立して走っているため)。
    /// 未存在 / generation 不一致時は `false`。
    #[must_use]
    pub fn destroy_container(&mut self, id: ContainerId) -> bool {
        self.container_world.destroy(id)
    }

    /// Container から子を 1 つ選んで指定バスに再生する (fire-and-forget)。
    ///
    /// 内部的には `play_to_bus()` と同じ経路。Container が無効な場合 `false`。
    #[must_use]
    pub fn play_container(
        &mut self,
        container: ContainerId,
        vol: f32,
        pitch: f32,
        bus: EntityId,
        looping: bool,
    ) -> bool {
        let Some(pick) = self.container_world.pick(container) else {
            return false;
        };
        let RandomPick::Source(buffer) = pick;
        let Some(audio_buffer_index) = self.buffer_pool.resolve(buffer) else {
            return false;
        };
        let Some(output_bus_dense) = self.bus_routing.resolve_dense(bus) else {
            return false;
        };
        self.command_producer
            .try_push(Command::PlayToBus {
                audio_buffer_index,
                vol,
                pitch,
                output_bus_dense,
                token: 0,
                looping,
                start_dsp_frame: 0,
            })
            .is_ok()
    }

    /// Container から子を 1 つ選んでハンドル付きで再生する。
    ///
    /// 戻る `EntityId` は **選ばれた 1 つの Source** のもの。Container 自体の
    /// ハンドルは `ContainerId` のまま。`play_with_handle()` と同じ意味論で、
    /// 返った EntityId は `set_source_volume` / `stop_source` 等に渡せる。
    #[must_use]
    pub fn play_container_with_handle(
        &mut self,
        container: ContainerId,
        vol: f32,
        pitch: f32,
        bus: EntityId,
        looping: bool,
    ) -> Option<EntityId> {
        let pick = self.container_world.pick(container)?;
        let RandomPick::Source(buffer) = pick;
        let audio_buffer_index = self.buffer_pool.resolve(buffer)?;
        let output_bus_dense = self.bus_routing.resolve_dense(bus)?;

        let id = self.source_slots.alloc()?;
        self.live_params.prime(id, vol, pitch);

        if self
            .command_producer
            .try_push(Command::SpawnSource {
                id,
                audio_buffer_index,
                vol,
                pitch,
                output_bus_dense,
                token: 0,
                looping,
                start_dsp_frame: 0,
            })
            .is_err()
        {
            self.source_slots.free(id);
            return None;
        }
        Some(id)
    }

    /// Container から子を 1 つ選んでハンドル付きで再生し、自然終了時に C 関数
    /// コールバックを呼ぶ (FFI 用、**alloc なし**)。
    ///
    /// 戻る `EntityId` は `play_container_with_handle()` と同じく **選ばれた 1 つの
    /// Source** のもの。`looping = true` の場合は終了通知が発火しないため
    /// コールバックは呼ばれない (Source 経路と同じ意味論)。Container / buffer / bus
    /// が無効、または `MAX_SOURCES` 上限などでコマンド送信に失敗した場合は
    /// コールバックは呼ばれずにキャンセルされる。
    ///
    /// # Safety
    /// `f` / `user_data` は `poll_events()` で発火するまで有効である必要がある。
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn play_container_with_handle_and_callback_native(
        &mut self,
        container: ContainerId,
        vol: f32,
        pitch: f32,
        bus: EntityId,
        looping: bool,
        f: NativeFinishFn,
        user_data: *mut c_void,
    ) -> Option<EntityId> {
        let pick = self.container_world.pick(container)?;
        let RandomPick::Source(buffer) = pick;
        let audio_buffer_index = self.buffer_pool.resolve(buffer)?;
        let output_bus_dense = self.bus_routing.resolve_dense(bus)?;

        let id = self.source_slots.alloc()?;
        self.live_params.prime(id, vol, pitch);

        let Some(token) = self.callbacks.register_native(f, user_data) else {
            self.source_slots.free(id);
            return None;
        };
        let ok = self
            .command_producer
            .try_push(Command::SpawnSource {
                id,
                audio_buffer_index,
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
