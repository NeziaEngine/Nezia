use std::f32::consts::{FRAC_PI_2, FRAC_PI_4};

use wide::f32x4;

use super::world::{AttenuationModel, SpatialWorld};

/// 3D 空間ゲイン計算システム。
///
/// `SpatialWorld` の密配列を SIMD で処理し、
/// `left_gains` / `right_gains` に結果を書き込む。
/// `SourceMixingSystem::update()` より前に呼び出すこと。
pub struct SpatialSystem;

impl SpatialSystem {
    /// 全ソースの空間ゲインを計算して `SpatialWorld` に書き込む。
    ///
    /// `vols` は `SourceWorld::vols()` のスライスを渡す。
    /// `n` はアクティブなソース数（`SourceWorld::len()`）。
    pub fn compute_gains(world: &mut SpatialWorld, vols: &[f32], n: usize) {
        if n == 0 {
            return;
        }

        let lx = world.listener.position[0];
        let ly = world.listener.position[1];
        let lz = world.listener.position[2];
        let rx = world.listener.right[0];
        let ry = world.listener.right[1];
        let rz = world.listener.right[2];
        let fx = world.listener.forward[0];
        let fy = world.listener.forward[1];
        let fz = world.listener.forward[2];

        // SIMD パス: 4 ソースずつ距離と方向成分を計算する。
        let simd_end = n / 4 * 4;
        let mut i = 0;

        while i < simd_end {
            let px = f32x4::from([
                world.positions_x[i],
                world.positions_x[i + 1],
                world.positions_x[i + 2],
                world.positions_x[i + 3],
            ]);
            let py = f32x4::from([
                world.positions_y[i],
                world.positions_y[i + 1],
                world.positions_y[i + 2],
                world.positions_y[i + 3],
            ]);
            let pz = f32x4::from([
                world.positions_z[i],
                world.positions_z[i + 1],
                world.positions_z[i + 2],
                world.positions_z[i + 3],
            ]);

            let dx = px - f32x4::splat(lx);
            let dy = py - f32x4::splat(ly);
            let dz = pz - f32x4::splat(lz);

            let dist: [f32; 4] = (dx * dx + dy * dy + dz * dz).sqrt().into();
            let local_x: [f32; 4] =
                (dx * f32x4::splat(rx) + dy * f32x4::splat(ry) + dz * f32x4::splat(rz)).into();
            let local_z: [f32; 4] =
                (dx * f32x4::splat(fx) + dy * f32x4::splat(fy) + dz * f32x4::splat(fz)).into();

            for j in 0..4 {
                apply_gains(world, vols, i + j, dist[j], local_x[j], local_z[j]);
            }

            i += 4;
        }

        // スカラーパス: 端数（n % 4 件）。
        while i < n {
            let dx = world.positions_x[i] - lx;
            let dy = world.positions_y[i] - ly;
            let dz = world.positions_z[i] - lz;
            let dist = (dx * dx + dy * dy + dz * dz).sqrt();
            let local_x = dx * rx + dy * ry + dz * rz;
            let local_z = dx * fx + dy * fy + dz * fz;
            apply_gains(world, vols, i, dist, local_x, local_z);
            i += 1;
        }
    }
}

/// ソース 1 体分のゲインを計算して `SpatialWorld` に書き込む。
#[inline]
fn apply_gains(
    world: &mut SpatialWorld,
    vols: &[f32],
    i: usize,
    dist: f32,
    local_x: f32,
    local_z: f32,
) {
    let vol = vols[i];

    if !world.spatial_enabled[i] {
        world.left_gains[i] = vol;
        world.right_gains[i] = vol;
        return;
    }

    let dist_gain = compute_attenuation(
        dist,
        world.attenuation_models[i],
        world.min_distances[i],
        world.max_distances[i],
        world.rolloff_factors[i],
    );

    // アジマス角 → イコールパワーパン。
    let azimuth = local_x.atan2(local_z);
    let pan_angle = azimuth.clamp(-FRAC_PI_2, FRAC_PI_2);
    let pan = pan_angle / FRAC_PI_2; // -1.0(左) 〜 1.0(右)
    let angle = (pan + 1.0) * FRAC_PI_4;

    let base = vol * dist_gain;
    world.left_gains[i] = base * angle.cos();
    world.right_gains[i] = base * angle.sin();
}

/// 距離減衰ゲインを計算する。
#[inline]
fn compute_attenuation(
    dist: f32,
    model: AttenuationModel,
    min_dist: f32,
    max_dist: f32,
    rolloff: f32,
) -> f32 {
    let dist = dist.clamp(min_dist, max_dist);
    match model {
        AttenuationModel::None => 1.0,
        AttenuationModel::Linear => {
            let range = max_dist - min_dist;
            if range <= 0.0 {
                return 0.0;
            }
            (1.0 - rolloff * (dist - min_dist) / range).clamp(0.0, 1.0)
        }
        AttenuationModel::InverseDistance => {
            let denom = min_dist + rolloff * (dist - min_dist);
            if denom <= 0.0 {
                return 1.0;
            }
            (min_dist / denom).clamp(0.0, 1.0)
        }
        AttenuationModel::Exponential => {
            if min_dist <= 0.0 {
                return 1.0;
            }
            (dist / min_dist).powf(-rolloff).clamp(0.0, 1.0)
        }
    }
}
