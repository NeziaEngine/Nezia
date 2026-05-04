//! Snapshot のデータ表現 + ランタイム補間状態。
//!
//! - `Snapshot`: 不変。ターゲット値の集合 (メインスレッドが builder で構築、registry に登録)。
//! - `ActiveSnapshot`: ミュータブル。サウンドスレッドが保持する進行中の補間状態。
//!   `Snapshot` を apply 時に展開し、各エントリの from/to/dense_index を resolve した形で持つ。

use crate::effect::EffectId;
use crate::entity::EntityId;

/// Snapshot に含めるエフェクト種別 (DSP モジュールの `EffectKind` と 1:1)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SnapshotEffectKind {
    Lpf = 0,
    Hpf = 1,
    Reverb = 2,
}

/// バスゲインの target 指定。
#[derive(Debug, Clone, Copy)]
pub struct BusGainEntry {
    pub bus: EntityId,
    pub gain: f32,
}

/// バスミュートの target 指定。bool は `t >= 0.5` でスナップ。
#[derive(Debug, Clone, Copy)]
pub struct BusMutedEntry {
    pub bus: EntityId,
    pub muted: bool,
}

/// エフェクトパラメータの target 指定。
/// `param` は `LpfParam` / `HpfParam` / `ReverbParam` の `as u8`。
#[derive(Debug, Clone, Copy)]
pub struct EffectParamEntry {
    pub effect: EffectId,
    pub kind: SnapshotEffectKind,
    pub param: u8,
    pub value: f32,
}

/// 不変 Snapshot。`SnapshotRegistry` 経由でサウンドスレッドへ共有される。
///
/// エントリは `Vec` で持つ (固定長にする利点が薄く、典型的に数十エントリで十分)。
/// サウンドスレッドは apply 時に 1 度だけ全エントリを走査して `ActiveSnapshot`
/// に展開するため、Vec 走査コストはレアパスにしか乗らない。
pub struct Snapshot {
    pub bus_gains: Vec<BusGainEntry>,
    pub bus_muted: Vec<BusMutedEntry>,
    pub effect_params: Vec<EffectParamEntry>,
}

impl Snapshot {
    pub fn new() -> Self {
        Self {
            bus_gains: Vec::new(),
            bus_muted: Vec::new(),
            effect_params: Vec::new(),
        }
    }
}

impl Default for Snapshot {
    fn default() -> Self {
        Self::new()
    }
}

/// サウンドスレッドが保持する進行中の補間状態。
///
/// `apply_snapshot` 時に `Snapshot` の全エントリを resolve + capture し、
/// SoA レイアウトの `Vec<...>` に展開する。以降 fade 完了まで毎コールバックで
/// 走査して値を BusWorld / LpfWorld / HpfWorld / ReverbWorld に書き戻す。
///
/// **エントリは「ID 解決済み + from 値キャプチャ済み」の中間表現**。
/// 元 Snapshot を保持しないため、apply 後は registry から該当 Snapshot が
/// destroy されても影響しない。
pub struct ActiveSnapshot {
    // ── バスゲイン補間 ──
    /// バス dense index。
    pub bus_gain_dense: Vec<u32>,
    /// fade 開始時点のゲイン (= apply 時の現在値)。
    pub bus_gain_from: Vec<f32>,
    /// ターゲットゲイン。
    pub bus_gain_to: Vec<f32>,

    // ── バスミュート (bool) ──
    /// バス dense index。
    pub bus_muted_dense: Vec<u32>,
    /// ターゲット muted 値 (bool 補間ではなく `t >= 0.5` でスナップ)。
    pub bus_muted_to: Vec<bool>,
    /// 適用済みフラグ (二度書きを避ける)。
    pub bus_muted_applied: Vec<bool>,

    // ── エフェクトパラメータ ──
    /// 種別。`SnapshotEffectKind`。
    pub effect_kind: Vec<SnapshotEffectKind>,
    /// 種別 World 内 dense index (resolve 済み)。
    pub effect_state_dense: Vec<u32>,
    /// パラメータ index (`LpfParam` 等の `as u8`)。
    pub effect_param: Vec<u8>,
    /// fade 開始時点のパラメータ値。
    pub effect_from: Vec<f32>,
    /// ターゲット値。
    pub effect_to: Vec<f32>,

    // ── fade 進行 ──
    /// fade 全長 (サンプル単位)。0 のときは即時適用 + ActiveSnapshot::clear。
    pub fade_total_samples: u64,
    /// 残り fade サンプル数。0 で完了。
    pub fade_remaining_samples: u64,
}

impl ActiveSnapshot {
    pub fn new() -> Self {
        Self {
            bus_gain_dense: Vec::new(),
            bus_gain_from: Vec::new(),
            bus_gain_to: Vec::new(),
            bus_muted_dense: Vec::new(),
            bus_muted_to: Vec::new(),
            bus_muted_applied: Vec::new(),
            effect_kind: Vec::new(),
            effect_state_dense: Vec::new(),
            effect_param: Vec::new(),
            effect_from: Vec::new(),
            effect_to: Vec::new(),
            fade_total_samples: 0,
            fade_remaining_samples: 0,
        }
    }

    /// 進行中の補間があれば true。
    #[inline]
    pub fn is_active(&self) -> bool {
        self.fade_remaining_samples > 0 || self.has_pending_bool_changes()
    }

    #[inline]
    fn has_pending_bool_changes(&self) -> bool {
        self.bus_muted_applied.iter().any(|&applied| !applied)
    }

    /// 全エントリをクリアする (新しい snapshot apply 時に呼ぶ)。
    pub fn clear(&mut self) {
        self.bus_gain_dense.clear();
        self.bus_gain_from.clear();
        self.bus_gain_to.clear();
        self.bus_muted_dense.clear();
        self.bus_muted_to.clear();
        self.bus_muted_applied.clear();
        self.effect_kind.clear();
        self.effect_state_dense.clear();
        self.effect_param.clear();
        self.effect_from.clear();
        self.effect_to.clear();
        self.fade_total_samples = 0;
        self.fade_remaining_samples = 0;
    }
}

impl Default for ActiveSnapshot {
    fn default() -> Self {
        Self::new()
    }
}
