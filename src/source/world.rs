use crate::entity::EntityId;

use super::MAX_SOURCES;

/// Source 生成時の初期パラメータ。
pub struct SourceComponent {
    pub vol: f32,
    pub pitch: f32,
    pub sample_offset: f32,
    /// 再生する AudioBuffer のインデックス。
    pub audio_buffer_index: u32,
    /// 出力先バスの密配列インデックス。0 = マスターバス。
    pub output_bus: u32,
}

impl Default for SourceComponent {
    fn default() -> Self {
        Self {
            vol: 1.0,
            pitch: 1.0,
            sample_offset: 0.0,
            audio_buffer_index: 0,
            output_bus: 0,
        }
    }
}

/// Source の再生状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceState {
    /// 再生中。
    Playing,
    /// 未使用（スロットは確保されているが再生していない）。
    Free,
    /// 一時停止中。再開可能。
    Pausing,
    /// 停止済み。次の update で despawn される。
    Stopped,
}

/// Source ワールド。
///
/// スパースセット方式の SoA（Structure of Arrays）レイアウトで
/// Source ごとのコンポーネントを管理する。
/// 各コンポーネント（vol, pitch, sample_offset）は独立した密配列に格納され、
/// キャッシュ効率の高い一括処理が可能。
pub struct SourceWorld {
    // ── 疎配列（sparse array） ──
    /// EntityId.index → 密配列インデックスへのマッピング。
    sparse: Vec<Option<SparseEntry>>,
    /// 密配列インデックス → EntityId.index への逆マッピング。
    dense_to_sparse: Vec<u32>,

    // ── 密配列（dense arrays / コンポーネント） ──
    /// 音量（0.0〜1.0）。
    pub(super) vol: Vec<f32>,
    /// ピッチ倍率（1.0 = 原音、2.0 = 1オクターブ上）。
    pub(super) pitch: Vec<f32>,
    /// サンプルオフセット（再生位置）。
    pub(super) sample_offset: Vec<f32>,
    /// 再生する AudioBuffer のインデックス。
    pub(super) audio_buffer_index: Vec<u32>,
    /// 再生状態。
    pub(super) state: Vec<SourceState>,
    /// 出力先バスの密配列インデックス。
    pub(super) output_bus: Vec<u32>,

    // ── スロット管理 ──
    free_list: Vec<u32>,
    next_index: u32,
}

#[derive(Debug, Clone, Copy)]
struct SparseEntry {
    dense_index: u32,
    generation: u32,
}

impl Default for SourceWorld {
    fn default() -> Self {
        Self::new()
    }
}

impl SourceWorld {
    pub fn new() -> Self {
        Self {
            sparse: Vec::with_capacity(MAX_SOURCES),
            dense_to_sparse: Vec::with_capacity(MAX_SOURCES),
            vol: Vec::with_capacity(MAX_SOURCES),
            pitch: Vec::with_capacity(MAX_SOURCES),
            sample_offset: Vec::with_capacity(MAX_SOURCES),
            audio_buffer_index: Vec::with_capacity(MAX_SOURCES),
            state: Vec::with_capacity(MAX_SOURCES),
            output_bus: Vec::with_capacity(MAX_SOURCES),
            free_list: Vec::with_capacity(MAX_SOURCES),
            next_index: 0,
        }
    }

    /// Source を追加し、EntityId を返す。
    ///
    /// `MAX_SOURCES` に達している場合は `None` を返す。
    pub fn spawn(&mut self, params: SourceComponent) -> Option<EntityId> {
        if self.vol.len() >= MAX_SOURCES {
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
        self.state.push(SourceState::Playing);
        self.output_bus.push(params.output_bus);

        Some(EntityId { index, generation })
    }

    /// EntityId を検証し、有効なら密配列インデックスを返す。
    pub(super) fn resolve(&self, id: EntityId) -> Option<usize> {
        let entry = self.sparse.get(id.index as usize)?.as_ref()?;
        if entry.generation != id.generation {
            return None;
        }
        Some(entry.dense_index as usize)
    }

    /// Source を削除する（swap-remove）。
    pub fn despawn(&mut self, id: EntityId) -> bool {
        let Some(dense_index) = self.resolve(id) else {
            return false;
        };
        let last_dense = self.vol.len() - 1;

        if let Some(entry) = &mut self.sparse[id.index as usize] {
            *entry = SparseEntry {
                dense_index: 0,
                generation: entry.generation + 1,
            };
        }
        self.free_list.push(id.index);

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
        self.output_bus.swap_remove(dense_index);

        true
    }

    /// EntityId が有効か確認する。
    pub fn contains(&self, id: EntityId) -> bool {
        self.resolve(id).is_some()
    }

    /// 現在の Source 数。
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

    pub fn state(&self, id: EntityId) -> Option<SourceState> {
        self.resolve(id).map(|i| self.state[i])
    }

    pub fn set_state(&mut self, id: EntityId, value: SourceState) -> bool {
        if let Some(i) = self.resolve(id) {
            self.state[i] = value;
            true
        } else {
            false
        }
    }

    /// ミキシングに必要な全スライスを同時に返す。
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

    pub fn states(&self) -> &[SourceState] {
        &self.state
    }

    pub fn states_mut(&mut self) -> &mut [SourceState] {
        &mut self.state
    }

    /// 密配列インデックスを指定して Source を削除する（swap-remove）。
    ///
    /// `SourceSystem::update()` が再生終了した Source を直接削除するために使用する。
    /// 逆順で呼び出すこと（swap-remove のため後ろから消さないとインデックスがずれる）。
    pub(super) fn despawn_by_dense_index(&mut self, dense_index: usize) {
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
        self.output_bus.swap_remove(dense_index);
    }
}
