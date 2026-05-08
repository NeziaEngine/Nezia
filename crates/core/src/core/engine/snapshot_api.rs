//! Mixer Snapshot 公開 API (Phase 3-2)。
//!
//! 宣言的ビルダーで Snapshot を構築し、`apply_snapshot` でクロスフェード適用する。

use ringbuf::traits::Producer;

use crate::bus::SendId;
use crate::command::Command;
use crate::effect::{EffectId, EffectParamId};
use crate::entity::EntityId;
use crate::snapshot::{
    BusGainEntry, BusMutedEntry, EffectParamEntry, SendGainEntry, Snapshot, SnapshotEffectKind,
    SnapshotId,
};

use super::SoundEngine;

/// Snapshot を宣言的に組み立てるビルダー。`commit()` で `SnapshotId` を発行する。
pub struct SnapshotBuilder<'a> {
    engine: &'a mut SoundEngine,
    snapshot: Snapshot,
}

impl<'a> SnapshotBuilder<'a> {
    pub(super) fn new(engine: &'a mut SoundEngine) -> Self {
        Self {
            engine,
            snapshot: Snapshot::new(),
        }
    }

    /// バスのゲインを追加。同じバスを複数回設定すると最後の値が採用される。
    #[must_use]
    pub fn set_bus_gain(mut self, bus: EntityId, gain: f32) -> Self {
        // 重複は最後の指定を優先 (上書き)。
        self.snapshot.bus_gains.retain(|e| e.bus != bus);
        self.snapshot.bus_gains.push(BusGainEntry { bus, gain });
        self
    }

    /// バスのミュート状態を追加。
    #[must_use]
    pub fn set_bus_muted(mut self, bus: EntityId, muted: bool) -> Self {
        self.snapshot.bus_muted.retain(|e| e.bus != bus);
        self.snapshot.bus_muted.push(BusMutedEntry { bus, muted });
        self
    }

    /// Phase 3-3: Send gain を追加。dB 空間で線形補間 (バスゲインと同じ)。
    #[must_use]
    pub fn set_send_gain(mut self, send: SendId, gain: f32) -> Self {
        // 重複は最後の指定を優先 (同じ SendId に対して上書き)。
        self.snapshot
            .send_gains
            .retain(|e| !(e.send.index == send.index && e.send.generation == send.generation));
        self.snapshot.send_gains.push(SendGainEntry { send, gain });
        self
    }

    /// エフェクトパラメータを追加。
    #[must_use]
    pub fn set_effect_param<P: EffectParamId>(
        mut self,
        effect: EffectId,
        param: P,
        value: f32,
    ) -> Self {
        let kind = match P::KIND {
            crate::effect::EffectKind::Lpf => SnapshotEffectKind::Lpf,
            crate::effect::EffectKind::Hpf => SnapshotEffectKind::Hpf,
            crate::effect::EffectKind::Reverb => SnapshotEffectKind::Reverb,
            crate::effect::EffectKind::Compressor => SnapshotEffectKind::Compressor,
            crate::effect::EffectKind::PeakingEq => SnapshotEffectKind::PeakingEq,
        };
        let p = param.as_u8();
        // 重複排除 (同 effect + 同 param) は最後の値を優先。
        self.snapshot
            .effect_params
            .retain(|e| !(e.effect == effect && e.kind == kind && e.param == p));
        self.snapshot.effect_params.push(EffectParamEntry {
            effect,
            kind,
            param: p,
            value,
        });
        self
    }

    /// Snapshot を registry に登録してハンドルを返す。`MAX_SNAPSHOTS` 超過時は `None`。
    pub fn commit(self) -> Option<SnapshotId> {
        self.engine.snapshot_registry.create(self.snapshot)
    }
}

impl SoundEngine {
    /// Snapshot ビルダーを取得する。
    ///
    /// ```ignore
    /// let s = engine.snapshot_builder()
    ///     .set_bus_gain(bgm, 0.3)
    ///     .set_bus_gain(sfx, 1.0)
    ///     .commit()?;
    /// ```
    pub fn snapshot_builder(&mut self) -> SnapshotBuilder<'_> {
        SnapshotBuilder::new(self)
    }

    /// Snapshot を破棄する。進行中の補間 (適用済み) には影響しない
    /// (`ActiveSnapshot` は apply 時に値を確定済みのため)。
    pub fn destroy_snapshot(&mut self, id: SnapshotId) -> bool {
        self.snapshot_registry.destroy(id)
    }

    /// Snapshot を適用する。`fade_seconds` かけて現在値からターゲット値へ線形補間する。
    /// `fade_seconds = 0.0` で即時適用。
    /// 既に進行中の補間がある場合は中断され、現在値を新たな from としてフェードを再開する。
    /// 解決失敗 (destroy 済 ID) は false。
    #[must_use]
    pub fn apply_snapshot(&mut self, id: SnapshotId, fade_seconds: f32) -> bool {
        let Some(index) = self.snapshot_registry.resolve(id) else {
            return false;
        };
        let fade_samples = fade_seconds_to_samples(fade_seconds, self.device_sample_rate);
        self.command_producer
            .try_push(Command::ApplySnapshot {
                snapshot_index: index,
                fade_samples,
            })
            .is_ok()
    }
}

#[inline]
fn fade_seconds_to_samples(fade_seconds: f32, sample_rate: f32) -> u64 {
    if fade_seconds <= 0.0 || sample_rate <= 0.0 {
        return 0;
    }
    (fade_seconds * sample_rate).max(0.0) as u64
}
