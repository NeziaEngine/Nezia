use crate::core::sparse_set::SparseSet;
use crate::effect::{EffectId, MAX_EFFECTS_PER_BUS};
use crate::entity::EntityId;

use super::send::{SendDestKind, SendId, SendPosition};
use super::send_table::SendTable;
use super::{MAX_BUSES, MAX_MIX_BUFFER_SIZE, MAX_SENDS, MAX_SENDS_PER_BUS};

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

    // ── DSP エフェクトチェーン (Phase 2-3) ──
    /// Pre-Fader エフェクトチェーン (gain 適用前)。
    pub(super) pre_chain: Vec<[EffectId; MAX_EFFECTS_PER_BUS]>,
    pub(super) pre_count: Vec<u8>,
    /// Post-Fader エフェクトチェーン (gain 適用後)。
    pub(super) post_chain: Vec<[EffectId; MAX_EFFECTS_PER_BUS]>,
    pub(super) post_count: Vec<u8>,

    // ── Send (Phase 3-3) ──
    /// バス起点 Send の SoA + SendId 逆引き。`SourceWorld::sends` と同形 (`SendTable<CAP>`)。
    pub(super) sends: SendTable<MAX_SENDS_PER_BUS>,

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
            pre_chain: Vec::with_capacity(MAX_BUSES),
            pre_count: Vec::with_capacity(MAX_BUSES),
            post_chain: Vec::with_capacity(MAX_BUSES),
            post_count: Vec::with_capacity(MAX_BUSES),
            sends: SendTable::new(MAX_BUSES, MAX_SENDS),
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
    pub fn resolve(&self, id: EntityId) -> Option<usize> {
        self.entities.resolve(id)
    }

    /// コンポーネント密配列にデータを追加する（内部用）。
    fn insert_components(&mut self, gain: f32, output_bus_dense_val: u32) {
        self.gain.push(gain);
        self.muted.push(false);
        self.output_bus_dense.push(output_bus_dense_val);
        self.pre_chain.push(
            [EffectId {
                index: 0,
                generation: 0,
            }; MAX_EFFECTS_PER_BUS],
        );
        self.pre_count.push(0);
        self.post_chain.push(
            [EffectId {
                index: 0,
                generation: 0,
            }; MAX_EFFECTS_PER_BUS],
        );
        self.post_count.push(0);
        self.sends.push_empty_row();
    }

    /// alloc 失敗時に push 済みコンポーネントを巻き戻す（内部用）。
    fn pop_components(&mut self) {
        self.gain.pop();
        self.muted.pop();
        self.output_bus_dense.pop();
        self.pre_chain.pop();
        self.pre_count.pop();
        self.post_chain.pop();
        self.post_count.pop();
        self.sends.pop_row();
    }

    /// 指定した EntityId でバスを生成する（サウンドスレッド用）。
    ///
    /// メインスレッドが EntityId を事前に決定し、コマンドで送ってきた場合に使用する。
    /// `MAX_BUSES` に達している場合は `false` を返す。
    pub fn spawn_with_id(&mut self, id: EntityId, params: BusComponent) -> bool {
        self.insert_components(params.gain, params.output_bus_dense);
        let Some((_id, _dense)) = self.entities.alloc_at_index(id.index) else {
            self.pop_components();
            return false;
        };
        true
    }

    /// バスを生成し、EntityId を返す。
    pub fn spawn(&mut self, params: BusComponent) -> Option<EntityId> {
        self.insert_components(params.gain, params.output_bus_dense);
        let Some((id, _dense)) = self.entities.alloc() else {
            self.pop_components();
            return None;
        };
        Some(id)
    }

    /// バスを削除する（swap-remove）。
    ///
    /// マスターバスは削除できない（`false` を返す）。
    /// 削除されたバスを参照していた `output_bus_dense` / Send 宛先はマスターバス（dense 0）に
    /// フォールバックする。当該バスを `src` または `dst` とする Send は一括除去される。
    pub fn despawn(&mut self, id: EntityId) -> bool {
        if id == self.master_entity {
            return false;
        }
        let Some(dense_index) = self.entities.dealloc(id) else {
            return false;
        };
        let last_dense = self.gain.len() - 1;
        let dense_u32 = dense_index as u32;
        let last_u32 = last_dense as u32;

        // 1. 当該バスを宛先としていた他バスの Send を一括除去 (本人 row は次の swap_remove_row で処理)。
        self.sends
            .remove_destinations_matching(dense_u32, dense_index);

        // 2. mix_buffer の swap (last_dense → dense_index)。
        if dense_index != last_dense {
            let src_start = last_dense * MAX_MIX_BUFFER_SIZE;
            let dst_start = dense_index * MAX_MIX_BUFFER_SIZE;
            self.mix_buffer
                .copy_within(src_start..src_start + MAX_MIX_BUFFER_SIZE, dst_start);
        }

        // 3. SoA 列を swap_remove (Send 部分は SendTable に委譲)。
        self.gain.swap_remove(dense_index);
        self.muted.swap_remove(dense_index);
        self.output_bus_dense.swap_remove(dense_index);
        self.pre_chain.swap_remove(dense_index);
        self.pre_count.swap_remove(dense_index);
        self.post_chain.swap_remove(dense_index);
        self.post_count.swap_remove(dense_index);
        self.sends.swap_remove_row(dense_index);

        // 4. output_bus_dense / send_dest_dense の再マップ:
        //    - dense_index を指していた参照はマスター (0) にフォールバック
        //    - last_dense を指していた参照は dense_index に書き換え
        for d in 0..self.output_bus_dense.len() {
            if self.output_bus_dense[d] == dense_u32 {
                self.output_bus_dense[d] = 0;
            } else if self.output_bus_dense[d] == last_u32 && dense_index != last_dense {
                self.output_bus_dense[d] = dense_u32;
            }
        }
        self.sends
            .remap_destinations_after_swap(dense_u32, last_u32, 0);

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

    /// dense 配列のゲインスライス (Phase 3-2 Snapshot 補間で使用)。
    #[inline]
    #[must_use]
    pub fn gains(&self) -> &[f32] {
        &self.gain
    }

    /// dense index 直指定でゲイン書き込み (Phase 3-2 Snapshot 補間で使用)。
    #[inline]
    pub fn write_gain_by_dense(&mut self, dense: usize, value: f32) {
        if let Some(slot) = self.gain.get_mut(dense) {
            *slot = value;
        }
    }

    /// dense index 直指定でミュート書き込み (Phase 3-2 Snapshot 補間で使用)。
    #[inline]
    pub fn write_muted_by_dense(&mut self, dense: usize, value: bool) {
        if let Some(slot) = self.muted.get_mut(dense) {
            *slot = value;
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

    // ── DSP エフェクトチェーン操作 ──

    /// バスチェーン (pre/post) の末尾に EffectId を追加する。
    /// 戻り値: 挿入された slot index。チェーン満杯時は `None`。
    pub fn push_effect(
        &mut self,
        bus_dense: usize,
        position: crate::effect::EffectPosition,
        eff: EffectId,
    ) -> Option<u8> {
        if bus_dense >= self.gain.len() {
            return None;
        }
        let (chain, count) = match position {
            crate::effect::EffectPosition::Pre => (
                &mut self.pre_chain[bus_dense],
                &mut self.pre_count[bus_dense],
            ),
            crate::effect::EffectPosition::Post => (
                &mut self.post_chain[bus_dense],
                &mut self.post_count[bus_dense],
            ),
        };
        let idx = *count as usize;
        if idx >= MAX_EFFECTS_PER_BUS {
            return None;
        }
        chain[idx] = eff;
        *count += 1;
        Some(idx as u8)
    }

    /// `eff` をチェーン中から探して削除し、後続要素を詰める。slot_index 整合のため shift.
    /// 戻り値: 見つかって削除できたら `true`。
    pub fn remove_effect(
        &mut self,
        bus_dense: usize,
        position: crate::effect::EffectPosition,
        eff: EffectId,
    ) -> bool {
        if bus_dense >= self.gain.len() {
            return false;
        }
        let (chain, count) = match position {
            crate::effect::EffectPosition::Pre => (
                &mut self.pre_chain[bus_dense],
                &mut self.pre_count[bus_dense],
            ),
            crate::effect::EffectPosition::Post => (
                &mut self.post_chain[bus_dense],
                &mut self.post_count[bus_dense],
            ),
        };
        let n = *count as usize;
        for i in 0..n {
            if chain[i] == eff {
                for j in i..n - 1 {
                    chain[j] = chain[j + 1];
                }
                *count -= 1;
                return true;
            }
        }
        false
    }

    pub fn pre_chain_slice(&self, bus_dense: usize) -> &[EffectId] {
        let n = self.pre_count[bus_dense] as usize;
        &self.pre_chain[bus_dense][..n]
    }
    pub fn post_chain_slice(&self, bus_dense: usize) -> &[EffectId] {
        let n = self.post_count[bus_dense] as usize;
        &self.post_chain[bus_dense][..n]
    }

    // ── Send (Phase 3-3) ──

    /// 指定バスに Send を追加する。chain 満杯または `bus_dense` 範囲外で `false`。
    /// メインスレッドで `id` を事前発行 + サイクル検出済みの前提。
    pub fn add_send(
        &mut self,
        bus_dense: usize,
        id: SendId,
        dest_dense: u32,
        dest_kind: SendDestKind,
        gain: f32,
        position: SendPosition,
    ) -> bool {
        self.sends
            .add_send(bus_dense, id, dest_dense, dest_kind as u8, gain, position as u8)
    }

    /// SendId で Send を削除する。stale (generation 不一致) または未存在で `false`。
    pub fn remove_send(&mut self, id: SendId) -> bool {
        self.sends.remove_by_id(id)
    }

    /// SendId の現在位置 `(bus_dense, slot)` を返す。stale なら `None`。
    pub fn resolve_send(&self, id: SendId) -> Option<(usize, usize)> {
        self.sends.resolve(id)
    }

    /// SendId の gain を設定する。
    pub fn set_send_gain(&mut self, id: SendId, gain: f32) -> bool {
        self.sends.set_gain(id, gain)
    }

    /// SendId のタップ位置を変更する。
    pub fn set_send_position(&mut self, id: SendId, position: SendPosition) -> bool {
        self.sends.set_position(id, position as u8)
    }

    /// dense index 指定で send_gain を直接書き込む (Snapshot 補間で使用)。
    #[inline]
    pub fn write_send_gain_by_dense(&mut self, bus_dense: usize, slot: usize, gain: f32) {
        self.sends.write_gain_by_dense(bus_dense, slot, gain);
    }

    /// dense index 指定で send_gain を読み出す (Snapshot apply 時の現在値キャプチャ用)。
    #[inline]
    #[must_use]
    pub fn send_gain_at(&self, bus_dense: usize, slot: usize) -> Option<f32> {
        self.sends.gain_at(bus_dense, slot)
    }

    /// バス `bus_dense` の Send 数。
    #[inline]
    pub fn send_count_at(&self, bus_dense: usize) -> usize {
        self.sends.count_at(bus_dense)
    }

    /// バス `bus_dense` の slot `slot` にある Send 情報 `(dest_dense, gain, position, dest_kind)`。
    /// `BusSystem` の hot loop から読まれる。
    #[inline]
    pub fn send_at(&self, bus_dense: usize, slot: usize) -> (u32, f32, u8, u8) {
        self.sends.send_at(bus_dense, slot)
    }
}

impl Default for BusWorld {
    fn default() -> Self {
        Self::new()
    }
}
