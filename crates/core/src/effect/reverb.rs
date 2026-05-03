//! Freeverb 系の縮小版 Reverb 実装。
//!
//! 8 comb filter + 4 allpass filter の直列構成 (Schroeder/Freeverb)。
//! 各 Reverb インスタンスは L/R それぞれに独立した遅延ラインを持つ。
//!
//! メモリ: 1 体 ~64KB (44.1kHz 想定、L+R 合算)
//!   - comb: 8 × 平均 1500 frame × 4 byte × 2ch ≈ 96 KB → 実際は短い lengths で ~50KB
//!   - allpass: 4 × 平均 400 frame × 4 byte × 2ch ≈ 12 KB
//!
//! 合計 `MAX_REVERBS = 16` 体で約 1 MB を事前確保 (sound thread alloc 0)。

use super::{EffectId, MAX_REVERBS};

/// comb filter 数 (Freeverb 既定)。
const N_COMBS: usize = 8;
/// allpass filter 数 (Freeverb 既定)。
const N_ALLPASS: usize = 4;
/// 44.1 kHz における Freeverb 既定の comb 遅延長 (frames)。
const COMB_TUNINGS_44K: [usize; N_COMBS] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
/// 同 allpass 遅延長 (frames)。
const ALLPASS_TUNINGS_44K: [usize; N_ALLPASS] = [556, 441, 341, 225];
/// stereo spread (R 側を遅らせる量、frames)。
const STEREO_SPREAD: usize = 23;

/// allpass 内部 feedback (Freeverb 既定)。
const ALLPASS_FEEDBACK: f32 = 0.5;

/// 1 reverb インスタンスあたりの最大遅延サンプル合計 (44.1kHz 基準で見積もり、Vec 容量計算用)。
fn delay_total_44k() -> usize {
    let comb_l: usize = COMB_TUNINGS_44K.iter().sum();
    let comb_r: usize = COMB_TUNINGS_44K.iter().map(|n| n + STEREO_SPREAD).sum();
    let ap_l: usize = ALLPASS_TUNINGS_44K.iter().sum();
    let ap_r: usize = ALLPASS_TUNINGS_44K.iter().map(|n| n + STEREO_SPREAD).sum();
    comb_l + comb_r + ap_l + ap_r
}

/// 1 reverb インスタンスの状態 (AoS)。
///
/// 設計判断 (docs/design/core/dsp.md): Reverb は遅延ラインへの読み書きが支配的で
/// SoA 細粒度化の恩恵が薄いため、フィールドを `ReverbState` に同居させた AoS で持つ。
pub struct ReverbState {
    pub effect_id: EffectId,

    // ── パラメータ (正規化値) ──
    pub room_size: f32, // [0.0, 1.0] → comb feedback (0.0..0.98)
    pub damping: f32,   // [0.0, 1.0] → comb damp filter cutoff
    pub wet: f32,       // [0.0, 1.0]
    pub dry: f32,       // [0.0, 1.0]
    pub width: f32,     // [0.0, 1.0] (L/R クロスフィード強度)

    // ── 派生係数 (パラメータ変更時に再計算) ──
    pub(super) comb_feedback: f32,
    pub(super) comb_damp: f32,

    // ── 遅延ラインプール内のオフセット (L/R 別) ──
    /// 各 comb の delay_pool 上での開始 offset と長さ (L)。
    pub(super) comb_offsets_l: [u32; N_COMBS],
    pub(super) comb_offsets_r: [u32; N_COMBS],
    pub(super) comb_lens: [u32; N_COMBS],
    pub(super) comb_lens_r: [u32; N_COMBS],
    /// 各 allpass の delay_pool 上での開始 offset と長さ (L)。
    pub(super) allpass_offsets_l: [u32; N_ALLPASS],
    pub(super) allpass_offsets_r: [u32; N_ALLPASS],
    pub(super) allpass_lens: [u32; N_ALLPASS],
    pub(super) allpass_lens_r: [u32; N_ALLPASS],

    // ── リングポインタ (フレームごとに進む) ──
    pub(super) comb_pos_l: [u32; N_COMBS],
    pub(super) comb_pos_r: [u32; N_COMBS],
    pub(super) allpass_pos_l: [u32; N_ALLPASS],
    pub(super) allpass_pos_r: [u32; N_ALLPASS],

    // ── damping LPF 状態 (各 comb ごと) ──
    pub(super) comb_filter_store_l: [f32; N_COMBS],
    pub(super) comb_filter_store_r: [f32; N_COMBS],

    /// dirty フラグ。`set_*` で立て、`flush_dirty` で派生係数を再計算する。
    pub(super) dirty: bool,
}

/// Reverb ワールド。`MAX_REVERBS` 体ぶんの状態と遅延ラインプールを保持する。
pub struct ReverbWorld {
    pub(super) states: Vec<ReverbState>,
    /// フラット遅延ラインプール。各 ReverbState のオフセットから参照される。
    /// `MAX_REVERBS × delay_total_44k()` を初期化時に一括確保。
    pub(super) delay_pool: Vec<f32>,
    /// 1 reverb あたりの遅延ライン総サンプル数 (delay_pool 上のストライド)。
    delay_stride: u32,
}

impl Default for ReverbWorld {
    fn default() -> Self {
        Self::new()
    }
}

impl ReverbWorld {
    pub fn new() -> Self {
        let stride = delay_total_44k();
        Self {
            states: Vec::with_capacity(MAX_REVERBS),
            delay_pool: vec![0.0; MAX_REVERBS * stride],
            delay_stride: stride as u32,
        }
    }

    pub fn len(&self) -> usize {
        self.states.len()
    }

    /// 新規 Reverb を確保し、種別 World 内 dense index を返す。`MAX_REVERBS` 到達時は `None`。
    pub fn spawn(&mut self, effect_id: EffectId) -> Option<u32> {
        if self.states.len() >= MAX_REVERBS {
            return None;
        }
        let dense = self.states.len() as u32;

        // 遅延プール上のオフセット計算。
        // インスタンス n の領域は `[n * stride, (n+1) * stride)`。
        let base = dense * self.delay_stride;
        let mut cursor = base;
        let mut comb_offsets_l = [0u32; N_COMBS];
        let mut comb_offsets_r = [0u32; N_COMBS];
        let mut comb_lens = [0u32; N_COMBS];
        let mut comb_lens_r = [0u32; N_COMBS];
        for i in 0..N_COMBS {
            let len_l = COMB_TUNINGS_44K[i] as u32;
            let len_r = (COMB_TUNINGS_44K[i] + STEREO_SPREAD) as u32;
            comb_offsets_l[i] = cursor;
            comb_lens[i] = len_l;
            cursor += len_l;
            comb_offsets_r[i] = cursor;
            comb_lens_r[i] = len_r;
            cursor += len_r;
        }
        let mut allpass_offsets_l = [0u32; N_ALLPASS];
        let mut allpass_offsets_r = [0u32; N_ALLPASS];
        let mut allpass_lens = [0u32; N_ALLPASS];
        let mut allpass_lens_r = [0u32; N_ALLPASS];
        for i in 0..N_ALLPASS {
            let len_l = ALLPASS_TUNINGS_44K[i] as u32;
            let len_r = (ALLPASS_TUNINGS_44K[i] + STEREO_SPREAD) as u32;
            allpass_offsets_l[i] = cursor;
            allpass_lens[i] = len_l;
            cursor += len_l;
            allpass_offsets_r[i] = cursor;
            allpass_lens_r[i] = len_r;
            cursor += len_r;
        }
        debug_assert!(cursor <= base + self.delay_stride);

        // 該当領域をクリア (前回利用時の残留サンプルを消す)。
        let pool_start = base as usize;
        let pool_end = (base + self.delay_stride) as usize;
        self.delay_pool[pool_start..pool_end].fill(0.0);

        self.states.push(ReverbState {
            effect_id,
            room_size: 0.5,
            damping: 0.5,
            wet: 0.33,
            dry: 0.7,
            width: 1.0,
            comb_feedback: 0.0, // dirty=true で次回 flush で計算
            comb_damp: 0.0,
            comb_offsets_l,
            comb_offsets_r,
            comb_lens,
            comb_lens_r,
            allpass_offsets_l,
            allpass_offsets_r,
            allpass_lens,
            allpass_lens_r,
            comb_pos_l: [0; N_COMBS],
            comb_pos_r: [0; N_COMBS],
            allpass_pos_l: [0; N_ALLPASS],
            allpass_pos_r: [0; N_ALLPASS],
            comb_filter_store_l: [0.0; N_COMBS],
            comb_filter_store_r: [0.0; N_COMBS],
            dirty: true,
        });
        Some(dense)
    }

    /// dense を swap-remove。戻り値は `LpfWorld::despawn` と同義。
    pub fn despawn(&mut self, dense: u32) -> Option<(EffectId, u32)> {
        let dense = dense as usize;
        if dense >= self.states.len() {
            return None;
        }
        let last = self.states.len() - 1;
        // 遅延プールも swap-remove する必要がある: 末尾領域を dense 位置にコピー、その後縮める。
        // ReverbState の各オフセットは「先頭からの絶対 offset」で持っているため、
        // 移動後の状態に合わせてオフセットを再ベースする。
        if dense != last {
            let last_base = last as u32 * self.delay_stride;
            let dense_base = dense as u32 * self.delay_stride;
            // メモリコピー (alloc なし)
            let stride = self.delay_stride as usize;
            self.delay_pool.copy_within(
                last_base as usize..last_base as usize + stride,
                dense_base as usize,
            );
            // 末尾 state のオフセットを dense_base 基準に張り替える
            let delta = last_base - dense_base; // 末尾 → dense へ移動した分のシフト幅
            let last_state = self.states.last_mut().unwrap();
            for o in last_state.comb_offsets_l.iter_mut() {
                *o -= delta;
            }
            for o in last_state.comb_offsets_r.iter_mut() {
                *o -= delta;
            }
            for o in last_state.allpass_offsets_l.iter_mut() {
                *o -= delta;
            }
            for o in last_state.allpass_offsets_r.iter_mut() {
                *o -= delta;
            }
        }
        self.states.swap_remove(dense);
        if dense < last {
            let moved_id = self.states[dense].effect_id;
            Some((moved_id, dense as u32))
        } else {
            None
        }
    }

    pub fn set_room_size(&mut self, dense: u32, value: f32) {
        let i = dense as usize;
        if i < self.states.len() {
            self.states[i].room_size = value.clamp(0.0, 1.0);
            self.states[i].dirty = true;
        }
    }

    pub fn set_damping(&mut self, dense: u32, value: f32) {
        let i = dense as usize;
        if i < self.states.len() {
            self.states[i].damping = value.clamp(0.0, 1.0);
            self.states[i].dirty = true;
        }
    }

    pub fn set_wet(&mut self, dense: u32, value: f32) {
        let i = dense as usize;
        if i < self.states.len() {
            self.states[i].wet = value.clamp(0.0, 1.0);
        }
    }

    pub fn set_dry(&mut self, dense: u32, value: f32) {
        let i = dense as usize;
        if i < self.states.len() {
            self.states[i].dry = value.clamp(0.0, 1.0);
        }
    }

    pub fn set_width(&mut self, dense: u32, value: f32) {
        let i = dense as usize;
        if i < self.states.len() {
            self.states[i].width = value.clamp(0.0, 1.0);
        }
    }

    /// dirty フラグを処理して派生係数を再計算する (コールバック冒頭で呼ぶ)。
    pub fn flush_dirty(&mut self) {
        for s in self.states.iter_mut() {
            if s.dirty {
                // Freeverb scaling
                s.comb_feedback = s.room_size * 0.28 + 0.7;
                s.comb_damp = s.damping * 0.4;
                s.dirty = false;
            }
        }
    }

    /// 1 チェーンスロット分の信号を処理する (in-place)。
    ///
    /// `channels >= 2` を想定。Bus 専用 (Phase 2-3) のためモノラル経路は実装しない。
    /// `dry` を残しつつ `wet` をブレンドする (in-place で wet/dry mix)。
    pub fn apply(&mut self, dense: u32, buf: &mut [f32], channels: usize) {
        let i = dense as usize;
        if i >= self.states.len() || channels < 2 {
            return;
        }
        let s = &mut self.states[i];
        let frames = buf.len() / channels;
        let wet1 = s.wet * (s.width * 0.5 + 0.5);
        let wet2 = s.wet * ((1.0 - s.width) * 0.5);
        let dry = s.dry;
        let feedback = s.comb_feedback;
        let damp1 = s.comb_damp;
        let damp2 = 1.0 - damp1;

        for n in 0..frames {
            let base = n * channels;
            let in_l = buf[base];
            let in_r = buf[base + 1];
            // Stereo Freeverb 入力ゲイン (Freeverb 既定 0.015)
            let input = (in_l + in_r) * 0.015;

            // 8 並列 comb の出力を加算する (L/R 独立)。
            let mut out_l = 0.0_f32;
            let mut out_r = 0.0_f32;
            for k in 0..N_COMBS {
                // L
                let len_l = s.comb_lens[k];
                let off_l = s.comb_offsets_l[k];
                let pos_l = s.comb_pos_l[k];
                let buf_idx_l = (off_l + pos_l) as usize;
                let y_l = Self::pool_at(&self.delay_pool, buf_idx_l);
                // damping LPF
                s.comb_filter_store_l[k] = y_l * damp2 + s.comb_filter_store_l[k] * damp1;
                let new_l = input + s.comb_filter_store_l[k] * feedback;
                Self::pool_set(&mut self.delay_pool, buf_idx_l, new_l);
                let next_pos_l = pos_l + 1;
                s.comb_pos_l[k] = if next_pos_l >= len_l { 0 } else { next_pos_l };
                out_l += y_l;

                // R
                let len_r = s.comb_lens_r[k];
                let off_r = s.comb_offsets_r[k];
                let pos_r = s.comb_pos_r[k];
                let buf_idx_r = (off_r + pos_r) as usize;
                let y_r = Self::pool_at(&self.delay_pool, buf_idx_r);
                s.comb_filter_store_r[k] = y_r * damp2 + s.comb_filter_store_r[k] * damp1;
                let new_r = input + s.comb_filter_store_r[k] * feedback;
                Self::pool_set(&mut self.delay_pool, buf_idx_r, new_r);
                let next_pos_r = pos_r + 1;
                s.comb_pos_r[k] = if next_pos_r >= len_r { 0 } else { next_pos_r };
                out_r += y_r;
            }

            // 4 直列 allpass (L/R 独立)。
            for k in 0..N_ALLPASS {
                // L
                let len_l = s.allpass_lens[k];
                let off_l = s.allpass_offsets_l[k];
                let pos_l = s.allpass_pos_l[k];
                let idx_l = (off_l + pos_l) as usize;
                let bufout_l = Self::pool_at(&self.delay_pool, idx_l);
                let new_l = out_l + bufout_l * ALLPASS_FEEDBACK;
                Self::pool_set(&mut self.delay_pool, idx_l, new_l);
                out_l = bufout_l - out_l;
                let next_pos_l = pos_l + 1;
                s.allpass_pos_l[k] = if next_pos_l >= len_l { 0 } else { next_pos_l };

                // R
                let len_r = s.allpass_lens_r[k];
                let off_r = s.allpass_offsets_r[k];
                let pos_r = s.allpass_pos_r[k];
                let idx_r = (off_r + pos_r) as usize;
                let bufout_r = Self::pool_at(&self.delay_pool, idx_r);
                let new_r = out_r + bufout_r * ALLPASS_FEEDBACK;
                Self::pool_set(&mut self.delay_pool, idx_r, new_r);
                out_r = bufout_r - out_r;
                let next_pos_r = pos_r + 1;
                s.allpass_pos_r[k] = if next_pos_r >= len_r { 0 } else { next_pos_r };
            }

            // wet/dry mix (in-place)。Freeverb のクロスフィード式。
            let mixed_l = out_l * wet1 + out_r * wet2 + in_l * dry;
            let mixed_r = out_r * wet1 + out_l * wet2 + in_r * dry;
            buf[base] = mixed_l;
            buf[base + 1] = mixed_r;
            // 3ch 以降はそのまま (Phase 2-3 では 5.1 等は対応しない)。
        }
    }

    #[inline]
    fn pool_at(pool: &[f32], idx: usize) -> f32 {
        // SAFETY: 呼出側でオフセット + リングポインタ < スプライス上限を保証している。
        // ただし通常 path で `unsafe` にする必要はないので bounds check 付きでアクセス。
        pool[idx]
    }

    #[inline]
    fn pool_set(pool: &mut [f32], idx: usize, value: f32) {
        pool[idx] = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::EntityId;

    #[test]
    fn spawn_and_despawn() {
        let mut w = ReverbWorld::new();
        let id = EntityId {
            index: 0,
            generation: 0,
        };
        let dense = w.spawn(id).unwrap();
        assert_eq!(w.len(), 1);
        let _ = w.despawn(dense);
        assert_eq!(w.len(), 0);
    }

    #[test]
    fn capacity_limit() {
        let mut w = ReverbWorld::new();
        for k in 0..MAX_REVERBS {
            let id = EntityId {
                index: k as u32,
                generation: 0,
            };
            assert!(w.spawn(id).is_some());
        }
        let id = EntityId {
            index: 999,
            generation: 0,
        };
        assert!(w.spawn(id).is_none());
    }

    #[test]
    fn passthrough_when_wet_is_zero() {
        // wet=0, dry=1.0 で完全に dry 信号通過 (リバーブ無効)。
        let mut w = ReverbWorld::new();
        let id = EntityId {
            index: 0,
            generation: 0,
        };
        let d = w.spawn(id).unwrap();
        w.set_wet(d, 0.0);
        w.set_dry(d, 1.0);
        w.flush_dirty();

        let original: Vec<f32> = (0..512).map(|n| (n as f32 * 0.01).sin()).collect();
        let mut buf = original.clone();
        w.apply(d, &mut buf, 2);
        // dry=1.0 / wet=0.0 ならば in_l と out_l は完全一致 (allpass/comb 出力は wet=0 で消える)。
        for (a, b) in buf.iter().zip(original.iter()) {
            assert!(
                (a - b).abs() < 1e-5,
                "expected dry passthrough, diff: {}",
                a - b
            );
        }
    }

    #[test]
    fn reverb_extends_signal_decay() {
        // インパルス入力 → wet 信号がしばらく続くことを確認 (residual energy)。
        let mut w = ReverbWorld::new();
        let id = EntityId {
            index: 0,
            generation: 0,
        };
        let d = w.spawn(id).unwrap();
        w.set_wet(d, 1.0);
        w.set_dry(d, 0.0);
        w.set_room_size(d, 0.8);
        w.set_damping(d, 0.2);
        w.flush_dirty();

        // インパルス: 最初の 2 サンプルに 1.0 を入れて、その後 8000 サンプル分処理。
        let frames = 8000;
        let mut buf = vec![0.0_f32; frames * 2];
        buf[0] = 1.0;
        buf[1] = 1.0;
        w.apply(d, &mut buf, 2);

        // 1000 frame 以降にも一定のエネルギーが残っていることをチェック。
        let tail_energy: f32 = buf[1000 * 2..].iter().map(|x| x * x).sum();
        assert!(
            tail_energy > 1e-3,
            "expected residual reverb energy, got {tail_energy}"
        );
    }
}
