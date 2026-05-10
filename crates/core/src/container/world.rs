//! ContainerWorld — generation 付きスロット管理 + Random 選択ロジック。
//!
//! BufferPool と同パターンの slot allocator。dense packing は行わない
//! (Container は random access 中心で一括イテレーションの利点が薄い)。

use super::{ContainerChild, ContainerId, MAX_CONTAINERS};
use crate::buffer_pool::BufferId;

/// `pick()` の戻り値。Phase 4-2 第一弾では `Source` のみだが、
/// 将来 `Container(ContainerId)` ネスト対応時に再帰解決の経路を作るための
/// 中間型として残す。
#[derive(Debug, Clone, Copy)]
pub(crate) enum RandomPick {
    Source(BufferId),
}

impl From<ContainerChild> for RandomPick {
    fn from(c: ContainerChild) -> Self {
        match c {
            ContainerChild::Source(b) => RandomPick::Source(b),
        }
    }
}

struct RandomContainer {
    children: Vec<ContainerChild>,
    last_picked: Option<usize>,
    rng_state: u64,
}

impl RandomContainer {
    fn new(children: Vec<ContainerChild>, seed: u64) -> Self {
        // xorshift64 は state == 0 で恒久 0 になるため、必ず非ゼロを保証する。
        let rng_state = if seed == 0 { 0x9E3779B97F4A7C15 } else { seed };
        Self {
            children,
            last_picked: None,
            rng_state,
        }
    }

    /// xorshift64 を 1 回回して u64 を返す。
    #[inline]
    fn next_u64(&mut self) -> u64 {
        let mut x = self.rng_state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng_state = x;
        x
    }

    fn pick(&mut self) -> RandomPick {
        let n = self.children.len();
        debug_assert!(n >= 1, "RandomContainer must have at least 1 child");

        if n == 1 {
            // avoid-last 不要。`last_picked` も更新しない (常に同じ index なので意味なし)。
            return self.children[0].into();
        }

        // 子 2 個以上: 直前と異なる index になるまで再抽選。
        // 子 2 個で再抽選確率 1/2、3 個で 1/3 ... 期待ループ回数は最大 2 回。
        loop {
            let r = (self.next_u64() % n as u64) as usize;
            if self.last_picked != Some(r) {
                self.last_picked = Some(r);
                return self.children[r].into();
            }
        }
    }
}

struct Slot {
    generation: u32,
    occupied: bool,
    container: Option<RandomContainer>,
}

/// メインスレッド側の Container 管理層。
///
/// `SoundEngine` が所有し、`create_*_container` / `play_container` API の
/// バックエンドとなる。audio thread には公開しない。
pub(crate) struct ContainerWorld {
    slots: Vec<Slot>,
    free_list: Vec<u32>,
    next_index: u32,
    /// `RandomContainer` の seed 生成に使う逐次カウンタ。
    /// 起動時刻 (ns) と組み合わせて個体差を出す。
    seed_counter: u64,
}

impl ContainerWorld {
    pub fn new() -> Self {
        Self {
            slots: Vec::with_capacity(MAX_CONTAINERS),
            free_list: Vec::new(),
            next_index: 0,
            seed_counter: time_seed(),
        }
    }

    /// 内部 `Vec` の確保ヒープ実バイト数 (`memory_stats` walker 用)。
    /// `Slot` 内の `RandomContainer.children` までは追わない (登録時の参照分のみ計上)。
    pub(crate) fn memory_bytes(&self) -> usize {
        use crate::memory::vec_cap_bytes;
        let mut total = vec_cap_bytes(&self.slots) + vec_cap_bytes(&self.free_list);
        for slot in &self.slots {
            if let Some(c) = &slot.container {
                total += vec_cap_bytes(&c.children);
            }
        }
        total
    }

    /// Random Container を生成する。子は 1 個以上必須。0 個なら `None`。
    /// 容量超過時も `None`。
    pub fn create_random(&mut self, children: &[BufferId]) -> Option<ContainerId> {
        if children.is_empty() {
            return None;
        }
        let mapped: Vec<ContainerChild> = children
            .iter()
            .map(|b| ContainerChild::Source(*b))
            .collect();

        let seed = self.next_seed();
        let container = RandomContainer::new(mapped, seed);

        if let Some(index) = self.free_list.pop() {
            let slot = &mut self.slots[index as usize];
            slot.generation = slot.generation.wrapping_add(1);
            slot.occupied = true;
            slot.container = Some(container);
            return Some(ContainerId {
                index,
                generation: slot.generation,
            });
        }

        if (self.next_index as usize) >= MAX_CONTAINERS {
            return None;
        }
        let index = self.next_index;
        self.next_index += 1;
        self.slots.push(Slot {
            generation: 0,
            occupied: true,
            container: Some(container),
        });
        Some(ContainerId {
            index,
            generation: 0,
        })
    }

    /// Container を破棄する。再生中の Source には影響しない。
    pub fn destroy(&mut self, id: ContainerId) -> bool {
        let Some(slot) = self.slots.get_mut(id.index as usize) else {
            return false;
        };
        if !slot.occupied || slot.generation != id.generation {
            return false;
        }
        slot.occupied = false;
        slot.container = None;
        self.free_list.push(id.index);
        true
    }

    /// Container から子を 1 つ選ぶ。Container が存在しない / generation 不一致なら `None`。
    pub fn pick(&mut self, id: ContainerId) -> Option<RandomPick> {
        let slot = self.slots.get_mut(id.index as usize)?;
        if !slot.occupied || slot.generation != id.generation {
            return None;
        }
        slot.container.as_mut().map(|c| c.pick())
    }

    /// 次の seed を生成する。起動時刻 (ns) ⊕ カウンタ で個体差を出す。
    fn next_seed(&mut self) -> u64 {
        self.seed_counter = self.seed_counter.wrapping_add(0x9E3779B97F4A7C15);
        self.seed_counter
    }
}

/// 起動時刻 (ns) を seed の初期値とする。
/// SystemTime の値域は問題ない (ns 単位で u64 に十分収まる)。
fn time_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0xDEAD_BEEF_CAFE_BABE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(i: u32) -> BufferId {
        BufferId {
            index: i,
            generation: 0,
        }
    }

    #[test]
    fn create_random_returns_id() {
        let mut w = ContainerWorld::new();
        let id = w.create_random(&[buf(0), buf(1)]).unwrap();
        assert_eq!(id.index, 0);
    }

    #[test]
    fn create_with_empty_children_fails() {
        let mut w = ContainerWorld::new();
        assert!(w.create_random(&[]).is_none());
    }

    #[test]
    fn destroy_invalidates_handle() {
        let mut w = ContainerWorld::new();
        let id = w.create_random(&[buf(0)]).unwrap();
        assert!(w.destroy(id));
        assert!(w.pick(id).is_none());
    }

    #[test]
    fn destroy_twice_returns_false() {
        let mut w = ContainerWorld::new();
        let id = w.create_random(&[buf(0)]).unwrap();
        assert!(w.destroy(id));
        assert!(!w.destroy(id));
    }

    #[test]
    fn slot_reuse_bumps_generation() {
        let mut w = ContainerWorld::new();
        let old = w.create_random(&[buf(0)]).unwrap();
        w.destroy(old);
        let new = w.create_random(&[buf(1)]).unwrap();
        assert_eq!(old.index, new.index);
        assert_ne!(old.generation, new.generation);
        assert!(w.pick(old).is_none());
        assert!(w.pick(new).is_some());
    }

    #[test]
    fn single_child_always_picks_same() {
        let mut w = ContainerWorld::new();
        let id = w.create_random(&[buf(42)]).unwrap();
        for _ in 0..100 {
            match w.pick(id).unwrap() {
                RandomPick::Source(b) => assert_eq!(b.index, 42),
            }
        }
    }

    #[test]
    fn avoid_last_with_two_children() {
        let mut w = ContainerWorld::new();
        let id = w.create_random(&[buf(10), buf(20)]).unwrap();
        let mut prev: Option<u32> = None;
        for _ in 0..100 {
            let RandomPick::Source(b) = w.pick(id).unwrap();
            if let Some(p) = prev {
                assert_ne!(p, b.index, "consecutive picks must differ");
            }
            prev = Some(b.index);
        }
    }

    #[test]
    fn many_children_distribution() {
        let mut w = ContainerWorld::new();
        let ids: Vec<BufferId> = (0..8).map(buf).collect();
        let id = w.create_random(&ids).unwrap();
        let mut counts = [0u32; 8];
        for _ in 0..1000 {
            let RandomPick::Source(b) = w.pick(id).unwrap();
            counts[b.index as usize] += 1;
        }
        // 一様性の sanity check: 全 index が 1 回以上引かれる + 突出して偏らない
        for c in &counts {
            assert!(*c > 30, "index undersampled: counts={counts:?}");
        }
    }

    #[test]
    fn capacity_limit() {
        let mut w = ContainerWorld::new();
        for _ in 0..MAX_CONTAINERS {
            assert!(w.create_random(&[buf(0)]).is_some());
        }
        assert!(w.create_random(&[buf(0)]).is_none());
    }
}
