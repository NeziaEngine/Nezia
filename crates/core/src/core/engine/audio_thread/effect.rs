//! DSP エフェクトの spawn / despawn / param 適用。
//!
//! メインスレッドが事前発行した `EffectId` を使い、
//! 種別 World (Lpf/Hpf/Reverb) に state を確保 → メタ層 (`EffectWorld`) に登録 →
//! owner (Bus / Source) のチェーンに slot を追加、という 3 段階を一括で扱う。

use crate::bus::BusWorld;
use crate::effect::{
    CompressorParam, CompressorWorld, EffectKind, EffectPosition, EffectTarget, EffectWorld,
    HpfParam, HpfWorld, LpfParam, LpfWorld, Owner, PeakingEqParam, PeakingEqWorld, ReverbParam,
    ReverbWorld,
};
use crate::source::SourceWorld;

/// エフェクトを生成する。
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_effect(
    id: crate::effect::EffectId,
    target: EffectTarget,
    kind: EffectKind,
    algo: u8,
    position: EffectPosition,
    bus_world: &mut BusWorld,
    source_world: &mut SourceWorld,
    effect_world: &mut EffectWorld,
    lpf_world: &mut LpfWorld,
    hpf_world: &mut HpfWorld,
    reverb_world: &mut ReverbWorld,
    compressor_world: &mut CompressorWorld,
    peq_world: &mut PeakingEqWorld,
) {
    // 1. owner dense を解決。
    let owner = match target {
        EffectTarget::Bus(bus_id) => match bus_world.resolve(bus_id) {
            Some(d) => Owner::Bus(d as u32),
            None => return,
        },
        EffectTarget::Source(src_id) => {
            // Source 対象 + Reverb / Compressor は Bus 専用 (Aux Bus + Send 経由で利用)。
            if matches!(kind, EffectKind::Reverb | EffectKind::Compressor) {
                return;
            }
            // Phase 2-3 では Source の Post-Spatial も未実装。
            if matches!(position, EffectPosition::Post) {
                return;
            }
            match source_world.resolve(src_id) {
                Some(d) => Owner::Source(d as u32),
                None => return,
            }
        }
    };

    // 2. 種別 World に state 確保。
    let state_index = match kind {
        EffectKind::Lpf => match lpf_world.spawn(id, 1000.0, 0.707) {
            Some(d) => d,
            None => return,
        },
        EffectKind::Hpf => match hpf_world.spawn(id, 200.0, 0.707) {
            Some(d) => d,
            None => return,
        },
        EffectKind::Reverb => match reverb_world.spawn(id) {
            Some(d) => d,
            None => return,
        },
        EffectKind::Compressor => match compressor_world.spawn(id) {
            Some(d) => d,
            None => return,
        },
        EffectKind::PeakingEq => match peq_world.spawn(id, 1000.0, 1.0, 0.0) {
            Some(d) => d,
            None => return,
        },
    };

    // 3. owner のチェーンに slot を追加。失敗したら state も巻き戻す。
    let slot_ok = match owner {
        Owner::Bus(d) => bus_world.push_effect(d as usize, position, id).is_some(),
        Owner::Source(d) => source_world.push_pre_effect(d as usize, id).is_some(),
    };
    if !slot_ok {
        // チェーン満杯。state を巻き戻す。
        match kind {
            EffectKind::Lpf => {
                let _ = lpf_world.despawn(state_index);
            }
            EffectKind::Hpf => {
                let _ = hpf_world.despawn(state_index);
            }
            EffectKind::Reverb => {
                let _ = reverb_world.despawn(state_index);
            }
            EffectKind::Compressor => {
                let _ = compressor_world.despawn(state_index);
            }
            EffectKind::PeakingEq => {
                let _ = peq_world.despawn(state_index);
            }
        }
        return;
    }
    let slot = match owner {
        Owner::Bus(d) => match position {
            EffectPosition::Pre => bus_world.pre_chain_slice(d as usize).len() as u8 - 1,
            EffectPosition::Post => bus_world.post_chain_slice(d as usize).len() as u8 - 1,
        },
        Owner::Source(d) => source_world.pre_chain_slice(d as usize).len() as u8 - 1,
    };

    // 4. メタ層に登録。
    effect_world.spawn_with_id(id, kind, algo, owner, position, slot, state_index);
}

#[allow(clippy::too_many_arguments)]
pub(super) fn despawn_effect(
    id: crate::effect::EffectId,
    bus_world: &mut BusWorld,
    source_world: &mut SourceWorld,
    effect_world: &mut EffectWorld,
    lpf_world: &mut LpfWorld,
    hpf_world: &mut HpfWorld,
    reverb_world: &mut ReverbWorld,
    compressor_world: &mut CompressorWorld,
    peq_world: &mut PeakingEqWorld,
) {
    let Some(meta_dense) = effect_world.resolve(id) else {
        return;
    };
    let owner = effect_world.owners()[meta_dense];
    let position = effect_world.positions()[meta_dense];

    // 1. owner のチェーンから slot を除去。
    match owner {
        Owner::Bus(d) => {
            let _ = bus_world.remove_effect(d as usize, position, id);
        }
        Owner::Source(d) => {
            let _ = source_world.remove_pre_effect(d as usize, id);
        }
    }

    // 2. メタ層から削除し state_index を取得。
    let Some((kind, state_index)) = effect_world.despawn(id) else {
        return;
    };

    // 3. 種別 World で state を swap-remove し、移動した state があればメタ層を再マップ。
    let moved = match kind {
        EffectKind::Lpf => lpf_world.despawn(state_index),
        EffectKind::Hpf => hpf_world.despawn(state_index),
        EffectKind::Reverb => reverb_world.despawn(state_index),
        EffectKind::Compressor => compressor_world.despawn(state_index),
        EffectKind::PeakingEq => peq_world.despawn(state_index),
    };
    if let Some((moved_id, new_state_index)) = moved {
        let _ = moved_id;
        // 末尾要素 (元の last_dense) が state_index 位置に移動した。
        // メタ層側で「kind 種別 + state_index == 旧末尾」を新位置に書き換える。
        // 旧末尾 index は "新サイズ" (despawn 後の len)。
        let last_after = match kind {
            EffectKind::Lpf => lpf_world.len() as u32,
            EffectKind::Hpf => hpf_world.len() as u32,
            EffectKind::Reverb => reverb_world.len() as u32,
            EffectKind::Compressor => compressor_world.len() as u32,
            EffectKind::PeakingEq => peq_world.len() as u32,
        };
        effect_world.remap_state_index(kind, last_after, new_state_index);
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn apply_effect_param(
    id: crate::effect::EffectId,
    param: u8,
    value: f32,
    effect_world: &EffectWorld,
    lpf_world: &mut LpfWorld,
    hpf_world: &mut HpfWorld,
    reverb_world: &mut ReverbWorld,
    compressor_world: &mut CompressorWorld,
    peq_world: &mut PeakingEqWorld,
) {
    let Some(meta_dense) = effect_world.resolve(id) else {
        return;
    };
    let kind = effect_world.kinds()[meta_dense];
    let state_index = effect_world.state_indices()[meta_dense];
    match kind {
        EffectKind::Lpf => {
            if param == LpfParam::Cutoff as u8 {
                lpf_world.set_cutoff(state_index, value);
            } else if param == LpfParam::Q as u8 {
                lpf_world.set_q(state_index, value);
            }
        }
        EffectKind::Hpf => {
            if param == HpfParam::Cutoff as u8 {
                hpf_world.set_cutoff(state_index, value);
            } else if param == HpfParam::Q as u8 {
                hpf_world.set_q(state_index, value);
            }
        }
        EffectKind::Reverb => {
            if param == ReverbParam::RoomSize as u8 {
                reverb_world.set_room_size(state_index, value);
            } else if param == ReverbParam::Damping as u8 {
                reverb_world.set_damping(state_index, value);
            } else if param == ReverbParam::Wet as u8 {
                reverb_world.set_wet(state_index, value);
            } else if param == ReverbParam::Dry as u8 {
                reverb_world.set_dry(state_index, value);
            } else if param == ReverbParam::Width as u8 {
                reverb_world.set_width(state_index, value);
            }
        }
        EffectKind::Compressor => {
            if param == CompressorParam::ThresholdDb as u8 {
                compressor_world.set_threshold_db(state_index, value);
            } else if param == CompressorParam::Ratio as u8 {
                compressor_world.set_ratio(state_index, value);
            } else if param == CompressorParam::AttackMs as u8 {
                compressor_world.set_attack_ms(state_index, value);
            } else if param == CompressorParam::ReleaseMs as u8 {
                compressor_world.set_release_ms(state_index, value);
            } else if param == CompressorParam::KneeDb as u8 {
                compressor_world.set_knee_db(state_index, value);
            } else if param == CompressorParam::MakeupDb as u8 {
                compressor_world.set_makeup_db(state_index, value);
            }
        }
        EffectKind::PeakingEq => {
            if param == PeakingEqParam::CenterHz as u8 {
                peq_world.set_center_hz(state_index, value);
            } else if param == PeakingEqParam::Q as u8 {
                peq_world.set_q(state_index, value);
            } else if param == PeakingEqParam::GainDb as u8 {
                peq_world.set_gain_db(state_index, value);
            }
        }
    }
}
