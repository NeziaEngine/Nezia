//! 基本 Source 再生 API (Rust クロージャ版、即時発音)。

use crate::buffer_pool::BufferId;
use crate::command::{Command, SpawnSpatialInit};
use crate::entity::EntityId;

use super::super::SoundEngine;

impl SoundEngine {
    /// ボイスをマスターバスに再生する（fire-and-forget）。
    #[must_use]
    pub fn play(&mut self, buffer: BufferId, vol: f32, pitch: f32, looping: bool) -> bool {
        let Some(index) = self.buffer_pool.resolve(buffer) else {
            return false;
        };
        self.try_send_command(Command::Play {
            audio_buffer_index: index,
            vol,
            pitch,
            token: 0,
            looping,
            start_dsp_frame: 0,
        })
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
        self.try_send_command(Command::PlayToBus {
            audio_buffer_index: index,
            vol,
            pitch,
            output_bus_dense,
            token: 0,
            looping,
            start_dsp_frame: 0,
        })
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
    ///
    /// 内部実装は `play_with_handle_init()` に転送する (priority=128 / spatial 無効)。
    #[must_use]
    pub fn play_with_handle(
        &mut self,
        buffer: BufferId,
        vol: f32,
        pitch: f32,
        bus: EntityId,
        looping: bool,
    ) -> Option<EntityId> {
        self.play_with_handle_init(buffer, vol, pitch, bus, looping, 128, SpawnSpatialInit::NONE)
    }

    /// `play_with_handle()` の spawn-時 priority/spatial 一括初期化版。
    ///
    /// 旧経路は spawn 後に `set_source_priority` / `set_source_spatial_params` /
    /// `set_source_doppler_level` 等を別コマンドで送るため、1 ボイスで 4〜5 個の
    /// SPSC コマンドを消費していた。このメソッドは全初期化を `Command::SpawnSource`
    /// に同梱し、1 ボイス = 1 コマンドに圧縮する。多数の 3D ソースを 1 フレームで
    /// バースト Play する用途 (例: 弾幕・群衆) でリング詰まりを回避するのに使う。
    ///
    /// `priority` は Voice Virtualization 用 (0..=255、高いほど優先、既定 128)。
    /// `spatial_init` で 2D / 3D を選ぶ:
    /// - 2D ソース: `SpawnSpatialInit::NONE` を渡す。
    /// - 3D ソース: `enabled = true` で各種距離減衰・Doppler パラメータを埋める。
    ///   spawn 後の position 更新は従来どおり `batch_set_source_positions()`。
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn play_with_handle_init(
        &mut self,
        buffer: BufferId,
        vol: f32,
        pitch: f32,
        bus: EntityId,
        looping: bool,
        priority: u8,
        spatial_init: SpawnSpatialInit,
    ) -> Option<EntityId> {
        let index = self.buffer_pool.resolve(buffer)?;
        let output_bus_dense = self.bus_routing.resolve_dense(bus)?;

        let id = self.source_slots.alloc()?;
        // ライブパラメータスロットを priming（古い値・古い generation を上書き）。
        // setter 経路がここに直接 atomic store するため、初期値も同じ場所に書く。
        // spawn 時に spatial を有効化する場合は live_params 側のフラグも合わせる
        // (audio thread の参照経路が live_params 経由のため)。
        self.live_params.prime(id, vol, pitch);
        if spatial_init.enabled {
            self.live_params.store_spatial_enabled(id, true);
        }

        if !self.try_send_command(Command::SpawnSource {
            id,
            audio_buffer_index: index,
            vol,
            pitch,
            output_bus_dense,
            token: 0,
            looping,
            start_dsp_frame: 0,
            priority,
            spatial_init,
        }) {
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
