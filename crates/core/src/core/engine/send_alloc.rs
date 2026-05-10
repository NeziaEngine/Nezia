//! メインスレッド側 Send スロットアロケータ。
//!
//! `SendId` を `[0, MAX_SENDS)` の範囲に収め、削除時の slot 再利用を
//! メインスレッドで管理する。`EffectIdAllocator` と同パターン。

use crate::bus::{MAX_SENDS, SendId};

pub(crate) struct SendIdAllocator {
    free_list: Vec<u32>,
    next_index: u32,
    generation: Vec<u32>,
}

impl SendIdAllocator {
    pub(crate) fn new() -> Self {
        Self {
            free_list: Vec::with_capacity(MAX_SENDS),
            next_index: 0,
            generation: vec![0; MAX_SENDS],
        }
    }

    /// 内部 `Vec` の確保ヒープ実バイト数 (`memory_stats` walker 用)。
    pub(crate) fn memory_bytes(&self) -> usize {
        use crate::memory::vec_cap_bytes;
        vec_cap_bytes(&self.free_list) + vec_cap_bytes(&self.generation)
    }

    pub(crate) fn alloc(&mut self) -> Option<SendId> {
        let index = if let Some(reused) = self.free_list.pop() {
            reused
        } else if (self.next_index as usize) < MAX_SENDS {
            let i = self.next_index;
            self.next_index += 1;
            i
        } else {
            return None;
        };
        let generation = self.generation[index as usize];
        Some(SendId { index, generation })
    }

    pub(crate) fn free(&mut self, id: SendId) {
        let i = id.index as usize;
        if i >= MAX_SENDS {
            return;
        }
        if self.generation[i] != id.generation {
            return;
        }
        self.generation[i] = self.generation[i].wrapping_add(1);
        self.free_list.push(id.index);
    }
}
