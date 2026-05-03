use ringbuf::traits::Producer;

use crate::command::Command;
use crate::effect::{EffectId, EffectKind, EffectParamId, EffectPosition, EffectTarget};

use super::SoundEngine;

impl SoundEngine {
    /// 対象の指定チェーン末尾にエフェクトを追加する。
    ///
    /// 拒否されるケース (戻り値 `None`):
    /// - `EffectId` プール (`MAX_EFFECTS`) 枯渇
    /// - コマンドキュー満杯
    /// - サウンドスレッド側で対象が解決できない / チェーン満杯 / 種別 World 上限
    ///   (sound thread で発生した時点で silently drop され、main 視点では成功として返る)
    ///
    /// Source 対象 + `EffectKind::Reverb` や `EffectPosition::Post` は Phase 2-3 (PR 1)
    /// では sound thread 側で無視される (PR 2 で対応)。
    #[must_use]
    pub fn add_effect(
        &mut self,
        target: EffectTarget,
        kind: EffectKind,
        position: EffectPosition,
    ) -> Option<EffectId> {
        self.add_effect_with_algo(target, kind, 0, position)
    }

    /// 物理アルゴリズムを直接指定して追加する (手動オーバーライド用)。
    /// Phase 2-3 では各種別 1 アルゴリズムなので `algo` は常に 0。
    #[must_use]
    pub fn add_effect_with_algo(
        &mut self,
        target: EffectTarget,
        kind: EffectKind,
        algo: u8,
        position: EffectPosition,
    ) -> Option<EffectId> {
        let id = self.effect_slots.alloc()?;
        let cmd = Command::SpawnEffect {
            id,
            target,
            kind,
            algo,
            position,
        };
        if self.command_producer.try_push(cmd).is_err() {
            // コマンドキュー満杯: slot を返す
            self.effect_slots.free(id);
            return None;
        }
        Some(id)
    }

    /// エフェクトを削除する。
    #[must_use]
    pub fn remove_effect(&mut self, id: EffectId) -> bool {
        if self
            .command_producer
            .try_push(Command::DespawnEffect { id })
            .is_err()
        {
            return false;
        }
        // メインスレッド側 slot を即時解放 (audio thread からの delete 通知は出さない)。
        self.effect_slots.free(id);
        true
    }

    /// エフェクトを enable / disable する (状態は保持、apply_chain でスキップされる)。
    #[must_use]
    pub fn set_effect_enabled(&mut self, id: EffectId, enabled: bool) -> bool {
        self.command_producer
            .try_push(Command::SetEffectEnabled { id, enabled })
            .is_ok()
    }

    /// 型安全なパラメータ設定。
    /// 例: `engine.set_effect_param(eff, LpfParam::Cutoff, 1000.0);`
    #[must_use]
    pub fn set_effect_param<P: EffectParamId>(
        &mut self,
        id: EffectId,
        param: P,
        value: f32,
    ) -> bool {
        self.command_producer
            .try_push(Command::SetEffectParam {
                id,
                param: param.as_u8(),
                value,
            })
            .is_ok()
    }
}
