//! ABI セーフな型定義。
//!
//! `nezia` クレート側の型は ABI 安定性を持たないため、ここで `#[repr(C)]` の
//! 鏡像を定義し、各エントリポイントで相互変換する。

use nezia_core::{AttenuationModel, BufferId, EntityId};

/// API 結果コード。
///
/// 状態変更系（`set_*`, `destroy`, `unload`）の戻り値。
/// 値は安定で、追加は末尾のみとする。
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // 一部バリアントは将来用 / C 側からの参照専用
pub enum NeziaResult {
    /// 成功。
    Ok = 0,
    /// 引数が NULL だった。
    NullPointer = -1,
    /// ハンドル（`EntityId` / `BufferId`）が無効。
    InvalidHandle = -2,
    /// コマンドキューが満杯で発行できなかった。
    QueueFull = -3,
    /// I/O エラー（ファイル読込失敗等）。
    IoError = -4,
    /// オーディオデコード失敗。
    DecodeError = -5,
    /// バス循環参照を検出した。
    BusLoopDetected = -6,
    /// 引数が範囲外（NaN, 容量超過 等）。
    InvalidArgument = -7,
    /// パニックを `catch_unwind` で捕捉した。
    Panic = -100,
    /// その他内部エラー。
    InternalError = -101,
}

/// 物理 ID（`core::EntityId` の ABI ミラー）。
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NeziaEntityId {
    pub index: u32,
    pub generation: u32,
}

impl NeziaEntityId {
    pub(crate) const INVALID: Self = Self {
        index: u32::MAX,
        generation: 0,
    };

    #[inline]
    pub(crate) fn from_core(id: EntityId) -> Self {
        Self {
            index: id.index,
            generation: id.generation,
        }
    }

    #[inline]
    pub(crate) fn to_core(self) -> EntityId {
        EntityId {
            index: self.index,
            generation: self.generation,
        }
    }
}

/// バッファ ID（`core::BufferId` の ABI ミラー）。
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NeziaBufferId {
    pub index: u32,
    pub generation: u32,
}

impl NeziaBufferId {
    pub(crate) const INVALID: Self = Self {
        index: u32::MAX,
        generation: 0,
    };

    #[inline]
    pub(crate) fn from_core(id: BufferId) -> Self {
        Self {
            index: id.index,
            generation: id.generation,
        }
    }

    #[inline]
    pub(crate) fn to_core(self) -> BufferId {
        BufferId {
            index: self.index,
            generation: self.generation,
        }
    }
}

/// 3 次元ベクトル（位置・方向）。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NeziaVec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl NeziaVec3 {
    #[inline]
    pub(crate) fn to_array(self) -> [f32; 3] {
        [self.x, self.y, self.z]
    }
}

/// 距離減衰モデル。`core::AttenuationModel` の ABI ミラー。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // 各バリアントは C 側からのみ構築される
pub enum NeziaAttenuationModel {
    None = 0,
    Linear = 1,
    InverseDistance = 2,
    Exponential = 3,
    /// Custom Attenuation Curve (Phase 3-1)。
    /// `nezia_source_set_attenuation_curve` で割り当てた curve を距離→ゲイン変換に使う。
    /// curve 未設定または curve 破棄済みの場合は silent (gain=0) フォールバックする。
    Custom = 4,
}

impl NeziaAttenuationModel {
    #[inline]
    pub(crate) fn to_core(self) -> AttenuationModel {
        match self {
            Self::None => AttenuationModel::None,
            Self::Linear => AttenuationModel::Linear,
            Self::InverseDistance => AttenuationModel::InverseDistance,
            Self::Exponential => AttenuationModel::Exponential,
            Self::Custom => AttenuationModel::Custom,
        }
    }
}

/// `nezia_source_play_with_handle` に渡す spawn 時の spatial 初期化パラメータ。
///
/// 旧経路は spawn 後に `set_priority` / `set_spatial_params` / `set_doppler_level` /
/// `set_attenuation_curve` を個別 FFI 呼び出しで送るため、1 ボイスあたり最大 4〜5 個の
/// SPSC コマンドを消費していた。本構造体に同梱して 1 コマンドで済ませることで、
/// 1 フレーム内のバースト Play (例: 群衆・弾幕) でリングが詰まる問題を構造的に解消する。
///
/// `enabled = 0` (= 2D) のときは spatial 系プロパティはダミー値で構わない。
/// `model = Custom` 以外のとき `curve_index` は無視される。`curve_index = 0xFFFF_FFFF`
/// は「未指定」を表すセンチネル (`CURVE_INDEX_NONE` 相当)。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NeziaSpawnSpatialInit {
    /// 0 = 2D ソース (旧 `set_spatial_enabled(0)` 相当) / 非 0 = 3D ソース。
    pub enabled: u8,
    /// 1 byte alignment padding。常に 0。
    pub _pad0: u8,
    pub _pad1: u16,
    /// 距離減衰モデル (`enabled = 0` のとき無視)。
    pub model: NeziaAttenuationModel,
    pub min_distance: f32,
    pub max_distance: f32,
    pub rolloff: f32,
    /// `[0.0, 1.0]`。Unity `AudioSource.dopplerLevel` 互換。
    pub doppler_level: f32,
    /// `model = Custom` のときのみ参照。`0xFFFF_FFFF` で未指定。
    pub curve_index: u32,
}

impl NeziaSpawnSpatialInit {
    #[inline]
    pub(crate) fn to_core(self) -> nezia_core::SpawnSpatialInit {
        nezia_core::SpawnSpatialInit {
            enabled: self.enabled != 0,
            model: self.model.to_core(),
            min_distance: self.min_distance,
            max_distance: self.max_distance,
            rolloff: self.rolloff,
            doppler_level: self.doppler_level,
            curve_index: self.curve_index,
        }
    }
}

/// `nezia_source_batch_set_positions` の入力要素。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NeziaSourcePositionUpdate {
    pub source: NeziaEntityId,
    pub position: NeziaVec3,
}

/// SP-10: `nezia_source_batch_set_velocities` の入力要素。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NeziaSourceVelocityUpdate {
    pub source: NeziaEntityId,
    pub velocity: NeziaVec3,
}

/// オーディオファイルのメタデータ（`nezia_audio_peek_metadata` の出力）。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NeziaAudioMetadata {
    pub sample_rate: u32,
    pub channels: u16,
    /// 16-bit alignment padding（`channels` の後）。常に 0。
    pub _pad: u16,
    /// 総フレーム数（チャンネル数で割る前のサンプル数）。
    /// コンテナがフレーム数を持たない場合は 0。
    pub total_frames: u64,
}

/// 個別の `play_*_with_callback` で渡す再生終了コールバック。
///
/// 自然終了時に `nezia_engine_poll_events` 経由で 1 度だけ呼ばれる。`user_data` は
/// 呼出側が任意に使う不透明ポインタ。AOT 環境では `MonoPInvokeCallback` 等で
/// 固定可能な static 関数のみを渡すこと。
pub type NeziaFinishCallback = Option<unsafe extern "C" fn(user_data: *mut core::ffi::c_void)>;
