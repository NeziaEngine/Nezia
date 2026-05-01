use crate::bus::MAX_BUSES;
use crate::entity::EntityId;

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
    /// ボイスをマスターバスに再生する。`audio_buffer_index` で AudioBuffer を指定する。
    Play {
        audio_buffer_index: u32,
        vol: f32,
        pitch: f32,
    },
    /// ボイスを指定バス（密配列インデックス）に再生する。
    PlayToBus {
        audio_buffer_index: u32,
        vol: f32,
        pitch: f32,
        /// 出力先バスの密配列インデックス（メインスレッドで解決済み）。
        output_bus_dense: u32,
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
}
