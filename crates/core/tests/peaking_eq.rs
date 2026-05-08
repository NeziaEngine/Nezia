//! Phase 3-5: Parametric / Peaking EQ の結合テスト。
//!
//! `SoundEngine` 経由で PeakingEq を spawn → param 変更 → despawn できることを確認する。
//! DSP 数値検証は `effect::biquad` のユニットテストで網羅済みのため、ここでは API 経路を見る。

use nezia::{EffectKind, EffectPosition, EffectTarget, PeakingEqParam, SoundEngine};

#[test]
fn spawn_and_despawn_peaking_eq_on_master_bus() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return, // 音声デバイス無しの CI ではスキップ
    };
    let master = engine.master_bus();

    let eff = engine
        .add_effect(
            EffectTarget::Bus(master),
            EffectKind::PeakingEq,
            EffectPosition::Post,
        )
        .expect("add_effect should succeed");

    // パラメータ変更が QueueFull 等にならないこと。
    assert!(engine.set_effect_param(eff, PeakingEqParam::CenterHz, 800.0));
    assert!(engine.set_effect_param(eff, PeakingEqParam::Q, 1.5));
    assert!(engine.set_effect_param(eff, PeakingEqParam::GainDb, 6.0));

    // enabled トグル経路。
    assert!(engine.set_effect_enabled(eff, false));
    assert!(engine.set_effect_enabled(eff, true));

    assert!(engine.remove_effect(eff));
}

#[test]
fn snapshot_can_target_peaking_eq_param() {
    // Snapshot 経路 (resolve + 補間) でも PeakingEq の param が指定できることを確認する。
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let master = engine.master_bus();

    let eff = engine
        .add_effect(
            EffectTarget::Bus(master),
            EffectKind::PeakingEq,
            EffectPosition::Post,
        )
        .expect("add_effect should succeed");

    let snap = engine
        .snapshot_builder()
        .set_effect_param(eff, PeakingEqParam::GainDb, -3.0)
        .set_effect_param(eff, PeakingEqParam::CenterHz, 2000.0)
        .commit()
        .expect("snapshot commit should succeed");

    assert!(engine.apply_snapshot(snap, 0.1));
    assert!(engine.destroy_snapshot(snap));
    assert!(engine.remove_effect(eff));
}
