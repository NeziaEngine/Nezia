mod lifecycle;
mod system;
mod world;

pub use lifecycle::SourceLifecycleSystem;
pub use system::SourceMixingSystem;
pub use world::{SourceComponent, SourceWorld};

/// 最大同時発音数。
pub const MAX_SOURCES: usize = 256;

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
}
