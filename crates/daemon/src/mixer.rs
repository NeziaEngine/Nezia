//! `LoadMixer` の本体: `MixerDef` (proto) を core の bus / effect / send API の
//! 呼び出し列に再生する。
//!
//! core はアセットフォーマットのパーサを持たない (daemon CONCEPT.md 非目標) ため、
//! 構造の解釈・バリデーション・構築順序の解決はすべてここで行う。
//! すべての関数はエンジンスレッド上で呼ばれる (SoundEngine は `!Send`)。

use std::collections::HashMap;

use nezia_core::{
    CompressorParam, EffectId, EffectKind, EffectPosition, EffectTarget, EntityId, HpfParam,
    LpfParam, ReverbParam, SendId, SendPosition, SoundEngine,
};

use crate::proto::v1::{ChainPosition, MixerDef, effect_def};

/// ロード済みミキサーの状態。再ロード時の破棄と、Play のバス名解決に使う。
pub struct MixerState {
    /// 生成順 (親 → 子)。破棄は逆順で行う。
    buses: Vec<(String, EntityId)>,
    name_map: HashMap<String, EntityId>,
    effect_ids: Vec<EffectId>,
    send_ids: Vec<SendId>,
}

impl MixerState {
    /// バス論理名をハンドルに解決する。
    pub fn resolve(&self, name: &str) -> Option<EntityId> {
        self.name_map.get(name).copied()
    }

    /// 論理名 → ハンドルの一覧 (LoadMixerResponse 用、生成順)。
    pub fn named_buses(&self) -> &[(String, EntityId)] {
        &self.buses
    }
}

/// 既存ミキサーを破棄する。再生中ソースは全停止する。
///
/// 破棄順: send → effect → bus (子 → 親)。バスにぶら下がる資源を先に
/// 解放してから、生成の逆順でバスを畳む。
pub fn destroy_mixer(engine: &mut SoundEngine, state: MixerState) {
    let _ = engine.stop_all();
    for id in state.send_ids {
        let _ = engine.remove_send(id);
    }
    for id in state.effect_ids {
        let _ = engine.remove_effect(id);
    }
    for (_, id) in state.buses.into_iter().rev() {
        let _ = engine.destroy_bus(id);
    }
}

/// `MixerDef` を検証しつつエンジンへ構築する。
///
/// 失敗時はそこまでに構築した資源を巻き戻してからエラーメッセージを返す
/// (半端な構成を残さない)。エラーはすべて呼び出し側で INVALID_ARGUMENT に
/// マップされる想定の人間可読文字列。
pub fn build_mixer(engine: &mut SoundEngine, def: &MixerDef) -> Result<MixerState, String> {
    let mut state = MixerState {
        buses: Vec::with_capacity(def.buses.len()),
        name_map: HashMap::with_capacity(def.buses.len()),
        effect_ids: Vec::new(),
        send_ids: Vec::new(),
    };
    // (バス名, effects index) → EffectId。Compressor sidechain ターゲットの解決用。
    let mut effect_lookup: HashMap<(String, usize), (EffectId, EffectKind)> = HashMap::new();

    match build_inner(engine, def, &mut state, &mut effect_lookup) {
        Ok(()) => Ok(state),
        Err(msg) => {
            destroy_mixer(engine, state);
            Err(msg)
        }
    }
}

fn build_inner(
    engine: &mut SoundEngine,
    def: &MixerDef,
    state: &mut MixerState,
    effect_lookup: &mut HashMap<(String, usize), (EffectId, EffectKind)>,
) -> Result<(), String> {
    // ── 名前の検証 ──
    {
        let mut seen = std::collections::HashSet::with_capacity(def.buses.len());
        for bus in &def.buses {
            if bus.name.is_empty() {
                return Err("bus name must not be empty".into());
            }
            if !seen.insert(bus.name.as_str()) {
                return Err(format!("duplicate bus name {:?}", bus.name));
            }
        }
    }

    // ── 親子順の解決 (Kahn 風: 置けるバスがなくなるまで置く) ──
    let mut remaining: Vec<usize> = (0..def.buses.len()).collect();
    while !remaining.is_empty() {
        let before = remaining.len();
        remaining.retain(|&i| {
            let bus = &def.buses[i];
            let parent_id = if bus.parent.is_empty() {
                Some(engine.master_bus())
            } else {
                state.name_map.get(&bus.parent).copied()
            };
            let Some(parent_id) = parent_id else {
                return true; // 親が未生成 → 次パスへ
            };
            if let Some(id) = engine.create_bus_routed(bus.gain, parent_id) {
                if bus.muted {
                    let _ = engine.set_bus_muted(id, true);
                }
                state.buses.push((bus.name.clone(), id));
                state.name_map.insert(bus.name.clone(), id);
                false // 配置済み
            } else {
                true // MAX_BUSES 等 — 後で全体エラーにする
            }
        });
        if remaining.len() == before {
            let names: Vec<&str> = remaining
                .iter()
                .map(|&i| def.buses[i].name.as_str())
                .collect();
            return Err(format!(
                "could not place buses {names:?}: unknown parent, cycle, or bus capacity reached"
            ));
        }
    }

    // ── エフェクト ──
    for bus in &def.buses {
        let bus_id = state.name_map[&bus.name];
        for (idx, eff) in bus.effects.iter().enumerate() {
            let position = to_effect_position(eff.position);
            let Some(params) = &eff.params else {
                return Err(format!("bus {:?} effect[{idx}] has no params", bus.name));
            };
            let (kind, id) = spawn_effect(engine, bus_id, position, params).ok_or_else(|| {
                format!("bus {:?} effect[{idx}]: effect capacity reached", bus.name)
            })?;
            if !eff.enabled {
                let _ = engine.set_effect_enabled(id, false);
            }
            state.effect_ids.push(id);
            effect_lookup.insert((bus.name.clone(), idx), (id, kind));
        }
    }

    // ── Send ──
    for (i, send) in def.sends.iter().enumerate() {
        let src = *state
            .name_map
            .get(&send.source_bus)
            .ok_or_else(|| format!("send[{i}]: unknown source bus {:?}", send.source_bus))?;
        let position = to_send_position(send.position);
        use crate::proto::v1::send_def::Target;
        let send_id = match &send.target {
            Some(Target::TargetBus(name)) => {
                let dst = *state
                    .name_map
                    .get(name)
                    .ok_or_else(|| format!("send[{i}]: unknown target bus {name:?}"))?;
                engine.add_send(src, dst, position, send.gain)
            }
            Some(Target::Compressor(t)) => {
                let key = (t.bus.clone(), t.effect_index as usize);
                let (effect_id, kind) = *effect_lookup.get(&key).ok_or_else(|| {
                    format!(
                        "send[{i}]: no effect at {:?}[{}]",
                        t.bus, t.effect_index
                    )
                })?;
                if kind != EffectKind::Compressor {
                    return Err(format!(
                        "send[{i}]: effect {:?}[{}] is not a compressor",
                        t.bus, t.effect_index
                    ));
                }
                engine.add_send_to_compressor(src, effect_id, position, send.gain)
            }
            None => return Err(format!("send[{i}] has no target")),
        };
        let send_id =
            send_id.ok_or_else(|| format!("send[{i}]: rejected (invalid route or capacity)"))?;
        state.send_ids.push(send_id);
    }

    Ok(())
}

/// EffectDef の oneof からエフェクトを spawn し、種別ごとのパラメータを流し込む。
fn spawn_effect(
    engine: &mut SoundEngine,
    bus: EntityId,
    position: EffectPosition,
    params: &effect_def::Params,
) -> Option<(EffectKind, EffectId)> {
    let target = EffectTarget::Bus(bus);
    match params {
        effect_def::Params::LowPass(p) => {
            let id = engine.add_effect(target, EffectKind::Lpf, position)?;
            let _ = engine.set_effect_param(id, LpfParam::Cutoff, p.cutoff);
            let _ = engine.set_effect_param(id, LpfParam::Q, p.q);
            Some((EffectKind::Lpf, id))
        }
        effect_def::Params::HighPass(p) => {
            let id = engine.add_effect(target, EffectKind::Hpf, position)?;
            let _ = engine.set_effect_param(id, HpfParam::Cutoff, p.cutoff);
            let _ = engine.set_effect_param(id, HpfParam::Q, p.q);
            Some((EffectKind::Hpf, id))
        }
        effect_def::Params::Reverb(p) => {
            let id = engine.add_effect(target, EffectKind::Reverb, position)?;
            let _ = engine.set_effect_param(id, ReverbParam::RoomSize, p.room_size);
            let _ = engine.set_effect_param(id, ReverbParam::Damping, p.damping);
            let _ = engine.set_effect_param(id, ReverbParam::Wet, p.wet);
            let _ = engine.set_effect_param(id, ReverbParam::Dry, p.dry);
            let _ = engine.set_effect_param(id, ReverbParam::Width, p.width);
            Some((EffectKind::Reverb, id))
        }
        effect_def::Params::Compressor(p) => {
            let id = engine.add_effect(target, EffectKind::Compressor, position)?;
            let _ = engine.set_effect_param(id, CompressorParam::ThresholdDb, p.threshold_db);
            let _ = engine.set_effect_param(id, CompressorParam::Ratio, p.ratio);
            let _ = engine.set_effect_param(id, CompressorParam::AttackMs, p.attack_ms);
            let _ = engine.set_effect_param(id, CompressorParam::ReleaseMs, p.release_ms);
            let _ = engine.set_effect_param(id, CompressorParam::KneeDb, p.knee_db);
            let _ = engine.set_effect_param(id, CompressorParam::MakeupDb, p.makeup_db);
            Some((EffectKind::Compressor, id))
        }
    }
}

fn to_effect_position(p: i32) -> EffectPosition {
    match ChainPosition::try_from(p) {
        Ok(ChainPosition::Post) => EffectPosition::Post,
        _ => EffectPosition::Pre,
    }
}

fn to_send_position(p: i32) -> SendPosition {
    match ChainPosition::try_from(p) {
        Ok(ChainPosition::Post) => SendPosition::Post,
        _ => SendPosition::Pre,
    }
}
