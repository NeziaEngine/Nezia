//! audio thread からのイベントをドレインしてコールバックを発火する。
//!
//! ゲームループの毎フレーム末尾で `poll_events()` を呼ぶ想定。`SourceFinished` /
//! `PlayFailed` / `SourceDespawned` / `StreamingUnderrun` / `CaptureOverflow` を
//! 種別ごとに dispatch し、最後にソース状態スナップショットを SoA キャッシュへ詰め替える。

use ringbuf::traits::Consumer;

use crate::event::Event;

use super::SoundEngine;
use super::callback_registry::CallbackKind;

impl SoundEngine {
    /// ゲームループの毎フレーム末尾で呼ぶ。
    ///
    /// サウンドスレッドからのイベントをドレインし、登録済みの `on_finish` コールバックを呼び出す。
    pub fn poll_events(&mut self) {
        while let Some(ev) = self.event_consumer.try_pop() {
            match ev {
                Event::SourceFinished { token } => {
                    match self.callbacks.complete(token) {
                        CallbackKind::Native { f, user_data } => {
                            // SAFETY: 呼出側契約により f / user_data は発火時まで有効。
                            // ABI 越境を最小化するため fn ptr を直呼びする（Box ナシ）。
                            unsafe { f(user_data as *mut std::ffi::c_void) };
                        }
                        CallbackKind::Rust(closure) => closure(),
                        CallbackKind::Empty => {}
                    }
                }
                Event::PlayFailed { token } => {
                    // コールバックを解放するのみ（呼び出しは行わない）。
                    self.callbacks.cancel(token);
                }
                Event::SourceDespawned { id } => {
                    // 当該ソース起点の Send ハンドルを一括解放する (Wwise / FMOD 互換の
                    // per-event aux send 自動 cleanup 規約)。audio thread 側は
                    // `SourceWorld::despawn_by_dense_index` で send_lookup を既にクリア済み。
                    for slot in 0..self.source_sends.len() {
                        if let Some((send_id, src)) = self.source_sends[slot]
                            && src == id
                        {
                            self.source_sends[slot] = None;
                            self.send_slots.free(send_id);
                        }
                    }
                    // スロット index を再利用キューに戻す。
                    self.source_slots.free(id);
                }
                Event::StreamingUnderrun { buffer } => {
                    // 現状は通知のみ。アプリ側がコールバック経由で観測するための
                    // 公開 API を Phase 2-4 後半で追加予定。
                    let _ = buffer;
                }
                Event::CaptureOverflow { dropped_samples } => {
                    // 通知のみ。累積カウンタ (`CaptureReader::dropped_samples`) で値は取れる。
                    // 連続発火時のレート抑制は audio thread 側に閉じる方針。
                    let _ = dropped_samples;
                }
            }
        }

        // ソース状態スナップショットを取り込む（AoS → SoA への詰め替え）。
        if self.source_snapshots_output.update() {
            let snapshots = self.source_snapshots_output.output_buffer_mut();
            self.source_state_cache.refill_from(snapshots);
        }
    }
}
