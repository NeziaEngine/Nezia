mod system;
mod world;

pub use system::SpatialSystem;
#[cfg(test)]
pub use world::DEFAULT_SOUND_SPEED;
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

    // ── SP-10 Doppler ────────────────────────────────────────────────────

    /// Doppler 計算のための基本セットアップ。ソースを `pos` に置き、`source_vel`/`listener_vel` を持たせる。
    fn doppler_world(
        source_pos: [f32; 3],
        source_vel: [f32; 3],
        listener_vel: [f32; 3],
    ) -> SpatialWorld {
        let mut world = SpatialWorld::new();
        world.push_defaults();
        world.set_position(0, source_pos);
        world.set_velocity(0, source_vel);
        world.set_params(0, AttenuationModel::None, 1.0, 100.0, 1.0);
        world.set_enabled(0, true);
        world.set_doppler_level(0, 1.0);
        world
            .listener
            .update([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]);
        world.listener.velocity = listener_vel;
        world
    }

    #[test]
    fn doppler_default_is_one_when_zero_velocities() {
        let world = doppler_world([10.0, 0.0, 0.0], [0.0; 3], [0.0; 3]);
        assert_eq!(world.sound_speed, DEFAULT_SOUND_SPEED);
        let mut world = world;
        SpatialSystem::compute_gains(&mut world, &[1.0_f32], 1);
        assert!(approx_eq(world.doppler_pitches[0], 1.0, 1e-6));
    }

    #[test]
    fn doppler_listener_approaching_increases_pitch() {
        // ソースは右方 [10,0,0]、リスナーは原点。リスナーが +X 方向（ソースに接近）に動く → ピッチ↑
        let mut world = doppler_world([10.0, 0.0, 0.0], [0.0; 3], [30.0, 0.0, 0.0]);
        SpatialSystem::compute_gains(&mut world, &[1.0_f32], 1);
        let p = world.doppler_pitches[0];
        assert!(p > 1.0, "approaching listener should raise pitch, got {p}");
    }

    #[test]
    fn doppler_listener_receding_decreases_pitch() {
        // リスナーが -X 方向（ソースから離反）→ ピッチ↓
        let mut world = doppler_world([10.0, 0.0, 0.0], [0.0; 3], [-30.0, 0.0, 0.0]);
        SpatialSystem::compute_gains(&mut world, &[1.0_f32], 1);
        let p = world.doppler_pitches[0];
        assert!(p < 1.0, "receding listener should lower pitch, got {p}");
    }

    #[test]
    fn doppler_source_approaching_increases_pitch() {
        // ソースが -X 方向（リスナーに接近）→ ピッチ↑
        let mut world = doppler_world([10.0, 0.0, 0.0], [-30.0, 0.0, 0.0], [0.0; 3]);
        SpatialSystem::compute_gains(&mut world, &[1.0_f32], 1);
        let p = world.doppler_pitches[0];
        assert!(p > 1.0, "approaching source should raise pitch, got {p}");
    }

    #[test]
    fn doppler_source_receding_decreases_pitch() {
        // ソースが +X 方向（リスナーから離反）→ ピッチ↓
        let mut world = doppler_world([10.0, 0.0, 0.0], [30.0, 0.0, 0.0], [0.0; 3]);
        SpatialSystem::compute_gains(&mut world, &[1.0_f32], 1);
        let p = world.doppler_pitches[0];
        assert!(p < 1.0, "receding source should lower pitch, got {p}");
    }

    #[test]
    fn doppler_perpendicular_motion_no_pitch_shift() {
        // ソース速度がリスナー方向に垂直（Y 軸方向）→ 視線方向の成分 0 → ピッチ不変
        let mut world = doppler_world([10.0, 0.0, 0.0], [0.0, 30.0, 0.0], [0.0; 3]);
        SpatialSystem::compute_gains(&mut world, &[1.0_f32], 1);
        assert!(approx_eq(world.doppler_pitches[0], 1.0, 1e-5));
    }

    #[test]
    fn doppler_level_zero_disables_effect() {
        let mut world = doppler_world([10.0, 0.0, 0.0], [-50.0, 0.0, 0.0], [0.0; 3]);
        world.set_doppler_level(0, 0.0);
        SpatialSystem::compute_gains(&mut world, &[1.0_f32], 1);
        assert_eq!(world.doppler_pitches[0], 1.0);
    }

    #[test]
    fn doppler_level_half_attenuates_effect() {
        let mut world_full = doppler_world([10.0, 0.0, 0.0], [-30.0, 0.0, 0.0], [0.0; 3]);
        let mut world_half = doppler_world([10.0, 0.0, 0.0], [-30.0, 0.0, 0.0], [0.0; 3]);
        world_half.set_doppler_level(0, 0.5);

        SpatialSystem::compute_gains(&mut world_full, &[1.0_f32], 1);
        SpatialSystem::compute_gains(&mut world_half, &[1.0_f32], 1);

        // どちらも >1.0 だが、half は full より 1.0 に近い
        assert!(world_full.doppler_pitches[0] > world_half.doppler_pitches[0]);
        assert!(world_half.doppler_pitches[0] > 1.0);
    }

    #[test]
    fn doppler_disabled_when_spatial_disabled() {
        let mut world = doppler_world([10.0, 0.0, 0.0], [-30.0, 0.0, 0.0], [0.0; 3]);
        world.set_enabled(0, false);
        SpatialSystem::compute_gains(&mut world, &[1.0_f32], 1);
        assert_eq!(world.doppler_pitches[0], 1.0);
    }

    #[test]
    fn doppler_sound_speed_affects_magnitude() {
        // 音速を 2 倍にすると同じ相対速度でも周波数偏移は約半分になる。
        let mut world_343 = doppler_world([10.0, 0.0, 0.0], [-30.0, 0.0, 0.0], [0.0; 3]);
        let mut world_686 = doppler_world([10.0, 0.0, 0.0], [-30.0, 0.0, 0.0], [0.0; 3]);
        world_686.set_sound_speed(686.0);

        SpatialSystem::compute_gains(&mut world_343, &[1.0_f32], 1);
        SpatialSystem::compute_gains(&mut world_686, &[1.0_f32], 1);

        let shift_343 = world_343.doppler_pitches[0] - 1.0;
        let shift_686 = world_686.doppler_pitches[0] - 1.0;
        assert!(shift_343 > shift_686);
        assert!(shift_686 > 0.0);
    }

    #[test]
    fn doppler_supersonic_clamps_to_max() {
        // ソースが音速以上で接近 → 分母 0 以下 → 最大ピッチへクランプ
        let mut world = doppler_world([10.0, 0.0, 0.0], [-400.0, 0.0, 0.0], [0.0; 3]);
        SpatialSystem::compute_gains(&mut world, &[1.0_f32], 1);
        let p = world.doppler_pitches[0];
        assert!(p > 1.0 && p.is_finite());
    }

    #[test]
    fn doppler_zero_distance_no_shift() {
        // ソースとリスナーが同位置 → 方向不定 → ピッチ不変
        let mut world = doppler_world([0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0; 3]);
        SpatialSystem::compute_gains(&mut world, &[1.0_f32], 1);
        assert_eq!(world.doppler_pitches[0], 1.0);
    }

    #[test]
    fn doppler_simd_path_consistent_with_scalar() {
        // SIMD パス (n>=4) とスカラーパス (n%4) の両方で同じ結果が出ることを確認。
        let mut world = SpatialWorld::new();
        for _ in 0..5 {
            world.push_defaults();
        }
        // 5 ソース: SIMD 4 + スカラー 1
        let positions = [
            [10.0, 0.0, 0.0],
            [0.0, 0.0, 10.0],
            [-10.0, 0.0, 0.0],
            [0.0, 0.0, -10.0],
            [10.0, 10.0, 0.0],
        ];
        let velocities = [
            [-30.0, 0.0, 0.0],
            [0.0, 0.0, -30.0],
            [30.0, 0.0, 0.0],
            [0.0, 0.0, 30.0],
            [-30.0, -30.0, 0.0],
        ];
        for i in 0..5 {
            world.set_position(i, positions[i]);
            world.set_velocity(i, velocities[i]);
            world.set_params(i, AttenuationModel::None, 1.0, 100.0, 1.0);
            world.set_enabled(i, true);
            world.set_doppler_level(i, 1.0);
        }
        world
            .listener
            .update([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]);

        let vols = [1.0_f32; 5];
        SpatialSystem::compute_gains(&mut world, &vols, 5);

        // 全ソースが接近中 → 全部ピッチ↑
        for i in 0..5 {
            assert!(
                world.doppler_pitches[i] > 1.0,
                "source {} should have pitch > 1.0, got {}",
                i,
                world.doppler_pitches[i]
            );
        }
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
