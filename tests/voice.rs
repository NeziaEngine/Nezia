use resia::voice::{VoiceParams, VoicePool, MAX_VOICES};

#[test]
fn spawn_and_access() {
    let mut pool = VoicePool::new();
    let id = pool
        .spawn(VoiceParams {
            vol: 0.8,
            pitch: 1.5,
            sample_offset: 100.0,
        })
        .unwrap();

    assert_eq!(pool.len(), 1);
    assert_eq!(pool.vol(id), Some(0.8));
    assert_eq!(pool.pitch(id), Some(1.5));
    assert_eq!(pool.sample_offset(id), Some(100.0));
}

#[test]
fn despawn_invalidates_handle() {
    let mut pool = VoicePool::new();
    let id = pool.spawn(VoiceParams::default()).unwrap();

    assert!(pool.despawn(id));
    assert!(!pool.contains(id));
    assert_eq!(pool.vol(id), None);
    assert!(pool.is_empty());
}

#[test]
fn generation_prevents_stale_access() {
    let mut pool = VoicePool::new();
    let old = pool
        .spawn(VoiceParams {
            vol: 0.5,
            ..VoiceParams::default()
        })
        .unwrap();
    pool.despawn(old);

    let new = pool
        .spawn(VoiceParams {
            vol: 0.9,
            ..VoiceParams::default()
        })
        .unwrap();

    // 同じ index が再利用されるが generation が異なる
    assert_eq!(old.index, new.index);
    assert_ne!(old.generation, new.generation);

    // 古いハンドルではアクセスできない
    assert_eq!(pool.vol(old), None);
    // 新しいハンドルは有効
    assert_eq!(pool.vol(new), Some(0.9));
}

#[test]
fn swap_remove_keeps_dense_contiguous() {
    let mut pool = VoicePool::new();
    let a = pool
        .spawn(VoiceParams {
            vol: 0.1,
            pitch: 1.0,
            sample_offset: 0.0,
        })
        .unwrap();
    let b = pool
        .spawn(VoiceParams {
            vol: 0.2,
            pitch: 2.0,
            sample_offset: 10.0,
        })
        .unwrap();
    let c = pool
        .spawn(VoiceParams {
            vol: 0.3,
            pitch: 3.0,
            sample_offset: 20.0,
        })
        .unwrap();

    // 中間要素を削除
    pool.despawn(b);

    assert_eq!(pool.len(), 2);
    // a, c は依然有効
    assert_eq!(pool.vol(a), Some(0.1));
    assert_eq!(pool.vol(c), Some(0.3));
    // 密配列が連続している
    assert_eq!(pool.vols().len(), 2);
}

#[test]
fn set_parameters() {
    let mut pool = VoicePool::new();
    let id = pool.spawn(VoiceParams::default()).unwrap();

    assert!(pool.set_vol(id, 0.42));
    assert!(pool.set_pitch(id, 2.0));
    assert!(pool.set_sample_offset(id, 512.0));

    assert_eq!(pool.vol(id), Some(0.42));
    assert_eq!(pool.pitch(id), Some(2.0));
    assert_eq!(pool.sample_offset(id), Some(512.0));
}

#[test]
fn bulk_access() {
    let mut pool = VoicePool::new();
    pool.spawn(VoiceParams {
        vol: 0.5,
        ..VoiceParams::default()
    })
    .unwrap();
    pool.spawn(VoiceParams {
        vol: 0.8,
        ..VoiceParams::default()
    })
    .unwrap();

    // 一括で音量を半減
    for v in pool.vols_mut() {
        *v *= 0.5;
    }

    assert_eq!(pool.vols(), &[0.25, 0.4]);
}

#[test]
fn spawn_returns_none_at_capacity() {
    let mut pool = VoicePool::new();
    for _ in 0..MAX_VOICES {
        assert!(pool.spawn(VoiceParams::default()).is_some());
    }
    // 257 個目は None
    assert!(pool.spawn(VoiceParams::default()).is_none());
    assert_eq!(pool.len(), MAX_VOICES);
}
