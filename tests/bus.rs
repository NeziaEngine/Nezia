use nezia::bus::{BusComponent, BusSystem, MAX_BUSES, MAX_MIX_BUFFER_SIZE};

// ── 生成・削除 ──────────────────────────────────────────────────────────────

#[test]
fn master_bus_exists_after_new() {
    let system = BusSystem::new();
    let master = system.master_entity();

    assert!(system.contains(master));
    assert_eq!(system.len(), 1);
    assert_eq!(master.index, 0);
    assert_eq!(master.generation, 0);
}

#[test]
fn cannot_despawn_master_bus() {
    let mut system = BusSystem::new();
    let master = system.master_entity();
    assert!(!system.despawn(master));
    assert!(system.contains(master));
}

#[test]
fn spawn_child_bus() {
    let mut system = BusSystem::new();
    let master = system.master_entity();

    let child = system
        .spawn(BusComponent {
            gain: 0.5,
            output_bus_dense: 0,
        })
        .unwrap();

    assert!(system.contains(child));
    assert_eq!(system.len(), 2);
    assert_eq!(system.gain(child), Some(0.5));
    assert_eq!(system.output_bus_dense(child), Some(0));
    // 新しいバスはミュートなし。
    assert_eq!(system.muted(child), Some(false));
    // 出力先はマスターバス（dense 0）。
    assert_eq!(system.output_bus_dense(child), Some(0));
    let _ = master;
}

#[test]
fn despawn_child_bus() {
    let mut system = BusSystem::new();
    let child = system
        .spawn(BusComponent {
            gain: 1.0,
            output_bus_dense: 0,
        })
        .unwrap();

    assert!(system.despawn(child));
    assert!(!system.contains(child));
    assert_eq!(system.len(), 1);
}

#[test]
fn generation_prevents_stale_access() {
    let mut system = BusSystem::new();
    let old = system
        .spawn(BusComponent {
            gain: 0.5,
            output_bus_dense: 0,
        })
        .unwrap();
    system.despawn(old);

    let new = system
        .spawn(BusComponent {
            gain: 0.9,
            output_bus_dense: 0,
        })
        .unwrap();

    // 同じ index が再利用される場合、generation で弾かれる。
    if old.index == new.index {
        assert_ne!(old.generation, new.generation);
        assert_eq!(system.gain(old), None);
    }
    assert_eq!(system.gain(new), Some(0.9));
}

#[test]
fn spawn_returns_none_at_capacity() {
    let mut system = BusSystem::new();
    // マスターバスが1つあるので MAX_BUSES - 1 個まで追加できる。
    for _ in 0..MAX_BUSES - 1 {
        assert!(
            system
                .spawn(BusComponent {
                    gain: 1.0,
                    output_bus_dense: 0,
                })
                .is_some()
        );
    }
    assert_eq!(system.len(), MAX_BUSES);
    // これ以上追加できない。
    assert!(
        system
            .spawn(BusComponent {
                gain: 1.0,
                output_bus_dense: 0,
            })
            .is_none()
    );
}

// ── パラメータ ──────────────────────────────────────────────────────────────

#[test]
fn set_gain() {
    let mut system = BusSystem::new();
    let master = system.master_entity();

    assert!(system.set_gain(master, 0.5));
    assert_eq!(system.gain(master), Some(0.5));
}

#[test]
fn set_muted() {
    let mut system = BusSystem::new();
    let child = system
        .spawn(BusComponent {
            gain: 1.0,
            output_bus_dense: 0,
        })
        .unwrap();

    assert_eq!(system.muted(child), Some(false));
    assert!(system.set_muted(child, true));
    assert_eq!(system.muted(child), Some(true));
    assert!(system.set_muted(child, false));
    assert_eq!(system.muted(child), Some(false));
}

#[test]
fn set_output_bus_dense() {
    let mut system = BusSystem::new();
    let a = system
        .spawn(BusComponent {
            gain: 1.0,
            output_bus_dense: 0,
        })
        .unwrap();
    let b = system
        .spawn(BusComponent {
            gain: 1.0,
            output_bus_dense: 0,
        })
        .unwrap();

    // b の出力先を a（dense index 1）に変更。
    let a_dense = system.output_bus_dense(a).unwrap();
    // a は master の次に追加されたので dense=1。
    assert_eq!(a_dense, 0); // a は master(0)を親とする。

    // b の output_bus_dense を a の dense index に設定。
    let a_dense_index = 1u32; // 実際の a の dense index
    assert!(system.set_output_bus_dense(b, a_dense_index));
    assert_eq!(system.output_bus_dense(b), Some(a_dense_index));
}

// ── ミキシング ──────────────────────────────────────────────────────────────

#[test]
fn mixing_single_bus_applies_gain() {
    let mut system = BusSystem::new();
    let master = system.master_entity();
    let sample_count = 4;

    // mix_buffer に直接書き込む。
    system.clear_mix_buffers(sample_count);
    {
        let buf = system.mix_buffer_mut();
        // マスターバス（dense 0）のスライスに 1.0 を書き込む。
        for i in 0..sample_count {
            buf[i] = 1.0;
        }
    }

    // gain を 0.5 に設定。
    system.set_gain(master, 0.5);
    // process_order はデフォルトで [0]（マスターバスのみ）。

    let mut output = vec![0.0f32; sample_count];
    system.update(&mut output, 2, sample_count);

    for &s in &output {
        assert!((s - 0.5).abs() < 1e-6, "expected 0.5, got {s}");
    }
}

#[test]
fn muted_bus_outputs_silence() {
    let mut system = BusSystem::new();
    let master = system.master_entity();
    let sample_count = 4;

    system.clear_mix_buffers(sample_count);
    {
        let buf = system.mix_buffer_mut();
        for i in 0..sample_count {
            buf[i] = 1.0;
        }
    }

    system.set_muted(master, true);

    let mut output = vec![0.0f32; sample_count];
    system.update(&mut output, 2, sample_count);

    for &s in &output {
        assert_eq!(s, 0.0, "muted bus should output silence");
    }
}

#[test]
fn child_bus_accumulates_to_parent() {
    let mut system = BusSystem::new();
    let sample_count = 4;

    // 子バスを追加。デフォルトでマスターバス（dense 0）に出力。
    let child = system
        .spawn(BusComponent {
            gain: 1.0,
            output_bus_dense: 0,
        })
        .unwrap();

    // process_order: [child_dense, 0]（リーフ→ルート）
    // child は dense 1 のはず。
    let child_dense = {
        // child の dense index を確認: 子バスは 2 番目に追加されるので dense=1。
        let _ = child;
        1u32
    };
    system.set_process_order(&[child_dense, 0]);

    system.clear_mix_buffers(sample_count);
    {
        let buf = system.mix_buffer_mut();
        // 子バス（dense 1）のスライスに 0.5 を書き込む。
        let offset = child_dense as usize * MAX_MIX_BUFFER_SIZE;
        for i in 0..sample_count {
            buf[offset + i] = 0.5;
        }
    }

    let mut output = vec![0.0f32; sample_count];
    system.update(&mut output, 2, sample_count);

    // 子バスの 0.5 がマスターバスに加算されて出力される。
    for &s in &output {
        assert!(
            (s - 0.5).abs() < 1e-6,
            "expected 0.5 from child bus, got {s}"
        );
    }
}

#[test]
fn child_bus_muted_does_not_propagate() {
    let mut system = BusSystem::new();
    let sample_count = 4;

    let child = system
        .spawn(BusComponent {
            gain: 1.0,
            output_bus_dense: 0,
        })
        .unwrap();
    let child_dense = 1u32;
    system.set_process_order(&[child_dense, 0]);

    // 子バスをミュート。
    system.set_muted(child, true);

    system.clear_mix_buffers(sample_count);
    {
        let buf = system.mix_buffer_mut();
        let offset = child_dense as usize * MAX_MIX_BUFFER_SIZE;
        for i in 0..sample_count {
            buf[offset + i] = 1.0; // ミュートされるので出力されないはず。
        }
    }

    let mut output = vec![0.0f32; sample_count];
    system.update(&mut output, 2, sample_count);

    for &s in &output {
        assert_eq!(s, 0.0, "muted child should not propagate");
    }
}

// ── despawn 時の output_bus_dense 再マッピング ──────────────────────────────

#[test]
fn despawn_remaps_output_bus_dense_to_master() {
    let mut system = BusSystem::new();

    // Bus A（dense 1）、Bus B（dense 2）を作成。B は A に出力。
    let _bus_a = system
        .spawn(BusComponent {
            gain: 1.0,
            output_bus_dense: 0,
        })
        .unwrap();
    let bus_b = system
        .spawn(BusComponent {
            gain: 1.0,
            output_bus_dense: 1, // Bus A（dense 1）に出力
        })
        .unwrap();

    assert_eq!(system.output_bus_dense(bus_b), Some(1));

    // Bus A を削除すると、Bus B の output_bus_dense はマスター（0）にフォールバック。
    system.despawn(_bus_a);
    assert_eq!(system.output_bus_dense(bus_b), Some(0));
}

// ── spawn_with_id ──────────────────────────────────────────────────────────

#[test]
fn spawn_with_id_uses_specified_entity_index() {
    use nezia::entity::EntityId;

    let mut system = BusSystem::new();
    let id = EntityId {
        index: 5,
        generation: 0,
    };

    let ok = system.spawn_with_id(
        id,
        BusComponent {
            gain: 0.7,
            output_bus_dense: 0,
        },
    );

    assert!(ok);
    assert!(system.contains(id));
    assert_eq!(system.gain(id), Some(0.7));
}
