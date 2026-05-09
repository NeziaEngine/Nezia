use crate::bus::{MAX_BUSES, SendId, SendPosition};
use crate::effect::{EffectId, EffectKind, EffectPosition, EffectTarget};
use crate::entity::EntityId;
use crate::spatial::AttenuationModel;

/// Send の宛先 (Phase 3-3)。コマンド経路で運ばれる識別子。
/// audio thread 側で `Bus` は dense_index、`CompressorSidechain` は EffectId →
/// CompressorWorld dense_index に解決される。
#[derive(Debug, Clone, Copy)]
pub enum SendDestination {
    Bus { dense: u32 },
    CompressorSidechain { effect: EffectId },
}

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
        /// Phase 3-4: 予約再生開始時刻 (絶対 DSP frame)。`0` で即時再生。
        start_dsp_frame: u64,
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
        /// Phase 3-4: 予約再生開始時刻 (絶対 DSP frame)。`0` で即時再生。
        start_dsp_frame: u64,
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
        /// Phase 3-4: 予約再生開始時刻 (絶対 DSP frame)。`0` で即時再生。
        start_dsp_frame: u64,
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

    /// Phase 3-1: ソースの Custom Attenuation Curve を curve registry slot で指定する。
    /// `curve_index = u32::MAX` (= `CURVE_INDEX_NONE`) で「カーブ未指定」(`Custom` モデル時に
    /// silent fallback)。`AttenuationModel::Custom` 以外のモデルでは無視される。
    SetSourceAttenuationCurve { id: EntityId, curve_index: u32 },

    /// Phase 3-2: Mixer Snapshot を適用する。サウンドスレッドが registry slot から
    /// Snapshot を引いて `ActiveSnapshot` に展開し、`fade_samples` かけて補間する。
    /// `fade_samples = 0` で即時切替。snapshot_index 解決失敗時は無視。
    ApplySnapshot {
        snapshot_index: u32,
        fade_samples: u64,
    },

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
    /// Voice Virtualization 用優先度を設定する。Wwise / ADX2 互換 0..255、**高いほど高優先**。
    SetSourcePriority { id: EntityId, priority: u8 },

    // ── DSP エフェクト (Phase 2-3) ──
    /// 事前発行された EffectId でエフェクトを生成する。
    /// `kind` の論理種別と `algo` の物理アルゴリズム index を渡す。
    /// 初期パラメータは種別ごとに設定後 `SetEffectParam` を発行する。
    SpawnEffect {
        id: EffectId,
        target: EffectTarget,
        kind: EffectKind,
        algo: u8,
        position: EffectPosition,
    },
    /// エフェクトを削除する。
    DespawnEffect { id: EffectId },
    /// エフェクトを enable / disable する (状態は保持、apply_chain でスキップされる)。
    SetEffectEnabled { id: EffectId, enabled: bool },
    /// エフェクトパラメータを設定する。`param` は種別ごとの enum を `as u8` でキャスト。
    SetEffectParam { id: EffectId, param: u8, value: f32 },

    // ── Send (Phase 3-3) ──
    /// バス → バス または バス → Compressor sidechain の Send を追加する。
    /// `id` はメインスレッドが事前発行。`src_dense` は BusWorld dense。
    /// `dst` は `Bus` の場合 BusWorld dense、`CompressorSidechain` の場合 EffectId
    /// (audio thread が CompressorWorld dense に resolve)。サイクル検出 + 容量確認はメインスレッドで完了済み。
    AddSend {
        id: SendId,
        src_dense: u32,
        dst: SendDestination,
        position: SendPosition,
        gain: f32,
    },
    /// Compressor の sidechain 入力を有効/無効にする。`true` で外部 sidechain (Send 経由) を使用、
    /// `false` で自バス内部検波に戻す。
    SetCompressorSidechainEnabled { id: EffectId, enabled: bool },
    /// ソース起点の Send (User-Defined Aux Send / Wwise・FMOD 互換) を追加する。
    /// `src_entity` は対象ソースの EntityId (audio thread が `SourceWorld::resolve` で dense 解決)。
    /// `dst` の解釈は `AddSend` と同じ (`Bus` は dense、`CompressorSidechain` は EffectId)。
    /// SendId プールはバス起点 Send と共通だが、登録先は `SourceWorld::send_*` SoA に分かれる。
    AddSourceSend {
        id: SendId,
        src_entity: EntityId,
        dst: SendDestination,
        position: SendPosition,
        gain: f32,
    },
    /// Send を削除する。
    RemoveSend { id: SendId },
    /// Send の gain を設定する。
    SetSendGain { id: SendId, gain: f32 },
    /// Send のタップ位置 (Pre/Post-Fader) を変更する。
    SetSendPosition { id: SendId, position: SendPosition },
}
