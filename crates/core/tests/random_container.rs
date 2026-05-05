//! Random Container (Phase 4-2) の結合テスト。
//!
//! 公開 API レベルで生成・破棄・stale ハンドル拒否・再生経路を検証する。
//! 選択分布や avoid-last の確率的性質は `container::world` のユニットテスト側で扱う。

use nezia::SoundEngine;

fn make_buffer(engine: &mut SoundEngine, sample: f32) -> nezia::BufferId {
    // 1ch / 100 frames の単純な PCM。値は識別用。
    engine.load_from_pcm(vec![sample; 100], 1, 48_000)
}

#[test]
fn create_with_empty_children_returns_none() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    assert!(engine.create_random_container(&[]).is_none());
}

#[test]
fn create_destroy_roundtrip() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let b0 = make_buffer(&mut engine, 0.1);
    let b1 = make_buffer(&mut engine, 0.2);
    let id = engine
        .create_random_container(&[b0, b1])
        .expect("should create");
    assert!(engine.destroy_container(id));
    // 二重 destroy は false。
    assert!(!engine.destroy_container(id));
}

#[test]
fn play_container_after_destroy_fails() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let b = make_buffer(&mut engine, 0.5);
    let id = engine.create_random_container(&[b]).unwrap();
    assert!(engine.destroy_container(id));

    let master = engine.master_bus();
    assert!(!engine.play_container(id, 1.0, 1.0, master, false));
    assert!(
        engine
            .play_container_with_handle(id, 1.0, 1.0, master, false)
            .is_none()
    );
}

#[test]
fn play_container_with_master_bus_succeeds() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let b0 = make_buffer(&mut engine, 0.1);
    let b1 = make_buffer(&mut engine, 0.2);
    let id = engine.create_random_container(&[b0, b1]).unwrap();
    let master = engine.master_bus();

    assert!(engine.play_container(id, 0.8, 1.0, master, false));
    let h = engine.play_container_with_handle(id, 0.8, 1.0, master, false);
    assert!(h.is_some());
}

#[test]
fn play_container_to_user_bus_succeeds() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let b = make_buffer(&mut engine, 0.3);
    let bus = engine.create_bus(1.0).expect("create bus");
    let id = engine.create_random_container(&[b]).unwrap();

    assert!(engine.play_container(id, 1.0, 1.0, bus, false));
}

#[test]
fn play_container_with_invalid_bus_fails() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let b = make_buffer(&mut engine, 0.3);
    let bus = engine.create_bus(1.0).expect("create bus");
    let id = engine.create_random_container(&[b]).unwrap();
    // バスを破棄するとハンドルは stale になる。
    assert!(engine.destroy_bus(bus));

    assert!(!engine.play_container(id, 1.0, 1.0, bus, false));
}

#[test]
fn slot_reuse_invalidates_old_handle() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let b = make_buffer(&mut engine, 0.5);
    let old = engine.create_random_container(&[b]).unwrap();
    assert!(engine.destroy_container(old));

    let new = engine.create_random_container(&[b]).unwrap();
    // 同じ slot index が再利用され、generation が変わる。
    assert_eq!(old.index, new.index);
    assert_ne!(old.generation, new.generation);

    // 旧ハンドルは弾かれる。
    let master = engine.master_bus();
    assert!(!engine.play_container(old, 1.0, 1.0, master, false));
    // 新ハンドルは通る。
    assert!(engine.play_container(new, 1.0, 1.0, master, false));
}
