use crate::effect::{EffectPosition, EffectSystem, EffectWorld, HpfWorld, LpfWorld, ReverbWorld};

use super::{MAX_MIX_BUFFER_SIZE, world::BusWorld};

/// バス処理システム。
///
/// `BusWorld` の mix_buffer に対して `Pre-Fader → gain/mute → Post-Fader` の
/// 3 ステージで処理を行い、最終出力を output_buffer に書き出す。
/// チェーンが空のバスはエフェクト関数呼出を完全にスキップする (最速経路)。
pub struct BusSystem;

impl BusSystem {
    /// バス処理を行い、最終出力を `output_buffer` に書き出す。
    ///
    /// `process_order` 順（リーフ→ルート）に:
    /// 1. Pre-Fader エフェクトチェーンを適用。
    /// 2. mute されていればゼロ埋め、そうでなければ gain を乗算 (Fader)。
    /// 3. Post-Fader エフェクトチェーンを適用。
    /// 4. マスターバス以外は親バスの mix_buffer に加算。
    /// 5. マスターバスの mix_buffer を `output_buffer` にコピー。
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

            if d != master_dense {
                let parent = world.output_bus_dense[d] as usize;
                let parent_start = parent * MAX_MIX_BUFFER_SIZE;
                debug_assert_ne!(d, parent, "バスが自己参照しています");
                // SAFETY: d != parent（木構造なので自己参照なし）。
                // d と parent は異なるバスのスライスを指すため、重複しない。
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
