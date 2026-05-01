use crate::entity::EntityId;

use super::{MAX_BUSES, MAX_MIX_BUFFER_SIZE};

/// バス生成時の初期パラメータ。mute は含まない。
pub struct BusComponent {
    /// 音量倍率（0.0〜）。
    pub gain: f32,
    /// 出力先バスの密配列インデックス。
    pub output_bus_dense: u32,
}

/// バスワールド。
///
/// SourceWorld と同じスパースセット方式の SoA レイアウトで
/// バスごとのコンポーネントを管理する。
/// マスターバスは `new()` で自動生成され、EntityId は常に `(index: 0, generation: 0)`。
pub struct BusWorld {
    // ── 疎配列（sparse array）──
    /// entity.index → 密配列インデックスへのマッピング。
    pub(super) sparse: Vec<Option<SparseEntry>>,
    /// 密配列インデックス → entity.index への逆マッピング。
    dense_to_sparse: Vec<u32>,

    // ── 密配列（dense arrays / コンポーネント）──
    /// 音量倍率（0.0〜）。
    pub(super) gain: Vec<f32>,
    /// ミュート状態。BusComponent とは独立した SoA フィールド（タグコンポーネント相当）。
    pub(super) muted: Vec<bool>,
    /// 出力先バスの密配列インデックス。ホットループでの EntityId 解決を避けるため密配列インデックスで保持。
    pub(super) output_bus_dense: Vec<u32>,

    // ── スロット管理 ──
    free_list: Vec<u32>,
    next_index: u32,

    // ── ミキシング ──
    /// フラットな中間ミキシングバッファ。
    /// レイアウト: `mix_buffer[dense_index * MAX_MIX_BUFFER_SIZE .. (dense_index + 1) * MAX_MIX_BUFFER_SIZE]`
    pub(super) mix_buffer: Vec<f32>,
    /// 処理順序（密配列インデックスの列、リーフ→ルート順）。
    pub(super) process_order: Vec<u32>,

    /// マスターバスの EntityId。
    master_entity: EntityId,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SparseEntry {
    pub(super) dense_index: u32,
    pub(super) generation: u32,
}

impl BusWorld {
    /// BusWorld を生成し、マスターバスを挿入した状態で返す。
    ///
    /// マスターバスの EntityId は常に `(index: 0, generation: 0)`。
    pub fn new() -> Self {
        let master_entity = EntityId {
            index: 0,
            generation: 0,
        };

        let mut world = Self {
            sparse: Vec::with_capacity(MAX_BUSES),
            dense_to_sparse: Vec::with_capacity(MAX_BUSES),
            gain: Vec::with_capacity(MAX_BUSES),
            muted: Vec::with_capacity(MAX_BUSES),
            output_bus_dense: Vec::with_capacity(MAX_BUSES),
            free_list: Vec::with_capacity(MAX_BUSES),
            next_index: 0,
            mix_buffer: vec![0.0; MAX_BUSES * MAX_MIX_BUFFER_SIZE],
            process_order: Vec::with_capacity(MAX_BUSES),
            master_entity,
        };

        // マスターバスを entity_index=0 で挿入。output_bus_dense=0（自己参照）。
        world.insert_at(0, 1.0, 0);
        // 初期 process_order: マスターバスのみ（dense index 0）。
        world.process_order.push(0);

        world
    }

    /// マスターバスの EntityId を返す。
    pub fn master_entity(&self) -> EntityId {
        self.master_entity
    }

    /// EntityId を検証し、有効なら密配列インデックスを返す。
    pub(super) fn resolve(&self, id: EntityId) -> Option<usize> {
        let entry = self.sparse.get(id.index as usize)?.as_ref()?;
        if entry.generation != id.generation {
            return None;
        }
        Some(entry.dense_index as usize)
    }

    /// 内部: 指定した entity_index でバスを挿入する。
    fn insert_at(&mut self, entity_index: u32, gain: f32, output_bus_dense_val: u32) -> EntityId {
        let dense_index = self.gain.len() as u32;

        if entity_index as usize >= self.sparse.len() {
            self.sparse.resize(entity_index as usize + 1, None);
        }

        let generation = self.sparse[entity_index as usize]
            .map(|e| e.generation)
            .unwrap_or(0);

        self.sparse[entity_index as usize] = Some(SparseEntry {
            dense_index,
            generation,
        });

        self.free_list.retain(|&i| i != entity_index);

        if entity_index >= self.next_index {
            self.next_index = entity_index + 1;
        }

        self.dense_to_sparse.push(entity_index);
        self.gain.push(gain);
        self.muted.push(false);
        self.output_bus_dense.push(output_bus_dense_val);

        EntityId {
            index: entity_index,
            generation,
        }
    }

    /// 指定した EntityId でバスを生成する（サウンドスレッド用）。
    ///
    /// メインスレッドが EntityId を事前に決定し、コマンドで送ってきた場合に使用する。
    /// `MAX_BUSES` に達している場合は `false` を返す。
    pub fn spawn_with_id(&mut self, id: EntityId, params: BusComponent) -> bool {
        if self.gain.len() >= MAX_BUSES {
            return false;
        }
        self.insert_at(id.index, params.gain, params.output_bus_dense);
        true
    }

    /// バスを生成し、EntityId を返す。
    pub fn spawn(&mut self, params: BusComponent) -> Option<EntityId> {
        if self.gain.len() >= MAX_BUSES {
            return None;
        }
        let entity_index = if let Some(reused) = self.free_list.pop() {
            reused
        } else {
            let index = self.next_index;
            self.next_index += 1;
            index
        };
        Some(self.insert_at(entity_index, params.gain, params.output_bus_dense))
    }

    /// バスを削除する（swap-remove）。
    ///
    /// マスターバスは削除できない（`false` を返す）。
    /// 削除されたバスを参照していた `output_bus_dense` はマスターバス（dense 0）にフォールバックする。
    pub fn despawn(&mut self, id: EntityId) -> bool {
        if id == self.master_entity {
            return false;
        }
        let Some(dense_index) = self.resolve(id) else {
            return false;
        };
        let last_dense = self.gain.len() - 1;

        if let Some(entry) = &mut self.sparse[id.index as usize] {
            *entry = SparseEntry {
                dense_index: 0,
                generation: entry.generation + 1,
            };
        }
        self.free_list.push(id.index);

        for d in 0..self.output_bus_dense.len() {
            if self.output_bus_dense[d] == dense_index as u32 {
                self.output_bus_dense[d] = 0;
            } else if self.output_bus_dense[d] == last_dense as u32 && dense_index != last_dense {
                self.output_bus_dense[d] = dense_index as u32;
            }
        }

        if dense_index != last_dense {
            let moved_sparse_index = self.dense_to_sparse[last_dense];
            if let Some(entry) = &mut self.sparse[moved_sparse_index as usize] {
                entry.dense_index = dense_index as u32;
            }
            self.dense_to_sparse[dense_index] = moved_sparse_index;

            let src_start = last_dense * MAX_MIX_BUFFER_SIZE;
            let dst_start = dense_index * MAX_MIX_BUFFER_SIZE;
            self.mix_buffer
                .copy_within(src_start..src_start + MAX_MIX_BUFFER_SIZE, dst_start);
        }

        self.dense_to_sparse.swap_remove(dense_index);
        self.gain.swap_remove(dense_index);
        self.muted.swap_remove(dense_index);
        self.output_bus_dense.swap_remove(dense_index);

        true
    }

    /// EntityId が有効か確認する。
    pub fn contains(&self, id: EntityId) -> bool {
        self.resolve(id).is_some()
    }

    /// 現在のバス数（マスターバスを含む）。
    pub fn len(&self) -> usize {
        self.gain.len()
    }

    pub fn is_empty(&self) -> bool {
        self.gain.is_empty()
    }

    // ── 個別アクセサ ──

    pub fn gain(&self, id: EntityId) -> Option<f32> {
        self.resolve(id).map(|i| self.gain[i])
    }

    pub fn set_gain(&mut self, id: EntityId, value: f32) -> bool {
        if let Some(i) = self.resolve(id) {
            self.gain[i] = value;
            true
        } else {
            false
        }
    }

    pub fn muted(&self, id: EntityId) -> Option<bool> {
        self.resolve(id).map(|i| self.muted[i])
    }

    pub fn set_muted(&mut self, id: EntityId, value: bool) -> bool {
        if let Some(i) = self.resolve(id) {
            self.muted[i] = value;
            true
        } else {
            false
        }
    }

    pub fn output_bus_dense(&self, id: EntityId) -> Option<u32> {
        self.resolve(id).map(|i| self.output_bus_dense[i])
    }

    pub fn set_output_bus_dense(&mut self, id: EntityId, dense: u32) -> bool {
        if let Some(i) = self.resolve(id) {
            self.output_bus_dense[i] = dense;
            true
        } else {
            false
        }
    }

    /// 処理順序を更新する（密配列インデックスのリスト、リーフ→ルート順）。
    pub fn set_process_order(&mut self, order: &[u32]) {
        self.process_order.clear();
        self.process_order.extend_from_slice(order);
    }

    // ── ミキシングバッファ ──

    /// バスの mix_buffer のストライド（バスあたりのサンプル数）。
    pub fn bus_stride(&self) -> usize {
        MAX_MIX_BUFFER_SIZE
    }

    /// フラット mix_buffer への可変参照。SourceSystem::update() に渡す用。
    pub fn mix_buffer_mut(&mut self) -> &mut [f32] {
        &mut self.mix_buffer
    }

    /// 全バスの mix_buffer を `sample_count` サンプル分ゼロクリアする。
    pub fn clear_mix_buffers(&mut self, sample_count: usize) {
        let bus_count = self.gain.len();
        let clear_len = sample_count.min(MAX_MIX_BUFFER_SIZE);
        for d in 0..bus_count {
            let start = d * MAX_MIX_BUFFER_SIZE;
            self.mix_buffer[start..start + clear_len].fill(0.0);
        }
    }

    // ── 密配列スライス（テスト・デバッグ用）──

    pub fn gains(&self) -> &[f32] {
        &self.gain
    }

    pub fn muteds(&self) -> &[bool] {
        &self.muted
    }

    pub fn output_bus_denses(&self) -> &[u32] {
        &self.output_bus_dense
    }

    pub fn process_order(&self) -> &[u32] {
        &self.process_order
    }
}

impl Default for BusWorld {
    fn default() -> Self {
        Self::new()
    }
}
