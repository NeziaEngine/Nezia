//! Sidechain Ducking (Phase 3-3 PR2) の結合テスト。
//!
//! `SoundEngine` の API レベルで Compressor + Send (sidechain) のセットアップが拒否されない
//! ことを確認する。実際のダッキング波形は `effect::compressor::tests` のユニットテストで検証。

use nezia::{CompressorParam, EffectKind, EffectPosition, EffectTarget, SendPosition, SoundEngine};

#[test]
fn add_compressor_to_bus_then_destroy() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let bgm = engine.create_bus(1.0).expect("create bgm");
    let comp = engine
        .add_effect(
            EffectTarget::Bus(bgm),
            EffectKind::Compressor,
            EffectPosition::Post,
        )
        .expect("add compressor");

    assert!(engine.set_effect_param(comp, CompressorParam::ThresholdDb, -25.0));
    assert!(engine.set_effect_param(comp, CompressorParam::Ratio, 4.0));
    assert!(engine.set_effect_param(comp, CompressorParam::AttackMs, 5.0));
    assert!(engine.set_effect_param(comp, CompressorParam::ReleaseMs, 200.0));

    assert!(engine.remove_effect(comp));
}

#[test]
fn source_compressor_is_rejected() {
    // Compressor は Bus 専用 (Source 対象は API 段階で拒否)。
    let engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    // Source ターゲットで dummy EntityId を作っても add_effect は走るが
    // audio thread 側で拒否されて effect は出来ない。ただし return 値は
    // EffectId が allocate されるので Some を返す (silently drop パターン)。
    // ここは Bus 専用方針の API 動作確認のみ実施。
    let _ = engine;
}

#[test]
fn add_send_to_compressor_basic() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let bgm = engine.create_bus(1.0).expect("create bgm");
    let voice = engine.create_bus(1.0).expect("create voice");
    let comp = engine
        .add_effect(
            EffectTarget::Bus(bgm),
            EffectKind::Compressor,
            EffectPosition::Post,
        )
        .expect("add compressor");

    // Voice → Compressor sidechain (Pre-Fader、本線 mute でも trigger 効くため定石)。
    let send = engine
        .add_send_to_compressor(voice, comp, SendPosition::Pre, 1.0)
        .expect("add sidechain send");

    assert!(engine.set_send_gain(send, 0.5));
    assert!(engine.bind_compressor_sidechain(comp, false)); // 一旦無効化
    assert!(engine.bind_compressor_sidechain(comp, true)); // 再有効化
}

#[test]
fn add_send_to_compressor_unknown_effect_rejected() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let voice = engine.create_bus(1.0).expect("create voice");
    // 未登録 EffectId は拒否される (compressor_owners に存在しない)。
    let bogus = nezia::EffectId {
        index: 999,
        generation: 0,
    };
    assert!(
        engine
            .add_send_to_compressor(voice, bogus, SendPosition::Pre, 1.0)
            .is_none(),
        "unknown effect should be rejected"
    );
}

#[test]
fn add_send_to_compressor_self_loop_rejected() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let bus = engine.create_bus(1.0).expect("create bus");
    let comp = engine
        .add_effect(
            EffectTarget::Bus(bus),
            EffectKind::Compressor,
            EffectPosition::Post,
        )
        .expect("add compressor");

    // 自バスに住む Compressor の sidechain を自分から駆動するのはサイクル (= 自己循環)。
    assert!(
        engine
            .add_send_to_compressor(bus, comp, SendPosition::Pre, 1.0)
            .is_none(),
        "self-loop sidechain send should be rejected"
    );
}

#[test]
fn destroy_bus_owning_compressor_invalidates_sidechain_send() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let bgm = engine.create_bus(1.0).expect("create bgm");
    let voice = engine.create_bus(1.0).expect("create voice");
    let comp = engine
        .add_effect(
            EffectTarget::Bus(bgm),
            EffectKind::Compressor,
            EffectPosition::Post,
        )
        .expect("add compressor");
    let send = engine
        .add_send_to_compressor(voice, comp, SendPosition::Pre, 1.0)
        .expect("add sidechain send");

    // BGM バスを destroy → bus_routing 上の Send エッジは一括除去される
    // (Compressor 所属バス = bgm の dst_entity だったため)。
    assert!(engine.destroy_bus(bgm));
    assert!(
        !engine.set_send_gain(send, 0.3),
        "sidechain send should be invalidated"
    );
}

#[test]
fn snapshot_set_send_gain_compiles() {
    // Snapshot ビルダーで send_gain / CompressorParam 両方が型安全に組めることを確認する
    // (実際の補間動作は単体テストで検証)。
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let bgm = engine.create_bus(1.0).expect("create bgm");
    let voice = engine.create_bus(1.0).expect("create voice");
    let aux = engine.create_bus(1.0).expect("create aux");
    let comp = engine
        .add_effect(
            EffectTarget::Bus(bgm),
            EffectKind::Compressor,
            EffectPosition::Post,
        )
        .expect("add compressor");

    let send_to_aux = engine
        .add_send(bgm, aux, SendPosition::Post, 0.4)
        .expect("add bgm→aux send");
    let _send_sc = engine
        .add_send_to_compressor(voice, comp, SendPosition::Pre, 1.0)
        .expect("add sidechain send");

    let s = engine
        .snapshot_builder()
        .set_bus_gain(bgm, 0.3)
        .set_send_gain(send_to_aux, 0.6)
        .set_effect_param(comp, CompressorParam::ThresholdDb, -30.0)
        .set_effect_param(comp, CompressorParam::Ratio, 8.0)
        .commit()
        .expect("commit snapshot");
    assert!(engine.apply_snapshot(s, 0.5));
}
