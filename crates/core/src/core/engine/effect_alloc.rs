//! メインスレッド側 Effect スロットアロケータ。
//!
//! `EffectId` (= `EntityId`) を `[0, MAX_EFFECTS)` の範囲に収め、削除時の slot 再利用を
//! メインスレッドで管理する。Source と異なり Effect は audio thread → main thread の
//! 削除通知経路を持たないため、`remove_effect` 呼び出し時に直接 `free()` する。

use crate::effect::MAX_EFFECTS;
use crate::entity::EntityId;

pub(crate) struct EffectIdAllocator {
    free_list: Vec<u32>,
    next_index: u32,
    generation: Vec<u32>, // 動的サイズ。固定 [u32; MAX_EFFECTS] でも可だが MAX 変更追従のため Vec。
}

impl EffectIdAllocator {
    pub(crate) fn new() -> Self {
        Self {
            free_list: Vec::with_capacity(MAX_EFFECTS),
            next_index: 0,
            generation: vec![0; MAX_EFFECTS],
        }
    }

    /// 内部 `Vec` の確保ヒープ実バイト数 (`memory_stats` walker 用)。
    pub(crate) fn memory_bytes(&self) -> usize {
        use crate::memory::vec_cap_bytes;
        vec_cap_bytes(&self.free_list) + vec_cap_bytes(&self.generation)
    }

    pub(crate) fn alloc(&mut self) -> Option<EntityId> {
        let index = if let Some(reused) = self.free_list.pop() {
            reused
        } else if (self.next_index as usize) < MAX_EFFECTS {
            let i = self.next_index;
            self.next_index += 1;
            i
        } else {
            return None;
        };
        let generation = self.generation[index as usize];
        Some(EntityId { index, generation })
    }

    pub(crate) fn free(&mut self, id: EntityId) {
        let i = id.index as usize;
        if i >= MAX_EFFECTS {
            return;
        }
        if self.generation[i] != id.generation {
            return;
        }
        self.generation[i] = self.generation[i].wrapping_add(1);
        self.free_list.push(id.index);
    }
}
