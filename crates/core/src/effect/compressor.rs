//! Sidechain 対応コンプレッサー実装 (Phase 3-3 PR2)。
//!
//! ピーク検波 + log domain attack/release + soft knee + ステレオリンクの 1 段コンプ。
//! `use_sidechain == true` のときは外部 sidechain buffer (per-instance, interleaved stereo)
//! を検波器入力に使い、それ以外は自バス入力で内部検波する。
//!
//! ステレオリンクは `max(|L|, |R|)` を検波器入力に使い、L/R に同じ gain reduction を適用する。
//! L/R 像が崩れない (業界標準パターン)。
//!
//! `MAX_COMPRESSORS` 体ぶん (= 16) の状態と sidechain_buffer を初期化時に一括確保する。
//! 1 体あたり: パラメータ + envelope L/R + 係数キャッシュ + 32KB sidechain buffer。

use super::{EffectId, MAX_COMPRESSORS};
use crate::bus::MAX_MIX_BUFFER_SIZE;

/// 1 体ぶんの状態 (AoS)。パラメータと envelope/係数キャッシュを同居。
pub struct CompressorState {
    pub effect_id: EffectId,

    // ── パラメータ ──
    pub threshold_db: f32,
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub knee_db: f32,
    pub makeup_db: f32,

    /// 外部 sidechain 入力を使うか (`bind_compressor_sidechain` で切替)。
    pub use_sidechain: bool,

    // ── 係数キャッシュ (dirty で再計算) ──
    pub(super) attack_coeff: f32,
    pub(super) release_coeff: f32,

    // ── 検波器エンベロープ (linear amplitude) ──
    pub(super) envelope: f32,

    /// 係数キャッシュ再計算フラグ。
    pub(super) dirty: bool,
}

/// Compressor ワールド。
pub struct CompressorWorld {
    pub(super) states: Vec<CompressorState>,
    /// per-instance sidechain 入力バッファ (interleaved stereo)。
    /// レイアウト: `[dense * MAX_MIX_BUFFER_SIZE .. (dense+1) * MAX_MIX_BUFFER_SIZE]`
    /// Send writer が複数の場合は加算ミックスされる。callback 冒頭で `clear_sidechain_buffers` する。
    pub(super) sidechain_buffer: Vec<f32>,
}

impl Default for CompressorWorld {
    fn default() -> Self {
        Self::new()
    }
}

impl CompressorWorld {
    pub fn new() -> Self {
        Self {
            states: Vec::with_capacity(MAX_COMPRESSORS),
            sidechain_buffer: vec![0.0; MAX_COMPRESSORS * MAX_MIX_BUFFER_SIZE],
        }
    }

    pub fn len(&self) -> usize {
        self.states.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    /// 新規 Compressor を確保し、種別 World 内 dense index を返す。
    /// 容量超過時は `None`。
    pub fn spawn(&mut self, effect_id: EffectId) -> Option<u32> {
        if self.states.len() >= MAX_COMPRESSORS {
            return None;
        }
        let dense = self.states.len() as u32;
        self.states.push(CompressorState {
            effect_id,
            threshold_db: -20.0,
            ratio: 4.0,
            attack_ms: 5.0,
            release_ms: 200.0,
            knee_db: 6.0,
            makeup_db: 0.0,
            use_sidechain: false,
            attack_coeff: 0.0,
            release_coeff: 0.0,
            envelope: 0.0,
            dirty: true,
        });
        // 当該 sidechain_buffer 領域をクリア (前回利用時の残留サンプルを消す)。
        let start = dense as usize * MAX_MIX_BUFFER_SIZE;
        let end = start + MAX_MIX_BUFFER_SIZE;
        self.sidechain_buffer[start..end].fill(0.0);
        Some(dense)
    }

    /// dense を swap-remove。戻り値は `LpfWorld::despawn` と同義 (移動した state があれば返す)。
    pub fn despawn(&mut self, dense: u32) -> Option<(EffectId, u32)> {
        let dense = dense as usize;
        if dense >= self.states.len() {
            return None;
        }
        let last = self.states.len() - 1;
        if dense != last {
            // sidechain_buffer の末尾領域を dense 位置にコピー。
            let last_start = last * MAX_MIX_BUFFER_SIZE;
            let dense_start = dense * MAX_MIX_BUFFER_SIZE;
            self.sidechain_buffer
                .copy_within(last_start..last_start + MAX_MIX_BUFFER_SIZE, dense_start);
        }
        self.states.swap_remove(dense);
        if dense < last {
            let moved_id = self.states[dense].effect_id;
            Some((moved_id, dense as u32))
        } else {
            None
        }
    }

    pub fn set_threshold_db(&mut self, dense: u32, value: f32) {
        if let Some(s) = self.states.get_mut(dense as usize) {
            s.threshold_db = value;
        }
    }

    pub fn set_ratio(&mut self, dense: u32, value: f32) {
        if let Some(s) = self.states.get_mut(dense as usize) {
            s.ratio = value.max(1.0);
        }
    }

    pub fn set_attack_ms(&mut self, dense: u32, value: f32) {
        if let Some(s) = self.states.get_mut(dense as usize) {
            s.attack_ms = value.max(0.0);
            s.dirty = true;
        }
    }

    pub fn set_release_ms(&mut self, dense: u32, value: f32) {
        if let Some(s) = self.states.get_mut(dense as usize) {
            s.release_ms = value.max(0.0);
            s.dirty = true;
        }
    }

    pub fn set_knee_db(&mut self, dense: u32, value: f32) {
        if let Some(s) = self.states.get_mut(dense as usize) {
            s.knee_db = value.max(0.0);
        }
    }

    pub fn set_makeup_db(&mut self, dense: u32, value: f32) {
        if let Some(s) = self.states.get_mut(dense as usize) {
            s.makeup_db = value;
        }
    }

    pub fn set_use_sidechain(&mut self, dense: u32, value: bool) {
        if let Some(s) = self.states.get_mut(dense as usize) {
            s.use_sidechain = value;
        }
    }

    /// dense index 直指定でパラメータ読み出し (Snapshot 補間用)。
    #[must_use]
    pub fn params_at(&self, dense: u32) -> Option<(f32, f32, f32, f32, f32, f32)> {
        let s = self.states.get(dense as usize)?;
        Some((
            s.threshold_db,
            s.ratio,
            s.attack_ms,
            s.release_ms,
            s.knee_db,
            s.makeup_db,
        ))
    }

    /// flat な sidechain_buffer 全体への可変参照 (SourceMixingSystem の Send tap で raw ptr 化して使う)。
    /// レイアウトは `[dense * MAX_MIX_BUFFER_SIZE .. (dense+1) * MAX_MIX_BUFFER_SIZE]` 固定。
    #[inline]
    pub fn sidechain_buffer_mut(&mut self) -> &mut [f32] {
        &mut self.sidechain_buffer
    }

    /// dense index で sidechain 入力バッファのスライスを取得 (BusSystem の Send tap 書き込み用)。
    #[inline]
    pub fn sidechain_slice_mut(&mut self, dense: u32, sample_count: usize) -> Option<&mut [f32]> {
        if (dense as usize) >= self.states.len() {
            return None;
        }
        let start = dense as usize * MAX_MIX_BUFFER_SIZE;
        let end = start + sample_count.min(MAX_MIX_BUFFER_SIZE);
        Some(&mut self.sidechain_buffer[start..end])
    }

    /// 全 Compressor の sidechain_buffer を `sample_count` サンプルぶんゼロクリアする。
    /// オーディオコールバック冒頭で呼ぶ。
    pub fn clear_sidechain_buffers(&mut self, sample_count: usize) {
        let count = self.states.len();
        let clear_len = sample_count.min(MAX_MIX_BUFFER_SIZE);
        for d in 0..count {
            let start = d * MAX_MIX_BUFFER_SIZE;
            self.sidechain_buffer[start..start + clear_len].fill(0.0);
        }
    }

    /// dirty フラグが立っているエントリの係数を再計算する (callback 冒頭で呼ぶ)。
    pub fn flush_dirty(&mut self, sample_rate: f32) {
        for s in self.states.iter_mut() {
            if s.dirty {
                s.attack_coeff = compute_smoothing_coeff(s.attack_ms, sample_rate);
                s.release_coeff = compute_smoothing_coeff(s.release_ms, sample_rate);
                s.dirty = false;
            }
        }
    }

    /// 1 チェーンスロット分の信号を処理する (in-place)。
    ///
    /// `channels >= 2` のステレオを想定。L/R 別検波ではなくステレオリンク (`max(|L|, |R|)`) で
    /// 同一 gain reduction を適用する。
    pub fn apply(&mut self, dense: u32, buf: &mut [f32], channels: usize) {
        let i = dense as usize;
        if i >= self.states.len() || channels < 2 {
            return;
        }
        let s = &mut self.states[i];
        let frames = buf.len() / channels;
        let attack = s.attack_coeff;
        let release = s.release_coeff;
        let threshold_db = s.threshold_db;
        let ratio = s.ratio;
        let knee_db = s.knee_db;
        let makeup_db = s.makeup_db;
        let use_sc = s.use_sidechain;

        let sc_start = i * MAX_MIX_BUFFER_SIZE;

        for n in 0..frames {
            let base = n * channels;
            let l = buf[base];
            let r = buf[base + 1];

            // 検波器入力: sidechain か自バス信号か。
            let det = if use_sc {
                let sc_l = self.sidechain_buffer[sc_start + base];
                let sc_r = self.sidechain_buffer[sc_start + base + 1];
                sc_l.abs().max(sc_r.abs())
            } else {
                l.abs().max(r.abs())
            };

            // ピーク検波 + 一段平滑 (log domain は線形振幅で OK; 上昇 = attack、下降 = release)。
            let coeff = if det > s.envelope { attack } else { release };
            s.envelope += coeff * (det - s.envelope);

            // log 振幅。
            let env_db = 20.0 * s.envelope.max(1e-7).log10();
            let over = env_db - threshold_db;

            // soft knee 付き gain reduction (dB)。
            let gr_db = if knee_db > 0.0 && over > -knee_db * 0.5 && over < knee_db * 0.5 {
                let x = over + knee_db * 0.5;
                -(1.0 - 1.0 / ratio) * x * x / (2.0 * knee_db)
            } else if over >= knee_db * 0.5 {
                -(1.0 - 1.0 / ratio) * over
            } else {
                0.0
            };

            // makeup ゲイン込みで線形ゲインに戻す。
            let gain_lin = db_to_linear(gr_db + makeup_db);

            buf[base] = l * gain_lin;
            buf[base + 1] = r * gain_lin;
            // 3ch 以降はそのまま (Phase 3-3 では surround 非対応)。
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
        let mut w = CompressorWorld::new();
        let d = w.spawn(id(0)).unwrap();
        assert_eq!(w.len(), 1);
        let _ = w.despawn(d);
        assert_eq!(w.len(), 0);
    }

    #[test]
    fn capacity_limit() {
        let mut w = CompressorWorld::new();
        for k in 0..MAX_COMPRESSORS {
            assert!(w.spawn(id(k as u32)).is_some());
        }
        assert!(w.spawn(id(999)).is_none());
    }

    #[test]
    fn below_threshold_passes_through() {
        // 信号 < threshold なら gain reduction なし (= 元信号と一致)。
        let mut w = CompressorWorld::new();
        let d = w.spawn(id(0)).unwrap();
        w.set_threshold_db(d, -10.0);
        w.set_attack_ms(d, 0.0);
        w.set_release_ms(d, 0.0);
        w.set_knee_db(d, 0.0);
        w.flush_dirty(48000.0);

        // 振幅 0.1 (= -20dB) は -10dB threshold より下。
        let mut buf = vec![0.1_f32; 256];
        let original = buf.clone();
        w.apply(d, &mut buf, 2);
        for (a, b) in buf.iter().zip(original.iter()) {
            assert!(
                (a - b).abs() < 1e-4,
                "expected pass-through, got {a} vs {b}"
            );
        }
    }

    #[test]
    fn above_threshold_reduces_gain() {
        // 信号 > threshold なら gain reduction が乗る。
        let mut w = CompressorWorld::new();
        let d = w.spawn(id(0)).unwrap();
        w.set_threshold_db(d, -20.0);
        w.set_ratio(d, 4.0);
        w.set_attack_ms(d, 0.0);
        w.set_release_ms(d, 1000.0);
        w.set_knee_db(d, 0.0);
        w.flush_dirty(48000.0);

        // 振幅 1.0 (= 0 dB) は -20dB threshold を 20dB 超える。
        // ratio=4 で reduction = 20 * (1 - 1/4) = 15dB → 線形 ≈ 0.178
        let mut buf = vec![1.0_f32; 256];
        w.apply(d, &mut buf, 2);
        // attack=0 なので最初のサンプルから即時適用。
        let last = buf[buf.len() - 2]; // L サンプル
        assert!(
            last > 0.1 && last < 0.3,
            "expected ~0.178 after reduction, got {last}"
        );
    }

    #[test]
    fn sidechain_drives_reduction_when_bound() {
        // 自バスは無音、sidechain_buffer に信号 → use_sidechain=true で検波器が反応する。
        let mut w = CompressorWorld::new();
        let d = w.spawn(id(0)).unwrap();
        w.set_threshold_db(d, -20.0);
        w.set_ratio(d, 4.0);
        w.set_attack_ms(d, 0.0);
        w.set_release_ms(d, 1000.0);
        w.set_knee_db(d, 0.0);
        w.set_use_sidechain(d, true);
        w.flush_dirty(48000.0);

        // 自バス信号は 1.0 だが sidechain_buffer も 1.0 にする (検波器は sidechain を見る)。
        let sc = w.sidechain_slice_mut(d, 256).unwrap();
        for s in sc.iter_mut() {
            *s = 1.0;
        }

        let mut buf = vec![1.0_f32; 256];
        w.apply(d, &mut buf, 2);
        // sidechain を見て reduction が乗っているはず。
        let last = buf[buf.len() - 2];
        assert!(last < 0.5, "sidechain should drive reduction; got {last}");
    }

    #[test]
    fn clear_sidechain_buffers_zeros_used_slots() {
        let mut w = CompressorWorld::new();
        let d = w.spawn(id(0)).unwrap();
        let sc = w.sidechain_slice_mut(d, 16).unwrap();
        for s in sc.iter_mut() {
            *s = 0.7;
        }
        w.clear_sidechain_buffers(16);
        let sc = w.sidechain_slice_mut(d, 16).unwrap();
        for &s in sc.iter() {
            assert_eq!(s, 0.0);
        }
    }
}
