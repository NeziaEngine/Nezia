//! Custom Attenuation Curve (Phase 3-1)。
//!
//! 距離→ゲインの対応を **固定長 LUT** で表現する。Unity の `AnimationCurve` 相当を
//! NEZIA では `[f32; CURVE_SAMPLES]` の uniform sample テーブルで持ち、hot loop では
//! 線形補間 1 回でゲインを得る。
//!
//! ## レイアウト判断
//!
//! - **固定長 64 サンプル**: 1 カーブ = 256 byte (= 4 cache line)。可変長は hot loop の
//!   バウンドチェックを増やすので採用しない。N=64 は典型的なゲーム用途で十分な
//!   解像度 (`min..max` 距離範囲を 64 分割)。
//! - **共有レジストリ**: 1 カーブを複数ソースが参照する想定 (例: 「銃声の距離特性」を
//!   100 体の弾丸ソースで共有)。所有は `CurveRegistry` (メインスレッド) に集約し、
//!   サウンドスレッドは `Arc<ArcSwap<...>>` 経由で lock-free snapshot を読む。
//! - **新 ID 型 `AttenuationCurveId`**: `BufferId` と空間を分離 (異なる責務)。

use std::sync::Arc;

use arc_swap::ArcSwap;

/// カーブの LUT サンプル数 (固定)。
pub const CURVE_SAMPLES: usize = 64;

/// 同時に保持できる Custom Attenuation Curve の最大数。
pub const MAX_CURVES: usize = 256;

/// Custom Attenuation Curve のハンドル。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AttenuationCurveId {
    pub index: u32,
    pub generation: u32,
}

/// 距離→ゲインを uniform に sample した LUT。
///
/// `samples[0]` が `dist == min_distance` 相当のゲイン、
/// `samples[CURVE_SAMPLES - 1]` が `dist == max_distance` 相当のゲイン。
/// 値は `[0.0, ∞)` だが通常 `[0.0, 1.0]` を想定。
pub struct AttenuationCurve {
    pub samples: [f32; CURVE_SAMPLES],
}

impl AttenuationCurve {
    /// 任意のサンプル数の制御点を渡し、`CURVE_SAMPLES` 個の LUT に再サンプリングする。
    /// 制御点は `[0.0, 1.0]` の正規化距離に対応する uniform sample として扱う。
    /// `points` が空または 1 要素の場合、その値で全 LUT を埋める。
    pub fn from_points(points: &[f32]) -> Self {
        let mut samples = [0.0_f32; CURVE_SAMPLES];
        if points.is_empty() {
            return Self { samples };
        }
        if points.len() == 1 {
            samples.fill(points[0]);
            return Self { samples };
        }

        // 制御点を `points[i]` (i in 0..points.len()) として、
        // LUT の各 sample j (j in 0..CURVE_SAMPLES) を線形補間で埋める。
        let n_points_minus_1 = (points.len() - 1) as f32;
        let n_samples_minus_1 = (CURVE_SAMPLES - 1) as f32;
        for (j, slot) in samples.iter_mut().enumerate() {
            let t = j as f32 / n_samples_minus_1; // [0,1]
            let pos = t * n_points_minus_1;
            let i0 = (pos as usize).min(points.len() - 1);
            let i1 = (i0 + 1).min(points.len() - 1);
            let frac = pos - i0 as f32;
            *slot = points[i0] + (points[i1] - points[i0]) * frac;
        }
        Self { samples }
    }

    /// 正規化距離 `t ∈ [0.0, 1.0]` から線形補間でゲインをサンプリングする。
    /// 値域外は端点に clamp。
    #[inline]
    #[must_use]
    pub fn sample(&self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        let pos = t * (CURVE_SAMPLES - 1) as f32;
        let i0 = pos as usize;
        // i0 は最大で CURVE_SAMPLES - 1。i1 はそれを clamp。
        let i1 = (i0 + 1).min(CURVE_SAMPLES - 1);
        let frac = pos - i0 as f32;
        self.samples[i0] + (self.samples[i1] - self.samples[i0]) * frac
    }
}

/// Custom Attenuation Curve のレジストリ (メインスレッド側所有)。
///
/// `AudioBufferPool` と同パターン: スロット + generation で安定 ID を発行し、
/// `Arc<ArcSwap<Vec<Option<Arc<AttenuationCurve>>>>>` 経由でサウンドスレッドへ公開。
pub struct CurveRegistry {
    slots: Vec<CurveSlot>,
    curves: Vec<Option<Arc<AttenuationCurve>>>,
    shared: Arc<ArcSwap<Vec<Option<Arc<AttenuationCurve>>>>>,
    free_list: Vec<u32>,
    next_index: u32,
}

#[derive(Clone, Copy)]
struct CurveSlot {
    generation: u32,
    occupied: bool,
}

impl CurveRegistry {
    pub fn new(shared: Arc<ArcSwap<Vec<Option<Arc<AttenuationCurve>>>>>) -> Self {
        Self {
            slots: Vec::with_capacity(MAX_CURVES),
            curves: Vec::with_capacity(MAX_CURVES),
            shared,
            free_list: Vec::new(),
            next_index: 0,
        }
    }

    /// レジストリ全体のヒープ実バイト数 (`memory_stats` walker 用)。
    /// 登録済み `AttenuationCurve` は固定長サンプル配列 (`[f32; CURVE_SAMPLES]`) を持つので
    /// occupied slot 数 × `size_of::<AttenuationCurve>()` を加算する。
    pub(crate) fn memory_bytes(&self) -> usize {
        use crate::memory::vec_cap_bytes;
        let curve_size = std::mem::size_of::<AttenuationCurve>();
        let occupied = self.curves.iter().filter(|c| c.is_some()).count();
        vec_cap_bytes(&self.slots)
            + vec_cap_bytes(&self.curves)
            + vec_cap_bytes(&self.free_list)
            + occupied * curve_size
    }

    /// カーブを登録してハンドルを返す。`MAX_CURVES` 超過時は `None`。
    pub fn create(&mut self, curve: AttenuationCurve) -> Option<AttenuationCurveId> {
        if self.slots.len() >= MAX_CURVES && self.free_list.is_empty() {
            return None;
        }
        let (index, generation) = self.allocate_slot();
        self.curves[index as usize] = Some(Arc::new(curve));
        self.sync_shared();
        Some(AttenuationCurveId { index, generation })
    }

    /// カーブを削除する。再生中のソースが参照していた場合、当該ソースは
    /// 「Custom 指定だが curve_index 未解決」の状態になり、ゲイン 0 (silent fallback) になる。
    pub fn destroy(&mut self, id: AttenuationCurveId) -> bool {
        let Some(slot) = self.slots.get_mut(id.index as usize) else {
            return false;
        };
        if slot.generation != id.generation || !slot.occupied {
            return false;
        }
        self.curves[id.index as usize] = None;
        slot.generation += 1;
        slot.occupied = false;
        self.free_list.push(id.index);
        self.sync_shared();
        true
    }

    /// ハンドル検証。有効ならスロット index を返す。
    pub fn resolve(&self, id: AttenuationCurveId) -> Option<u32> {
        let slot = self.slots.get(id.index as usize)?;
        if slot.generation != id.generation || !slot.occupied {
            return None;
        }
        Some(id.index)
    }

    fn allocate_slot(&mut self) -> (u32, u32) {
        if let Some(index) = self.free_list.pop() {
            let generation = self.slots[index as usize].generation;
            self.slots[index as usize] = CurveSlot {
                generation,
                occupied: true,
            };
            (index, generation)
        } else {
            let index = self.next_index;
            self.next_index += 1;
            self.slots.push(CurveSlot {
                generation: 0,
                occupied: true,
            });
            self.curves.push(None);
            (index, 0)
        }
    }

    fn sync_shared(&self) {
        self.shared.store(Arc::new(self.curves.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn from_points_single_value_fills_lut() {
        let curve = AttenuationCurve::from_points(&[0.5]);
        for s in &curve.samples {
            assert!(approx(*s, 0.5));
        }
    }

    #[test]
    fn from_points_endpoints_match() {
        let curve = AttenuationCurve::from_points(&[1.0, 0.5, 0.0]);
        assert!(approx(curve.samples[0], 1.0));
        assert!(approx(curve.samples[CURVE_SAMPLES - 1], 0.0));
    }

    #[test]
    fn sample_t_zero_returns_first_sample() {
        let curve = AttenuationCurve::from_points(&[0.9, 0.1]);
        assert!(approx(curve.sample(0.0), 0.9));
    }

    #[test]
    fn sample_t_one_returns_last_sample() {
        let curve = AttenuationCurve::from_points(&[0.9, 0.1]);
        assert!(approx(curve.sample(1.0), 0.1));
    }

    #[test]
    fn sample_clamps_out_of_range() {
        let curve = AttenuationCurve::from_points(&[1.0, 0.0]);
        assert!(approx(curve.sample(-0.5), 1.0));
        assert!(approx(curve.sample(1.5), 0.0));
    }

    #[test]
    fn sample_lerp_midpoint() {
        // 線形 [1, 0] なら中点で 0.5
        let curve = AttenuationCurve::from_points(&[1.0, 0.0]);
        assert!((curve.sample(0.5) - 0.5).abs() < 0.05);
    }

    #[test]
    fn registry_create_and_resolve() {
        let shared = Arc::new(ArcSwap::from_pointee(Vec::new()));
        let mut reg = CurveRegistry::new(shared);
        let id = reg
            .create(AttenuationCurve::from_points(&[1.0, 0.0]))
            .unwrap();
        assert!(reg.resolve(id).is_some());
    }

    #[test]
    fn registry_destroy_invalidates_handle() {
        let shared = Arc::new(ArcSwap::from_pointee(Vec::new()));
        let mut reg = CurveRegistry::new(shared);
        let id = reg.create(AttenuationCurve::from_points(&[1.0])).unwrap();
        assert!(reg.destroy(id));
        assert!(reg.resolve(id).is_none());
        assert!(!reg.destroy(id));
    }

    #[test]
    fn registry_slot_reuse_bumps_generation() {
        let shared = Arc::new(ArcSwap::from_pointee(Vec::new()));
        let mut reg = CurveRegistry::new(shared);
        let id1 = reg.create(AttenuationCurve::from_points(&[1.0])).unwrap();
        reg.destroy(id1);
        let id2 = reg.create(AttenuationCurve::from_points(&[0.5])).unwrap();
        assert_eq!(id1.index, id2.index);
        assert_ne!(id1.generation, id2.generation);
        assert!(reg.resolve(id1).is_none(), "old id should not resolve");
        assert!(reg.resolve(id2).is_some());
    }
}
