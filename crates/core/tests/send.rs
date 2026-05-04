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
