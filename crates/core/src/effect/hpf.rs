use super::biquad::{BiquadCoeffs, BiquadState};
use super::{EffectId, MAX_HPF};

/// High-Pass Filter ワールド (Biquad 1 段、2ch 対応、細粒度 SoA)。
/// 構造は `LpfWorld` と対称。係数式のみ HPF (RBJ) を使用する。
pub struct HpfWorld {
    pub(super) effect_id_at_dense: Vec<EffectId>,

    pub(super) cutoff_hz: Vec<f32>,
    pub(super) q: Vec<f32>,
    pub(super) dirty: Vec<bool>,

    pub(super) coeffs: Vec<BiquadCoeffs>,
    pub(super) state_l: Vec<BiquadState>,
    pub(super) state_r: Vec<BiquadState>,
}

impl Default for HpfWorld {
    fn default() -> Self {
        Self::new()
    }
}

impl HpfWorld {
    pub fn new() -> Self {
        Self {
            effect_id_at_dense: Vec::with_capacity(MAX_HPF),
            cutoff_hz: Vec::with_capacity(MAX_HPF),
            q: Vec::with_capacity(MAX_HPF),
            dirty: Vec::with_capacity(MAX_HPF),
            coeffs: Vec::with_capacity(MAX_HPF),
            state_l: Vec::with_capacity(MAX_HPF),
            state_r: Vec::with_capacity(MAX_HPF),
        }
    }

    pub fn len(&self) -> usize {
        self.cutoff_hz.len()
    }

    pub fn spawn(&mut self, effect_id: EffectId, cutoff_hz: f32, q: f32) -> Option<u32> {
        if self.cutoff_hz.len() >= MAX_HPF {
            return None;
        }
        let dense = self.cutoff_hz.len() as u32;
        self.effect_id_at_dense.push(effect_id);
        self.cutoff_hz.push(cutoff_hz);
        self.q.push(q);
        self.dirty.push(true);
        self.coeffs.push(BiquadCoeffs::PASSTHROUGH);
        self.state_l.push(BiquadState::default());
        self.state_r.push(BiquadState::default());
        Some(dense)
    }

    pub fn despawn(&mut self, dense: u32) -> Option<(EffectId, u32)> {
        let dense = dense as usize;
        if dense >= self.cutoff_hz.len() {
            return None;
        }
        let last = self.cutoff_hz.len() - 1;
        self.effect_id_at_dense.swap_remove(dense);
        self.cutoff_hz.swap_remove(dense);
        self.q.swap_remove(dense);
        self.dirty.swap_remove(dense);
        self.coeffs.swap_remove(dense);
        self.state_l.swap_remove(dense);
        self.state_r.swap_remove(dense);
        if dense < last {
            let moved_id = self.effect_id_at_dense[dense];
            Some((moved_id, dense as u32))
        } else {
            None
        }
    }

    pub fn set_cutoff(&mut self, dense: u32, value: f32) {
        let i = dense as usize;
        if i < self.cutoff_hz.len() {
            self.cutoff_hz[i] = value;
            self.dirty[i] = true;
        }
    }

    pub fn set_q(&mut self, dense: u32, value: f32) {
        let i = dense as usize;
        if i < self.q.len() {
            self.q[i] = value;
            self.dirty[i] = true;
        }
    }

    /// cutoff slice 読み出し (Phase 3-2 Snapshot)。
    #[inline]
    #[must_use]
    pub fn cutoffs(&self) -> &[f32] {
        &self.cutoff_hz
    }

    /// Q slice 読み出し (Phase 3-2 Snapshot)。
    #[inline]
    #[must_use]
    pub fn qs(&self) -> &[f32] {
        &self.q
    }

    pub fn flush_dirty(&mut self, sample_rate: f32) {
        for i in 0..self.dirty.len() {
            if self.dirty[i] {
                self.coeffs[i] = BiquadCoeffs::hpf(self.cutoff_hz[i], self.q[i], sample_rate);
                self.dirty[i] = false;
            }
        }
    }

    pub fn apply(&mut self, dense: u32, buf: &mut [f32], channels: usize) {
        let i = dense as usize;
        if i >= self.coeffs.len() {
            return;
        }
        let c = self.coeffs[i];
        let frames = buf.len() / channels.max(1);
        match channels {
            1 => {
                let s = &mut self.state_l[i];
                for sample in buf.iter_mut().take(frames) {
                    *sample = s.process(*sample, &c);
                }
            }
            _ => {
                let sl = &mut self.state_l[i];
                let sr = &mut self.state_r[i];
                for n in 0..frames {
                    let base = n * channels;
                    let l = buf[base];
                    let r = buf[base + 1];
                    buf[base] = sl.process(l, &c);
                    buf[base + 1] = sr.process(r, &c);
                    for ch in 2..channels {
                        let v = buf[base + ch];
                        buf[base + ch] = sl.process(v, &c);
                    }
                }
            }
        }
    }
}
