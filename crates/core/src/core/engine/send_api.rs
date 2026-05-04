//! Send (副ルート) 関連の公開 API。
//!
//! Phase 3-3 PR1 ではバス → バス Send のみ実装する。Compressor sidechain への Send は
//! PR2 で `add_send_to_compressor` として追加する。

use ringbuf::traits::Producer;

use crate::bus::{SendId, SendPosition};
use crate::command::Command;
use crate::core::bus_routing::SendEdge;
use crate::entity::EntityId;

use super::SoundEngine;

impl SoundEngine {
    /// バス → バスの Send を作成する。
    ///
    /// 失敗ケース (返り値 `None`):
    /// - `src` または `dst` が無効
    /// - `src` がマスターバス (マスターから出ていく Send は禁止)
    /// - サイクルが生じる
    /// - `MAX_SENDS` プール枯渇 / `MAX_SENDS_PER_BUS` 超過
    /// - コマンドリングバッファ満杯
    pub fn add_send(
        &mut self,
        src: EntityId,
        dst: EntityId,
        position: SendPosition,
        gain: f32,
    ) -> Option<SendId> {
        if src == self.bus_routing.master_bus_id {
            return None;
        }
        if src == dst {
            return None;
        }
        let src_dense = self.bus_routing.resolve_dense(src)?;
        let dst_dense = self.bus_routing.resolve_dense(dst)?;

        if self
            .bus_routing
            .would_create_send_cycle(src.index, dst.index)
        {
            return None;
        }

        let send_id = self.send_slots.alloc()?;

        // メインスレッドミラーに Send エッジを登録 + 新しい process_order を計算。
        let edge = SendEdge {
            send_id,
            src_entity: src.index,
            dst_entity: dst.index,
        };
        if !self.bus_routing.add_send(edge) {
            self.send_slots.free(send_id);
            return None;
        }
        let order = self.bus_routing.compute_process_order();

        if self
            .command_producer
            .try_push(Command::AddSend {
                id: send_id,
                src_dense,
                dst_dense,
                position,
                gain,
            })
            .is_err()
        {
            // ロールバック: ミラーから edge を取り除き、SendId を解放。
            self.bus_routing.remove_send(send_id);
            self.send_slots.free(send_id);
            return None;
        }
        self.push_process_order(&order);

        Some(send_id)
    }

    /// Send を削除する。stale な SendId なら `false`。
    #[must_use]
    pub fn remove_send(&mut self, id: SendId) -> bool {
        if self.bus_routing.send(id).is_none() {
            return false;
        }
        self.bus_routing.remove_send(id);
        let order = self.bus_routing.compute_process_order();

        if self
            .command_producer
            .try_push(Command::RemoveSend { id })
            .is_err()
        {
            return false;
        }
        self.push_process_order(&order);
        self.send_slots.free(id);
        true
    }

    /// Send の gain を設定する。
    #[must_use]
    pub fn set_send_gain(&mut self, id: SendId, gain: f32) -> bool {
        if self.bus_routing.send(id).is_none() {
            return false;
        }
        self.command_producer
            .try_push(Command::SetSendGain { id, gain })
            .is_ok()
    }

    /// Send のタップ位置 (Pre-Fader / Post-Fader) を変更する。
    #[must_use]
    pub fn set_send_position(&mut self, id: SendId, position: SendPosition) -> bool {
        if self.bus_routing.send(id).is_none() {
            return false;
        }
        self.command_producer
            .try_push(Command::SetSendPosition { id, position })
            .is_ok()
    }
}
