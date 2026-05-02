use crate::bus::MAX_BUSES;
use crate::entity::EntityId;
use crate::spatial::AttenuationModel;

/// `BatchSetSourcePositions` の 1 コマンドあたりの最大エントリ数。
///
/// ゲーム側 ECS が Transform を一括イテレートした結果をそのまま詰めて送ることで、
/// N 体の位置更新を `ceil(N / SPATIAL_BATCH_SIZE)` 件のコマンドに圧縮する。
pub const SPATIAL_BATCH_SIZE: usize = 32;

/// メインスレッドからサウンドスレッドへ送るコマンド。
///
/// リングバッファ経由で送信されるため、すべてのバリアントは
/// 固定サイズかつ `Copy` でなければならない。
/// `UpdateProcessOrder` が `[u32; MAX_BUSES]` を保持するため enum サイズが大きくなるが、
/// これはリアルタイム制約上ヒープ確保を避けるための意図的な設計である。
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

    // ── 3D 空間コマンド ──

    /// ソースの距離減衰パラメータを設定する（初期化・変更時のみ）。
    ///
    /// 毎フレーム送る必要はない。位置のみ変わる場合は `BatchSetSourcePositions` を使う。
    SetSourceSpatialParams {
        id: EntityId,
        model: AttenuationModel,
        min_distance: f32,
        max_distance: f32,
        rolloff: f32,
    },
    /// ソースの空間演算を有効化・無効化する。
    SetSourceSpatialEnabled { id: EntityId, enabled: bool },
    /// 複数ソースの位置を一括更新する（毎フレーム用）。
    ///
    /// `count` 件のみ有効。ゲーム側 ECS が Transform を一括イテレートした結果を
    /// そのまま詰めて送ることで、コマンド件数を圧縮する。
    BatchSetSourcePositions {
        count: u8,
        updates: [(EntityId, [f32; 3]); SPATIAL_BATCH_SIZE],
    },
    /// リスナーの状態を更新する（毎フレーム）。
    ///
    /// `forward` / `up` は正規化済みであること（サウンドスレッド側でも正規化する）。
    SetListener {
        position: [f32; 3],
        forward: [f32; 3],
        up: [f32; 3],
    },
}
