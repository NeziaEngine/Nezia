//! Snapshot レジストリ (メインスレッド側所有 + lock-free snapshot 共有)。
//!
//! `AudioBufferPool` / `CurveRegistry` と同じパターン。`Arc<ArcSwap<Vec<Option<Arc<Snapshot>>>>>`
//! でサウンドスレッドが apply 時に 1 度だけ load する (継続 read はしない)。

use std::sync::Arc;

use arc_swap::ArcSwap;

use super::world::Snapshot;

/// 同時に保持できる Snapshot の最大数。
pub const MAX_SNAPSHOTS: usize = 64;

/// Snapshot のハンドル。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SnapshotId {
    pub index: u32,
    pub generation: u32,
}

#[derive(Clone, Copy)]
struct SnapshotSlot {
    generation: u32,
    occupied: bool,
}

/// Snapshot 集合 (メインスレッド所有)。
pub struct SnapshotRegistry {
    slots: Vec<SnapshotSlot>,
    snapshots: Vec<Option<Arc<Snapshot>>>,
    shared: Arc<ArcSwap<Vec<Option<Arc<Snapshot>>>>>,
    free_list: Vec<u32>,
    next_index: u32,
}

impl SnapshotRegistry {
    pub fn new(shared: Arc<ArcSwap<Vec<Option<Arc<Snapshot>>>>>) -> Self {
        Self {
            slots: Vec::with_capacity(MAX_SNAPSHOTS),
            snapshots: Vec::with_capacity(MAX_SNAPSHOTS),
            shared,
            free_list: Vec::new(),
            next_index: 0,
        }
    }

    /// レジストリ全体のヒープ実バイト数 (`memory_stats` walker 用)。
    /// 各 `Snapshot` 内 SoA Vec も再帰的に集計する (Arc 共有分は registry 側で 1 回計上)。
    pub(crate) fn memory_bytes(&self) -> usize {
        use crate::memory::vec_cap_bytes;
        let mut total = vec_cap_bytes(&self.slots)
            + vec_cap_bytes(&self.snapshots)
            + vec_cap_bytes(&self.free_list);
        for s in self.snapshots.iter().flatten() {
            total += vec_cap_bytes(&s.bus_gains)
                + vec_cap_bytes(&s.bus_muted)
                + vec_cap_bytes(&s.effect_params)
                + vec_cap_bytes(&s.send_gains);
        }
        total
    }

    /// Snapshot を登録してハンドルを返す。`MAX_SNAPSHOTS` 超過時は `None`。
    pub fn create(&mut self, snapshot: Snapshot) -> Option<SnapshotId> {
        if self.slots.len() >= MAX_SNAPSHOTS && self.free_list.is_empty() {
            return None;
        }
        let (index, generation) = self.allocate_slot();
        self.snapshots[index as usize] = Some(Arc::new(snapshot));
        self.sync_shared();
        Some(SnapshotId { index, generation })
    }

    /// Snapshot を破棄する。
    /// 既に apply 済みで進行中の補間 (`ActiveSnapshot`) には影響しない
    /// (`ActiveSnapshot` は apply 時に値をコピー済みのため)。
    pub fn destroy(&mut self, id: SnapshotId) -> bool {
        let Some(slot) = self.slots.get_mut(id.index as usize) else {
            return false;
        };
        if slot.generation != id.generation || !slot.occupied {
            return false;
        }
        self.snapshots[id.index as usize] = None;
        slot.generation += 1;
        slot.occupied = false;
        self.free_list.push(id.index);
        self.sync_shared();
        true
    }

    /// ハンドル検証。有効ならスロット index を返す。
    pub fn resolve(&self, id: SnapshotId) -> Option<u32> {
        let slot = self.slots.get(id.index as usize)?;
        if slot.generation != id.generation || !slot.occupied {
            return None;
        }
        Some(id.index)
    }

    fn allocate_slot(&mut self) -> (u32, u32) {
        if let Some(index) = self.free_list.pop() {
            let generation = self.slots[index as usize].generation;
            self.slots[index as usize] = SnapshotSlot {
                generation,
                occupied: true,
            };
            (index, generation)
        } else {
            let index = self.next_index;
            self.next_index += 1;
            self.slots.push(SnapshotSlot {
                generation: 0,
                occupied: true,
            });
            self.snapshots.push(None);
            (index, 0)
        }
    }

    fn sync_shared(&self) {
        self.shared.store(Arc::new(self.snapshots.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_resolve() {
        let shared = Arc::new(ArcSwap::from_pointee(Vec::new()));
        let mut reg = SnapshotRegistry::new(shared);
        let id = reg.create(Snapshot::new()).unwrap();
        assert!(reg.resolve(id).is_some());
    }

    #[test]
    fn destroy_invalidates_handle() {
        let shared = Arc::new(ArcSwap::from_pointee(Vec::new()));
        let mut reg = SnapshotRegistry::new(shared);
        let id = reg.create(Snapshot::new()).unwrap();
        assert!(reg.destroy(id));
        assert!(reg.resolve(id).is_none());
        assert!(!reg.destroy(id));
    }

    #[test]
    fn slot_reuse_bumps_generation() {
        let shared = Arc::new(ArcSwap::from_pointee(Vec::new()));
        let mut reg = SnapshotRegistry::new(shared);
        let id1 = reg.create(Snapshot::new()).unwrap();
        reg.destroy(id1);
        let id2 = reg.create(Snapshot::new()).unwrap();
        assert_eq!(id1.index, id2.index);
        assert_ne!(id1.generation, id2.generation);
        assert!(reg.resolve(id1).is_none());
        assert!(reg.resolve(id2).is_some());
    }
}
