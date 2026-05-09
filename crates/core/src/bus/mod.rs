mod send;
mod system;
mod world;

pub use send::{SendDestKind, SendId, SendPosition};
pub use system::BusSystem;
pub use world::{BusComponent, BusWorld};

/// 最大バス数。
pub const MAX_BUSES: usize = 64;

/// バスの mix_buffer のサイズ上限（バスあたり）。
/// 4096 フレーム × 2ch = 8192 サンプル。
pub const MAX_MIX_BUFFER_SIZE: usize = 8192;

/// Phase 3-3: バスあたりの最大 Send 数。
pub const MAX_SENDS_PER_BUS: usize = 4;

/// Phase 3-3: 全バス合計の最大 Send 数 (SendId プール上限)。
/// 理論最大は `MAX_BUSES × MAX_SENDS_PER_BUS = 256` だが、実運用で全バス満杯にはならない。
pub const MAX_SENDS: usize = 128;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::EntityId;

    /// テスト用に空の effect world 一式をまとめて生成し、`BusSystem::update` 呼び出しを薄く
    /// ラップするヘルパ。エフェクト種別の追加で BusSystem シグネチャが伸びても、
    /// 各テストの呼び出し形は変わらない。
    struct TestFx {
        effect: crate::effect::EffectWorld,
        lpf: crate::effect::LpfWorld,
        hpf: crate::effect::HpfWorld,
        reverb: crate::effect::ReverbWorld,
        compressor: crate::effect::CompressorWorld,
        peq: crate::effect::PeakingEqWorld,
        limiter: crate::effect::LimiterWorld,
    }

    impl TestFx {
        fn new() -> Self {
            Self {
                effect: crate::effect::EffectWorld::new(),
                lpf: crate::effect::LpfWorld::new(),
                hpf: crate::effect::HpfWorld::new(),
                reverb: crate::effect::ReverbWorld::new(),
                compressor: crate::effect::CompressorWorld::new(),
                peq: crate::effect::PeakingEqWorld::new(),
                limiter: crate::effect::LimiterWorld::new(),
            }
        }

        fn run(
            &mut self,
            world: &mut BusWorld,
            output: &mut [f32],
            channels: usize,
            sample_count: usize,
        ) {
            BusSystem::update(
                world,
                &self.effect,
                &mut self.lpf,
                &mut self.hpf,
                &mut self.reverb,
                &mut self.compressor,
                &mut self.peq,
                &mut self.limiter,
                output,
                channels,
                sample_count,
            );
        }
    }

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
        TestFx::new().run(&mut world, &mut output, 2, sample_count);

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
        TestFx::new().run(&mut world, &mut output, 2, sample_count);

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
        TestFx::new().run(&mut world, &mut output, 2, sample_count);

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
        TestFx::new().run(&mut world, &mut output, 2, sample_count);

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

    // ── Send (Phase 3-3) ──────────────────────────────────────────────────────

    #[test]
    fn add_send_basic() {
        let mut world = BusWorld::new();
        let bus = world
            .spawn(BusComponent {
                gain: 1.0,
                output_bus_dense: 0,
            })
            .unwrap();
        let dense = world.resolve(bus).unwrap();

        let sid = SendId {
            index: 0,
            generation: 0,
        };
        assert!(world.add_send(dense, sid, 0, SendDestKind::Bus, 0.5, SendPosition::Post));
        assert_eq!(world.send_count_at(dense), 1);
        let (dest, gain, pos, _kind) = world.send_at(dense, 0);
        assert_eq!(dest, 0);
        assert!((gain - 0.5).abs() < 1e-6);
        assert_eq!(pos, SendPosition::Post as u8);
    }

    #[test]
    fn add_send_resolves_back() {
        let mut world = BusWorld::new();
        let bus = world
            .spawn(BusComponent {
                gain: 1.0,
                output_bus_dense: 0,
            })
            .unwrap();
        let dense = world.resolve(bus).unwrap();
        let sid = SendId {
            index: 7,
            generation: 3,
        };
        world.add_send(dense, sid, 0, SendDestKind::Bus, 0.5, SendPosition::Pre);
        assert_eq!(world.resolve_send(sid), Some((dense, 0)));

        // Stale generation は弾く。
        let stale = SendId {
            index: 7,
            generation: 2,
        };
        assert!(world.resolve_send(stale).is_none());
    }

    #[test]
    fn remove_send_swap_compacts() {
        let mut world = BusWorld::new();
        let bus = world
            .spawn(BusComponent {
                gain: 1.0,
                output_bus_dense: 0,
            })
            .unwrap();
        let dense = world.resolve(bus).unwrap();
        let s0 = SendId {
            index: 0,
            generation: 0,
        };
        let s1 = SendId {
            index: 1,
            generation: 0,
        };
        let s2 = SendId {
            index: 2,
            generation: 0,
        };
        world.add_send(dense, s0, 0, SendDestKind::Bus, 0.1, SendPosition::Pre);
        world.add_send(dense, s1, 0, SendDestKind::Bus, 0.2, SendPosition::Pre);
        world.add_send(dense, s2, 0, SendDestKind::Bus, 0.3, SendPosition::Pre);
        assert_eq!(world.send_count_at(dense), 3);

        // 中間 (s1) を削除。s2 が slot 1 に詰められる。
        assert!(world.remove_send(s1));
        assert_eq!(world.send_count_at(dense), 2);
        assert_eq!(world.resolve_send(s2), Some((dense, 1)));
        assert_eq!(world.resolve_send(s0), Some((dense, 0)));
        assert!(world.resolve_send(s1).is_none());
    }

    #[test]
    fn add_send_at_capacity_returns_false() {
        let mut world = BusWorld::new();
        let bus = world
            .spawn(BusComponent {
                gain: 1.0,
                output_bus_dense: 0,
            })
            .unwrap();
        let dense = world.resolve(bus).unwrap();
        for i in 0..MAX_SENDS_PER_BUS {
            let sid = SendId {
                index: i as u32,
                generation: 0,
            };
            assert!(world.add_send(dense, sid, 0, SendDestKind::Bus, 0.5, SendPosition::Post));
        }
        let overflow = SendId {
            index: 100,
            generation: 0,
        };
        assert!(!world.add_send(
            dense,
            overflow,
            0,
            SendDestKind::Bus,
            0.5,
            SendPosition::Post
        ));
    }

    #[test]
    fn despawn_removes_sends_originating_from_bus() {
        let mut world = BusWorld::new();
        let bus = world
            .spawn(BusComponent {
                gain: 1.0,
                output_bus_dense: 0,
            })
            .unwrap();
        let dense = world.resolve(bus).unwrap();
        let sid = SendId {
            index: 0,
            generation: 0,
        };
        world.add_send(dense, sid, 0, SendDestKind::Bus, 0.5, SendPosition::Post);
        assert!(world.despawn(bus));
        // Send は消滅、resolve も None。
        assert!(world.resolve_send(sid).is_none());
    }

    #[test]
    fn despawn_removes_sends_targeting_bus() {
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
                output_bus_dense: 0,
            })
            .unwrap();
        let dense_a = world.resolve(bus_a).unwrap();
        let dense_b = world.resolve(bus_b).unwrap();
        let sid = SendId {
            index: 0,
            generation: 0,
        };
        // A から B への Send。
        world.add_send(
            dense_a,
            sid,
            dense_b as u32,
            SendDestKind::Bus,
            0.5,
            SendPosition::Post,
        );
        assert_eq!(world.send_count_at(dense_a), 1);

        // B を despawn → A の Send もクリーンアップされる。
        assert!(world.despawn(bus_b));
        let dense_a_now = world.resolve(bus_a).unwrap();
        assert_eq!(world.send_count_at(dense_a_now), 0);
        assert!(world.resolve_send(sid).is_none());
    }

    #[test]
    fn send_tap_post_fader_routes_to_aux_bus() {
        // BGM (master 直結) から Aux Bus への Post-Fader Send を貼り、
        // Aux Bus 側の mix_buffer に gain 乗算で加算されることを確認する。
        let mut world = BusWorld::new();
        let aux = world
            .spawn(BusComponent {
                gain: 1.0,
                output_bus_dense: 0,
            })
            .unwrap();
        let bgm = world
            .spawn(BusComponent {
                gain: 1.0,
                output_bus_dense: 0,
            })
            .unwrap();
        let dense_aux = world.resolve(aux).unwrap();
        let dense_bgm = world.resolve(bgm).unwrap();
        let sid = SendId {
            index: 0,
            generation: 0,
        };
        world.add_send(
            dense_bgm,
            sid,
            dense_aux as u32,
            SendDestKind::Bus,
            0.5,
            SendPosition::Post,
        );

        // Process order: BGM, Aux, Master (どちらが先でも入力先行順なら OK)。
        // BGM は input が無い + Aux への Send。
        // Aux は BGM からの Send + Master 本線へ。
        // Master は BGM 本線 + Aux 本線。
        // → BGM, Aux, Master の順。
        world.set_process_order(&[dense_bgm as u32, dense_aux as u32, 0]);

        let sample_count = 4;
        world.clear_mix_buffers(sample_count);
        let bgm_start = dense_bgm * MAX_MIX_BUFFER_SIZE;
        for s in &mut world.mix_buffer_mut()[bgm_start..bgm_start + sample_count] {
            *s = 1.0;
        }

        let mut output = vec![0.0f32; sample_count];
        TestFx::new().run(&mut world, &mut output, 2, sample_count);

        // Master 出力 = BGM 本線 (1.0) + Aux 本線 (BGM Send 経由 0.5) = 1.5。
        for &s in &output {
            assert!(
                (s - 1.5).abs() < 1e-5,
                "expected 1.5 (BGM + 0.5 * BGM via Aux), got {s}"
            );
        }
    }

    #[test]
    fn pre_fader_send_bypasses_mute() {
        // Pre-Fader Send は本線 mute を貫通する (sidechain trigger 用途で重要)。
        let mut world = BusWorld::new();
        let aux = world
            .spawn(BusComponent {
                gain: 1.0,
                output_bus_dense: 0,
            })
            .unwrap();
        let bgm = world
            .spawn(BusComponent {
                gain: 1.0,
                output_bus_dense: 0,
            })
            .unwrap();
        let dense_aux = world.resolve(aux).unwrap();
        let dense_bgm = world.resolve(bgm).unwrap();

        // BGM を mute、Pre-Fader Send で Aux に流す。
        world.set_muted(bgm, true);
        let sid = SendId {
            index: 0,
            generation: 0,
        };
        world.add_send(
            dense_bgm,
            sid,
            dense_aux as u32,
            SendDestKind::Bus,
            1.0,
            SendPosition::Pre,
        );

        world.set_process_order(&[dense_bgm as u32, dense_aux as u32, 0]);

        let sample_count = 4;
        world.clear_mix_buffers(sample_count);
        let bgm_start = dense_bgm * MAX_MIX_BUFFER_SIZE;
        for s in &mut world.mix_buffer_mut()[bgm_start..bgm_start + sample_count] {
            *s = 1.0;
        }

        let mut output = vec![0.0f32; sample_count];
        TestFx::new().run(&mut world, &mut output, 2, sample_count);

        // BGM 本線 = mute で 0、Pre-Send は mute 前に tap されるため Aux に 1.0 流れる。
        // Master = 0 + 1.0 = 1.0。
        for &s in &output {
            assert!(
                (s - 1.0).abs() < 1e-5,
                "Pre-Fader send should bypass mute; expected 1.0, got {s}"
            );
        }
    }

    #[test]
    fn post_fader_send_respects_mute() {
        // Post-Fader Send は本線 mute なら 0 (mute 後の信号を tap)。
        let mut world = BusWorld::new();
        let aux = world
            .spawn(BusComponent {
                gain: 1.0,
                output_bus_dense: 0,
            })
            .unwrap();
        let bgm = world
            .spawn(BusComponent {
                gain: 1.0,
                output_bus_dense: 0,
            })
            .unwrap();
        let dense_aux = world.resolve(aux).unwrap();
        let dense_bgm = world.resolve(bgm).unwrap();

        world.set_muted(bgm, true);
        let sid = SendId {
            index: 0,
            generation: 0,
        };
        world.add_send(
            dense_bgm,
            sid,
            dense_aux as u32,
            SendDestKind::Bus,
            1.0,
            SendPosition::Post,
        );

        world.set_process_order(&[dense_bgm as u32, dense_aux as u32, 0]);

        let sample_count = 4;
        world.clear_mix_buffers(sample_count);
        let bgm_start = dense_bgm * MAX_MIX_BUFFER_SIZE;
        for s in &mut world.mix_buffer_mut()[bgm_start..bgm_start + sample_count] {
            *s = 1.0;
        }

        let mut output = vec![0.0f32; sample_count];
        TestFx::new().run(&mut world, &mut output, 2, sample_count);

        for &s in &output {
            assert!(
                s.abs() < 1e-5,
                "Post-Fader send should respect mute; expected 0, got {s}"
            );
        }
    }

    #[test]
    fn empty_send_count_skips_apply() {
        // send_count == 0 のバスは hot path が完全スキップされる (回帰確認)。
        let mut world = BusWorld::new();
        let bus = world
            .spawn(BusComponent {
                gain: 1.0,
                output_bus_dense: 0,
            })
            .unwrap();
        let dense = world.resolve(bus).unwrap();
        assert_eq!(world.send_count_at(dense), 0);

        world.set_process_order(&[dense as u32, 0]);
        let sample_count = 4;
        world.clear_mix_buffers(sample_count);
        let start = dense * MAX_MIX_BUFFER_SIZE;
        for s in &mut world.mix_buffer_mut()[start..start + sample_count] {
            *s = 0.5;
        }

        let mut output = vec![0.0f32; sample_count];
        TestFx::new().run(&mut world, &mut output, 2, sample_count);
        for &s in &output {
            assert!((s - 0.5).abs() < 1e-6);
        }
    }
}
