mod lifecycle;
mod system;
mod virtualizer;
mod world;

pub use lifecycle::SourceLifecycleSystem;
pub use system::SourceMixingSystem;
pub use world::{SourceComponent, SourceState, SourceWorld};

/// 最大論理ソース数 (`SourceWorld` の上限)。
///
/// `MAX_PHYSICAL_VOICES` を超えるソースは spawn 直後の rebalance で `is_virtual = true`
/// となり、ミキシング段でスキップされる (sample_offset は前進する)。
pub const MAX_SOURCES: usize = 4096;

/// 物理ボイス数 (実 DSP / ミキシングを行うボイスの上限)。
///
/// この数を超えるソースは Voice Virtualization で仮想化され、ミキシング処理がスキップされる。
/// Unity / Wwise / FMOD と同様、論理上限 (`MAX_SOURCES`) より小さく設定する。
pub const MAX_PHYSICAL_VOICES: usize = 32;

/// Source 起点 Send 上限 (ソース 1 体あたりの user-defined aux send 数)。
/// Wwise / FMOD の per-event aux send が 1〜2 本で済むケースが大半なので、
/// バスの `MAX_SENDS_PER_BUS` と同じ 4 を上限としておく。
pub const MAX_SENDS_PER_SOURCE: usize = 4;

#[cfg(test)]
mod tests {
    use super::world::SourceState;
    use super::*;

    #[test]
    fn spawn_and_access() {
        let mut world = SourceWorld::new();
        let id = world
            .spawn(SourceComponent {
                vol: 0.8,
                pitch: 1.5,
                sample_offset: 100.0,
                ..SourceComponent::default()
            })
            .unwrap();

        assert_eq!(world.len(), 1);
        assert_eq!(world.vol(id), Some(0.8));
        assert_eq!(world.pitch(id), Some(1.5));
        assert_eq!(world.sample_offset(id), Some(100.0));
        assert_eq!(world.state(id), Some(SourceState::Playing));
    }

    #[test]
    fn despawn_invalidates_handle() {
        let mut world = SourceWorld::new();
        let id = world.spawn(SourceComponent::default()).unwrap();

        assert!(world.despawn(id));
        assert!(!world.contains(id));
        assert_eq!(world.vol(id), None);
        assert!(world.is_empty());
    }

    #[test]
    fn generation_prevents_stale_access() {
        let mut world = SourceWorld::new();
        let old = world
            .spawn(SourceComponent {
                vol: 0.5,
                ..SourceComponent::default()
            })
            .unwrap();
        world.despawn(old);

        let new = world
            .spawn(SourceComponent {
                vol: 0.9,
                ..SourceComponent::default()
            })
            .unwrap();

        assert_eq!(old.index, new.index);
        assert_ne!(old.generation, new.generation);

        assert_eq!(world.vol(old), None);
        assert_eq!(world.vol(new), Some(0.9));
    }

    #[test]
    fn swap_remove_keeps_dense_contiguous() {
        let mut world = SourceWorld::new();
        let a = world
            .spawn(SourceComponent {
                vol: 0.1,
                pitch: 1.0,
                sample_offset: 0.0,
                ..SourceComponent::default()
            })
            .unwrap();
        let b = world
            .spawn(SourceComponent {
                vol: 0.2,
                pitch: 2.0,
                sample_offset: 10.0,
                ..SourceComponent::default()
            })
            .unwrap();
        let c = world
            .spawn(SourceComponent {
                vol: 0.3,
                pitch: 3.0,
                sample_offset: 20.0,
                ..SourceComponent::default()
            })
            .unwrap();

        world.despawn(b);

        assert_eq!(world.len(), 2);
        assert_eq!(world.vol(a), Some(0.1));
        assert_eq!(world.vol(c), Some(0.3));
        assert_eq!(world.vols().len(), 2);
    }

    #[test]
    fn set_parameters() {
        let mut world = SourceWorld::new();
        let id = world.spawn(SourceComponent::default()).unwrap();

        assert!(world.set_vol(id, 0.42));
        assert!(world.set_pitch(id, 2.0));
        assert!(world.set_sample_offset(id, 512.0));

        assert_eq!(world.vol(id), Some(0.42));
        assert_eq!(world.pitch(id), Some(2.0));
        assert_eq!(world.sample_offset(id), Some(512.0));
    }

    #[test]
    fn bulk_access() {
        let mut world = SourceWorld::new();
        world
            .spawn(SourceComponent {
                vol: 0.5,
                ..SourceComponent::default()
            })
            .unwrap();
        world
            .spawn(SourceComponent {
                vol: 0.8,
                ..SourceComponent::default()
            })
            .unwrap();

        for v in world.vols_mut() {
            *v *= 0.5;
        }

        assert_eq!(world.vols(), &[0.25, 0.4]);
    }

    #[test]
    fn state_transitions() {
        let mut world = SourceWorld::new();
        let id = world.spawn(SourceComponent::default()).unwrap();

        assert_eq!(world.state(id), Some(SourceState::Playing));

        assert!(world.set_state(id, SourceState::Pausing));
        assert_eq!(world.state(id), Some(SourceState::Pausing));

        assert!(world.set_state(id, SourceState::Playing));
        assert_eq!(world.state(id), Some(SourceState::Playing));

        assert!(world.set_state(id, SourceState::Stopped));
        assert_eq!(world.state(id), Some(SourceState::Stopped));
    }

    #[test]
    fn spawn_returns_none_at_capacity() {
        let mut world = SourceWorld::new();
        for _ in 0..MAX_SOURCES {
            assert!(world.spawn(SourceComponent::default()).is_some());
        }
        assert!(world.spawn(SourceComponent::default()).is_none());
        assert_eq!(world.len(), MAX_SOURCES);
    }

    // ── Source 起点 Send (User-Defined Aux Send) のユニットテスト ──

    use crate::bus::{SendDestKind, SendId, SendPosition};

    fn sid(i: u32) -> SendId {
        SendId {
            index: i,
            generation: 0,
        }
    }

    #[test]
    fn source_add_send_basic() {
        let mut world = SourceWorld::new();
        let id = world.spawn(SourceComponent::default()).unwrap();
        let dense = world.resolve(id).unwrap();
        assert!(world.add_send(dense, sid(0), 3, SendDestKind::Bus, 0.5, SendPosition::Post));
        assert_eq!(world.send_count_at(dense), 1);
        let (dest, gain, pos, kind) = world.send_at(dense, 0);
        assert_eq!(dest, 3);
        assert!((gain - 0.5).abs() < 1e-6);
        assert_eq!(pos, SendPosition::Post as u8);
        assert_eq!(kind, SendDestKind::Bus as u8);
        assert_eq!(world.resolve_send(sid(0)), Some((dense, 0)));
    }

    #[test]
    fn source_send_set_gain_and_position() {
        let mut world = SourceWorld::new();
        let id = world.spawn(SourceComponent::default()).unwrap();
        let dense = world.resolve(id).unwrap();
        world.add_send(dense, sid(7), 2, SendDestKind::Bus, 0.5, SendPosition::Post);
        assert!(world.set_send_gain(sid(7), 0.25));
        assert_eq!(world.send_gain_at(dense, 0), Some(0.25));
        assert!(world.set_send_position(sid(7), SendPosition::Pre));
        let (_, _, pos, _) = world.send_at(dense, 0);
        assert_eq!(pos, SendPosition::Pre as u8);
    }

    #[test]
    fn source_remove_send_compacts() {
        let mut world = SourceWorld::new();
        let id = world.spawn(SourceComponent::default()).unwrap();
        let dense = world.resolve(id).unwrap();
        world.add_send(dense, sid(0), 1, SendDestKind::Bus, 0.1, SendPosition::Pre);
        world.add_send(dense, sid(1), 2, SendDestKind::Bus, 0.2, SendPosition::Pre);
        world.add_send(dense, sid(2), 3, SendDestKind::Bus, 0.3, SendPosition::Pre);
        assert!(world.remove_send(sid(1)));
        assert_eq!(world.send_count_at(dense), 2);
        // s2 が slot 1 に詰められる。
        assert_eq!(world.resolve_send(sid(2)), Some((dense, 1)));
        assert_eq!(world.resolve_send(sid(0)), Some((dense, 0)));
        assert!(world.resolve_send(sid(1)).is_none());
    }

    #[test]
    fn source_despawn_clears_send_lookup() {
        let mut world = SourceWorld::new();
        let id = world.spawn(SourceComponent::default()).unwrap();
        let dense = world.resolve(id).unwrap();
        world.add_send(dense, sid(0), 1, SendDestKind::Bus, 0.5, SendPosition::Post);
        assert!(world.despawn(id));
        // ソース despawn で send は invalidate される。
        assert!(world.resolve_send(sid(0)).is_none());
        assert!(!world.set_send_gain(sid(0), 0.1));
    }

    #[test]
    fn source_send_capacity_limit() {
        let mut world = SourceWorld::new();
        let id = world.spawn(SourceComponent::default()).unwrap();
        let dense = world.resolve(id).unwrap();
        for i in 0..MAX_SENDS_PER_SOURCE {
            assert!(world.add_send(
                dense,
                sid(i as u32),
                1,
                SendDestKind::Bus,
                0.5,
                SendPosition::Post,
            ));
        }
        assert!(!world.add_send(
            dense,
            sid(99),
            1,
            SendDestKind::Bus,
            0.5,
            SendPosition::Post,
        ));
    }

    #[test]
    fn source_swap_remove_remaps_send_lookup() {
        // ソース A, B, C を作って B を despawn → C が dense=B に移動。
        // C の send_lookup の source_dense が更新されることを確認 (= 回帰テスト)。
        let mut world = SourceWorld::new();
        let a = world.spawn(SourceComponent::default()).unwrap();
        let b = world.spawn(SourceComponent::default()).unwrap();
        let c = world.spawn(SourceComponent::default()).unwrap();
        let dense_c = world.resolve(c).unwrap();
        // C に send を貼る。
        world.add_send(
            dense_c,
            sid(0),
            1,
            SendDestKind::Bus,
            0.5,
            SendPosition::Post,
        );
        // B を despawn → C が swap_remove で B の dense に移動。
        world.despawn(b);
        let new_dense_c = world.resolve(c).unwrap();
        assert_ne!(
            new_dense_c, dense_c,
            "C should have been moved by swap_remove"
        );
        // send_lookup も追従しているはず。
        assert_eq!(world.resolve_send(sid(0)), Some((new_dense_c, 0)));
        let _ = a;
    }
}
