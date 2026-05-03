use std::f32::consts::FRAC_PI_4;

use wide::f32x4;

use super::world::{AttenuationModel, SpatialWorld};

/// SP-10: Doppler ピッチ倍率の許容範囲。
/// 上限はリスナー超音速・分母 0 近傍の発散を抑える。
/// 下限は近接時の極端な低ピッチを抑える。
/// Unity の `AudioSource.dopplerLevel` 効果幅もおおよそこの範囲に収まる。
const DOPPLER_PITCH_MIN: f32 = 0.5;
const DOPPLER_PITCH_MAX: f32 = 4.0;

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

        // SP-10: Doppler 計算用にリスナー速度・音速を取り出す。
        // 距離計算には実リスナー位置を使う（フォーカスは仮想位置を距離減衰のみに適用するため）。
        let lpos_x = world.listener.position[0];
        let lpos_y = world.listener.position[1];
        let lpos_z = world.listener.position[2];
        let lvx = world.listener.velocity[0];
        let lvy = world.listener.velocity[1];
        let lvz = world.listener.velocity[2];
        let sound_speed = world.sound_speed.max(1e-3);

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
                apply_doppler(
                    world,
                    i + j,
                    lpos_x,
                    lpos_y,
                    lpos_z,
                    lvx,
                    lvy,
                    lvz,
                    sound_speed,
                );
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
            apply_doppler(world, i, lpos_x, lpos_y, lpos_z, lvx, lvy, lvz, sound_speed);
            i += 1;
        }
    }
}

/// SP-10: ソース 1 体分の Doppler ピッチ倍率を計算して `doppler_pitches[i]` に書き込む。
///
/// 物理式（OpenAL / 一般物理学準拠）:
/// ```text
/// f_obs / f_src = (c + v_l_toward) / (c - v_s_toward)
/// ```
/// `v_*_toward` は対側に向かう速度成分（接近正・離反負）。
/// `dopplerLevel` (Unity) で速度成分をスケールし、効果の強弱を制御する。
#[inline]
#[allow(clippy::too_many_arguments)]
fn apply_doppler(
    world: &mut SpatialWorld,
    i: usize,
    lpos_x: f32,
    lpos_y: f32,
    lpos_z: f32,
    lvx: f32,
    lvy: f32,
    lvz: f32,
    sound_speed: f32,
) {
    // spatial 無効・Doppler レベル 0・両者静止 のいずれかなら即 1.0 で抜ける。
    let level = world.doppler_levels[i];
    if !world.spatial_enabled[i] || level <= 0.0 {
        world.doppler_pitches[i] = 1.0;
        return;
    }
    let svx = world.velocities_x[i];
    let svy = world.velocities_y[i];
    let svz = world.velocities_z[i];
    if lvx == 0.0 && lvy == 0.0 && lvz == 0.0 && svx == 0.0 && svy == 0.0 && svz == 0.0 {
        world.doppler_pitches[i] = 1.0;
        return;
    }

    // listener → source の単位ベクトル。距離 0 近傍では方向不定なので無効化。
    let dx = world.positions_x[i] - lpos_x;
    let dy = world.positions_y[i] - lpos_y;
    let dz = world.positions_z[i] - lpos_z;
    let dist_sq = dx * dx + dy * dy + dz * dz;
    if dist_sq < 1e-8 {
        world.doppler_pitches[i] = 1.0;
        return;
    }
    let inv_dist = 1.0 / dist_sq.sqrt();
    let nx = dx * inv_dist;
    let ny = dy * inv_dist;
    let nz = dz * inv_dist;

    // listener が source へ向かう速度成分（正で接近）。
    let v_l_toward = lvx * nx + lvy * ny + lvz * nz;
    // source が listener へ向かう速度成分（正で接近）= -dot(v_s, n)。
    let v_s_toward = -(svx * nx + svy * ny + svz * nz);

    // dopplerLevel で効果の強弱を線形スケール。
    let v_l_scaled = v_l_toward * level;
    let v_s_scaled = v_s_toward * level;

    let num = sound_speed + v_l_scaled;
    let den = sound_speed - v_s_scaled;
    // 超音速ソース接近で den <= 0 になると ratio が非物理的（負・発散）になるため
    // 最大ピッチへ張り付かせる。リスナーが超音速で離反する (num <= 0) ケースも同様に
    // 物理的に意味がないため最小ピッチへ張り付かせる。
    let ratio = if den <= 1e-3 {
        DOPPLER_PITCH_MAX
    } else if num <= 1e-3 {
        DOPPLER_PITCH_MIN
    } else {
        num / den
    };
    world.doppler_pitches[i] = ratio.clamp(DOPPLER_PITCH_MIN, DOPPLER_PITCH_MAX);
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

    // 水平面投影ベクトルの sin(azimuth) を直接パン値に使う。
    // 真前/真後ろで pan=0、真横で pan=±1。後方半球では真後ろに向かって滑らかにセンターへ戻る。
    // 前後の区別は付かない（前後混同）が、これは HRTF を導入するまでステレオでは原理的に解消できない。
    // 旧実装の atan2 + clamp は真後ろで full L↔R の不連続を生み、後方半球が常に飽和していた。
    let horiz = (local_x * local_x + local_z * local_z).sqrt();
    let pan = if horiz > 0.0 {
        (local_x / horiz).clamp(-1.0, 1.0)
    } else {
        0.0
    };
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
