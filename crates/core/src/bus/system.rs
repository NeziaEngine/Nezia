use crate::effect::{EffectPosition, EffectSystem, EffectWorld, HpfWorld, LpfWorld, ReverbWorld};

use super::send::SendPosition;
use super::{MAX_MIX_BUFFER_SIZE, MAX_SENDS_PER_BUS, world::BusWorld};

/// バス処理システム。
///
/// `BusWorld` の mix_buffer に対して `Pre-Fader chain → Pre Send tap → Fader → Post-Fader chain
/// → Post Send tap → 親加算` の順で処理する。
/// チェーンが空のバス・Send が無いバスはエフェクト関数呼出 / Send 加算ループを完全にスキップする
/// (最速経路)。
pub struct BusSystem;

impl BusSystem {
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        world: &mut BusWorld,
        effect_world: &EffectWorld,
        lpf_world: &mut LpfWorld,
        hpf_world: &mut HpfWorld,
        reverb_world: &mut ReverbWorld,
        output_buffer: &mut [f32],
        device_channels: usize,
        sample_count: usize,
    ) {
        let sample_count = sample_count.min(MAX_MIX_BUFFER_SIZE);
        let master_dense = world.resolve(world.master_entity()).unwrap_or(0);

        // process_order をコピーして world.mix_buffer の可変借用と干渉しないようにする。
        let order: Vec<u32> = world.process_order.clone();

        for &d in &order {
            let d = d as usize;
            let start = d * MAX_MIX_BUFFER_SIZE;

            // Pre-Fader chain (空ならスキップ)。
            if world.pre_count[d] > 0 {
                let chain_len = world.pre_count[d] as usize;
                let chain_copy: [crate::effect::EffectId; crate::effect::MAX_EFFECTS_PER_BUS] =
                    world.pre_chain[d];
                let buf = &mut world.mix_buffer[start..start + sample_count];
                EffectSystem::apply_chain(
                    effect_world,
                    lpf_world,
                    hpf_world,
                    reverb_world,
                    &chain_copy[..chain_len],
                    buf,
                    device_channels,
                );
            }
            let _ = EffectPosition::Pre;

            // Pre-Fader Send tap (Fader 適用前で tap、本線 mute / gain 0 でも流れる)。
            apply_sends(world, d, start, sample_count, SendPosition::Pre);

            // Fader: mute / gain。
            if world.muted[d] {
                world.mix_buffer[start..start + sample_count].fill(0.0);
            } else {
                let g = world.gain[d];
                if g != 1.0 {
                    for s in &mut world.mix_buffer[start..start + sample_count] {
                        *s *= g;
                    }
                }
            }

            // Post-Fader chain (空ならスキップ)。
            if world.post_count[d] > 0 {
                let chain_len = world.post_count[d] as usize;
                let chain_copy: [crate::effect::EffectId; crate::effect::MAX_EFFECTS_PER_BUS] =
                    world.post_chain[d];
                let buf = &mut world.mix_buffer[start..start + sample_count];
                EffectSystem::apply_chain(
                    effect_world,
                    lpf_world,
                    hpf_world,
                    reverb_world,
                    &chain_copy[..chain_len],
                    buf,
                    device_channels,
                );
            }

            // Post-Fader Send tap (本線 mute なら 0 ミックス)。
            apply_sends(world, d, start, sample_count, SendPosition::Post);

            if d != master_dense {
                let parent = world.output_bus_dense[d] as usize;
                let parent_start = parent * MAX_MIX_BUFFER_SIZE;
                debug_assert_ne!(d, parent, "バスが自己参照しています");
                // SAFETY: d != parent（DAG なので src != parent）。
                // 重複しないスライスを別々に書き換える。
                unsafe {
                    let src_ptr = world.mix_buffer.as_ptr().add(start);
                    let dst_ptr = world.mix_buffer.as_mut_ptr().add(parent_start);
                    for i in 0..sample_count {
                        *dst_ptr.add(i) += *src_ptr.add(i);
                    }
                }
            }
        }

        let master_start = master_dense * MAX_MIX_BUFFER_SIZE;
        let copy_len = sample_count.min(output_buffer.len());
        output_buffer[..copy_len]
            .copy_from_slice(&world.mix_buffer[master_start..master_start + copy_len]);
    }
}

/// 指定バスの Send を `position` フィルタで tap して各 dest mix_buffer に gain 乗算で加算する。
///
/// `send_count == 0` のバスは即 return で hot path コストゼロ。
#[inline]
fn apply_sends(
    world: &mut BusWorld,
    src_dense: usize,
    src_start: usize,
    sample_count: usize,
    position: SendPosition,
) {
    let count = world.send_count_at(src_dense);
    if count == 0 {
        return;
    }

    // 固定長配列で send 情報をキャプチャ (借用衝突を避ける)。
    let mut destinations: [(u32, f32); MAX_SENDS_PER_BUS] = [(0, 0.0); MAX_SENDS_PER_BUS];
    let mut active = 0usize;
    for slot in 0..count {
        let (dest, gain, pos_u8) = world.send_at(src_dense, slot);
        if pos_u8 == position as u8 {
            destinations[active] = (dest, gain);
            active += 1;
        }
    }
    if active == 0 {
        return;
    }

    // src と dest が同じスライスを指すと UB になるが、サイクル検出済みのため発生しない。
    for &(dest_dense, gain) in &destinations[..active] {
        let dest_start = dest_dense as usize * MAX_MIX_BUFFER_SIZE;
        if dest_start == src_start {
            // 自分自身への Send (DAG 上ありえない) は防御的にスキップ。
            continue;
        }
        // SAFETY: src と dest は別のバスのスライスで重複しない。
        unsafe {
            let src_ptr = world.mix_buffer.as_ptr().add(src_start);
            let dst_ptr = world.mix_buffer.as_mut_ptr().add(dest_start);
            for s in 0..sample_count {
                *dst_ptr.add(s) += *src_ptr.add(s) * gain;
            }
        }
    }
}
