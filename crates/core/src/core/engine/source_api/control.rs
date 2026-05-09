//! Source のライブ制御 API。spawn 後の動的パラメータ変更、状態遷移、停止。
//!
//! `set_source_volume` / `set_source_pitch` は SPSC コマンドキューを経由せず、
//! 共有 atomic スロット (`SourceLiveParams`) へ直接書き込む。それ以外
//! (seek / pause / resume / stop / set_loop / set_priority) はコマンド経由で
//! audio thread に届ける。

use ringbuf::traits::Producer;

use crate::command::Command;
use crate::entity::EntityId;

use super::super::SoundEngine;

impl SoundEngine {
    /// すべてのボイスを停止する。
    ///
    /// 登録済みのコールバックは解放されるが呼び出されない。
    #[must_use]
    pub fn stop_all(&mut self) -> bool {
        self.callbacks.clear();
        self.command_producer.try_push(Command::StopAll).is_ok()
    }

    /// ソースの音量を設定する（spawn 後の動的変更）。
    ///
    /// SPSC コマンドキューを経由せず、共有 atomic スロットへ直接書き込む。
    /// 反映は次のオーディオコールバックで（典型 5〜10 ms）。キュー満杯失敗は発生しない。
    /// 戻り値は常に `true`（範囲外 index・stale generation でも silent に無視される）。
    #[must_use]
    pub fn set_source_volume(&mut self, id: EntityId, vol: f32) -> bool {
        self.live_params.store_volume(id, vol);
        true
    }

    /// ソースのピッチを設定する（spawn 後の動的変更）。詳細は `set_source_volume` 参照。
    #[must_use]
    pub fn set_source_pitch(&mut self, id: EntityId, pitch: f32) -> bool {
        self.live_params.store_pitch(id, pitch);
        true
    }

    /// ソースの再生位置（フレーム単位）をシークする。
    #[must_use]
    pub fn seek_source(&mut self, id: EntityId, frame_offset: f32) -> bool {
        self.command_producer
            .try_push(Command::SeekSource { id, frame_offset })
            .is_ok()
    }

    /// ソースを一時停止する。再生位置は保持される。
    #[must_use]
    pub fn pause_source(&mut self, id: EntityId) -> bool {
        self.command_producer
            .try_push(Command::PauseSource { id })
            .is_ok()
    }

    /// 一時停止中のソースを再開する。
    #[must_use]
    pub fn resume_source(&mut self, id: EntityId) -> bool {
        self.command_producer
            .try_push(Command::ResumeSource { id })
            .is_ok()
    }

    /// ソースを停止する。次の audio callback で despawn される。
    #[must_use]
    pub fn stop_source(&mut self, id: EntityId) -> bool {
        self.command_producer
            .try_push(Command::StopSource { id })
            .is_ok()
    }

    /// ソースのループフラグを動的に変更する。
    #[must_use]
    pub fn set_source_loop(&mut self, id: EntityId, looping: bool) -> bool {
        self.command_producer
            .try_push(Command::SetSourceLoop { id, looping })
            .is_ok()
    }

    /// Voice Virtualization 用優先度を設定する (Wwise / CRI ADX2 互換)。
    ///
    /// 値域 `0..=255`、**高い値ほど高優先**。既定値 128 (中央値)。
    /// Wwise の Priority は 0..100、ADX2 の Voice Priority は 0..255 だが、
    /// いずれも「高い値ほど重要」という共通セマンティクスに従う。
    /// 物理ボイス上限 (`MAX_PHYSICAL_VOICES`) を超えるアクティブソースが存在するとき、
    /// 優先度・音量・距離減衰の総合スコアが下位のソースが仮想化される (ミキシングはスキップ、
    /// `sample_offset` のみ前進して時間同期を維持)。
    #[must_use]
    pub fn set_source_priority(&mut self, id: EntityId, priority: u8) -> bool {
        self.command_producer
            .try_push(Command::SetSourcePriority { id, priority })
            .is_ok()
    }
}
