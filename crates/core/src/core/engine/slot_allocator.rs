//! メインスレッド側 Source スロットアロケータ。
//!
//! `EntityId.index` を `[0, MAX_SOURCES)` の範囲に収めるために、メインスレッドが
//! サウンドスレッドのスパースセットと同等の slot 再利用ロジックを持つ。
//! `Event::SourceDespawned` を `poll_events()` で受け取り、freed index を再利用キューに戻す。
//!
//! これにより、ライブパラメータ用の `[AtomicU64; MAX_SOURCES]` を fixed-size 配列で持てる。

use crate::entity::EntityId;
use crate::source::MAX_SOURCES;

/// Source の `EntityId` を発行・回収する。
///
/// - `next_index` は未使用のスロットを上から順に消費する（最大 `MAX_SOURCES`）。
/// - `free_list` は despawn 通知で戻ってきたスロットを LIFO で再利用する。
/// - `generation` は各スロットごとに spawn のたびに 1 ずつ増える。
pub(crate) struct SourceSlotAllocator {
    free_list: Vec<u32>,
    next_index: u32,
    /// 各スロットの「次に発行する generation」。スロット再利用時にインクリメント済み。
    generation: [u32; MAX_SOURCES],
}

impl SourceSlotAllocator {
    pub(crate) fn new() -> Self {
        Self {
            free_list: Vec::with_capacity(MAX_SOURCES),
            next_index: 0,
            generation: [0; MAX_SOURCES],
        }
    }

    /// 新しい `EntityId` を確保する。`MAX_SOURCES` 上限で `None`。
    pub(crate) fn alloc(&mut self) -> Option<EntityId> {
        let index = if let Some(reused) = self.free_list.pop() {
            reused
        } else if (self.next_index as usize) < MAX_SOURCES {
            let i = self.next_index;
            self.next_index += 1;
            i
        } else {
            return None;
        };
        let generation = self.generation[index as usize];
        Some(EntityId { index, generation })
    }

    /// despawn された `EntityId` のスロットを再利用キューに戻す。
    ///
    /// generation が一致しない通知（重複・古い）は無視する。
    pub(crate) fn free(&mut self, id: EntityId) {
        let i = id.index as usize;
        if i >= MAX_SOURCES {
            return;
        }
        if self.generation[i] != id.generation {
            return;
        }
        // 次回 alloc で再利用する際は新しい generation を発行する。
        self.generation[i] = self.generation[i].wrapping_add(1);
        self.free_list.push(id.index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_returns_unique_ids() {
        let mut a = SourceSlotAllocator::new();
        let id0 = a.alloc().unwrap();
        let id1 = a.alloc().unwrap();
        assert_ne!(id0.index, id1.index);
    }

    #[test]
    fn free_then_alloc_bumps_generation() {
        let mut a = SourceSlotAllocator::new();
        let id0 = a.alloc().unwrap();
        a.free(id0);
        let id1 = a.alloc().unwrap();
        assert_eq!(id0.index, id1.index);
        assert_eq!(id1.generation, id0.generation + 1);
    }

    #[test]
    fn capacity_limit() {
        let mut a = SourceSlotAllocator::new();
        for _ in 0..MAX_SOURCES {
            assert!(a.alloc().is_some());
        }
        assert!(a.alloc().is_none());
    }

    #[test]
    fn stale_free_ignored() {
        let mut a = SourceSlotAllocator::new();
        let id0 = a.alloc().unwrap();
        a.free(id0);
        let _id1 = a.alloc().unwrap();
        // 古い generation の free は無視される（id1 のスロットを誤って解放しない）
        a.free(id0);
        // 次の alloc で id1 のスロットが横取りされていないこと
        let id2 = a.alloc().unwrap();
        assert_ne!(id2.index, id0.index);
    }
}
