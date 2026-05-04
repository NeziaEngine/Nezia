//! Phase 3-2: Mixer Snapshot のサウンドスレッド側適用ロジック。
//!
//! `Command::ApplySnapshot` 受信時に Snapshot を ID 解決 + 現在値キャプチャして
//! `ActiveSnapshot` に展開し、毎コールバックで lerp して各 *World に書き戻す。

use std::sync::Arc;

use arc_swap::ArcSwap;

use crate::bus::BusWorld;
use crate::effect::{EffectWorld, HpfParam, HpfWorld, LpfParam, LpfWorld, ReverbParam, ReverbWorld};

/// `Command::ApplySnapshot` 処理本体。Snapshot を resolve + 全エントリを ID 解決 +
/// 現在値キャプチャして `ActiveSnapshot` に展開する。
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_snapshot(
    snapshot_index: u32,
    fade_samples: u64,
    shared_snapshots: &Arc<ArcSwap<Vec<Option<Arc<crate::snapshot::Snapshot>>>>>,
    active: &mut crate::snapshot::ActiveSnapshot,
    bus_world: &BusWorld,
    effect_world: &EffectWorld,
    lpf_world: &LpfWorld,
    hpf_world: &HpfWorld,
    reverb_world: &ReverbWorld,
) {
    let snapshots = shared_snapshots.load();
    let Some(snapshot) = snapshots
        .get(snapshot_index as usize)
        .and_then(|s| s.as_ref())
    else {
        return;
    };

    // 既存の進行中補間を破棄して再構築する (interrupt-and-restart)。
    active.clear();
    active.fade_total_samples = fade_samples;
    active.fade_remaining_samples = fade_samples;

    // ── バスゲイン ──
    for entry in &snapshot.bus_gains {
        if let Some(dense) = bus_world.resolve(entry.bus) {
            let from = bus_world.gains()[dense];
            active.bus_gain_dense.push(dense as u32);
            active.bus_gain_from.push(from);
            active.bus_gain_to.push(entry.gain);
        }
    }
    // ── バスミュート ──
    for entry in &snapshot.bus_muted {
        if let Some(dense) = bus_world.resolve(entry.bus) {
            active.bus_muted_dense.push(dense as u32);
            active.bus_muted_to.push(entry.muted);
            active.bus_muted_applied.push(false);
        }
    }
    // ── エフェクトパラメータ ──
    for entry in &snapshot.effect_params {
        let Some(meta_dense) = effect_world.resolve(entry.effect) else {
            continue;
        };
        let kind = effect_world.kinds()[meta_dense];
        let state_index = effect_world.state_indices()[meta_dense];
        // kind 不一致は no-op (ユーザー側の指定ミス)。
        let from = match (kind, entry.kind) {
            (crate::effect::EffectKind::Lpf, crate::snapshot::SnapshotEffectKind::Lpf) => {
                read_lpf_param(lpf_world, state_index, entry.param)
            }
            (crate::effect::EffectKind::Hpf, crate::snapshot::SnapshotEffectKind::Hpf) => {
                read_hpf_param(hpf_world, state_index, entry.param)
            }
            (crate::effect::EffectKind::Reverb, crate::snapshot::SnapshotEffectKind::Reverb) => {
                read_reverb_param(reverb_world, state_index, entry.param)
            }
            _ => continue,
        };
        active.effect_kind.push(entry.kind);
        active.effect_state_dense.push(state_index);
        active.effect_param.push(entry.param);
        active.effect_from.push(from);
        active.effect_to.push(entry.value);
    }

    // fade_samples = 0 のときは active.is_active() が
    // has_pending_bool_changes / 補間有 のいずれかで true になり、続く
    // tick_snapshot_interpolation で即時適用される (`fade_total=0` で `t=1.0` 計算)。
}

/// 1 コールバック分 (`samples` フレーム) 進めて補間値を BusWorld / 各 *World に書き戻す。
pub(super) fn tick_snapshot_interpolation(
    active: &mut crate::snapshot::ActiveSnapshot,
    samples: u64,
    bus_world: &mut BusWorld,
    lpf_world: &mut LpfWorld,
    hpf_world: &mut HpfWorld,
    reverb_world: &mut ReverbWorld,
) {
    // 進行率 t を計算。fade_total_samples == 0 のときは即時 (t = 1.0)。
    let t = if active.fade_total_samples == 0 {
        1.0_f32
    } else {
        let consumed = active
            .fade_total_samples
            .saturating_sub(active.fade_remaining_samples);
        let next_consumed = (consumed + samples).min(active.fade_total_samples);
        next_consumed as f32 / active.fade_total_samples as f32
    };

    // 残りサンプルを減算。
    if active.fade_remaining_samples > samples {
        active.fade_remaining_samples -= samples;
    } else {
        active.fade_remaining_samples = 0;
    }

    // ── バスゲイン lerp (dB 空間で線形補間) ──
    // 線形ゲイン空間で lerp すると人間の聴覚 (対数) と一致せず、
    // 「序盤ほぼ変化なし → 終盤急減」と感じる。dB 空間で lerp することで
    // 聴感的に一様なフェードになる (Unity AudioMixer と互換)。
    for i in 0..active.bus_gain_dense.len() {
        let dense = active.bus_gain_dense[i] as usize;
        let from = active.bus_gain_from[i];
        let to = active.bus_gain_to[i];
        let v = lerp_db_gain(from, to, t);
        bus_world.write_gain_by_dense(dense, v);
    }

    // ── バスミュート ──
    // 補間できない bool 値は **fade 完了時にのみ snap** する (Phase 3-2 設計)。
    // ユーザーが「フェードアウトしてからミュート」を実現したい場合は、同じ snapshot に
    // `set_bus_gain(0.0)` も併記することで gain が滑らかに 0 へ向かい、完了時にミュート
    // フラグが立つ。中点 (`t >= 0.5`) snap は時間差で gain と muted の整合が取れず
    // プチノイズの原因になるため採用しない。
    // 完了書き込みは下の fade_remaining == 0 ブロックで行う。

    // ── エフェクトパラメータ lerp ──
    for i in 0..active.effect_kind.len() {
        let state_dense = active.effect_state_dense[i];
        let param = active.effect_param[i];
        let from = active.effect_from[i];
        let to = active.effect_to[i];
        let v = from + (to - from) * t;
        match active.effect_kind[i] {
            crate::snapshot::SnapshotEffectKind::Lpf => {
                if param == LpfParam::Cutoff as u8 {
                    lpf_world.set_cutoff(state_dense, v);
                } else if param == LpfParam::Q as u8 {
                    lpf_world.set_q(state_dense, v);
                }
            }
            crate::snapshot::SnapshotEffectKind::Hpf => {
                if param == HpfParam::Cutoff as u8 {
                    hpf_world.set_cutoff(state_dense, v);
                } else if param == HpfParam::Q as u8 {
                    hpf_world.set_q(state_dense, v);
                }
            }
            crate::snapshot::SnapshotEffectKind::Reverb => {
                if param == ReverbParam::RoomSize as u8 {
                    reverb_world.set_room_size(state_dense, v);
                } else if param == ReverbParam::Damping as u8 {
                    reverb_world.set_damping(state_dense, v);
                } else if param == ReverbParam::Wet as u8 {
                    reverb_world.set_wet(state_dense, v);
                } else if param == ReverbParam::Dry as u8 {
                    reverb_world.set_dry(state_dense, v);
                } else if param == ReverbParam::Width as u8 {
                    reverb_world.set_width(state_dense, v);
                }
            }
        }
    }

    // fade 完了で全 muted を確実に適用 + clear。
    if active.fade_remaining_samples == 0 {
        for i in 0..active.bus_muted_dense.len() {
            if !active.bus_muted_applied[i] {
                let dense = active.bus_muted_dense[i] as usize;
                bus_world.write_muted_by_dense(dense, active.bus_muted_to[i]);
                active.bus_muted_applied[i] = true;
            }
        }
        active.clear();
    }
}

/// dB 空間で線形補間してから linear gain に戻す (Phase 3-2)。
/// 端点 (`t <= 0` / `t >= 1`) では正確に from / to を返す。
/// `from` / `to` が 0 (= -∞ dB) のとき内部で `1e-5` (= -100 dB) に floor して計算するが、
/// 端点では正確な 0 を返すため、フェード完了時は厳密に 0 になる。
#[inline]
fn lerp_db_gain(from: f32, to: f32, t: f32) -> f32 {
    if t >= 1.0 {
        return to;
    }
    if t <= 0.0 {
        return from;
    }
    if from == to {
        return from;
    }
    // -100 dB をフロアとして扱う (聴感上は完全な silence と区別不能)。
    const MIN_LIN: f32 = 1e-5;
    let from_db = 20.0 * from.max(MIN_LIN).log10();
    let to_db = 20.0 * to.max(MIN_LIN).log10();
    let v_db = from_db + (to_db - from_db) * t;
    10.0_f32.powf(v_db / 20.0)
}

#[inline]
fn read_lpf_param(world: &LpfWorld, state_dense: u32, param: u8) -> f32 {
    if param == LpfParam::Cutoff as u8 {
        world.cutoffs()[state_dense as usize]
    } else if param == LpfParam::Q as u8 {
        world.qs()[state_dense as usize]
    } else {
        0.0
    }
}

#[inline]
fn read_hpf_param(world: &HpfWorld, state_dense: u32, param: u8) -> f32 {
    if param == HpfParam::Cutoff as u8 {
        world.cutoffs()[state_dense as usize]
    } else if param == HpfParam::Q as u8 {
        world.qs()[state_dense as usize]
    } else {
        0.0
    }
}

#[inline]
fn read_reverb_param(world: &ReverbWorld, state_dense: u32, param: u8) -> f32 {
    let Some((rs, dp, wet, dry, width)) = world.params_at(state_dense) else {
        return 0.0;
    };
    if param == ReverbParam::RoomSize as u8 {
        rs
    } else if param == ReverbParam::Damping as u8 {
        dp
    } else if param == ReverbParam::Wet as u8 {
        wet
    } else if param == ReverbParam::Dry as u8 {
        dry
    } else if param == ReverbParam::Width as u8 {
        width
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::lerp_db_gain;

    fn approx(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps
    }

    #[test]
    fn db_lerp_returns_exact_endpoints() {
        assert!(approx(lerp_db_gain(1.0, 0.0, 0.0), 1.0, 1e-6));
        assert!(approx(lerp_db_gain(1.0, 0.0, 1.0), 0.0, 1e-6));
        assert!(approx(lerp_db_gain(0.5, 0.25, 0.0), 0.5, 1e-6));
        assert!(approx(lerp_db_gain(0.5, 0.25, 1.0), 0.25, 1e-6));
    }

    #[test]
    fn db_lerp_no_change_when_from_eq_to() {
        assert!(approx(lerp_db_gain(0.7, 0.7, 0.0), 0.7, 1e-6));
        assert!(approx(lerp_db_gain(0.7, 0.7, 0.5), 0.7, 1e-6));
        assert!(approx(lerp_db_gain(0.7, 0.7, 1.0), 0.7, 1e-6));
    }

    #[test]
    fn db_lerp_midpoint_below_linear() {
        // 1.0 (= 0 dB) → 0.5 (= -6.02 dB) の中点。
        // 線形空間: 0.75。dB 空間: 約 0.707 (= -3 dB)。
        // dB 空間補間は線形より小さい値になる (聴感的に半分まで下がる timing が早い)。
        let mid = lerp_db_gain(1.0, 0.5, 0.5);
        assert!(
            mid < 0.75,
            "dB lerp midpoint should be < linear (0.75), got {mid}"
        );
        assert!(approx(mid, 0.7071, 1e-3));
    }

    #[test]
    fn db_lerp_to_zero_floors_then_snaps_at_endpoint() {
        // from=1.0, to=0.0 で fade。
        // 中盤は MIN_LIN floor に従い -∞ ではなく -100 dB へ向かうが、
        // t=1.0 では厳密に 0.0 を返す (snap)。
        let mid = lerp_db_gain(1.0, 0.0, 0.5);
        assert!(
            mid > 0.0 && mid < 0.01,
            "midpoint should be near silence, got {mid}"
        );
        assert_eq!(lerp_db_gain(1.0, 0.0, 1.0), 0.0, "endpoint must be exact 0");
    }

    #[test]
    fn db_lerp_from_zero_starts_at_silence() {
        // fade-in: 0.0 → 1.0。
        // 開始は厳密に 0.0、終端は厳密に 1.0、中盤は対数的に立ち上がる。
        assert_eq!(lerp_db_gain(0.0, 1.0, 0.0), 0.0);
        assert_eq!(lerp_db_gain(0.0, 1.0, 1.0), 1.0);
        let mid = lerp_db_gain(0.0, 1.0, 0.5);
        // dB lerp from -100 to 0 at t=0.5 → -50 dB ≈ 0.00316
        assert!(mid > 0.0 && mid < 0.01);
    }

    #[test]
    fn db_lerp_clamps_t_out_of_range() {
        // t < 0 / t > 1 でも端点に clamp される。
        assert_eq!(lerp_db_gain(1.0, 0.0, -0.5), 1.0);
        assert_eq!(lerp_db_gain(1.0, 0.0, 1.5), 0.0);
    }
}
