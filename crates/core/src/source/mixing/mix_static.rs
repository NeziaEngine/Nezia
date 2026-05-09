//! 静的バッファのミキシング (looping wrap あり、random access)。
//!
//! ファイル全体が事前展開済みの `&[f32]` に乗っているケース。`sample_offset` を
//! frame 単位で進めながら線形補間し、L/R にゲインを掛けて bus_buf へ加算する。
//! Pre-Spatial chain または send が貼られている場合は mono_scratch 経由で書き出す。

use crate::effect::{EffectId, EffectSystem, EffectWorld, EffectWorlds, MAX_EFFECTS_PER_SOURCE};

use super::send_route::{SendOutput, apply_send_outputs};

#[allow(clippy::too_many_arguments)]
pub(super) fn mix_static(
    samples: &[f32],
    src_channels: usize,
    src_frame_count: usize,
    initial_offset: f32,
    advance: f32,
    looping: bool,
    left_gain: f32,
    right_gain: f32,
    bus_buf: &mut [f32],
    device_channels: usize,
    total_frames: usize,
    pre_count: usize,
    chain_copy: &[EffectId; MAX_EFFECTS_PER_SOURCE],
    mono_scratch: &mut [f32],
    effect_world: &EffectWorld,
    effect_worlds: &mut EffectWorlds,
    send_outputs: &[SendOutput],
) -> f32 {
    let frame_count_f = src_frame_count as f32;
    let mut offset = initial_offset;

    // Send が貼られていれば pre-chain 有無に関わらず mono_scratch 経由で書き出す。
    // `pre_count == 0 && send_outputs.is_empty()` の最速経路は従来通り interleaved 直書き。
    let needs_mono_path = pre_count > 0 || !send_outputs.is_empty();

    if needs_mono_path && total_frames <= mono_scratch.len() {
        let mut local_offset = offset;
        for n in 0..total_frames {
            if looping && local_offset >= frame_count_f {
                local_offset = if frame_count_f > 0.0 {
                    local_offset.rem_euclid(frame_count_f)
                } else {
                    0.0
                };
            }
            let frame_idx = local_offset as usize;
            if frame_idx >= src_frame_count {
                for s in mono_scratch[n..total_frames].iter_mut() {
                    *s = 0.0;
                }
                break;
            }
            let frac = local_offset - local_offset.floor();
            let idx1 = if looping {
                (frame_idx + 1) % src_frame_count
            } else {
                (frame_idx + 1).min(src_frame_count - 1)
            };
            let mut acc = 0.0_f32;
            for c in 0..src_channels {
                let s0 = samples[frame_idx * src_channels + c];
                let s1 = samples[idx1 * src_channels + c];
                acc += s0 + (s1 - s0) * frac;
            }
            mono_scratch[n] = acc / src_channels.max(1) as f32;
            local_offset += advance;
        }
        if pre_count > 0 {
            EffectSystem::apply_chain(
                effect_world,
                effect_worlds,
                &chain_copy[..pre_count],
                &mut mono_scratch[..total_frames],
                1,
            );
        }
        for (n, frame) in bus_buf
            .chunks_mut(device_channels)
            .take(total_frames)
            .enumerate()
        {
            let s = mono_scratch[n];
            for (ch, out) in frame.iter_mut().enumerate() {
                let gain = match ch {
                    0 => left_gain,
                    1 => right_gain,
                    _ => (left_gain + right_gain) * 0.5,
                };
                *out += s * gain;
            }
        }
        apply_send_outputs(
            mono_scratch,
            total_frames,
            left_gain,
            right_gain,
            device_channels,
            send_outputs,
        );
        offset = local_offset;
    } else {
        for frame in bus_buf.chunks_mut(device_channels) {
            if looping && offset >= frame_count_f {
                offset = if frame_count_f > 0.0 {
                    offset.rem_euclid(frame_count_f)
                } else {
                    0.0
                };
            }
            let frame_idx = offset as usize;
            if frame_idx >= src_frame_count {
                break;
            }
            let frac = offset - offset.floor();
            let idx0 = frame_idx;
            let idx1 = if looping {
                (idx0 + 1) % src_frame_count
            } else {
                (idx0 + 1).min(src_frame_count - 1)
            };
            for (ch, out) in frame.iter_mut().enumerate() {
                let src_ch = ch % src_channels;
                let s0 = samples[idx0 * src_channels + src_ch];
                let s1 = samples[idx1 * src_channels + src_ch];
                let sample = s0 + (s1 - s0) * frac;
                let gain = match ch {
                    0 => left_gain,
                    1 => right_gain,
                    _ => (left_gain + right_gain) * 0.5,
                };
                *out += sample * gain;
            }
            offset += advance;
        }
    }

    offset
}
