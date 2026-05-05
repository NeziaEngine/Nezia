use crate::core::sparse_set::SparseSet;
use crate::effect::{EffectId, MAX_EFFECTS_PER_BUS};
use crate::entity::EntityId;

use super::send::{SendDestKind, SendId, SendPosition};
use super::{MAX_BUSES, MAX_MIX_BUFFER_SIZE, MAX_SENDS, MAX_SENDS_PER_BUS};

/// SendId.index → (bus_dense, slot) の逆引きエントリ。
///
/// `bus_dense == u32::MAX` は未割当。`generation` は割当時の SendId.generation を保持し、
/// stale な操作 (slot 再利用後の古い SendId) を弾くのに使う。
#[derive(Copy, Clone, Debug)]
pub(super) struct SendLookup {
    pub(super) bus_dense: u32,
    pub(super) slot: u8,
    pub(super) generation: u32,
}

impl SendLookup {
    pub(super) const EMPTY: SendLookup = SendLookup {
        bus_dense: u32::MAX,
        slot: 0,
        generation: 0,
    };

    pub(super) fn is_empty(&self) -> bool {
        self.bus_dense == u32::MAX
    }
}

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
    /// 各バスから出ていく Send の宛先 dense index。
    /// `send_dest_kind` の値によって BusWorld dense か CompressorWorld dense かが変わる。
    pub(super) send_dest_dense: Vec<[u32; MAX_SENDS_PER_BUS]>,
    /// 各 Send の gain。
    pub(super) send_gain: Vec<[f32; MAX_SENDS_PER_BUS]>,
    /// 各 Send のタップ位置 (`SendPosition` を u8 で格納)。
    pub(super) send_position: Vec<[u8; MAX_SENDS_PER_BUS]>,
    /// 各 Send の宛先種別 (`SendDestKind` を u8 で格納、PR2)。
    pub(super) send_dest_kind: Vec<[u8; MAX_SENDS_PER_BUS]>,
    /// 各 Send の SendId (despawn / Set 系の逆引きと整合確認用)。
    pub(super) send_id: Vec<[SendId; MAX_SENDS_PER_BUS]>,
    /// 各バスの有効 Send 数 (0..=MAX_SENDS_PER_BUS)。
    pub(super) send_count: Vec<u8>,

    /// SendId.index → (bus_dense, slot) の逆引き。`SendLookup::EMPTY` で未割当。
    /// audio thread 側 (このフィールド) と main thread 側 (`SendIdAllocator`) の generation を
    /// 突き合わせて stale 検出する。サイズは `MAX_SENDS` で固定確保。
    send_lookup: Vec<SendLookup>,

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
            send_dest_dense: Vec::with_capacity(MAX_BUSES),
            send_gain: Vec::with_capacity(MAX_BUSES),
            send_position: Vec::with_capacity(MAX_BUSES),
            send_dest_kind: Vec::with_capacity(MAX_BUSES),
            send_id: Vec::with_capacity(MAX_BUSES),
            send_count: Vec::with_capacity(MAX_BUSES),
            send_lookup: vec![SendLookup::EMPTY; MAX_SENDS],
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
        self.send_dest_dense.push([0; MAX_SENDS_PER_BUS]);
        self.send_gain.push([0.0; MAX_SENDS_PER_BUS]);
        self.send_position.push([0; MAX_SENDS_PER_BUS]);
        self.send_dest_kind.push([0; MAX_SENDS_PER_BUS]);
        self.send_id.push([SendId::INVALID; MAX_SENDS_PER_BUS]);
        self.send_count.push(0);
    }

    /// バス despawn 時に Send 関連の dense 配列を pop して整合を保つ（内部用）。
    fn pop_send_components(&mut self) {
        self.send_dest_dense.pop();
        self.send_gain.pop();
        self.send_position.pop();
        self.send_dest_kind.pop();
        self.send_id.pop();
        self.send_count.pop();
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
            self.pre_chain.pop();
            self.pre_count.pop();
            self.post_chain.pop();
            self.post_count.pop();
            self.pop_send_components();
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
            self.pre_chain.pop();
            self.pre_count.pop();
            self.post_chain.pop();
            self.post_count.pop();
            self.pop_send_components();
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

        // 1. 当該バスから出ていた Send の send_lookup を全クリア (本体は後で swap_remove)。
        let originating_count = self.send_count[dense_index] as usize;
        for slot in 0..originating_count {
            let sid = self.send_id[dense_index][slot];
            if sid.is_valid() && (sid.index as usize) < self.send_lookup.len() {
                let lk = &self.send_lookup[sid.index as usize];
                if lk.generation == sid.generation {
                    self.send_lookup[sid.index as usize] = SendLookup::EMPTY;
                }
            }
        }

        // 2. 当該バスを宛先としていた他バスの Send を一括除去。
        for src_dense in 0..self.gain.len() {
            if src_dense == dense_index {
                continue;
            }
            let mut s = 0;
            while s < self.send_count[src_dense] as usize {
                if self.send_dest_dense[src_dense][s] == dense_u32 {
                    self.swap_remove_send_slot(src_dense, s);
                    // s はそのまま (末尾要素が入った)。
                } else {
                    s += 1;
                }
            }
        }

        // 3. mix_buffer の swap (last_dense → dense_index)。
        if dense_index != last_dense {
            let src_start = last_dense * MAX_MIX_BUFFER_SIZE;
            let dst_start = dense_index * MAX_MIX_BUFFER_SIZE;
            self.mix_buffer
                .copy_within(src_start..src_start + MAX_MIX_BUFFER_SIZE, dst_start);
        }

        // 4. SoA 列を swap_remove。
        self.gain.swap_remove(dense_index);
        self.muted.swap_remove(dense_index);
        self.output_bus_dense.swap_remove(dense_index);
        self.pre_chain.swap_remove(dense_index);
        self.pre_count.swap_remove(dense_index);
        self.post_chain.swap_remove(dense_index);
        self.post_count.swap_remove(dense_index);
        self.send_dest_dense.swap_remove(dense_index);
        self.send_gain.swap_remove(dense_index);
        self.send_position.swap_remove(dense_index);
        self.send_dest_kind.swap_remove(dense_index);
        self.send_id.swap_remove(dense_index);
        self.send_count.swap_remove(dense_index);

        // 5. output_bus_dense / send_dest_dense の再マップ:
        //    - dense_index を指していた参照はマスター (0) にフォールバック
        //    - last_dense を指していた参照は dense_index に書き換え
        for d in 0..self.output_bus_dense.len() {
            if self.output_bus_dense[d] == dense_u32 {
                self.output_bus_dense[d] = 0;
            } else if self.output_bus_dense[d] == last_u32 && dense_index != last_dense {
                self.output_bus_dense[d] = dense_u32;
            }
        }
        for d in 0..self.send_count.len() {
            let n = self.send_count[d] as usize;
            for s in 0..n {
                let dest = self.send_dest_dense[d][s];
                if dest == dense_u32 {
                    self.send_dest_dense[d][s] = 0;
                } else if dest == last_u32 && dense_index != last_dense {
                    self.send_dest_dense[d][s] = dense_u32;
                }
            }
        }

        // 6. last_dense にあった Send 群が dense_index に移動したので send_lookup を更新。
        if dense_index != last_dense && dense_index < self.send_count.len() {
            let n = self.send_count[dense_index] as usize;
            for slot in 0..n {
                let sid = self.send_id[dense_index][slot];
                if sid.is_valid() && (sid.index as usize) < self.send_lookup.len() {
                    let lk = &mut self.send_lookup[sid.index as usize];
                    if lk.bus_dense == last_u32 && lk.generation == sid.generation {
                        lk.bus_dense = dense_u32;
                    }
                }
            }
        }

        true
    }

    /// 内部用: バス `bus_dense` の slot `slot` にある Send を swap-remove する。
    /// `send_lookup` の整合も維持する (除去された Send をクリア、移動された Send の slot を更新)。
    fn swap_remove_send_slot(&mut self, bus_dense: usize, slot: usize) {
        let count = self.send_count[bus_dense] as usize;
        debug_assert!(slot < count);

        // 除去対象の lookup をクリア。
        let removed_sid = self.send_id[bus_dense][slot];
        if removed_sid.is_valid() && (removed_sid.index as usize) < self.send_lookup.len() {
            let lk = &self.send_lookup[removed_sid.index as usize];
            if lk.generation == removed_sid.generation {
                self.send_lookup[removed_sid.index as usize] = SendLookup::EMPTY;
            }
        }

        let last = count - 1;
        if slot != last {
            self.send_dest_dense[bus_dense][slot] = self.send_dest_dense[bus_dense][last];
            self.send_gain[bus_dense][slot] = self.send_gain[bus_dense][last];
            self.send_position[bus_dense][slot] = self.send_position[bus_dense][last];
            self.send_dest_kind[bus_dense][slot] = self.send_dest_kind[bus_dense][last];
            self.send_id[bus_dense][slot] = self.send_id[bus_dense][last];

            // 移動した Send の lookup slot を更新。
            let moved_sid = self.send_id[bus_dense][slot];
            if moved_sid.is_valid() && (moved_sid.index as usize) < self.send_lookup.len() {
                let lk = &mut self.send_lookup[moved_sid.index as usize];
                if lk.bus_dense == bus_dense as u32 && lk.generation == moved_sid.generation {
                    lk.slot = slot as u8;
                }
            }
        }
        self.send_count[bus_dense] -= 1;
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
        if bus_dense >= self.send_count.len() {
            return false;
        }
        let count = self.send_count[bus_dense] as usize;
        if count >= MAX_SENDS_PER_BUS {
            return false;
        }
        if (id.index as usize) >= self.send_lookup.len() {
            return false;
        }
        self.send_dest_dense[bus_dense][count] = dest_dense;
        self.send_gain[bus_dense][count] = gain;
        self.send_position[bus_dense][count] = position as u8;
        self.send_dest_kind[bus_dense][count] = dest_kind as u8;
        self.send_id[bus_dense][count] = id;
        self.send_count[bus_dense] = (count + 1) as u8;
        self.send_lookup[id.index as usize] = SendLookup {
            bus_dense: bus_dense as u32,
            slot: count as u8,
            generation: id.generation,
        };
        true
    }

    /// SendId で Send を削除する。stale (generation 不一致) または未存在で `false`。
    pub fn remove_send(&mut self, id: SendId) -> bool {
        let Some((bus_dense, slot)) = self.resolve_send(id) else {
            return false;
        };
        self.swap_remove_send_slot(bus_dense, slot);
        true
    }

    /// SendId の現在位置 `(bus_dense, slot)` を返す。stale なら `None`。
    pub fn resolve_send(&self, id: SendId) -> Option<(usize, usize)> {
        if !id.is_valid() {
            return None;
        }
        let lookup_idx = id.index as usize;
        let lk = self.send_lookup.get(lookup_idx)?;
        if lk.is_empty() || lk.generation != id.generation {
            return None;
        }
        Some((lk.bus_dense as usize, lk.slot as usize))
    }

    /// SendId の gain を設定する。
    pub fn set_send_gain(&mut self, id: SendId, gain: f32) -> bool {
        if let Some((bus_dense, slot)) = self.resolve_send(id) {
            self.send_gain[bus_dense][slot] = gain;
            true
        } else {
            false
        }
    }

    /// SendId のタップ位置を変更する。
    pub fn set_send_position(&mut self, id: SendId, position: SendPosition) -> bool {
        if let Some((bus_dense, slot)) = self.resolve_send(id) {
            self.send_position[bus_dense][slot] = position as u8;
            true
        } else {
            false
        }
    }

    /// dense index 指定で send_gain を直接書き込む (Snapshot 補間で使用)。
    #[inline]
    pub fn write_send_gain_by_dense(&mut self, bus_dense: usize, slot: usize, gain: f32) {
        if let Some(arr) = self.send_gain.get_mut(bus_dense)
            && slot < MAX_SENDS_PER_BUS
        {
            arr[slot] = gain;
        }
    }

    /// dense index 指定で send_gain を読み出す (Snapshot apply 時の現在値キャプチャ用)。
    #[inline]
    #[must_use]
    pub fn send_gain_at(&self, bus_dense: usize, slot: usize) -> Option<f32> {
        self.send_gain
            .get(bus_dense)
            .and_then(|arr| arr.get(slot))
            .copied()
    }

    /// バス `bus_dense` の Send 数。
    #[inline]
    pub fn send_count_at(&self, bus_dense: usize) -> usize {
        self.send_count
            .get(bus_dense)
            .copied()
            .map(|c| c as usize)
            .unwrap_or(0)
    }

    /// バス `bus_dense` の slot `slot` にある Send 情報 `(dest_dense, gain, position, dest_kind)`。
    /// `BusSystem` の hot loop から読まれる。
    #[inline]
    pub fn send_at(&self, bus_dense: usize, slot: usize) -> (u32, f32, u8, u8) {
        (
            self.send_dest_dense[bus_dense][slot],
            self.send_gain[bus_dense][slot],
            self.send_position[bus_dense][slot],
            self.send_dest_kind[bus_dense][slot],
        )
    }
}

impl Default for BusWorld {
    fn default() -> Self {
        Self::new()
    }
}
