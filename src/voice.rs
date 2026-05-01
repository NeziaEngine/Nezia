use std::sync::Arc;

use crate::audio::AudioBuffer;
use crate::entity::EntityId;

/// 最大同時発音数。
pub const MAX_VOICES: usize = 256;

/// ボイスの再生状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceState {
    /// 再生中。
    Playing,
    /// 未使用（スロットは確保されているが再生していない）。
    Free,
    /// 一時停止中。再開可能。
    Pausing,
    /// 停止済み。次の update で despawn される。
    Stopped,
}

/// ボイスプール。
///
/// スパースセット方式の SoA（Structure of Arrays）レイアウトで
/// ボイスごとのコンポーネントを管理する。
/// 各コンポーネント（vol, pitch, sample_offset）は独立した密配列に格納され、
/// キャッシュ効率の高い一括処理が可能。
pub struct VoicePoolSystem {
    // ── 疎配列（sparse array） ──
    /// EntityId.index → 密配列インデックスへのマッピング。
    sparse: Vec<Option<SparseEntry>>,
    /// 密配列インデックス → EntityId.index への逆マッピング。
    dense_to_sparse: Vec<u32>,

    // ── 密配列（dense arrays / コンポーネント） ──
    /// 音量（0.0〜1.0）。
    vol: Vec<f32>,
    /// ピッチ倍率（1.0 = 原音、2.0 = 1オクターブ上）。
    pitch: Vec<f32>,
    /// サンプルオフセット（再生位置）。
    sample_offset: Vec<f32>,
    /// 再生する AudioBuffer のインデックス。
    audio_buffer_index: Vec<u32>,
    /// 再生状態。
    state: Vec<VoiceState>,

    // ── スロット管理 ──
    free_list: Vec<u32>,
    next_index: u32,
}

#[derive(Debug, Clone, Copy)]
struct SparseEntry {
    dense_index: u32,
    generation: u32,
}

/// ボイス生成時の初期パラメータ。
pub struct VoiceComponent {
    pub vol: f32,
    pub pitch: f32,
    pub sample_offset: f32,
    /// 再生する AudioBuffer のインデックス。
    pub audio_buffer_index: u32,
}

impl Default for VoiceComponent {
    fn default() -> Self {
        Self {
            vol: 1.0,
            pitch: 1.0,
            sample_offset: 0.0,
            audio_buffer_index: 0,
        }
    }
}

impl Default for VoicePoolSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl VoicePoolSystem {
    pub fn new() -> Self {
        Self {
            sparse: Vec::with_capacity(MAX_VOICES),
            dense_to_sparse: Vec::with_capacity(MAX_VOICES),
            vol: Vec::with_capacity(MAX_VOICES),
            pitch: Vec::with_capacity(MAX_VOICES),
            sample_offset: Vec::with_capacity(MAX_VOICES),
            audio_buffer_index: Vec::with_capacity(MAX_VOICES),
            state: Vec::with_capacity(MAX_VOICES),
            free_list: Vec::with_capacity(MAX_VOICES),
            next_index: 0,
        }
    }

    /// ボイスを追加し、EntityId を返す。
    ///
    /// `MAX_VOICES` に達している場合は `None` を返す。
    pub fn spawn(&mut self, params: VoiceComponent) -> Option<EntityId> {
        if self.vol.len() >= MAX_VOICES {
            return None;
        }
        let dense_index = self.vol.len() as u32;

        let (index, generation) = if let Some(reused) = self.free_list.pop() {
            let reused_gen = self.sparse[reused as usize]
                .map(|e| e.generation)
                .unwrap_or(0);
            self.sparse[reused as usize] = Some(SparseEntry {
                dense_index,
                generation: reused_gen,
            });
            (reused, reused_gen)
        } else {
            let index = self.next_index;
            self.next_index += 1;
            if index as usize >= self.sparse.len() {
                self.sparse.resize(index as usize + 1, None);
            }
            self.sparse[index as usize] = Some(SparseEntry {
                dense_index,
                generation: 0,
            });
            (index, 0)
        };

        self.dense_to_sparse.push(index);
        self.vol.push(params.vol);
        self.pitch.push(params.pitch);
        self.sample_offset.push(params.sample_offset);
        self.audio_buffer_index.push(params.audio_buffer_index);
        self.state.push(VoiceState::Playing);

        Some(EntityId { index, generation })
    }

    /// EntityId を検証し、有効なら密配列インデックスを返す。
    fn resolve(&self, id: EntityId) -> Option<usize> {
        let entry = self.sparse.get(id.index as usize)?.as_ref()?;
        if entry.generation != id.generation {
            return None;
        }
        Some(entry.dense_index as usize)
    }

    /// ボイスを削除する（swap-remove）。
    pub fn despawn(&mut self, id: EntityId) -> bool {
        let Some(dense_index) = self.resolve(id) else {
            return false;
        };
        let last_dense = self.vol.len() - 1;

        // 疎エントリを無効化し generation をインクリメント。
        // 次に同じ index が再利用されたとき、古い EntityId は
        // generation が合わないので resolve() で弾かれる。
        if let Some(entry) = &mut self.sparse[id.index as usize] {
            *entry = SparseEntry {
                dense_index: 0,
                generation: entry.generation + 1,
            };
        }
        self.free_list.push(id.index);

        // swap-remove: 末尾要素を削除位置に移動し、
        // 移動した要素の疎配列エントリも更新する。
        if dense_index != last_dense {
            let moved_sparse_index = self.dense_to_sparse[last_dense];
            if let Some(entry) = &mut self.sparse[moved_sparse_index as usize] {
                entry.dense_index = dense_index as u32;
            }
            self.dense_to_sparse[dense_index] = moved_sparse_index;
        }

        self.dense_to_sparse.swap_remove(dense_index);
        self.vol.swap_remove(dense_index);
        self.pitch.swap_remove(dense_index);
        self.sample_offset.swap_remove(dense_index);
        self.audio_buffer_index.swap_remove(dense_index);
        self.state.swap_remove(dense_index);

        true
    }

    /// EntityId が有効か確認する。
    pub fn contains(&self, id: EntityId) -> bool {
        self.resolve(id).is_some()
    }

    /// 現在のボイス数。
    pub fn len(&self) -> usize {
        self.vol.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vol.is_empty()
    }

    // ── 個別アクセス ──

    pub fn vol(&self, id: EntityId) -> Option<f32> {
        self.resolve(id).map(|i| self.vol[i])
    }

    pub fn pitch(&self, id: EntityId) -> Option<f32> {
        self.resolve(id).map(|i| self.pitch[i])
    }

    pub fn sample_offset(&self, id: EntityId) -> Option<f32> {
        self.resolve(id).map(|i| self.sample_offset[i])
    }

    pub fn set_vol(&mut self, id: EntityId, value: f32) -> bool {
        if let Some(i) = self.resolve(id) {
            self.vol[i] = value;
            true
        } else {
            false
        }
    }

    pub fn set_pitch(&mut self, id: EntityId, value: f32) -> bool {
        if let Some(i) = self.resolve(id) {
            self.pitch[i] = value;
            true
        } else {
            false
        }
    }

    pub fn set_sample_offset(&mut self, id: EntityId, value: f32) -> bool {
        if let Some(i) = self.resolve(id) {
            self.sample_offset[i] = value;
            true
        } else {
            false
        }
    }

    pub fn audio_buffer_index(&self, id: EntityId) -> Option<u32> {
        self.resolve(id).map(|i| self.audio_buffer_index[i])
    }

    pub fn state(&self, id: EntityId) -> Option<VoiceState> {
        self.resolve(id).map(|i| self.state[i])
    }

    pub fn set_state(&mut self, id: EntityId, value: VoiceState) -> bool {
        if let Some(i) = self.resolve(id) {
            self.state[i] = value;
            true
        } else {
            false
        }
    }

    /// ミキシングに必要な全スライスを同時に返す。
    ///
    /// `sample_offset` のみ `&mut` で返し、他は `&` で返す。
    /// Rust の借用ルールにより、個別のアクセサでは同時に取得できないため、
    /// 構造体のフィールドを直接分割借用してタプルで返す。
    pub fn mixing_slices(&mut self) -> (&[f32], &[f32], &mut [f32], &[u32]) {
        (
            &self.vol,
            &self.pitch,
            &mut self.sample_offset,
            &self.audio_buffer_index,
        )
    }

    // ── 一括アクセス（密配列スライス） ──

    pub fn vols(&self) -> &[f32] {
        &self.vol
    }

    pub fn vols_mut(&mut self) -> &mut [f32] {
        &mut self.vol
    }

    pub fn pitches(&self) -> &[f32] {
        &self.pitch
    }

    pub fn pitches_mut(&mut self) -> &mut [f32] {
        &mut self.pitch
    }

    pub fn sample_offsets(&self) -> &[f32] {
        &self.sample_offset
    }

    pub fn sample_offsets_mut(&mut self) -> &mut [f32] {
        &mut self.sample_offset
    }

    pub fn audio_buffer_indices(&self) -> &[u32] {
        &self.audio_buffer_index
    }

    pub fn states(&self) -> &[VoiceState] {
        &self.state
    }

    pub fn states_mut(&mut self) -> &mut [VoiceState] {
        &mut self.state
    }

    /// 毎オーディオコールバックで呼び出す update 処理。
    ///
    /// 全アクティブボイスの AudioBuffer からサンプルを読み出し、
    /// `output_buffer` に加算ミキシングする。
    /// 再生が完了したボイスは自動的に despawn される。
    ///
    /// `output_buffer` は呼び出し前にゼロクリアされている前提。
    pub fn update(
        &mut self,
        output_buffer: &mut [f32],
        device_channels: usize,
        device_sample_rate: f32,
        master_volume: f32,
        buffers: &[Option<Arc<AudioBuffer>>],
    ) {
        let voice_count = self.vol.len();
        if voice_count == 0 {
            return;
        }

        let (vols, pitches, offsets, buf_indices, states) = (
            &self.vol,
            &self.pitch,
            &mut self.sample_offset,
            &self.audio_buffer_index,
            &self.state,
        );

        // 各ボイスからサンプルを読み出し、出力バッファに加算ミキシングする。
        // Playing 状態のボイスのみミキシング対象。
        for voice_i in 0..voice_count {
            if states[voice_i] != VoiceState::Playing {
                continue;
            }
            let buf_idx = buf_indices[voice_i] as usize;
            let Some(audio_buf) = buffers.get(buf_idx).and_then(|b| b.as_ref()) else {
                continue;
            };

            let vol = vols[voice_i] * master_volume;
            let pitch = pitches[voice_i];
            // サンプルレート変換比率。
            // ソースのサンプルレートがデバイスと異なる場合に補正する。
            let rate_ratio = audio_buf.sample_rate as f32 / device_sample_rate;
            let advance = pitch * rate_ratio;
            let src_channels = audio_buf.channels as usize;
            let src_frame_count = audio_buf.frame_count();

            let mut offset = offsets[voice_i];

            for frame in output_buffer.chunks_mut(device_channels) {
                let frame_idx = offset as usize;
                if frame_idx >= src_frame_count {
                    break;
                }

                // 線形補間でサブサンプル精度の再生位置をサポート。
                let frac = offset - offset.floor();
                let idx0 = frame_idx;
                let idx1 = (idx0 + 1).min(src_frame_count - 1);

                for (ch, out) in frame.iter_mut().enumerate() {
                    let src_ch = ch % src_channels;
                    let s0 = audio_buf.samples[idx0 * src_channels + src_ch];
                    let s1 = audio_buf.samples[idx1 * src_channels + src_ch];
                    let sample = s0 + (s1 - s0) * frac;
                    *out += sample * vol;
                }

                offset += advance;
            }

            offsets[voice_i] = offset;
        }

        // 再生が終了した / 停止済み / Free のボイスを逆順で despawn
        // （swap-remove のため後ろから）。
        for voice_i in (0..self.vol.len()).rev() {
            let should_despawn = match self.state[voice_i] {
                VoiceState::Stopped | VoiceState::Free => true,
                VoiceState::Playing => {
                    let buf_idx = self.audio_buffer_index[voice_i] as usize;
                    match buffers.get(buf_idx).and_then(|b| b.as_ref()) {
                        Some(ab) => self.sample_offset[voice_i] as usize >= ab.frame_count(),
                        None => true,
                    }
                }
                VoiceState::Pausing => false,
            };
            if should_despawn {
                self.despawn_by_dense_index(voice_i);
            }
        }
    }

    /// 密配列インデックスを指定してボイスを削除する（swap-remove）。
    ///
    /// サウンドスレッド内で再生終了したボイスを直接削除するために使用する。
    /// 逆順で呼び出すこと（swap-remove のため後ろから消さないとインデックスがずれる）。
    pub fn despawn_by_dense_index(&mut self, dense_index: usize) {
        if dense_index >= self.vol.len() {
            return;
        }

        let sparse_index = self.dense_to_sparse[dense_index];
        if let Some(entry) = &mut self.sparse[sparse_index as usize] {
            *entry = SparseEntry {
                dense_index: 0,
                generation: entry.generation + 1,
            };
        }
        self.free_list.push(sparse_index);

        let last_dense = self.vol.len() - 1;
        if dense_index != last_dense {
            let moved_sparse_index = self.dense_to_sparse[last_dense];
            if let Some(entry) = &mut self.sparse[moved_sparse_index as usize] {
                entry.dense_index = dense_index as u32;
            }
            self.dense_to_sparse[dense_index] = moved_sparse_index;
        }

        self.dense_to_sparse.swap_remove(dense_index);
        self.vol.swap_remove(dense_index);
        self.pitch.swap_remove(dense_index);
        self.sample_offset.swap_remove(dense_index);
        self.audio_buffer_index.swap_remove(dense_index);
        self.state.swap_remove(dense_index);
    }
}
