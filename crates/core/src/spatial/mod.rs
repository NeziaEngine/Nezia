mod system;
mod world;

pub use system::SpatialSystem;
pub use world::{AttenuationModel, ListenerState, SpatialWorld};

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps
    }

    fn approx_eq_v3(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b.iter()).all(|(x, y)| approx_eq(*x, *y, 1e-5))
    }

    #[test]
    fn focus_default_is_disabled() {
        let listener = ListenerState::default();
        assert_eq!(listener.distance_focus_level, 0.0);
        assert_eq!(listener.direction_focus_level, 0.0);
        // 仮想位置はリスナー位置と一致する。
        assert!(approx_eq_v3(
            listener.virtual_position_for_distance(),
            listener.position
        ));
        assert!(approx_eq_v3(
            listener.virtual_position_for_direction(),
            listener.position
        ));
    }

    #[test]
    fn focus_lerps_virtual_positions() {
        let mut listener = ListenerState {
            position: [0.0, 0.0, 0.0],
            ..ListenerState::default()
        };
        listener.set_focus([10.0, 0.0, 0.0], 0.5, 0.25);
        assert!(approx_eq_v3(
            listener.virtual_position_for_distance(),
            [5.0, 0.0, 0.0]
        ));
        assert!(approx_eq_v3(
            listener.virtual_position_for_direction(),
            [2.5, 0.0, 0.0]
        ));
    }

    #[test]
    fn focus_levels_are_clamped() {
        let mut listener = ListenerState::default();
        listener.set_focus([1.0, 2.0, 3.0], -0.5, 1.7);
        assert_eq!(listener.distance_focus_level, 0.0);
        assert_eq!(listener.direction_focus_level, 1.0);
    }

    #[test]
    fn update_pose_preserves_focus() {
        let mut listener = ListenerState::default();
        listener.set_focus([4.0, 5.0, 6.0], 0.7, 0.3);
        listener.update([1.0, 1.0, 1.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]);
        assert_eq!(listener.focus_point, [4.0, 5.0, 6.0]);
        assert!(approx_eq(listener.distance_focus_level, 0.7, 1e-6));
        assert!(approx_eq(listener.direction_focus_level, 0.3, 1e-6));
    }

    #[test]
    fn focus_affects_distance_attenuation_gain() {
        // ソースは [10, 0, 0]、リスナーは原点、フォーカスは [10, 0, 0]。
        // distance_focus_level = 1.0 で仮想リスナーがソースに重なり距離 0 → 最大ゲイン。
        // distance_focus_level = 0.0 でリスナー基準の距離 10 → 減衰。
        let mut world = SpatialWorld::new();
        world.push_defaults();
        world.set_position(0, [10.0, 0.0, 0.0]);
        world.set_params(0, AttenuationModel::InverseDistance, 1.0, 100.0, 1.0);
        world.set_enabled(0, true);
        world
            .listener
            .update([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]);

        let vols = [1.0_f32];

        // フォーカス無効 → 距離 10 の減衰
        world.listener.set_focus([10.0, 0.0, 0.0], 0.0, 0.0);
        SpatialSystem::compute_gains(&mut world, &vols, 1);
        let gain_no_focus = world.left_gains[0] + world.right_gains[0];

        // 距離フォーカス完全 → 距離 0 (>= min=1.0 にクランプ) → 最大近く
        world.listener.set_focus([10.0, 0.0, 0.0], 1.0, 0.0);
        SpatialSystem::compute_gains(&mut world, &vols, 1);
        let gain_full_focus = world.left_gains[0] + world.right_gains[0];

        assert!(
            gain_full_focus > gain_no_focus,
            "distance focus should reduce attenuation: {gain_no_focus} vs {gain_full_focus}"
        );
    }

    #[test]
    fn direction_focus_changes_panning() {
        // ソースは [10, 0, 0] (リスナー右)、リスナー原点、forward=-Z。
        // direction_focus_level = 1.0 でフォーカス点 [10, 0, 0] = ソース位置 → 仮想リスナーがソースに重なり方向不定。
        // 検証は「フォーカス点をソースから L 方向に置く」で行う。
        // フォーカス [20, 0, 0] (ソースの右側) → 仮想リスナーがソース右 → ソースは仮想リスナーから見て左。
        let mut world = SpatialWorld::new();
        world.push_defaults();
        world.set_position(0, [10.0, 0.0, 0.0]);
        world.set_params(0, AttenuationModel::None, 1.0, 100.0, 1.0);
        world.set_enabled(0, true);
        world
            .listener
            .update([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]);

        let vols = [1.0_f32];

        // フォーカス無効: ソースは右 → R > L
        world.listener.set_focus([0.0, 0.0, 0.0], 0.0, 0.0);
        SpatialSystem::compute_gains(&mut world, &vols, 1);
        assert!(world.right_gains[0] > world.left_gains[0]);

        // フォーカス [20,0,0] direction=1.0: 仮想リスナーは [20,0,0]、ソース [10,0,0] は左 → L > R
        world.listener.set_focus([20.0, 0.0, 0.0], 0.0, 1.0);
        SpatialSystem::compute_gains(&mut world, &vols, 1);
        assert!(
            world.left_gains[0] > world.right_gains[0],
            "direction focus should flip panning: L={}, R={}",
            world.left_gains[0],
            world.right_gains[0]
        );
    }
}
