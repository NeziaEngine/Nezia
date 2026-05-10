//! メインスレッド側のソース状態スナップショットとキャッシュ。
//!
//! サウンドスレッドが各オーディオコールバック末尾で `SourceSnapshot` を triple buffer
//! 経由で publish し、メインスレッドが `poll_events()` で `SourceStateCache` に詰め替える。
//! `is_source_alive` / `source_position` / `batch_*` 系のクエリはこのキャッシュを線形
//! スキャンする。`SourceWorld` の所有自体はサウンドスレッド側に残し、クエリだけが
//! 同期される構造。

use crate::entity::EntityId;

/// メインスレッドからソースの生存・再生位置を確認するためのスナップショット。
///
/// triple buffer に乗せる側は AoS（フォーマットが固定で扱いやすい）、
/// メインスレッド側のクエリキャッシュは SoA（連続スキャンが速い）で持つ。
#[derive(Debug, Clone, Copy)]
pub(crate) struct SourceSnapshot {
    pub(crate) index: u32,
    pub(crate) generation: u32,
    pub(crate) sample_offset: f32,
}

/// メインスレッド側のクエリキャッシュ（SoA）。
///
/// `is_source_alive` / `source_position` の単発検索でも、`batch_*` の一括検索でも
/// 共通でこの構造を線形スキャンする。`indices` 配列だけ触れば generation 一致を
/// 確認するときまで他の配列にアクセスしないので、L1 効率が高い。
#[derive(Default)]
pub(crate) struct SourceStateCache {
    pub(crate) indices: Vec<u32>,
    pub(crate) generations: Vec<u32>,
    pub(crate) sample_offsets: Vec<f32>,
}

impl SourceStateCache {
    pub(crate) fn with_capacity(cap: usize) -> Self {
        Self {
            indices: Vec::with_capacity(cap),
            generations: Vec::with_capacity(cap),
            sample_offsets: Vec::with_capacity(cap),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.indices.clear();
        self.generations.clear();
        self.sample_offsets.clear();
    }

    /// 内部 `Vec` の確保ヒープ実バイト数 (`memory_stats` walker 用)。
    pub(crate) fn memory_bytes(&self) -> usize {
        use crate::memory::vec_cap_bytes;
        vec_cap_bytes(&self.indices)
            + vec_cap_bytes(&self.generations)
            + vec_cap_bytes(&self.sample_offsets)
    }

    pub(crate) fn refill_from(&mut self, snapshots: &[SourceSnapshot]) {
        self.clear();
        for s in snapshots {
            self.indices.push(s.index);
            self.generations.push(s.generation);
            self.sample_offsets.push(s.sample_offset);
        }
    }

    /// `id` の dense 位置を返す（generation も一致する場合のみ）。
    /// 未存在 / stale generation なら `None`。
    #[inline]
    pub(crate) fn find(&self, id: EntityId) -> Option<usize> {
        for (i, &idx) in self.indices.iter().enumerate() {
            if idx == id.index && self.generations[i] == id.generation {
                return Some(i);
            }
        }
        None
    }
}

pub(crate) type SourceSnapshotsIn = triple_buffer::Input<Vec<SourceSnapshot>>;
pub(crate) type SourceSnapshotsOut = triple_buffer::Output<Vec<SourceSnapshot>>;

/// ソーススナップショット用の triple buffer を初期化する。
///
/// 全 3 スロットに `MAX_SOURCES` ぶんの capacity を確保しておくことで、
/// サウンドスレッドの `clear + push` で再確保が起きないようにする。
/// 入力側は publish 直後に空 Vec で 1 回 publish しておき、初回 `update()` で
/// ダミー位置データが apply されるのを防ぐ。
pub(crate) fn build_source_snapshots_buffer(
    max_sources: usize,
) -> (SourceSnapshotsIn, SourceSnapshotsOut) {
    let initial: Vec<SourceSnapshot> = vec![
        SourceSnapshot {
            index: 0,
            generation: 0,
            sample_offset: 0.0
        };
        max_sources
    ];
    let (mut input, mut output) = triple_buffer::triple_buffer(&initial);
    input.input_buffer_mut().clear();
    input.publish();
    output.update();
    (input, output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(index: u32, generation: u32, sample_offset: f32) -> SourceSnapshot {
        SourceSnapshot {
            index,
            generation,
            sample_offset,
        }
    }

    #[test]
    fn cache_find_returns_none_when_empty() {
        let cache = SourceStateCache::default();
        assert!(
            cache
                .find(EntityId {
                    index: 0,
                    generation: 0
                })
                .is_none()
        );
    }

    #[test]
    fn cache_find_matches_index_and_generation() {
        let mut cache = SourceStateCache::with_capacity(4);
        cache.refill_from(&[snap(3, 7, 100.0), snap(5, 1, 200.0)]);
        assert_eq!(
            cache.find(EntityId {
                index: 3,
                generation: 7
            }),
            Some(0)
        );
        assert_eq!(
            cache.find(EntityId {
                index: 5,
                generation: 1
            }),
            Some(1)
        );
        assert_eq!(
            cache.find(EntityId {
                index: 3,
                generation: 8
            }),
            None
        );
        assert_eq!(
            cache.find(EntityId {
                index: 99,
                generation: 0
            }),
            None
        );
    }

    #[test]
    fn cache_refill_clears_old_entries() {
        let mut cache = SourceStateCache::with_capacity(4);
        cache.refill_from(&[snap(3, 7, 100.0)]);
        cache.refill_from(&[snap(5, 1, 200.0)]);
        assert!(
            cache
                .find(EntityId {
                    index: 3,
                    generation: 7
                })
                .is_none()
        );
        assert_eq!(
            cache.find(EntityId {
                index: 5,
                generation: 1
            }),
            Some(0)
        );
    }
}
