mod system;
mod world;

pub use system::BusSystem;
pub use world::{BusComponent, BusWorld};

/// 最大バス数。
pub const MAX_BUSES: usize = 64;

/// バスの mix_buffer のサイズ上限（バスあたり）。
/// 4096 フレーム × 2ch = 8192 サンプル。
pub const MAX_MIX_BUFFER_SIZE: usize = 8192;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::EntityId;

    // ── 生成・削除 ──────────────────────────────────────────────────────────────

    #[test]
    fn master_bus_exists_after_new() {
        let world = BusWorld::new();
        let master = world.master_entity();

        assert!(world.contains(master));
        assert_eq!(world.len(), 1);
        assert_eq!(master.index, 0);
        assert_eq!(master.generation, 0);
    }

    #[test]
    fn cannot_despawn_master_bus() {
        let mut world = BusWorld::new();
        let master = world.master_entity();
        assert!(!world.despawn(master));
        assert!(world.contains(master));
    }

    #[test]
    fn spawn_child_bus() {
        let mut world = BusWorld::new();
        let master = world.master_entity();

        let child = world
            .spawn(BusComponent {
                gain: 0.5,
                output_bus_dense: 0,
            })
            .unwrap();

        assert!(world.contains(child));
        assert_eq!(world.len(), 2);
        assert_eq!(world.gain(child), Some(0.5));
        assert_eq!(world.output_bus_dense(child), Some(0));
        assert_eq!(world.muted(child), Some(false));
        assert_eq!(world.output_bus_dense(child), Some(0));
        let _ = master;
    }

    #[test]
    fn despawn_child_bus() {
        let mut world = BusWorld::new();
        let child = world
            .spawn(BusComponent {
                gain: 1.0,
                output_bus_dense: 0,
            })
            .unwrap();

        assert!(world.despawn(child));
        assert!(!world.contains(child));
        assert_eq!(world.len(), 1);
    }

    #[test]
    fn generation_prevents_stale_access() {
        let mut world = BusWorld::new();
        let old = world
            .spawn(BusComponent {
                gain: 0.5,
                output_bus_dense: 0,
            })
            .unwrap();
        world.despawn(old);

        let new = world
            .spawn(BusComponent {
                gain: 0.9,
                output_bus_dense: 0,
            })
            .unwrap();

        if old.index == new.index {
            assert_ne!(old.generation, new.generation);
            assert_eq!(world.gain(old), None);
        }
        assert_eq!(world.gain(new), Some(0.9));
    }

    #[test]
    fn spawn_returns_none_at_capacity() {
        let mut world = BusWorld::new();
        for _ in 0..MAX_BUSES - 1 {
            assert!(
                world
                    .spawn(BusComponent {
                        gain: 1.0,
                        output_bus_dense: 0,
                    })
                    .is_some()
            );
        }
        assert_eq!(world.len(), MAX_BUSES);
        assert!(
            world
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
        let mut world = BusWorld::new();
        let master = world.master_entity();

        assert!(world.set_gain(master, 0.5));
        assert_eq!(world.gain(master), Some(0.5));
    }

    #[test]
    fn set_muted() {
        let mut world = BusWorld::new();
        let child = world
            .spawn(BusComponent {
                gain: 1.0,
                output_bus_dense: 0,
            })
            .unwrap();

        assert_eq!(world.muted(child), Some(false));
        assert!(world.set_muted(child, true));
        assert_eq!(world.muted(child), Some(true));
        assert!(world.set_muted(child, false));
        assert_eq!(world.muted(child), Some(false));
    }

    #[test]
    fn set_output_bus_dense() {
        let mut world = BusWorld::new();
        let a = world
            .spawn(BusComponent {
                gain: 1.0,
                output_bus_dense: 0,
            })
            .unwrap();
        let b = world
            .spawn(BusComponent {
                gain: 1.0,
                output_bus_dense: 0,
            })
            .unwrap();

        let a_dense = world.output_bus_dense(a).unwrap();
        assert_eq!(a_dense, 0);

        let a_dense_index = 1u32;
        assert!(world.set_output_bus_dense(b, a_dense_index));
        assert_eq!(world.output_bus_dense(b), Some(a_dense_index));
    }

    // ── ミキシング ──────────────────────────────────────────────────────────────

    #[test]
    fn mixing_single_bus_applies_gain() {
        let mut world = BusWorld::new();
        let master = world.master_entity();
        let sample_count = 4;

        world.clear_mix_buffers(sample_count);
        {
            let buf = world.mix_buffer_mut();
            for s in buf.iter_mut().take(sample_count) {
                *s = 1.0;
            }
        }

        world.set_gain(master, 0.5);

        let mut output = vec![0.0f32; sample_count];
        BusSystem::update(&mut world, &mut output, 2, sample_count);

        for &s in &output {
            assert!((s - 0.5).abs() < 1e-6, "expected 0.5, got {s}");
        }
    }

    #[test]
    fn muted_bus_outputs_silence() {
        let mut world = BusWorld::new();
        let master = world.master_entity();
        let sample_count = 4;

        world.clear_mix_buffers(sample_count);
        {
            let buf = world.mix_buffer_mut();
            for s in buf.iter_mut().take(sample_count) {
                *s = 1.0;
            }
        }

        world.set_muted(master, true);

        let mut output = vec![0.0f32; sample_count];
        BusSystem::update(&mut world, &mut output, 2, sample_count);

        for &s in &output {
            assert_eq!(s, 0.0, "muted bus should output silence");
        }
    }

    #[test]
    fn child_bus_accumulates_to_parent() {
        let mut world = BusWorld::new();
        let sample_count = 4;

        let child = world
            .spawn(BusComponent {
                gain: 1.0,
                output_bus_dense: 0,
            })
            .unwrap();

        let child_dense = {
            let _ = child;
            1u32
        };
        world.set_process_order(&[child_dense, 0]);

        world.clear_mix_buffers(sample_count);
        {
            let buf = world.mix_buffer_mut();
            let offset = child_dense as usize * MAX_MIX_BUFFER_SIZE;
            for i in 0..sample_count {
                buf[offset + i] = 0.5;
            }
        }

        let mut output = vec![0.0f32; sample_count];
        BusSystem::update(&mut world, &mut output, 2, sample_count);

        for &s in &output {
            assert!(
                (s - 0.5).abs() < 1e-6,
                "expected 0.5 from child bus, got {s}"
            );
        }
    }

    #[test]
    fn child_bus_muted_does_not_propagate() {
        let mut world = BusWorld::new();
        let sample_count = 4;

        let child = world
            .spawn(BusComponent {
                gain: 1.0,
                output_bus_dense: 0,
            })
            .unwrap();
        let child_dense = 1u32;
        world.set_process_order(&[child_dense, 0]);

        world.set_muted(child, true);

        world.clear_mix_buffers(sample_count);
        {
            let buf = world.mix_buffer_mut();
            let offset = child_dense as usize * MAX_MIX_BUFFER_SIZE;
            for i in 0..sample_count {
                buf[offset + i] = 1.0;
            }
        }

        let mut output = vec![0.0f32; sample_count];
        BusSystem::update(&mut world, &mut output, 2, sample_count);

        for &s in &output {
            assert_eq!(s, 0.0, "muted child should not propagate");
        }
    }

    // ── despawn 時の output_bus_dense 再マッピング ──────────────────────────────

    #[test]
    fn despawn_remaps_output_bus_dense_to_master() {
        let mut world = BusWorld::new();

        let bus_a = world
            .spawn(BusComponent {
                gain: 1.0,
                output_bus_dense: 0,
            })
            .unwrap();
        let bus_b = world
            .spawn(BusComponent {
                gain: 1.0,
                output_bus_dense: 1,
            })
            .unwrap();

        assert_eq!(world.output_bus_dense(bus_b), Some(1));

        world.despawn(bus_a);
        assert_eq!(world.output_bus_dense(bus_b), Some(0));
    }

    // ── spawn_with_id ──────────────────────────────────────────────────────────

    #[test]
    fn spawn_with_id_uses_specified_entity_index() {
        let mut world = BusWorld::new();
        let id = EntityId {
            index: 5,
            generation: 0,
        };

        let ok = world.spawn_with_id(
            id,
            BusComponent {
                gain: 0.7,
                output_bus_dense: 0,
            },
        );

        assert!(ok);
        assert!(world.contains(id));
        assert_eq!(world.gain(id), Some(0.7));
    }
}
