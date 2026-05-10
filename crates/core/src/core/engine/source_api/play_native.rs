//! FFI 用 alloc-free Native コールバック版 play API。
//!
//! 内部の `CallbackRegistry` に `Box<dyn FnOnce>` を生成せず、関数ポインタと
//! `user_data` を固定スロットに直接書き込む。FFI 越しに頻繁に発音される
//! シーン（ヒット音多用 等）でヒープアロケーションが発生しない。

use std::ffi::c_void;

use crate::buffer_pool::BufferId;
use crate::command::{Command, SpawnSpatialInit};
use crate::entity::EntityId;

use super::super::SoundEngine;
use super::NativeFinishFn;

impl SoundEngine {
    /// マスターバスに C 関数コールバック付きで再生する（FFI 用、**alloc なし**）。
    ///
    /// # Safety
    /// `f` / `user_data` は `poll_events()` で発火するまで有効である必要がある。
    #[must_use]
    pub unsafe fn play_with_callback_native(
        &mut self,
        buffer: BufferId,
        vol: f32,
        pitch: f32,
        looping: bool,
        f: NativeFinishFn,
        user_data: *mut c_void,
    ) -> bool {
        let Some(index) = self.buffer_pool.resolve(buffer) else {
            return false;
        };
        let Some(token) = self.callbacks.register_native(f, user_data) else {
            return false;
        };
        let ok = self.try_send_command(Command::Play {
            audio_buffer_index: index,
            vol,
            pitch,
            token,
            looping,
            start_dsp_frame: 0,
        });
        if !ok {
            self.callbacks.cancel(token);
        }
        ok
    }

    /// 指定バスに C 関数コールバック付きで再生する（FFI 用、**alloc なし**）。
    ///
    /// # Safety
    /// `f` / `user_data` は `poll_events()` で発火するまで有効である必要がある。
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn play_to_bus_with_callback_native(
        &mut self,
        buffer: BufferId,
        vol: f32,
        pitch: f32,
        bus: EntityId,
        looping: bool,
        f: NativeFinishFn,
        user_data: *mut c_void,
    ) -> bool {
        let Some(index) = self.buffer_pool.resolve(buffer) else {
            return false;
        };
        let Some(output_bus_dense) = self.bus_routing.resolve_dense(bus) else {
            return false;
        };
        let Some(token) = self.callbacks.register_native(f, user_data) else {
            return false;
        };
        let ok = self.try_send_command(Command::PlayToBus {
            audio_buffer_index: index,
            vol,
            pitch,
            output_bus_dense,
            token,
            looping,
            start_dsp_frame: 0,
        });
        if !ok {
            self.callbacks.cancel(token);
        }
        ok
    }

    /// 制御ハンドル付き + C 関数コールバック付きで再生する（FFI 用、**alloc なし**）。
    ///
    /// # Safety
    /// `f` / `user_data` は `poll_events()` で発火するまで有効である必要がある。
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn play_with_handle_and_callback_native(
        &mut self,
        buffer: BufferId,
        vol: f32,
        pitch: f32,
        bus: EntityId,
        looping: bool,
        f: NativeFinishFn,
        user_data: *mut c_void,
    ) -> Option<EntityId> {
        let index = self.buffer_pool.resolve(buffer)?;
        let output_bus_dense = self.bus_routing.resolve_dense(bus)?;

        let id = self.source_slots.alloc()?;
        self.live_params.prime(id, vol, pitch);

        let Some(token) = self.callbacks.register_native(f, user_data) else {
            self.source_slots.free(id);
            return None;
        };
        let ok = self.try_send_command(Command::SpawnSource {
            id,
            audio_buffer_index: index,
            vol,
            pitch,
            output_bus_dense,
            token,
            looping,
            start_dsp_frame: 0,
            priority: 128,
            spatial_init: SpawnSpatialInit::NONE,
        });
        if !ok {
            self.callbacks.cancel(token);
            self.source_slots.free(id);
            return None;
        }
        Some(id)
    }
}
