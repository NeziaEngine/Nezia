use crate::source::MAX_SOURCES;

/// 距離減衰モデル。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AttenuationModel {
    /// 減衰なし（音量変化しない）。
    None = 0,
    /// 線形減衰: `gain = 1 - rolloff * (dist - min) / (max - min)`
    Linear = 1,
    /// 逆距離減衰（OpenAL AL_INVERSE_DISTANCE_CLAMPED 相当）:
    /// `gain = min / (min + rolloff * (dist - min))`
    InverseDistance = 2,
    /// 指数減衰: `gain = (dist / min) ^ (-rolloff)`
    Exponential = 3,
}

/// リスナーの状態。
///
/// `right` は `forward` と `up` から派生する。
/// `update()` を呼ぶと同時に再計算される。
///
/// SP-06: フォーカスポイント補間係数を持つ。距離減衰用とパンニング用で
/// 独立した補間係数を取り、空間演算では仮想リスナー位置
/// `lerp(position, focus_point, *_focus_level)` を使用する。
#[derive(Clone, Copy)]
pub struct ListenerState {
    pub position: [f32; 3],
    /// 正規化済み前方ベクトル。
    pub forward: [f32; 3],
    /// 正規化済み上方ベクトル。
    pub up: [f32; 3],
    /// 派生値: `normalize(cross(forward, up))`。`update()` 時に自動更新。
    pub right: [f32; 3],

    /// SP-06: フォーカスポイント（ワールド空間）。
    /// `*_focus_level` が 0 の場合は使用されない。
    pub focus_point: [f32; 3],
    /// SP-06: 距離減衰計算用の補間係数 `[0.0, 1.0]`。
    /// 0.0 でリスナー位置のみ使用、1.0 でフォーカス点完全採用。
    pub distance_focus_level: f32,
    /// SP-06: パンニング計算用の補間係数 `[0.0, 1.0]`。
    pub direction_focus_level: f32,
}

impl Default for ListenerState {
    fn default() -> Self {
        Self {
            position: [0.0, 0.0, 0.0],
            forward: [0.0, 0.0, -1.0],
            up: [0.0, 1.0, 0.0],
            right: [1.0, 0.0, 0.0],
            focus_point: [0.0, 0.0, 0.0],
            distance_focus_level: 0.0,
            direction_focus_level: 0.0,
        }
    }
}

impl ListenerState {
    /// リスナーの位置・向きを更新する。`right` は自動計算される。
    /// フォーカスポイント関連フィールドは変更しない。
    pub fn update(&mut self, position: [f32; 3], forward: [f32; 3], up: [f32; 3]) {
        self.position = position;
        self.forward = vec3_normalize(forward);
        self.up = vec3_normalize(up);
        self.right = vec3_normalize(vec3_cross(self.forward, self.up));
    }

    /// SP-06: フォーカスポイントと補間係数を設定する。
    /// 補間係数は `[0.0, 1.0]` にクランプされる。
    pub fn set_focus(
        &mut self,
        focus_point: [f32; 3],
        distance_focus_level: f32,
        direction_focus_level: f32,
    ) {
        self.focus_point = focus_point;
        self.distance_focus_level = distance_focus_level.clamp(0.0, 1.0);
        self.direction_focus_level = direction_focus_level.clamp(0.0, 1.0);
    }

    /// SP-06: 距離減衰計算に使う仮想リスナー位置を返す。
    #[inline]
    pub fn virtual_position_for_distance(&self) -> [f32; 3] {
        lerp3(self.position, self.focus_point, self.distance_focus_level)
    }

    /// SP-06: パンニング計算に使う仮想リスナー位置を返す。
    #[inline]
    pub fn virtual_position_for_direction(&self) -> [f32; 3] {
        lerp3(self.position, self.focus_point, self.direction_focus_level)
    }
}

#[inline]
fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

pub(super) fn vec3_cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub(super) fn vec3_normalize(v: [f32; 3]) -> [f32; 3] {
    let len_sq = v[0] * v[0] + v[1] * v[1] + v[2] * v[2];
    if len_sq < 1e-12 {
        return v;
    }
    let inv_len = 1.0 / len_sq.sqrt();
    [v[0] * inv_len, v[1] * inv_len, v[2] * inv_len]
}

/// 3D 空間コンポーネントのワールド。
///
/// `SourceWorld` と常に同じ長さの密配列を保持する（同じ dense_index で対応する）。
/// `SourceWorld` がスポーン/デスポーンするたびに `push_defaults()` / `swap_remove()` を
/// 同時に呼び出すことで同期を保つ。
///
/// `SpatialSystem::compute_gains()` が事前計算した `left_gains` / `right_gains` を
/// `SourceMixingSystem` が参照してミキシングする。
pub struct SpatialWorld {
    // ── 密配列: 空間コンポーネント（SIMD 対応のため x/y/z を分離） ──
    pub(super) positions_x: Vec<f32>,
    pub(super) positions_y: Vec<f32>,
    pub(super) positions_z: Vec<f32>,
    pub(super) attenuation_models: Vec<AttenuationModel>,
    pub(super) min_distances: Vec<f32>,
    pub(super) max_distances: Vec<f32>,
    pub(super) rolloff_factors: Vec<f32>,
    pub(super) spatial_enabled: Vec<bool>,

    /// 事前計算済み L チャンネルゲイン（`SpatialSystem::compute_gains()` が書き込む）。
    pub left_gains: Vec<f32>,
    /// 事前計算済み R チャンネルゲイン（`SpatialSystem::compute_gains()` が書き込む）。
    pub right_gains: Vec<f32>,

    /// リスナー（シングルトン）。
    pub listener: ListenerState,
}

impl Default for SpatialWorld {
    fn default() -> Self {
        Self::new()
    }
}

impl SpatialWorld {
    pub fn new() -> Self {
        Self {
            positions_x: Vec::with_capacity(MAX_SOURCES),
            positions_y: Vec::with_capacity(MAX_SOURCES),
            positions_z: Vec::with_capacity(MAX_SOURCES),
            attenuation_models: Vec::with_capacity(MAX_SOURCES),
            min_distances: Vec::with_capacity(MAX_SOURCES),
            max_distances: Vec::with_capacity(MAX_SOURCES),
            rolloff_factors: Vec::with_capacity(MAX_SOURCES),
            spatial_enabled: Vec::with_capacity(MAX_SOURCES),
            left_gains: Vec::with_capacity(MAX_SOURCES),
            right_gains: Vec::with_capacity(MAX_SOURCES),
            listener: ListenerState::default(),
        }
    }

    /// `SourceWorld` がソースをスポーンしたタイミングで呼び出す。
    /// デフォルト値でエントリを末尾に追加する。`spatial_enabled = false` がデフォルトなので
    /// 既存の 2D ソースには影響しない。
    pub fn push_defaults(&mut self) {
        self.positions_x.push(0.0);
        self.positions_y.push(0.0);
        self.positions_z.push(0.0);
        self.attenuation_models
            .push(AttenuationModel::InverseDistance);
        self.min_distances.push(1.0);
        self.max_distances.push(500.0);
        self.rolloff_factors.push(1.0);
        self.spatial_enabled.push(false);
        self.left_gains.push(0.0);
        self.right_gains.push(0.0);
    }

    /// `SourceWorld` がデスポーンしたタイミングで呼び出す（swap-remove）。
    pub fn swap_remove(&mut self, dense_index: usize) {
        if dense_index >= self.positions_x.len() {
            return;
        }
        self.positions_x.swap_remove(dense_index);
        self.positions_y.swap_remove(dense_index);
        self.positions_z.swap_remove(dense_index);
        self.attenuation_models.swap_remove(dense_index);
        self.min_distances.swap_remove(dense_index);
        self.max_distances.swap_remove(dense_index);
        self.rolloff_factors.swap_remove(dense_index);
        self.spatial_enabled.swap_remove(dense_index);
        self.left_gains.swap_remove(dense_index);
        self.right_gains.swap_remove(dense_index);
    }

    // ── 空間コンポーネント setter ──

    /// 距離減衰パラメータを設定する（密配列インデックスで指定）。
    pub fn set_params(
        &mut self,
        dense_index: usize,
        model: AttenuationModel,
        min_distance: f32,
        max_distance: f32,
        rolloff: f32,
    ) {
        self.attenuation_models[dense_index] = model;
        self.min_distances[dense_index] = min_distance;
        self.max_distances[dense_index] = max_distance;
        self.rolloff_factors[dense_index] = rolloff;
    }

    /// 空間演算の有効/無効を設定する（密配列インデックスで指定）。
    pub fn set_enabled(&mut self, dense_index: usize, enabled: bool) {
        self.spatial_enabled[dense_index] = enabled;
    }

    /// ソースのワールド座標を設定する（密配列インデックスで指定）。
    pub fn set_position(&mut self, dense_index: usize, position: [f32; 3]) {
        self.positions_x[dense_index] = position[0];
        self.positions_y[dense_index] = position[1];
        self.positions_z[dense_index] = position[2];
    }
}
