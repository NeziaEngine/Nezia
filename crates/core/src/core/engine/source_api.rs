use std::ffi::c_void;

use ringbuf::traits::Producer;

use crate::buffer_pool::BufferId;
use crate::command::Command;
use crate::entity::EntityId;

use super::SoundEngine;

/// FFI 用の C 関数ポインタコールバック型。
///
/// `play_*_with_callback_native` 系で受け取る型。`extern "C"` のため
/// クロージャキャプチャはできず、`user_data` を経由して呼出側のコンテキストを伝える。
pub type NativeFinishFn = unsafe extern "C" fn(user_data: *mut c_void);

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
            })
            .is_ok();
        if !ok {
            self.callbacks.cancel(token);
            self.source_slots.free(id);
            return None;
        }
        Some(id)
    }

    /// マスターバスに C 関数コールバック付きで再生する（FFI 用、**alloc なし**）。
    ///
    /// 内部の `CallbackRegistry` に `Box<dyn FnOnce>` を生成せず、関数ポインタと
    /// `user_data` を固定スロットに直接書き込む。FFI 越しに頻繁に発音される
    /// シーン（ヒット音多用 等）でヒープアロケーションが発生しない。
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
            self.source_slots.free(id);
            return None;
        }
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
    ///
    /// SPSC コマンドキューを経由せず、共有 atomic スロットへ直接書き込む。
    /// 反映は次のオーディオコールバックで（典型 5〜10 ms）。キュー満杯失敗は発生しない。
    /// 戻り値は常に `true`（範囲外 index・stale generation でも silent に無視される）。
    #[must_use]
    pub fn set_source_volume(&mut self, id: EntityId, vol: f32) -> bool {
        self.live_params.store_volume(id, vol);
        true
    }

    /// ソースのピッチを設定する（spawn 後の動的変更）。詳細は `set_source_volume` 参照。
    #[must_use]
    pub fn set_source_pitch(&mut self, id: EntityId, pitch: f32) -> bool {
        self.live_params.store_pitch(id, pitch);
        true
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

    /// Voice Virtualization 用優先度を設定する (Wwise / CRI ADX2 互換)。
    ///
    /// 値域 `0..=255`、**高い値ほど高優先**。既定値 128 (中央値)。
    /// Wwise の Priority は 0..100、ADX2 の Voice Priority は 0..255 だが、
    /// いずれも「高い値ほど重要」という共通セマンティクスに従う。
    /// 物理ボイス上限 (`MAX_PHYSICAL_VOICES`) を超えるアクティブソースが存在するとき、
    /// 優先度・音量・距離減衰の総合スコアが下位のソースが仮想化される (ミキシングはスキップ、
    /// `sample_offset` のみ前進して時間同期を維持)。
    #[must_use]
    pub fn set_source_priority(&mut self, id: EntityId, priority: u8) -> bool {
        self.command_producer
            .try_push(Command::SetSourcePriority { id, priority })
            .is_ok()
    }
}
