//! Phase 3-5: 単体 Limiter エフェクトの結合テスト。
//!
//! `SoundEngine` 経由で Limiter を spawn → param 変更 → despawn できることを確認する。
//! ブリックウォール特性 (`|out| <= ceiling`) のサンプルレベル検証は
//! `effect::limiter` のユニットテストで網羅済みのため、ここでは API 経路を見る。

use nezia::{EffectKind, EffectPosition, EffectTarget, LimiterParam, SoundEngine};

#[test]
fn spawn_and_despawn_limiter_on_master_bus() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return, // 音声デバイス無しの CI ではスキップ
    };
    let master = engine.master_bus();

    let eff = engine
        .add_effect(
            EffectTarget::Bus(master),
            EffectKind::Limiter,
            EffectPosition::Post,
        )
        .expect("add_effect should succeed");

    assert!(engine.set_effect_param(eff, LimiterParam::CeilingDb, -1.0));
    assert!(engine.set_effect_param(eff, LimiterParam::ReleaseMs, 100.0));

    assert!(engine.set_effect_enabled(eff, false));
    assert!(engine.set_effect_enabled(eff, true));

    assert!(engine.remove_effect(eff));
}

#[test]
fn limiter_rejected_on_source_target() {
    // Limiter は Bus 専用。Source 対象は audio thread 側で silently drop される
    // (Reverb / Compressor と同じ運用)。API レベルでは EffectId 自体は発行される。
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    // Source なしでもこのテストは「API 経路が落ちないこと」を確認するだけなので、
    // 適当な EntityId で叩いて拒否されないこと (戻り値は EffectId 発行) を見る。
    let dummy_source = nezia::EntityId {
        index: u32::MAX,
        generation: 0,
    };
    // メインスレッド側 alloc は通る (sound thread 側で resolve 失敗 → 何も起きない)。
    let eff = engine.add_effect(
        EffectTarget::Source(dummy_source),
        EffectKind::Limiter,
        EffectPosition::Pre,
    );
    if let Some(id) = eff {
        assert!(engine.remove_effect(id));
    }
}

#[test]
fn snapshot_can_target_limiter_param() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let master = engine.master_bus();

    let eff = engine
        .add_effect(
            EffectTarget::Bus(master),
            EffectKind::Limiter,
            EffectPosition::Post,
        )
        .expect("add_effect should succeed");

    let snap = engine
        .snapshot_builder()
        .set_effect_param(eff, LimiterParam::CeilingDb, -3.0)
        .set_effect_param(eff, LimiterParam::ReleaseMs, 30.0)
        .commit()
        .expect("snapshot commit should succeed");

    assert!(engine.apply_snapshot(snap, 0.1));
    assert!(engine.destroy_snapshot(snap));
    assert!(engine.remove_effect(eff));
}
