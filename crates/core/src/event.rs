use crate::buffer_pool::BufferId;
use crate::entity::EntityId;

/// サウンドスレッド → メインスレッド方向のイベント。
///
/// 固定サイズ・`Copy` が必須（リングバッファに積むため）。
#[derive(Debug, Clone, Copy)]
pub enum Event {
    /// Source がバッファ末尾まで再生して自然終了した。
    SourceFinished { token: u32 },
    /// `play_with_callback()` 時に `MAX_SOURCES` 上限に達し再生できなかった。
    PlayFailed { token: u32 },
    /// Source が despawn された（自然終了 / Stop / StopAll いずれか）。
    /// メインスレッドがスロット index を再利用できるように通知する。
    SourceDespawned { id: EntityId },
    /// ストリーミングバッファでサウンドスレッドが必要量を読み出せなかった (Phase 2-4)。
    /// 短時間 (1〜2 callback) なら自然解消。連続発火時はワーカが追いついていない。
    StreamingUnderrun { buffer: BufferId },
}
