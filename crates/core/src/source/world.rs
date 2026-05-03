use crate::core::sparse_set::SparseSet;
use crate::effect::{EffectId, MAX_EFFECTS_PER_SOURCE};
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
    /// コールバックトークン。0 = コールバックなし。
    pub token: u32,
    /// ループ再生フラグ。`true` の場合、バッファ末尾到達時に先頭へ巻き戻す。
    pub looping: bool,
    /// Voice Virtualization 用優先度。Unity `AudioSource.priority` 互換 (0..255、低いほど高優先)。
    /// 既定値 128 (Unity と一致)。
    pub priority: u8,
}

impl Default for SourceComponent {
    fn default() -> Self {
        Self {
            vol: 1.0,
            pitch: 1.0,
            sample_offset: 0.0,
            audio_buffer_index: 0,
            output_bus: 0,
            token: 0,
            looping: false,
            priority: 128,
        }
    }
}

/// Source の再生状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceState {
    /// 再生中。
    Playing,
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
    // ── エンティティ管理 ──
    entities: SparseSet,

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
    /// コールバックトークン。0 = コールバックなし。
    pub(super) token: Vec<u32>,
    /// ループ再生フラグ。
    pub(super) looping: Vec<bool>,
    /// Voice Virtualization 用優先度 (0..255, 低いほど高優先, Unity 互換)。
    pub(super) priority: Vec<u8>,
    /// Voice Virtualization タグ。`true` のソースはミキシング段でスキップされ、
    /// `sample_offset` だけ前進する (時間同期維持)。
    /// 毎フレーム冒頭の rebalance で再評価される。
    pub(super) is_virtual: Vec<bool>,

    // ── DSP Pre-Spatial エフェクトチェーン (Phase 2-3) ──
    /// Pre-Spatial エフェクトチェーン (resampler 後、Spatial 適用前のモノラル信号に作用)。
    pub(super) pre_chain: Vec<[EffectId; MAX_EFFECTS_PER_SOURCE]>,
    pub(super) pre_count: Vec<u8>,
}

impl Default for SourceWorld {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
impl SourceWorld {
    pub fn new() -> Self {
        Self {
            entities: SparseSet::new(MAX_SOURCES),
            vol: Vec::with_capacity(MAX_SOURCES),
            pitch: Vec::with_capacity(MAX_SOURCES),
            sample_offset: Vec::with_capacity(MAX_SOURCES),
            audio_buffer_index: Vec::with_capacity(MAX_SOURCES),
            state: Vec::with_capacity(MAX_SOURCES),
            output_bus: Vec::with_capacity(MAX_SOURCES),
            token: Vec::with_capacity(MAX_SOURCES),
            looping: Vec::with_capacity(MAX_SOURCES),
            priority: Vec::with_capacity(MAX_SOURCES),
            is_virtual: Vec::with_capacity(MAX_SOURCES),
            pre_chain: Vec::with_capacity(MAX_SOURCES),
            pre_count: Vec::with_capacity(MAX_SOURCES),
        }
    }

    /// Source を追加し、EntityId を返す（fire-and-forget 用）。
    ///
    /// `MAX_SOURCES` に達している場合は `None` を返す。
    pub fn spawn(&mut self, params: SourceComponent) -> Option<EntityId> {
        let (id, _dense) = self.entities.alloc()?;
        self.vol.push(params.vol);
        self.pitch.push(params.pitch);
        self.sample_offset.push(params.sample_offset);
        self.audio_buffer_index.push(params.audio_buffer_index);
        self.state.push(SourceState::Playing);
        self.output_bus.push(params.output_bus);
        self.token.push(params.token);
        self.looping.push(params.looping);
        self.priority.push(params.priority);
        // 仮想化判定は次のフレーム冒頭で行う。spawn 時点では物理として登録。
        self.is_virtual.push(false);
        self.pre_chain.push(
            [EffectId {
                index: 0,
                generation: 0,
            }; MAX_EFFECTS_PER_SOURCE],
        );
        self.pre_count.push(0);
        Some(id)
    }

    /// 事前割り当てされた EntityId を使って Source をスポーンする（3D ソース用）。
    ///
    /// 同じ index が既に使用中の場合は `false` を返す。
    /// メインスレッドが EntityId を事前発行し、`SpawnSource` コマンドで渡す想定。
    pub fn spawn_with_id(&mut self, id: EntityId, params: SourceComponent) -> bool {
        let Some(_dense) = self.entities.alloc_with_id(id) else {
            return false;
        };
        self.vol.push(params.vol);
        self.pitch.push(params.pitch);
        self.sample_offset.push(params.sample_offset);
        self.audio_buffer_index.push(params.audio_buffer_index);
        self.state.push(SourceState::Playing);
        self.output_bus.push(params.output_bus);
        self.token.push(params.token);
        self.looping.push(params.looping);
        self.priority.push(params.priority);
        self.is_virtual.push(false);
        self.pre_chain.push(
            [EffectId {
                index: 0,
                generation: 0,
            }; MAX_EFFECTS_PER_SOURCE],
        );
        self.pre_count.push(0);
        true
    }

    /// EntityId を検証し、有効なら密配列インデックスを返す。
    pub fn resolve(&self, id: EntityId) -> Option<usize> {
        self.entities.resolve(id)
    }

    /// Source を削除する（swap-remove）。
    pub fn despawn(&mut self, id: EntityId) -> bool {
        let Some(dense_index) = self.entities.dealloc(id) else {
            return false;
        };
        self.vol.swap_remove(dense_index);
        self.pitch.swap_remove(dense_index);
        self.sample_offset.swap_remove(dense_index);
        self.audio_buffer_index.swap_remove(dense_index);
        self.state.swap_remove(dense_index);
        self.output_bus.swap_remove(dense_index);
        self.token.swap_remove(dense_index);
        self.looping.swap_remove(dense_index);
        self.priority.swap_remove(dense_index);
        self.is_virtual.swap_remove(dense_index);
        self.pre_chain.swap_remove(dense_index);
        self.pre_count.swap_remove(dense_index);
        true
    }

    /// EntityId が有効か確認する。
    pub fn contains(&self, id: EntityId) -> bool {
        self.entities.contains(id)
    }

    /// 現在の Source 数。
    pub fn len(&self) -> usize {
        self.entities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
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

    pub fn looping(&self, id: EntityId) -> Option<bool> {
        self.resolve(id).map(|i| self.looping[i])
    }

    pub fn set_looping(&mut self, id: EntityId, value: bool) -> bool {
        if let Some(i) = self.resolve(id) {
            self.looping[i] = value;
            true
        } else {
            false
        }
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

    // ── 一括アクセス（密配列スライス） ──

    pub fn vols(&self) -> &[f32] {
        &self.vol
    }

    pub fn vols_mut(&mut self) -> &mut [f32] {
        &mut self.vol
    }

    /// 密配列の (entity, sample_offset) を順に走査するイテレータ。
    pub fn snapshots(&self) -> impl Iterator<Item = (EntityId, f32)> + '_ {
        let len = self.sample_offset.len();
        (0..len).filter_map(move |dense| {
            let id = self.entities.entity_at_dense(dense)?;
            Some((id, self.sample_offset[dense]))
        })
    }

    /// dense index に対応する EntityId を取得する。
    pub fn entity_at_dense(&self, dense_index: usize) -> Option<EntityId> {
        self.entities.entity_at_dense(dense_index)
    }

    /// volume / pitch の dense 配列に直接書き込む。
    ///
    /// `apply_live_params` 等で SoA 一括反映する際に使う。
    pub fn write_vol(&mut self, dense_index: usize, value: f32) {
        if dense_index < self.vol.len() {
            self.vol[dense_index] = value;
        }
    }

    pub fn write_pitch(&mut self, dense_index: usize, value: f32) {
        if dense_index < self.pitch.len() {
            self.pitch[dense_index] = value;
        }
    }

    /// 密配列インデックスを指定して Source を削除する（swap-remove）。
    ///
    /// `SourceMixingSystem::update()` が再生終了した Source を直接削除するために使用する。
    /// 逆順で呼び出すこと（swap-remove のため後ろから消さないとインデックスがずれる）。
    pub(super) fn despawn_by_dense_index(&mut self, dense_index: usize) {
        if !self.entities.dealloc_by_dense_index(dense_index) {
            return;
        }
        self.vol.swap_remove(dense_index);
        self.pitch.swap_remove(dense_index);
        self.sample_offset.swap_remove(dense_index);
        self.audio_buffer_index.swap_remove(dense_index);
        self.state.swap_remove(dense_index);
        self.output_bus.swap_remove(dense_index);
        self.token.swap_remove(dense_index);
        self.looping.swap_remove(dense_index);
        self.priority.swap_remove(dense_index);
        self.is_virtual.swap_remove(dense_index);
        self.pre_chain.swap_remove(dense_index);
        self.pre_count.swap_remove(dense_index);
    }

    // ── Voice Virtualization アクセサ ────────────────────────────────

    pub fn priority(&self, id: EntityId) -> Option<u8> {
        self.resolve(id).map(|i| self.priority[i])
    }

    pub fn set_priority(&mut self, id: EntityId, value: u8) -> bool {
        if let Some(i) = self.resolve(id) {
            self.priority[i] = value;
            true
        } else {
            false
        }
    }

    pub fn is_virtual(&self, id: EntityId) -> Option<bool> {
        self.resolve(id).map(|i| self.is_virtual[i])
    }

    /// SoA 一括アクセス (`SourceMixingSystem` / rebalance 用)。
    pub fn priorities(&self) -> &[u8] {
        &self.priority
    }
    pub fn is_virtuals(&self) -> &[bool] {
        &self.is_virtual
    }
    pub fn is_virtuals_mut(&mut self) -> &mut [bool] {
        &mut self.is_virtual
    }
    pub fn states(&self) -> &[SourceState] {
        &self.state
    }
    pub fn pitches(&self) -> &[f32] {
        &self.pitch
    }
    pub fn output_buses(&self) -> &[u32] {
        &self.output_bus
    }

    // ── DSP Pre-Spatial エフェクトチェーン操作 (Phase 2-3) ──

    /// ソースの Pre-Spatial チェーン末尾に EffectId を追加する。
    /// 戻り値: 挿入された slot index。チェーン満杯時は `None`。
    pub fn push_pre_effect(&mut self, source_dense: usize, eff: EffectId) -> Option<u8> {
        if source_dense >= self.pre_count.len() {
            return None;
        }
        let idx = self.pre_count[source_dense] as usize;
        if idx >= MAX_EFFECTS_PER_SOURCE {
            return None;
        }
        self.pre_chain[source_dense][idx] = eff;
        self.pre_count[source_dense] += 1;
        Some(idx as u8)
    }

    /// `eff` をチェーンから削除し後続を詰める。見つかれば `true`。
    pub fn remove_pre_effect(&mut self, source_dense: usize, eff: EffectId) -> bool {
        if source_dense >= self.pre_count.len() {
            return false;
        }
        let n = self.pre_count[source_dense] as usize;
        let chain = &mut self.pre_chain[source_dense];
        for i in 0..n {
            if chain[i] == eff {
                for j in i..n - 1 {
                    chain[j] = chain[j + 1];
                }
                self.pre_count[source_dense] -= 1;
                return true;
            }
        }
        false
    }

    pub fn pre_chain_slice(&self, source_dense: usize) -> &[EffectId] {
        let n = self.pre_count[source_dense] as usize;
        &self.pre_chain[source_dense][..n]
    }
}
