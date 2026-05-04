use super::biquad::{BiquadCoeffs, BiquadState};
use super::{EffectId, MAX_LPF};

/// Low-Pass Filter ワールド (Biquad 1 段、2ch 対応、細粒度 SoA)。
///
/// パラメータ更新時は `dirty[i] = true` を立て、サウンドスレッドが次回コールバック冒頭で
/// 係数を再計算する (詳細は `docs/design/core/dsp.md` 参照)。
pub struct LpfWorld {
    /// 種別 World → メタ層への逆引き (despawn 再マップ用)。
    pub(super) effect_id_at_dense: Vec<EffectId>,

    pub(super) cutoff_hz: Vec<f32>,
    pub(super) q: Vec<f32>,
    pub(super) dirty: Vec<bool>,

    pub(super) coeffs: Vec<BiquadCoeffs>,
    pub(super) state_l: Vec<BiquadState>,
    pub(super) state_r: Vec<BiquadState>,
}

impl Default for LpfWorld {
    fn default() -> Self {
        Self::new()
    }
}

impl LpfWorld {
    pub fn new() -> Self {
        Self {
            effect_id_at_dense: Vec::with_capacity(MAX_LPF),
            cutoff_hz: Vec::with_capacity(MAX_LPF),
            q: Vec::with_capacity(MAX_LPF),
            dirty: Vec::with_capacity(MAX_LPF),
            coeffs: Vec::with_capacity(MAX_LPF),
            state_l: Vec::with_capacity(MAX_LPF),
            state_r: Vec::with_capacity(MAX_LPF),
        }
    }

    pub fn len(&self) -> usize {
        self.cutoff_hz.len()
    }

    /// 新規 LPF を追加し、種別 World 内の dense index を返す。
    /// `MAX_LPF` 到達時は `None`。
    pub fn spawn(&mut self, effect_id: EffectId, cutoff_hz: f32, q: f32) -> Option<u32> {
        if self.cutoff_hz.len() >= MAX_LPF {
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

    /// dense を swap-remove。
    /// 戻り値: `Some((moved_effect_id, moved_new_dense))` — 末尾要素が dense へ移動した場合、
    /// その元 effect_id と新 dense index。`None` なら末尾削除でメタ層側の再マップ不要。
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
            // 末尾要素が dense に移動した。
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

    /// dirty フラグが立っているエントリの係数を再計算する (コールバック冒頭で呼ぶ)。
    pub fn flush_dirty(&mut self, sample_rate: f32) {
        for i in 0..self.dirty.len() {
            if self.dirty[i] {
                self.coeffs[i] = BiquadCoeffs::lpf(self.cutoff_hz[i], self.q[i], sample_rate);
                self.dirty[i] = false;
            }
        }
    }

    /// in-place で 1 チェーンスロット分の信号を処理する。
    ///
    /// `channels = 1` ならモノラル (state_l のみ)、`>= 2` なら L/R 別フィルタ。
    /// 第 3 チャネル以降はモノラル LPF でラウンド。
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
                    // 3ch 以降は左 state を使い回し (5.1 等は将来対応)。
                    for ch in 2..channels {
                        let v = buf[base + ch];
                        buf[base + ch] = sl.process(v, &c);
                    }
                }
            }
        }
    }
}
