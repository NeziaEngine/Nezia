use super::biquad::{BiquadCoeffs, BiquadState};
use super::{EffectId, MAX_PEQ};

/// Parametric / Peaking EQ ワールド (Biquad 1 段、2ch 対応、細粒度 SoA)。
///
/// LPF/HPF と同じく `dirty[i] = true` で次回コールバック冒頭の `flush_dirty` 時に
/// 係数を再計算する。1 エフェクト = 1 ピーキングバンドであり、複数バンド EQ は
/// 同一バスに複数 PeakingEq を chain することで構成する (Unity ParameterEQ と同設計)。
///
/// 設計詳細: [docs/design/core/dsp.md](../../../docs/design/core/dsp.md) を参照。
pub struct PeakingEqWorld {
    /// 種別 World → メタ層への逆引き (despawn 再マップ用)。
    pub(super) effect_id_at_dense: Vec<EffectId>,

    pub(super) center_hz: Vec<f32>,
    pub(super) q: Vec<f32>,
    pub(super) gain_db: Vec<f32>,
    pub(super) dirty: Vec<bool>,

    pub(super) coeffs: Vec<BiquadCoeffs>,
    pub(super) state_l: Vec<BiquadState>,
    pub(super) state_r: Vec<BiquadState>,
}

impl Default for PeakingEqWorld {
    fn default() -> Self {
        Self::new()
    }
}

impl PeakingEqWorld {
    pub fn new() -> Self {
        Self {
            effect_id_at_dense: Vec::with_capacity(MAX_PEQ),
            center_hz: Vec::with_capacity(MAX_PEQ),
            q: Vec::with_capacity(MAX_PEQ),
            gain_db: Vec::with_capacity(MAX_PEQ),
            dirty: Vec::with_capacity(MAX_PEQ),
            coeffs: Vec::with_capacity(MAX_PEQ),
            state_l: Vec::with_capacity(MAX_PEQ),
            state_r: Vec::with_capacity(MAX_PEQ),
        }
    }

    pub fn len(&self) -> usize {
        self.center_hz.len()
    }

    pub(crate) fn memory_bytes(&self) -> usize {
        use crate::memory::vec_cap_bytes;
        vec_cap_bytes(&self.effect_id_at_dense)
            + vec_cap_bytes(&self.center_hz)
            + vec_cap_bytes(&self.q)
            + vec_cap_bytes(&self.gain_db)
            + vec_cap_bytes(&self.dirty)
            + vec_cap_bytes(&self.coeffs)
            + vec_cap_bytes(&self.state_l)
            + vec_cap_bytes(&self.state_r)
    }

    /// 新規 PeakingEq を追加。`MAX_PEQ` 到達時は `None`。
    /// デフォルトは center=1kHz / Q=1.0 / gain_db=0 で素通し。
    pub fn spawn(
        &mut self,
        effect_id: EffectId,
        center_hz: f32,
        q: f32,
        gain_db: f32,
    ) -> Option<u32> {
        if self.center_hz.len() >= MAX_PEQ {
            return None;
        }
        let dense = self.center_hz.len() as u32;
        self.effect_id_at_dense.push(effect_id);
        self.center_hz.push(center_hz);
        self.q.push(q);
        self.gain_db.push(gain_db);
        self.dirty.push(true);
        self.coeffs.push(BiquadCoeffs::PASSTHROUGH);
        self.state_l.push(BiquadState::default());
        self.state_r.push(BiquadState::default());
        Some(dense)
    }

    /// dense を swap-remove。
    /// 戻り値: `Some((moved_effect_id, moved_new_dense))` — 末尾要素が dense へ移動した場合、
    /// その元 effect_id と新 dense index。`None` なら末尾削除でメタ層側の再マップ不要。
    pub fn despawn(&mut self, dense: u32) -> Option<(EffectId, u32)> {
        let dense = dense as usize;
        if dense >= self.center_hz.len() {
            return None;
        }
        let last = self.center_hz.len() - 1;
        self.effect_id_at_dense.swap_remove(dense);
        self.center_hz.swap_remove(dense);
        self.q.swap_remove(dense);
        self.gain_db.swap_remove(dense);
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

    pub fn set_center_hz(&mut self, dense: u32, value: f32) {
        let i = dense as usize;
        if i < self.center_hz.len() {
            self.center_hz[i] = value;
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

    pub fn set_gain_db(&mut self, dense: u32, value: f32) {
        let i = dense as usize;
        if i < self.gain_db.len() {
            self.gain_db[i] = value;
            self.dirty[i] = true;
        }
    }

    /// center_hz slice 読み出し (Snapshot 経路)。
    #[inline]
    #[must_use]
    pub fn center_hzs(&self) -> &[f32] {
        &self.center_hz
    }

    /// Q slice 読み出し (Snapshot 経路)。
    #[inline]
    #[must_use]
    pub fn qs(&self) -> &[f32] {
        &self.q
    }

    /// gain_db slice 読み出し (Snapshot 経路)。
    #[inline]
    #[must_use]
    pub fn gain_dbs(&self) -> &[f32] {
        &self.gain_db
    }

    /// dirty フラグが立っているエントリの係数を再計算する。
    pub fn flush_dirty(&mut self, sample_rate: f32) {
        for i in 0..self.dirty.len() {
            if self.dirty[i] {
                self.coeffs[i] = BiquadCoeffs::peaking_eq(
                    self.center_hz[i],
                    self.q[i],
                    self.gain_db[i],
                    sample_rate,
                );
                self.dirty[i] = false;
            }
        }
    }

    /// in-place で 1 チェーンスロット分の信号を処理する。
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
