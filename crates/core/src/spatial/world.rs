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
    /// Custom Attenuation Curve (Phase 3-1)。
    /// `SpatialWorld.curve_indices[i]` の curve registry slot を参照し、
    /// `t = (dist - min) / (max - min)` で正規化距離をカーブから線形補間サンプリングしてゲインを得る。
    /// `rolloff` は使用しない (Custom 専用に定義された curve が形状を完全に決める)。
    Custom = 4,
}

/// リスナーの状態。
///
/// `right` は `up` と `forward` から派生する（左手系 Y-up）。
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
    /// 派生値: `normalize(cross(up, forward))`（左手系）。`update()` 時に自動更新。
    pub right: [f32; 3],

    /// SP-06: フォーカスポイント（ワールド空間）。
    /// `*_focus_level` が 0 の場合は使用されない。
    pub focus_point: [f32; 3],
    /// SP-06: 距離減衰計算用の補間係数 `[0.0, 1.0]`。
    /// 0.0 でリスナー位置のみ使用、1.0 でフォーカス点完全採用。
    pub distance_focus_level: f32,
    /// SP-06: パンニング計算用の補間係数 `[0.0, 1.0]`。
    pub direction_focus_level: f32,

    /// SP-10: リスナーの速度ベクトル（m/s）。Doppler 計算に使用。
    /// 既定値 [0,0,0] では Doppler 効果は発生しない。
    pub velocity: [f32; 3],
}

impl Default for ListenerState {
    fn default() -> Self {
        Self {
            position: [0.0, 0.0, 0.0],
            forward: [0.0, 0.0, 1.0],
            up: [0.0, 1.0, 0.0],
            right: [1.0, 0.0, 0.0],
            focus_point: [0.0, 0.0, 0.0],
            distance_focus_level: 0.0,
            direction_focus_level: 0.0,
            velocity: [0.0, 0.0, 0.0],
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
        self.right = vec3_normalize(vec3_cross(self.up, self.forward));
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
    /// Phase 3-1: `AttenuationModel::Custom` のときに参照する curve registry の slot index。
    /// `CURVE_INDEX_NONE` (= u32::MAX) のとき未指定 (Custom 指定時はゲイン 0 にフォールバック)。
    /// 他のモデルでは無視される。
    pub(super) curve_indices: Vec<u32>,
    pub(super) spatial_enabled: Vec<bool>,

    // ── SP-10 Doppler: 速度 SoA (m/s)。既定値は [0,0,0]（効果なし） ──
    pub(super) velocities_x: Vec<f32>,
    pub(super) velocities_y: Vec<f32>,
    pub(super) velocities_z: Vec<f32>,
    /// SP-10: ソースごとの Doppler 効果係数 `[0.0, 1.0]`（Unity `AudioSource.dopplerLevel` 互換）。
    /// 0.0 で Doppler 無効、1.0 で完全適用。中間値は速度成分を線形スケール。
    pub(super) doppler_levels: Vec<f32>,

    /// 事前計算済み L チャンネルゲイン（`SpatialSystem::compute_gains()` が書き込む）。
    pub left_gains: Vec<f32>,
    /// 事前計算済み R チャンネルゲイン（`SpatialSystem::compute_gains()` が書き込む）。
    pub right_gains: Vec<f32>,
    /// SP-10: 事前計算済み Doppler ピッチ倍率（`SpatialSystem::compute_gains()` が書き込む）。
    /// `SourceMixingSystem` 側で `pitch * doppler_pitches[i]` を再生レートに反映する。
    /// `spatial_enabled = false` または `doppler_level = 0.0` の場合は常に `1.0`。
    pub doppler_pitches: Vec<f32>,

    /// リスナー（シングルトン）。
    pub listener: ListenerState,

    /// SP-10: 媒質中の音速（m/s）。既定値 343.0（Unity 互換）。
    pub sound_speed: f32,
}

/// 既定音速（m/s, 空気中・常温）。Unity `AudioSettings.speedOfSound` のデフォルトと一致。
pub const DEFAULT_SOUND_SPEED: f32 = 343.0;

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
            curve_indices: Vec::with_capacity(MAX_SOURCES),
            spatial_enabled: Vec::with_capacity(MAX_SOURCES),
            velocities_x: Vec::with_capacity(MAX_SOURCES),
            velocities_y: Vec::with_capacity(MAX_SOURCES),
            velocities_z: Vec::with_capacity(MAX_SOURCES),
            doppler_levels: Vec::with_capacity(MAX_SOURCES),
            left_gains: Vec::with_capacity(MAX_SOURCES),
            right_gains: Vec::with_capacity(MAX_SOURCES),
            doppler_pitches: Vec::with_capacity(MAX_SOURCES),
            listener: ListenerState::default(),
            sound_speed: DEFAULT_SOUND_SPEED,
        }
    }

    /// 現在保持している spatial エントリ数 (= `SourceWorld::len()` と一致)。
    #[must_use]
    pub fn len(&self) -> usize {
        self.positions_x.len()
    }

    /// `len() == 0` を返す (clippy::len_without_is_empty 対策)。
    #[must_use]
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.positions_x.is_empty()
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
        self.curve_indices.push(super::CURVE_INDEX_NONE);
        self.spatial_enabled.push(false);
        self.velocities_x.push(0.0);
        self.velocities_y.push(0.0);
        self.velocities_z.push(0.0);
        self.doppler_levels.push(1.0);
        self.left_gains.push(0.0);
        self.right_gains.push(0.0);
        self.doppler_pitches.push(1.0);
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
        self.curve_indices.swap_remove(dense_index);
        self.spatial_enabled.swap_remove(dense_index);
        self.velocities_x.swap_remove(dense_index);
        self.velocities_y.swap_remove(dense_index);
        self.velocities_z.swap_remove(dense_index);
        self.doppler_levels.swap_remove(dense_index);
        self.left_gains.swap_remove(dense_index);
        self.right_gains.swap_remove(dense_index);
        self.doppler_pitches.swap_remove(dense_index);
    }

    // ── 空間コンポーネント setter ──

    /// 距離減衰パラメータを設定する（密配列インデックスで指定）。
    /// `model = Custom` の場合 `rolloff` は無視され、`set_curve_index` で別途指定した
    /// curve registry slot からゲインを読む。
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

    /// Phase 3-1: Custom Attenuation Curve を参照する curve registry slot index を設定する。
    /// `super::CURVE_INDEX_NONE` で「未指定」(Custom 指定時はゲイン 0 にフォールバック)。
    pub fn set_curve_index(&mut self, dense_index: usize, curve_index: u32) {
        self.curve_indices[dense_index] = curve_index;
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

    /// SP-10: ソースの速度ベクトル (m/s) を設定する（密配列インデックスで指定）。
    pub fn set_velocity(&mut self, dense_index: usize, velocity: [f32; 3]) {
        self.velocities_x[dense_index] = velocity[0];
        self.velocities_y[dense_index] = velocity[1];
        self.velocities_z[dense_index] = velocity[2];
    }

    /// SP-10: ソースの Doppler レベル `[0.0, 1.0]` を設定する。
    /// 値域外は内部でクランプされる。
    pub fn set_doppler_level(&mut self, dense_index: usize, level: f32) {
        self.doppler_levels[dense_index] = level.clamp(0.0, 1.0);
    }

    /// SP-10: 媒質中の音速 (m/s) を設定する。0 以下の値は無視される。
    pub fn set_sound_speed(&mut self, speed: f32) {
        if speed > 0.0 {
            self.sound_speed = speed;
        }
    }
}
