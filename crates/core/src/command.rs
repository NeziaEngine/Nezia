use crate::bus::MAX_BUSES;
use crate::entity::EntityId;
use crate::spatial::AttenuationModel;

/// メインスレッドからサウンドスレッドへ送るコマンド。
///
/// リングバッファ経由で送信されるため、すべてのバリアントは
/// 固定サイズかつ `Copy` でなければならない。
/// `UpdateProcessOrder` が `[u32; MAX_BUSES]` を保持するため enum サイズが大きくなるが、
/// これはリアルタイム制約上ヒープ確保を避けるための意図的な設計である。
///
/// 毎フレーム送る「最新値さえ届けばよい」状態（リスナー姿勢・ソース位置）は
/// このコマンド経路ではなく triple buffer 共有メモリ経由で受け渡す。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Copy)]
pub enum Command {
    /// マスター音量を設定する（0.0〜1.0）。マスターバスの gain として処理される。
    SetVolume(f32),
    /// ボイスをマスターバスに再生する（fire-and-forget）。
    Play {
        audio_buffer_index: u32,
        vol: f32,
        pitch: f32,
        /// コールバックトークン。0 = コールバックなし。
        token: u32,
        /// ループ再生フラグ。
        looping: bool,
    },
    /// ボイスを指定バス（密配列インデックス）に再生する（fire-and-forget）。
    PlayToBus {
        audio_buffer_index: u32,
        vol: f32,
        pitch: f32,
        /// 出力先バスの密配列インデックス（メインスレッドで解決済み）。
        output_bus_dense: u32,
        /// コールバックトークン。0 = コールバックなし。
        token: u32,
        /// ループ再生フラグ。
        looping: bool,
    },
    /// EntityId 付きでソースをスポーンする（3D ソース用）。
    ///
    /// メインスレッドが事前発行した EntityId を使うことで、
    /// スポーン後に空間パラメータを EntityId で参照できる。
    SpawnSource {
        id: EntityId,
        audio_buffer_index: u32,
        vol: f32,
        pitch: f32,
        /// 出力先バスの密配列インデックス（メインスレッドで解決済み）。
        output_bus_dense: u32,
        /// コールバックトークン。0 = コールバックなし。
        token: u32,
        /// ループ再生フラグ。
        looping: bool,
    },
    /// すべてのボイスを停止する。
    StopAll,
    /// バスを生成する。EntityId はメインスレッド側で事前計算済み。
    SpawnBus {
        id: EntityId,
        gain: f32,
        /// 出力先バスの密配列インデックス（メインスレッドで解決済み）。
        output_bus_dense: u32,
    },
    /// バスを削除する。
    DespawnBus { id: EntityId },
    /// バスのゲインを設定する。
    SetBusGain { id: EntityId, gain: f32 },
    /// バスのミュートを設定する。
    SetBusMuted { id: EntityId, muted: bool },
    /// バスの出力先を変更する（密配列インデックスで指定）。
    SetBusOutput { id: EntityId, output_bus_dense: u32 },
    /// バスの処理順序を更新する（リーフ→ルート順の密配列インデックス列）。
    UpdateProcessOrder { order: [u32; MAX_BUSES], len: u8 },

    // ── 3D 空間コマンド（個別変更系。毎フレーム系は triple buffer 経由） ──
    /// ソースの距離減衰パラメータを設定する（初期化・変更時のみ）。
    SetSourceSpatialParams {
        id: EntityId,
        model: AttenuationModel,
        min_distance: f32,
        max_distance: f32,
        rolloff: f32,
    },
    // SetSourceSpatialEnabled は live_params の atomic スロット経由に変更されたため削除。
    /// SP-06: リスナーフォーカスを設定する（変更時のみ送信）。
    /// `*_focus_level` は内部で `[0.0, 1.0]` にクランプされる。
    /// 0.0 でフォーカス無効（リスナー位置のみ使用）。
    SetListenerFocus {
        focus_point: [f32; 3],
        distance_focus_level: f32,
        direction_focus_level: f32,
    },
    /// SP-10: ソースの Doppler レベル `[0.0, 1.0]` を設定する。
    /// 0.0 で Doppler 無効、1.0 で完全適用。値域外は内部でクランプされる。
    SetSourceDopplerLevel { id: EntityId, level: f32 },
    /// SP-10: 媒質中の音速 (m/s) を設定する。0 以下は無視される。既定値 343.0。
    SetSoundSpeed { speed: f32 },

    // ── ライブソース制御（spawn 後の挙動変更） ──
    // SetSourceVolume / SetSourcePitch は live_params の atomic スロット経由に変更されたため削除。
    /// ソースの再生位置（フレーム単位）を設定する。
    SeekSource { id: EntityId, frame_offset: f32 },
    /// ソースを一時停止する。
    PauseSource { id: EntityId },
    /// ソースを再開する。
    ResumeSource { id: EntityId },
    /// ソースを停止する（次の update で despawn される）。
    StopSource { id: EntityId },
    /// ソースのループフラグを設定する（再生中の動的変更）。
    SetSourceLoop { id: EntityId, looping: bool },
}
