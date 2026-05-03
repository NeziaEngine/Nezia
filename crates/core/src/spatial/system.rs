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

        // SP-06: 距離計算用とパンニング計算用の仮想リスナー位置を
        // フレーム頭で 1 回だけ計算（ホットループ内で lerp しない）。
        let vpos_dist = world.listener.virtual_position_for_distance();
        let vpos_pan = world.listener.virtual_position_for_direction();

        let ldx = vpos_dist[0];
        let ldy = vpos_dist[1];
        let ldz = vpos_dist[2];
        let lpx = vpos_pan[0];
        let lpy = vpos_pan[1];
        let lpz = vpos_pan[2];

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

            // 距離は vpos_dist 基準。
            let ddx = px - f32x4::splat(ldx);
            let ddy = py - f32x4::splat(ldy);
            let ddz = pz - f32x4::splat(ldz);
            let dist: [f32; 4] = (ddx * ddx + ddy * ddy + ddz * ddz).sqrt().into();

            // パンニングは vpos_pan 基準。
            let pdx = px - f32x4::splat(lpx);
            let pdy = py - f32x4::splat(lpy);
            let pdz = pz - f32x4::splat(lpz);
            let local_x: [f32; 4] =
                (pdx * f32x4::splat(rx) + pdy * f32x4::splat(ry) + pdz * f32x4::splat(rz)).into();
            let local_z: [f32; 4] =
                (pdx * f32x4::splat(fx) + pdy * f32x4::splat(fy) + pdz * f32x4::splat(fz)).into();

            for j in 0..4 {
                apply_gains(world, vols, i + j, dist[j], local_x[j], local_z[j]);
            }

            i += 4;
        }

        // スカラーパス: 端数（n % 4 件）。
        while i < n {
            let ddx = world.positions_x[i] - ldx;
            let ddy = world.positions_y[i] - ldy;
            let ddz = world.positions_z[i] - ldz;
            let dist = (ddx * ddx + ddy * ddy + ddz * ddz).sqrt();

            let pdx = world.positions_x[i] - lpx;
            let pdy = world.positions_y[i] - lpy;
            let pdz = world.positions_z[i] - lpz;
            let local_x = pdx * rx + pdy * ry + pdz * rz;
            let local_z = pdx * fx + pdy * fy + pdz * fz;

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
