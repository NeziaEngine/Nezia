use crate::effect::{
    CompressorWorld, EffectPosition, EffectSystem, EffectWorld, HpfWorld, LpfWorld, PeakingEqWorld,
    ReverbWorld,
};

use super::send::{SendDestKind, SendPosition};
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
        compressor_world: &mut CompressorWorld,
        peq_world: &mut PeakingEqWorld,
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
                    compressor_world,
                    peq_world,
                    &chain_copy[..chain_len],
                    buf,
                    device_channels,
                );
            }
            let _ = EffectPosition::Pre;

            // Pre-Fader Send tap (Fader 適用前で tap、本線 mute / gain 0 でも流れる)。
            apply_sends(
                world,
                compressor_world,
                d,
                start,
                sample_count,
                SendPosition::Pre,
            );

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
                    compressor_world,
                    peq_world,
                    &chain_copy[..chain_len],
                    buf,
                    device_channels,
                );
            }

            // Post-Fader Send tap (本線 mute なら 0 ミックス)。
            apply_sends(
                world,
                compressor_world,
                d,
                start,
                sample_count,
                SendPosition::Post,
            );

            if d != master_dense {
                let parent = world.output_bus_dense[d] as usize;
                let parent_start = parent * MAX_MIX_BUFFER_SIZE;
                debug_assert_ne!(d, parent, "バスが自己参照しています");
                // src と dest は別バスのスライスで重複しない (DAG 保証)。
                // `from_raw_parts(_mut)` で slice にして autovectorizer に no-alias を伝える。
                // SAFETY: d != parent（DAG）なので 2 領域は重複しない。
                unsafe {
                    let src_ptr = world.mix_buffer.as_ptr().add(start);
                    let dst_ptr = world.mix_buffer.as_mut_ptr().add(parent_start);
                    let src = std::slice::from_raw_parts(src_ptr, sample_count);
                    let dst = std::slice::from_raw_parts_mut(dst_ptr, sample_count);
                    for (d_s, s) in dst.iter_mut().zip(src.iter()) {
                        *d_s += *s;
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

/// 指定バスの Send を `position` フィルタで tap し、`SendDestKind` で振り分けて加算する。
///
/// - `Bus`: BusWorld の mix_buffer (dest_dense) に加算
/// - `CompressorSidechain`: CompressorWorld の sidechain_buffer (dest_dense) に加算
///
/// `send_count == 0` のバスは即 return で hot path コストゼロ。
#[inline]
fn apply_sends(
    world: &mut BusWorld,
    compressor_world: &mut CompressorWorld,
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
    let mut destinations: [(u32, f32, u8); MAX_SENDS_PER_BUS] = [(0, 0.0, 0); MAX_SENDS_PER_BUS];
    let mut active = 0usize;
    for slot in 0..count {
        let (dest, gain, pos_u8, kind_u8) = world.send_at(src_dense, slot);
        if pos_u8 == position as u8 {
            destinations[active] = (dest, gain, kind_u8);
            active += 1;
        }
    }
    if active == 0 {
        return;
    }

    for &(dest_dense, gain, kind_u8) in &destinations[..active] {
        match SendDestKind::from_u8(kind_u8) {
            Some(SendDestKind::Bus) => {
                let dest_start = dest_dense as usize * MAX_MIX_BUFFER_SIZE;
                if dest_start == src_start {
                    // 自分自身への Send (DAG 上ありえない) は防御的にスキップ。
                    continue;
                }
                // src と dest は別バスのスライスで重複しない (DAG 保証)。
                // 借用チェッカは同一 Vec の 2 領域を同時に貸せないため、
                // `from_raw_parts(_mut)` で **slice として** 参照を作って autovectorizer
                // に no-alias を伝える (`*ptr.add(s)` のままだと SIMD 化されない)。
                // SAFETY: DAG なので 2 領域は重複せず、有効な BusWorld 内バッファ。
                unsafe {
                    let src_ptr = world.mix_buffer.as_ptr().add(src_start);
                    let dst_ptr = world.mix_buffer.as_mut_ptr().add(dest_start);
                    let src = std::slice::from_raw_parts(src_ptr, sample_count);
                    let dst = std::slice::from_raw_parts_mut(dst_ptr, sample_count);
                    for (d, s) in dst.iter_mut().zip(src.iter()) {
                        *d += *s * gain;
                    }
                }
            }
            Some(SendDestKind::CompressorSidechain) => {
                // BusWorld と CompressorWorld は別オブジェクト → 借用は両立する。
                // src スライスを先に切り出して compressor の可変借用と組み合わせる。
                let src_end = src_start + sample_count;
                if src_end > world.mix_buffer.len() {
                    continue;
                }
                let src_ptr = world.mix_buffer.as_ptr();
                if let Some(dst) = compressor_world.sidechain_slice_mut(dest_dense, sample_count) {
                    // SAFETY: src は BusWorld 内、dst は CompressorWorld 内で重複なし。
                    // `from_raw_parts` で slice 化して autovectorize を許す。
                    let src =
                        unsafe { std::slice::from_raw_parts(src_ptr.add(src_start), dst.len()) };
                    for (d, s) in dst.iter_mut().zip(src.iter()) {
                        *d += *s * gain;
                    }
                }
            }
            None => {} // 未知 kind は無視 (将来 variant 追加への余地)。
        }
    }
}
