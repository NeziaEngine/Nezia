use crate::core::sparse_set::SparseSet;
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
    // ── エンティティ管理 ──
    entities: SparseSet,

    // ── 密配列（dense arrays / コンポーネント）──
    /// 音量倍率（0.0〜）。
    pub(super) gain: Vec<f32>,
    /// ミュート状態。BusComponent とは独立した SoA フィールド（タグコンポーネント相当）。
    pub(super) muted: Vec<bool>,
    /// 出力先バスの密配列インデックス。ホットループでの EntityId 解決を避けるため密配列インデックスで保持。
    pub(super) output_bus_dense: Vec<u32>,

    // ── ミキシング ──
    /// フラットな中間ミキシングバッファ。
    /// レイアウト: `mix_buffer[dense_index * MAX_MIX_BUFFER_SIZE .. (dense_index + 1) * MAX_MIX_BUFFER_SIZE]`
    pub(super) mix_buffer: Vec<f32>,
    /// 処理順序（密配列インデックスの列、リーフ→ルート順）。
    pub(super) process_order: Vec<u32>,

    /// マスターバスの EntityId。
    master_entity: EntityId,
}

#[allow(dead_code)]
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
            entities: SparseSet::new(MAX_BUSES),
            gain: Vec::with_capacity(MAX_BUSES),
            muted: Vec::with_capacity(MAX_BUSES),
            output_bus_dense: Vec::with_capacity(MAX_BUSES),
            mix_buffer: vec![0.0; MAX_BUSES * MAX_MIX_BUFFER_SIZE],
            process_order: Vec::with_capacity(MAX_BUSES),
            master_entity,
        };

        // マスターバスを entity_index=0 で挿入。output_bus_dense=0（自己参照）。
        world.insert_components(1.0, 0);
        world.entities.alloc_at_index(0);
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
        self.entities.resolve(id)
    }

    /// コンポーネント密配列にデータを追加する（内部用）。
    fn insert_components(&mut self, gain: f32, output_bus_dense_val: u32) {
        self.gain.push(gain);
        self.muted.push(false);
        self.output_bus_dense.push(output_bus_dense_val);
    }

    /// 指定した EntityId でバスを生成する（サウンドスレッド用）。
    ///
    /// メインスレッドが EntityId を事前に決定し、コマンドで送ってきた場合に使用する。
    /// `MAX_BUSES` に達している場合は `false` を返す。
    pub fn spawn_with_id(&mut self, id: EntityId, params: BusComponent) -> bool {
        self.insert_components(params.gain, params.output_bus_dense);
        let Some((_id, _dense)) = self.entities.alloc_at_index(id.index) else {
            // alloc 失敗時、push したコンポーネントを除去する。
            self.gain.pop();
            self.muted.pop();
            self.output_bus_dense.pop();
            return false;
        };
        true
    }

    /// バスを生成し、EntityId を返す。
    pub fn spawn(&mut self, params: BusComponent) -> Option<EntityId> {
        self.insert_components(params.gain, params.output_bus_dense);
        let Some((id, _dense)) = self.entities.alloc() else {
            // alloc 失敗時、push したコンポーネントを除去する。
            self.gain.pop();
            self.muted.pop();
            self.output_bus_dense.pop();
            return None;
        };
        Some(id)
    }

    /// バスを削除する（swap-remove）。
    ///
    /// マスターバスは削除できない（`false` を返す）。
    /// 削除されたバスを参照していた `output_bus_dense` はマスターバス（dense 0）にフォールバックする。
    pub fn despawn(&mut self, id: EntityId) -> bool {
        if id == self.master_entity {
            return false;
        }
        let Some(dense_index) = self.entities.dealloc(id) else {
            return false;
        };
        let last_dense = self.gain.len() - 1;

        for d in 0..self.output_bus_dense.len() {
            if self.output_bus_dense[d] == dense_index as u32 {
                self.output_bus_dense[d] = 0;
            } else if self.output_bus_dense[d] == last_dense as u32 && dense_index != last_dense {
                self.output_bus_dense[d] = dense_index as u32;
            }
        }

        if dense_index != last_dense {
            let src_start = last_dense * MAX_MIX_BUFFER_SIZE;
            let dst_start = dense_index * MAX_MIX_BUFFER_SIZE;
            self.mix_buffer
                .copy_within(src_start..src_start + MAX_MIX_BUFFER_SIZE, dst_start);
        }

        self.gain.swap_remove(dense_index);
        self.muted.swap_remove(dense_index);
        self.output_bus_dense.swap_remove(dense_index);

        true
    }

    /// EntityId が有効か確認する。
    pub fn contains(&self, id: EntityId) -> bool {
        self.entities.contains(id)
    }

    /// 現在のバス数（マスターバスを含む）。
    pub fn len(&self) -> usize {
        self.entities.len()
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

    /// フラット mix_buffer への可変参照。SourceMixingSystem::update() に渡す用。
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

}

impl Default for BusWorld {
    fn default() -> Self {
        Self::new()
    }
}
