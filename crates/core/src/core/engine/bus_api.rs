use ringbuf::traits::Producer;

use crate::bus::MAX_BUSES;
use crate::command::Command;
use crate::entity::EntityId;

use super::SoundEngine;

impl SoundEngine {
    /// マスター音量を設定する（0.0〜1.0）。マスターバスの gain を変更する。
    #[must_use]
    pub fn set_volume(&mut self, volume: f32) -> bool {
        self.command_producer
            .try_push(Command::SetVolume(volume))
            .is_ok()
    }

    /// マスターバスの EntityId を返す。
    pub fn master_bus(&self) -> EntityId {
        self.bus_routing.master_bus_id
    }

    /// マスターバスに接続されたバスを生成する。
    pub fn create_bus(&mut self, gain: f32) -> Option<EntityId> {
        let master = self.bus_routing.master_bus_id;
        self.create_bus_routed(gain, master)
    }

    /// 指定した親バスに接続されたバスを生成する。
    pub fn create_bus_routed(&mut self, gain: f32, parent: EntityId) -> Option<EntityId> {
        if self.bus_routing.len() >= MAX_BUSES {
            return None;
        }
        let parent_dense = self.bus_routing.resolve_dense(parent)?;

        let new_index = self.bus_routing.next_index;
        self.bus_routing.next_index += 1;

        let new_dense = self.bus_routing.len() as u32;
        self.bus_routing.insert(new_index, parent.index, new_dense);

        let order = self.bus_routing.compute_process_order();

        let new_id = EntityId {
            index: new_index,
            generation: 0,
        };

        if self
            .command_producer
            .try_push(Command::SpawnBus {
                id: new_id,
                gain,
                output_bus_dense: parent_dense,
            })
            .is_err()
        {
            self.bus_routing.remove(new_index);
            self.bus_routing.next_index -= 1;
            return None;
        }

        self.push_process_order(&order);

        Some(new_id)
    }

    /// バスを削除する。マスターバスは削除できない（`false` を返す）。
    pub fn destroy_bus(&mut self, id: EntityId) -> bool {
        if id == self.bus_routing.master_bus_id {
            return false;
        }
        if self.bus_routing.resolve_dense(id).is_none() {
            return false;
        }

        self.bus_routing.remove(id.index);

        let order = self.bus_routing.compute_process_order();

        if self
            .command_producer
            .try_push(Command::DespawnBus { id })
            .is_err()
        {
            return false;
        }
        self.push_process_order(&order);
        true
    }

    /// バスのゲインを設定する。
    #[must_use]
    pub fn set_bus_gain(&mut self, id: EntityId, gain: f32) -> bool {
        self.command_producer
            .try_push(Command::SetBusGain { id, gain })
            .is_ok()
    }

    /// バスのミュートを設定する。
    #[must_use]
    pub fn set_bus_muted(&mut self, id: EntityId, muted: bool) -> bool {
        self.command_producer
            .try_push(Command::SetBusMuted { id, muted })
            .is_ok()
    }

    /// バスの出力先を変更する。ループが検出された場合は `false` を返す。
    #[must_use]
    pub fn set_bus_output(&mut self, id: EntityId, parent: EntityId) -> bool {
        if id == self.bus_routing.master_bus_id {
            return false;
        }
        if self.bus_routing.has_loop(id.index, parent.index) {
            return false;
        }
        let Some(output_bus_dense) = self.bus_routing.resolve_dense(parent) else {
            return false;
        };
        if self.bus_routing.resolve_dense(id).is_none() {
            return false;
        }

        self.bus_routing.set_parent(id.index, parent.index);
        let order = self.bus_routing.compute_process_order();

        if self
            .command_producer
            .try_push(Command::SetBusOutput {
                id,
                output_bus_dense,
            })
            .is_err()
        {
            return false;
        }
        self.push_process_order(&order);
        true
    }

    pub(super) fn push_process_order(&mut self, order: &[u32]) {
        let mut arr = [0u32; MAX_BUSES];
        let len = order.len().min(MAX_BUSES);
        arr[..len].copy_from_slice(&order[..len]);
        let _ = self.command_producer.try_push(Command::UpdateProcessOrder {
            order: arr,
            len: len as u8,
        });
    }
}
