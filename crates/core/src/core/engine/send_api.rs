//! Send (副ルート) 関連の公開 API。
//!
//! - `add_send`: バス → バスの Send (Phase 3-3 PR1)
//! - `add_send_to_compressor`: バス → Compressor sidechain 入力の Send (Phase 3-3 PR2)
//! - `bind_compressor_sidechain`: Compressor の sidechain 駆動を on/off

use ringbuf::traits::Producer;

use crate::bus::{SendId, SendPosition};
use crate::command::{Command, SendDestination};
use crate::core::bus_routing::SendEdge;
use crate::effect::EffectId;
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
                dst: SendDestination::Bus { dense: dst_dense },
                position,
                gain,
            })
            .is_err()
        {
            self.bus_routing.remove_send(send_id);
            self.send_slots.free(send_id);
            return None;
        }
        self.push_process_order(&order);

        Some(send_id)
    }

    /// バス → Compressor sidechain 入力の Send を作成する (Phase 3-3 PR2)。
    ///
    /// 内部的には Compressor の所属バスへの DAG エッジとしてサイクル検出 + topo sort に
    /// 反映される (sidechain trigger として src バスは Compressor 所属バスより先に処理される
    /// 必要があるため)。実際の信号は audio thread で `CompressorWorld.sidechain_buffer` に
    /// 加算ミックスされ、内部検波ではなくこの buffer が検波器入力に使われる
    /// (`use_sidechain` フラグが自動で true になる)。
    ///
    /// 失敗ケース (返り値 `None`):
    /// - `src` がマスターバスまたは無効
    /// - `compressor` が `EffectKind::Compressor` でない / 所属バスが追跡されていない
    /// - サイクルが生じる
    /// - `MAX_SENDS` プール枯渇 / `MAX_SENDS_PER_BUS` 超過
    /// - コマンドリングバッファ満杯
    pub fn add_send_to_compressor(
        &mut self,
        src: EntityId,
        compressor: EffectId,
        position: SendPosition,
        gain: f32,
    ) -> Option<SendId> {
        if src == self.bus_routing.master_bus_id {
            return None;
        }
        let owner_bus = self.compressor_owners.get(&compressor).copied()?;
        if src == owner_bus {
            return None;
        }
        let src_dense = self.bus_routing.resolve_dense(src)?;
        // owner_bus は dense は使わないが存在チェックのために resolve する。
        self.bus_routing.resolve_dense(owner_bus)?;

        if self
            .bus_routing
            .would_create_send_cycle(src.index, owner_bus.index)
        {
            return None;
        }

        let send_id = self.send_slots.alloc()?;

        let edge = SendEdge {
            send_id,
            src_entity: src.index,
            // sidechain Send は audio thread の信号フロー上は別チャネルだが、
            // topo sort 上は「src は owner_bus より先に処理される必要がある」ので owner_bus
            // への論理エッジとして登録する。
            dst_entity: owner_bus.index,
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
                dst: SendDestination::CompressorSidechain { effect: compressor },
                position,
                gain,
            })
            .is_err()
        {
            self.bus_routing.remove_send(send_id);
            self.send_slots.free(send_id);
            return None;
        }
        self.push_process_order(&order);

        Some(send_id)
    }

    /// Compressor の sidechain 駆動を on/off する。
    /// - `enabled = true`: 紐付けられた Send (`add_send_to_compressor` で貼ったもの) からの
    ///   信号を検波器入力に使う。
    /// - `enabled = false`: 内部検波 (自バスの post-fader 信号) に戻す。
    ///
    /// `add_send_to_compressor` は内部で自動的に sidechain を有効化するため、後から無効化
    /// したい場合や、Send は維持したまま一時的に内部検波へ切替たい場合に呼ぶ。
    #[must_use]
    pub fn bind_compressor_sidechain(&mut self, compressor: EffectId, enabled: bool) -> bool {
        if !self.compressor_owners.contains_key(&compressor) {
            return false;
        }
        self.command_producer
            .try_push(Command::SetCompressorSidechainEnabled {
                id: compressor,
                enabled,
            })
            .is_ok()
    }

    /// ソース起点 Send (User-Defined Aux Send) を作成する。
    ///
    /// Wwise / FMOD の per-event aux send 互換。同じ Reverb Bus を共有しつつ「銃声は dry、
    /// 足音は wet」のように音ごとに reverb 量を変えるのに使う。
    ///
    /// 失敗ケース (戻り値 `None`):
    /// - `dst` が無効
    /// - `MAX_SENDS` プール枯渇 / `MAX_SENDS_PER_SOURCE` 超過 (audio thread 側で silently drop)
    /// - コマンドリングバッファ満杯
    ///
    /// `src` の有効性 (= 現在も spawn 中か) は audio thread 側で resolve するため、
    /// メインスレッドでは早期チェックしない。已に despawn 済みの source に貼った場合は
    /// audio thread が無視し、`Event::SourceDespawned` 経路で自動解放される。
    pub fn add_source_send(
        &mut self,
        src: EntityId,
        dst: EntityId,
        position: SendPosition,
        gain: f32,
    ) -> Option<SendId> {
        let dst_dense = self.bus_routing.resolve_dense(dst)?;
        let send_id = self.send_slots.alloc()?;

        if self
            .command_producer
            .try_push(Command::AddSourceSend {
                id: send_id,
                src_entity: src,
                dst: SendDestination::Bus { dense: dst_dense },
                position,
                gain,
            })
            .is_err()
        {
            self.send_slots.free(send_id);
            return None;
        }
        if let Some(slot) = self.source_sends.get_mut(send_id.index as usize) {
            *slot = Some((send_id, src));
        }
        Some(send_id)
    }

    /// ソース起点 Compressor sidechain Send を作成する。
    ///
    /// バス起点版 (`add_send_to_compressor`) と異なり sidechain 駆動はサイクル検出 / topo sort
    /// に絡まない (source は DAG 内ノードではなく leaf 入力なので)。
    ///
    /// 失敗ケース: `compressor` が `EffectKind::Compressor` でない、プール枯渇、コマンド満杯。
    pub fn add_source_send_to_compressor(
        &mut self,
        src: EntityId,
        compressor: EffectId,
        position: SendPosition,
        gain: f32,
    ) -> Option<SendId> {
        // 主スレッド側では Compressor の存在確認だけ行う (audio thread が dense 解決)。
        self.compressor_owners.get(&compressor)?;
        let send_id = self.send_slots.alloc()?;

        if self
            .command_producer
            .try_push(Command::AddSourceSend {
                id: send_id,
                src_entity: src,
                dst: SendDestination::CompressorSidechain { effect: compressor },
                position,
                gain,
            })
            .is_err()
        {
            self.send_slots.free(send_id);
            return None;
        }
        if let Some(slot) = self.source_sends.get_mut(send_id.index as usize) {
            *slot = Some((send_id, src));
        }
        Some(send_id)
    }

    /// `id` がソース起点 Send (= `source_sends` ミラーに存在) か判定するヘルパ。
    fn is_source_send(&self, id: SendId) -> bool {
        self.source_sends
            .get(id.index as usize)
            .and_then(|s| s.as_ref())
            .map(|(sid, _)| sid.generation == id.generation)
            .unwrap_or(false)
    }

    /// Send を削除する。stale な SendId なら `false`。
    #[must_use]
    pub fn remove_send(&mut self, id: SendId) -> bool {
        // バス起点 → 既存ロジック (DAG 反映 + topo sort 再計算)。
        if self.bus_routing.send(id).is_some() {
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
            return true;
        }
        // ソース起点 → DAG / topo sort に絡まないので command + slot 解放のみ。
        if self.is_source_send(id) {
            if self
                .command_producer
                .try_push(Command::RemoveSend { id })
                .is_err()
            {
                return false;
            }
            if let Some(slot) = self.source_sends.get_mut(id.index as usize) {
                *slot = None;
            }
            self.send_slots.free(id);
            return true;
        }
        false
    }

    /// Send の gain を設定する。
    #[must_use]
    pub fn set_send_gain(&mut self, id: SendId, gain: f32) -> bool {
        if self.bus_routing.send(id).is_none() && !self.is_source_send(id) {
            return false;
        }
        self.command_producer
            .try_push(Command::SetSendGain { id, gain })
            .is_ok()
    }

    /// Send のタップ位置 (Pre-Fader / Post-Fader) を変更する。
    #[must_use]
    pub fn set_send_position(&mut self, id: SendId, position: SendPosition) -> bool {
        if self.bus_routing.send(id).is_none() && !self.is_source_send(id) {
            return false;
        }
        self.command_producer
            .try_push(Command::SetSendPosition { id, position })
            .is_ok()
    }
}
