//! 単体 Limiter エフェクト (Phase 3-5)。
//!
//! `Compressor` が「ratio に応じた連続的な gain reduction」を作るのに対し、本 Limiter は
//! **ceiling を絶対に超えない brick-wall 動作** に特化する。アタックは瞬時 (lookahead 無し、
//! 検出した瞬間に gain を `ceiling / det` まで snap)、リリースは指数平滑で回復する。
//!
//! Unity AudioMixer の Limiter / FMOD `DSP_TYPE_LIMITER` 同等の用法を想定し、master や
//! 集約バスに 1 体載せて「他バスでどんなに信号が暴れても出力 ±ceiling に収まる」保証を作る。
//! Compressor を `ratio = ∞ / attack = 0 / knee = 0` で代用することも理屈上は可能だが、
//! 公式・既定値の意味付け (ceiling/release だけで完結) と、ホットループの式の単純さで
//! 別 Effect として分離する方が運用しやすい。
//!
//! ## アルゴリズム
//!
//! ```text
//! det      = max(|L|, |R|)                     // ステレオリンク (L/R 像保持)
//! target_g = if det > ceiling { ceiling / det } else { 1.0 }
//! gain    -= max(0, gain - target_g)            // attack: 瞬時 (target が下なら snap)
//! gain    += release_coeff * (target_g - gain)  // release: 指数平滑
//! out_L    = L * gain
//! out_R    = R * gain
//! ```
//!
//! `gain <= target_g <= ceiling / det` が保たれるため `|out| <= ceiling` を厳密に保証する
//! (master の `apply_soft_clip` と違って漸近ではなく**ハード**な上限)。
//!
//! `MAX_LIMITERS` 体ぶんの状態を初期化時に一括確保し、サウンドスレッドでは alloc しない。

use super::{EffectId, MAX_LIMITERS};

/// 1 体ぶんの状態 (AoS、Compressor と同形)。
pub struct LimiterState {
    pub effect_id: EffectId,

    // ── パラメータ ──
    pub ceiling_db: f32,
    pub release_ms: f32,

    // ── 係数キャッシュ (dirty で再計算) ──
    pub(super) ceiling_lin: f32,
    pub(super) release_coeff: f32,

    // ── エンベロープ (linear gain、上限 1.0) ──
    pub(super) gain: f32,

    pub(super) dirty: bool,
}

pub struct LimiterWorld {
    pub(super) states: Vec<LimiterState>,
}

impl Default for LimiterWorld {
    fn default() -> Self {
        Self::new()
    }
}

impl LimiterWorld {
    pub fn new() -> Self {
        Self {
            states: Vec::with_capacity(MAX_LIMITERS),
        }
    }

    pub fn len(&self) -> usize {
        self.states.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    /// 新規 Limiter を確保。容量超過時は `None`。
    /// デフォルト: ceiling = -0.3 dB / release = 50 ms。
    pub fn spawn(&mut self, effect_id: EffectId) -> Option<u32> {
        if self.states.len() >= MAX_LIMITERS {
            return None;
        }
        let dense = self.states.len() as u32;
        self.states.push(LimiterState {
            effect_id,
            ceiling_db: -0.3,
            release_ms: 50.0,
            ceiling_lin: db_to_linear(-0.3),
            release_coeff: 0.0,
            gain: 1.0,
            dirty: true,
        });
        Some(dense)
    }

    /// dense を swap-remove。LpfWorld と同じ戻り値規約。
    pub fn despawn(&mut self, dense: u32) -> Option<(EffectId, u32)> {
        let dense = dense as usize;
        if dense >= self.states.len() {
            return None;
        }
        let last = self.states.len() - 1;
        self.states.swap_remove(dense);
        if dense < last {
            let moved_id = self.states[dense].effect_id;
            Some((moved_id, dense as u32))
        } else {
            None
        }
    }

    pub fn set_ceiling_db(&mut self, dense: u32, value: f32) {
        if let Some(s) = self.states.get_mut(dense as usize) {
            s.ceiling_db = value;
            s.dirty = true;
        }
    }

    pub fn set_release_ms(&mut self, dense: u32, value: f32) {
        if let Some(s) = self.states.get_mut(dense as usize) {
            s.release_ms = value.max(0.0);
            s.dirty = true;
        }
    }

    /// dense index 直指定でパラメータ読み出し (Snapshot 補間用)。
    #[must_use]
    pub fn params_at(&self, dense: u32) -> Option<(f32, f32)> {
        let s = self.states.get(dense as usize)?;
        Some((s.ceiling_db, s.release_ms))
    }

    pub fn flush_dirty(&mut self, sample_rate: f32) {
        for s in self.states.iter_mut() {
            if s.dirty {
                s.ceiling_lin = db_to_linear(s.ceiling_db);
                s.release_coeff = compute_smoothing_coeff(s.release_ms, sample_rate);
                s.dirty = false;
            }
        }
    }

    /// 1 チェーンスロット分の信号を処理する (in-place)。
    ///
    /// `channels < 2` のときは何もしない (Compressor と同じ運用、Bus は常にステレオ)。
    pub fn apply(&mut self, dense: u32, buf: &mut [f32], channels: usize) {
        let i = dense as usize;
        if i >= self.states.len() || channels < 2 {
            return;
        }
        let s = &mut self.states[i];
        let frames = buf.len() / channels;
        let ceiling = s.ceiling_lin;
        let release = s.release_coeff;

        for n in 0..frames {
            let base = n * channels;
            let l = buf[base];
            let r = buf[base + 1];
            let det = l.abs().max(r.abs());

            let target = if det > ceiling && det > 0.0 {
                ceiling / det
            } else {
                1.0
            };

            // attack: 瞬時。target が現在より小さい (= さらに絞る必要がある) 場合は snap。
            if target < s.gain {
                s.gain = target;
            } else {
                // release: 指数平滑で 1.0 (= ceiling 以下) に向けて回復。
                s.gain += release * (target - s.gain);
            }

            buf[base] = l * s.gain;
            buf[base + 1] = r * s.gain;
            // 3ch 以降は同じ gain を適用 (5.1 等は将来対応)。
            for ch in 2..channels {
                buf[base + ch] *= s.gain;
            }
        }
    }
}

/// 一段平滑器の係数 (`1 - exp(-1 / (time_ms * 1e-3 * sample_rate))`)。
/// time_ms = 0 は瞬時応答 (係数 1.0)。
#[inline]
fn compute_smoothing_coeff(time_ms: f32, sample_rate: f32) -> f32 {
    if time_ms <= 0.0 || sample_rate <= 0.0 {
        return 1.0;
    }
    let tau_samples = time_ms * 0.001 * sample_rate;
    1.0 - (-1.0 / tau_samples).exp()
}

#[inline]
fn db_to_linear(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::EntityId;

    fn id(i: u32) -> EntityId {
        EntityId {
            index: i,
            generation: 0,
        }
    }

    #[test]
    fn spawn_and_despawn() {
        let mut w = LimiterWorld::new();
        let d = w.spawn(id(0)).unwrap();
        assert_eq!(w.len(), 1);
        let _ = w.despawn(d);
        assert_eq!(w.len(), 0);
    }

    #[test]
    fn capacity_limit() {
        let mut w = LimiterWorld::new();
        for k in 0..MAX_LIMITERS {
            assert!(w.spawn(id(k as u32)).is_some());
        }
        assert!(w.spawn(id(999)).is_none());
    }

    #[test]
    fn below_ceiling_passes_through() {
        let mut w = LimiterWorld::new();
        let d = w.spawn(id(0)).unwrap();
        w.set_ceiling_db(d, 0.0); // ceiling = 1.0
        w.flush_dirty(48000.0);

        // 信号は ±0.5 で ceiling 以下なので素通し (gain は 1.0 のまま)。
        let mut buf = vec![0.5_f32; 256];
        let original = buf.clone();
        w.apply(d, &mut buf, 2);
        for (a, b) in buf.iter().zip(original.iter()) {
            assert!((a - b).abs() < 1e-6, "expected pass-through: {a} vs {b}");
        }
    }

    #[test]
    fn brick_wall_caps_output_at_ceiling() {
        let mut w = LimiterWorld::new();
        let d = w.spawn(id(0)).unwrap();
        w.set_ceiling_db(d, 0.0); // ceiling = 1.0
        w.set_release_ms(d, 1000.0); // release を遅めにして release で漏れない確認
        w.flush_dirty(48000.0);

        // 振幅 2.0 の信号は ceiling = 1.0 を 2x オーバー。出力は ceiling 以下に張り付く。
        let mut buf = vec![2.0_f32; 256];
        w.apply(d, &mut buf, 2);
        for &s in buf.iter() {
            assert!(
                s.abs() <= 1.0 + 1e-5,
                "limiter must clamp |out| <= ceiling, got {s}"
            );
        }
        // 定常状態では出力は ceiling 近傍。
        let last = buf[buf.len() - 2].abs();
        assert!(
            (last - 1.0).abs() < 1e-3,
            "steady-state output should be ~ceiling, got {last}"
        );
    }

    #[test]
    fn negative_ceiling_db_caps_correctly() {
        let mut w = LimiterWorld::new();
        let d = w.spawn(id(0)).unwrap();
        w.set_ceiling_db(d, -6.02); // -6.02 dB ≈ 0.5
        w.set_release_ms(d, 1000.0);
        w.flush_dirty(48000.0);

        let mut buf = vec![1.0_f32; 256];
        w.apply(d, &mut buf, 2);
        for &s in buf.iter() {
            assert!(s.abs() <= 0.5 + 1e-3, "exceeded -6 dB ceiling: {s}");
        }
    }

    #[test]
    fn release_recovers_after_overshoot() {
        let mut w = LimiterWorld::new();
        let d = w.spawn(id(0)).unwrap();
        w.set_ceiling_db(d, 0.0);
        w.set_release_ms(d, 1.0); // 速いリリース
        w.flush_dirty(48000.0);

        // 一度 overshoot させて gain を下げる。
        let mut hot = vec![2.0_f32; 64];
        w.apply(d, &mut hot, 2);
        let gain_after_hot = w.states[d as usize].gain;
        assert!(
            gain_after_hot < 1.0,
            "gain should be reduced after overshoot, got {gain_after_hot}"
        );

        // 信号が ceiling 以下に戻れば release で gain が 1.0 に向けて回復する。
        let mut quiet = vec![0.1_f32; 4096];
        w.apply(d, &mut quiet, 2);
        let gain_after_quiet = w.states[d as usize].gain;
        assert!(
            gain_after_quiet > gain_after_hot,
            "gain should recover during quiet section: {gain_after_hot} -> {gain_after_quiet}"
        );
    }
}
