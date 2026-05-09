//! Send (副ルート) Phase 3-3 PR1 の結合テスト。
//!
//! `SoundEngine` の公開 API レベルでサイクル検出・容量制御・stale ハンドル拒否を検証する。
//! 音声出力までの検証は単体テスト (bus::tests::send_*) 側で行う。

use nezia::{SendPosition, SoundEngine};

#[test]
fn add_send_basic_then_remove() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let bgm = engine.create_bus(1.0).expect("create bgm bus");
    let aux = engine.create_bus(1.0).expect("create aux bus");

    let sid = engine
        .add_send(bgm, aux, SendPosition::Post, 0.5)
        .expect("add_send should succeed");
    assert!(engine.set_send_gain(sid, 0.3));
    assert!(engine.set_send_position(sid, SendPosition::Pre));
    assert!(engine.remove_send(sid));
    // 削除後は stale → false。
    assert!(!engine.set_send_gain(sid, 0.1));
    assert!(!engine.remove_send(sid));
}

#[test]
fn add_send_master_src_rejected() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let aux = engine.create_bus(1.0).expect("create aux bus");
    let master = engine.master_bus();
    // master からの Send は禁止。
    assert!(
        engine
            .add_send(master, aux, SendPosition::Post, 0.5)
            .is_none()
    );
}

#[test]
fn add_send_self_loop_rejected() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let bus = engine.create_bus(1.0).expect("create bus");
    assert!(
        engine.add_send(bus, bus, SendPosition::Post, 0.5).is_none(),
        "self-loop should be rejected"
    );
}

#[test]
fn add_send_cycle_via_existing_send_rejected() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let a = engine.create_bus(1.0).expect("create a");
    let b = engine.create_bus(1.0).expect("create b");
    let sid_ab = engine
        .add_send(a, b, SendPosition::Post, 0.5)
        .expect("add a→b");
    // a → b が既にあるので b → a を貼ると循環。
    assert!(
        engine.add_send(b, a, SendPosition::Post, 0.5).is_none(),
        "cycle should be rejected"
    );
    assert!(engine.remove_send(sid_ab));
}

#[test]
fn source_send_to_bus_basic() {
    // Wwise / FMOD 互換の per-event aux send が API として通ることを確認する。
    // 実音響挙動は source/mod.rs のユニットテスト群で網羅。
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let aux = engine.create_bus(1.0).expect("create aux bus");
    // 仮想 source EntityId (実際の spawn は audio thread 側、main 側は EntityId 発行のみ)。
    // ここでは set_send_gain / remove が SendId に効くことだけ検査する。
    // Source の発行は通常 play_with_callback 等で行うが、ここでは簡略化のため
    // 任意 EntityId を渡し audio thread 側 silently drop の経路に任せる。
    let dummy_src = nezia::EntityId {
        index: 12345,
        generation: 0,
    };
    let sid = engine
        .add_source_send(dummy_src, aux, SendPosition::Post, 0.4)
        .expect("add_source_send should succeed (main thread allocates)");
    assert!(engine.set_send_gain(sid, 0.25));
    assert!(engine.set_send_position(sid, SendPosition::Pre));
    assert!(engine.remove_send(sid));
    assert!(!engine.set_send_gain(sid, 0.1));
}

#[test]
fn source_send_with_invalid_dst_rejected() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let bogus_dst = nezia::EntityId {
        index: 9999,
        generation: 0,
    };
    let dummy_src = nezia::EntityId {
        index: 1,
        generation: 0,
    };
    assert!(
        engine
            .add_source_send(dummy_src, bogus_dst, SendPosition::Post, 0.5)
            .is_none(),
        "invalid dst should fail at main thread before SendId allocation"
    );
}

#[test]
fn destroy_bus_frees_related_sends() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let a = engine.create_bus(1.0).expect("create a");
    let b = engine.create_bus(1.0).expect("create b");
    let sid_ab = engine
        .add_send(a, b, SendPosition::Post, 0.5)
        .expect("add a→b");

    // b を destroy → a → b の Send は invalidate される。
    assert!(engine.destroy_bus(b));
    assert!(
        !engine.set_send_gain(sid_ab, 0.3),
        "stale send should be rejected"
    );
}
