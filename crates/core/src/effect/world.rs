use crate::core::sparse_set::SparseSet;
use crate::entity::EntityId;

use super::{EffectId, MAX_EFFECTS};

/// 公開: 論理エフェクト種別。
///
/// 物理アルゴリズム (`*Algo`) は内部で隠蔽し、Phase 6+ の自動切替に備える。
/// 詳細は `docs/design/core/dsp.md` を参照。
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectKind {
    Lpf = 0,
    Hpf = 1,
    Reverb = 2,
    /// Phase 3-3: ピーク検波 + soft knee + attack/release コンプレッサー。
    /// Sidechain 入力 (`bind_compressor_sidechain` で他バスからの Send を紐付け) で
    /// ダッキング動作する。Bus 専用。
    Compressor = 3,
}

/// エフェクトを取り付ける対象。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectTarget {
    Bus(EntityId),
    Source(EntityId),
}

/// チェーン内の挿入位置。
///
/// - `Bus`: `Pre` = Pre-Fader (gain 適用前) / `Post` = Post-Fader
/// - `Source`: `Pre` = Pre-Spatial (モノラル) / `Post` = Post-Spatial (ステレオ)
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectPosition {
    Pre = 0,
    Post = 1,
}

/// メタ層の owner 多態。dense index で保持。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Bus(u32),
    Source(u32),
}

/// メタ層エフェクト World。
///
/// 各エフェクトの「種別 / 物理アルゴリズム / 所属 / チェーン位置 / enabled / 種別 World 内 dense」を
/// 薄いメタ SoA で保持する。実際の DSP 状態は種別ごとの `*World` (LpfWorld 等) が持つ。
pub struct EffectWorld {
    entities: SparseSet,

    pub(super) kind: Vec<EffectKind>,
    /// 物理アルゴリズム index。Phase 2-3 では各種別 1 アルゴリズムなので常に 0。
    pub(super) algo: Vec<u8>,
    pub(super) owner: Vec<Owner>,
    pub(super) position: Vec<EffectPosition>,
    pub(super) slot_index: Vec<u8>,
    pub(super) enabled: Vec<bool>,
    /// 種別別 World における dense index。種別 World 側 swap-remove 時に再マップされる。
    pub(super) state_index: Vec<u32>,
}

impl Default for EffectWorld {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)] // 一部 API は外部クレート (FFI / 統合テスト) からのみ使用。
impl EffectWorld {
    pub fn new() -> Self {
        Self {
            entities: SparseSet::new(MAX_EFFECTS),
            kind: Vec::with_capacity(MAX_EFFECTS),
            algo: Vec::with_capacity(MAX_EFFECTS),
            owner: Vec::with_capacity(MAX_EFFECTS),
            position: Vec::with_capacity(MAX_EFFECTS),
            slot_index: Vec::with_capacity(MAX_EFFECTS),
            enabled: Vec::with_capacity(MAX_EFFECTS),
            state_index: Vec::with_capacity(MAX_EFFECTS),
        }
    }

    pub fn len(&self) -> usize {
        self.entities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    pub fn contains(&self, id: EffectId) -> bool {
        self.entities.contains(id)
    }

    pub fn resolve(&self, id: EffectId) -> Option<usize> {
        self.entities.resolve(id)
    }

    /// 事前発行された EffectId でエフェクトを生成する (`SpawnEffect` コマンド経由)。
    /// `state_index` は呼び出し側 (種別 World 側) が値を確定させてから渡す。
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_with_id(
        &mut self,
        id: EffectId,
        kind: EffectKind,
        algo: u8,
        owner: Owner,
        position: EffectPosition,
        slot_index: u8,
        state_index: u32,
    ) -> bool {
        if self.entities.alloc_with_id(id).is_none() {
            return false;
        }
        self.kind.push(kind);
        self.algo.push(algo);
        self.owner.push(owner);
        self.position.push(position);
        self.slot_index.push(slot_index);
        self.enabled.push(true);
        self.state_index.push(state_index);
        true
    }

    /// EffectId を指定して swap-remove。種別 World 側の同期は呼び出し側の責務。
    /// 戻り値: 削除されたエフェクトの (kind, state_index)。
    pub fn despawn(&mut self, id: EffectId) -> Option<(EffectKind, u32)> {
        let dense = self.entities.dealloc(id)?;
        let kind = self.kind.swap_remove(dense);
        let _algo = self.algo.swap_remove(dense);
        let _owner = self.owner.swap_remove(dense);
        let _position = self.position.swap_remove(dense);
        let _slot = self.slot_index.swap_remove(dense);
        let _enabled = self.enabled.swap_remove(dense);
        let state_index = self.state_index.swap_remove(dense);
        Some((kind, state_index))
    }

    /// メタ層の `state_index` を再マップする (種別 World 側 swap-remove 後に呼ぶ)。
    /// `kind` 種別の `state_index == old` のエントリを `new` に書き換える。
    pub fn remap_state_index(&mut self, kind: EffectKind, old: u32, new: u32) {
        for i in 0..self.kind.len() {
            if self.kind[i] == kind && self.state_index[i] == old {
                self.state_index[i] = new;
                return;
            }
        }
    }

    pub fn enabled(&self, id: EffectId) -> Option<bool> {
        self.resolve(id).map(|i| self.enabled[i])
    }

    pub fn set_enabled(&mut self, id: EffectId, value: bool) -> bool {
        if let Some(i) = self.resolve(id) {
            self.enabled[i] = value;
            true
        } else {
            false
        }
    }

    pub fn kind(&self, id: EffectId) -> Option<EffectKind> {
        self.resolve(id).map(|i| self.kind[i])
    }

    pub fn state_index(&self, id: EffectId) -> Option<u32> {
        self.resolve(id).map(|i| self.state_index[i])
    }

    /// SoA 一括スライス (apply_chain ホットループ用)。
    pub fn kinds(&self) -> &[EffectKind] {
        &self.kind
    }
    pub fn algos(&self) -> &[u8] {
        &self.algo
    }
    pub fn owners(&self) -> &[Owner] {
        &self.owner
    }
    pub fn positions(&self) -> &[EffectPosition] {
        &self.position
    }
    pub fn enableds(&self) -> &[bool] {
        &self.enabled
    }
    pub fn state_indices(&self) -> &[u32] {
        &self.state_index
    }
}
