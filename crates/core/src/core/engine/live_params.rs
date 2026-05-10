//! 共有 SoA ライブパラメータ。
//!
//! メインスレッドのセッター（`set_source_volume` 等）は SPSC コマンドを介さず、
//! ここに直接 atomic store する。サウンドスレッドはオーディオコールバック冒頭で
//! 各アクティブソースのスロットを atomic load し、dense 配列に反映する。
//!
//! ## レイテンシ
//! 次のオーディオコールバックで反映される（典型 5〜10 ms）。
//! triple buffer による「フレーム末尾 publish」は経由しないため、
//! `poll_events()` 待ちの 1 フレーム遅延は発生しない。
//!
//! ## generation discrimination
//! 各スロットの値は `(generation: u32) << 32 | bits: u32` でパックする。
//! スロット再利用時に古い `EntityId` が誤適用されるのを防ぐ。
//! `EntityId.generation` と一致しないスロット読み取りは破棄される。

use std::sync::atomic::{AtomicU64, Ordering};

use crate::entity::EntityId;

/// パック表現。
#[inline]
fn pack_f32(generation: u32, value: f32) -> u64 {
    ((generation as u64) << 32) | value.to_bits() as u64
}

#[inline]
fn pack_u32(generation: u32, value: u32) -> u64 {
    ((generation as u64) << 32) | value as u64
}

#[inline]
fn unpack(packed: u64) -> (u32, u32) {
    ((packed >> 32) as u32, packed as u32)
}

/// ソースごとのライブパラメータ。各フィールドは独立 atomic スロット。
///
/// `Arc<Self>` でメインスレッド・サウンドスレッド間で共有する。
pub(crate) struct SourceLiveParams {
    volume: Box<[AtomicU64]>,
    pitch: Box<[AtomicU64]>,
    spatial_enabled: Box<[AtomicU64]>,
}

impl SourceLiveParams {
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::with_capacity(crate::source::DEFAULT_MAX_SOURCES)
    }

    pub(crate) fn with_capacity(max_sources: usize) -> Self {
        // generation = 0, value = 初期値（vol=1.0, pitch=1.0, spatial=0）。
        let volume = (0..max_sources)
            .map(|_| AtomicU64::new(pack_f32(0, 1.0)))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let pitch = (0..max_sources)
            .map(|_| AtomicU64::new(pack_f32(0, 1.0)))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let spatial_enabled = (0..max_sources)
            .map(|_| AtomicU64::new(pack_u32(0, 0)))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            volume,
            pitch,
            spatial_enabled,
        }
    }

    /// spawn 時にスロットを初期値で priming する。
    ///
    /// 3 本の `Box<[AtomicU64]>` の確保ヒープ実バイト数 (`memory_stats` walker 用)。
    pub(crate) fn memory_bytes(&self) -> usize {
        use crate::memory::boxed_slice_bytes;
        boxed_slice_bytes(&self.volume)
            + boxed_slice_bytes(&self.pitch)
            + boxed_slice_bytes(&self.spatial_enabled)
    }

    /// メインスレッド側の `play_with_handle*` で呼ぶ。`generation` が更新済みの
    /// `EntityId` をそのまま受け取り、スロットの古い値を上書きする。
    pub(crate) fn prime(&self, id: EntityId, vol: f32, pitch: f32) {
        let i = id.index as usize;
        if i >= self.volume.len() {
            return;
        }
        self.volume[i].store(pack_f32(id.generation, vol), Ordering::Relaxed);
        self.pitch[i].store(pack_f32(id.generation, pitch), Ordering::Relaxed);
        self.spatial_enabled[i].store(pack_u32(id.generation, 0), Ordering::Relaxed);
    }

    /// メインスレッド: volume を書き込む。
    pub(crate) fn store_volume(&self, id: EntityId, vol: f32) {
        let i = id.index as usize;
        if i >= self.volume.len() {
            return;
        }
        self.volume[i].store(pack_f32(id.generation, vol), Ordering::Relaxed);
    }

    /// メインスレッド: pitch を書き込む。
    pub(crate) fn store_pitch(&self, id: EntityId, pitch: f32) {
        let i = id.index as usize;
        if i >= self.pitch.len() {
            return;
        }
        self.pitch[i].store(pack_f32(id.generation, pitch), Ordering::Relaxed);
    }

    /// メインスレッド: spatial_enabled を書き込む。
    pub(crate) fn store_spatial_enabled(&self, id: EntityId, enabled: bool) {
        let i = id.index as usize;
        if i >= self.spatial_enabled.len() {
            return;
        }
        self.spatial_enabled[i].store(pack_u32(id.generation, enabled as u32), Ordering::Relaxed);
    }

    /// サウンドスレッド: 指定 `EntityId` の volume を取得（generation 一致時のみ `Some`）。
    pub(crate) fn load_volume(&self, id: EntityId) -> Option<f32> {
        let i = id.index as usize;
        if i >= self.volume.len() {
            return None;
        }
        let (slot_gen, bits) = unpack(self.volume[i].load(Ordering::Relaxed));
        if slot_gen == id.generation {
            Some(f32::from_bits(bits))
        } else {
            None
        }
    }

    pub(crate) fn load_pitch(&self, id: EntityId) -> Option<f32> {
        let i = id.index as usize;
        if i >= self.pitch.len() {
            return None;
        }
        let (slot_gen, bits) = unpack(self.pitch[i].load(Ordering::Relaxed));
        if slot_gen == id.generation {
            Some(f32::from_bits(bits))
        } else {
            None
        }
    }

    pub(crate) fn load_spatial_enabled(&self, id: EntityId) -> Option<bool> {
        let i = id.index as usize;
        if i >= self.spatial_enabled.len() {
            return None;
        }
        let (slot_gen, bits) = unpack(self.spatial_enabled[i].load(Ordering::Relaxed));
        if slot_gen == id.generation {
            Some(bits != 0)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_then_load_roundtrip() {
        let p = SourceLiveParams::new();
        let id = EntityId {
            index: 5,
            generation: 3,
        };
        p.prime(id, 0.5, 1.5);
        assert_eq!(p.load_volume(id), Some(0.5));
        assert_eq!(p.load_pitch(id), Some(1.5));
        assert_eq!(p.load_spatial_enabled(id), Some(false));

        p.store_volume(id, 0.25);
        p.store_spatial_enabled(id, true);
        assert_eq!(p.load_volume(id), Some(0.25));
        assert_eq!(p.load_spatial_enabled(id), Some(true));
    }

    #[test]
    fn stale_generation_returns_none() {
        let p = SourceLiveParams::new();
        let new_id = EntityId {
            index: 5,
            generation: 3,
        };
        let stale_id = EntityId {
            index: 5,
            generation: 2,
        };
        p.prime(new_id, 0.5, 1.5);
        assert_eq!(p.load_volume(stale_id), None);
        assert_eq!(p.load_volume(new_id), Some(0.5));
    }

    #[test]
    fn stale_store_does_not_affect_new_slot() {
        let p = SourceLiveParams::new();
        let new_id = EntityId {
            index: 5,
            generation: 3,
        };
        let stale_id = EntityId {
            index: 5,
            generation: 2,
        };
        p.prime(new_id, 0.5, 1.0);
        // 古い generation の store は新しい generation の load では拾われない
        p.store_volume(stale_id, 99.0);
        assert_eq!(p.load_volume(new_id), None);
        // ただし「generation が古いから捨てる」のは load 側の判断。
        // 新しい spawn で priming すれば回復する。
        p.prime(new_id, 0.5, 1.0);
        assert_eq!(p.load_volume(new_id), Some(0.5));
    }
}
