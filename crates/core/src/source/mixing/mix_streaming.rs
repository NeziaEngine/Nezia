//! ストリーミングバッファのミキシング (worker が loop / EOF を吸収済み、wrap 不要)。
//!
//! `MirrorRing` から peek した contiguous slice を線形補間で読みつつ bus_buf へ加算する。
//! 戻り値は ring から消費したフレーム数 (`sample_offset` の進行量)。

use std::sync::Arc;

use crate::effect::{EffectId, EffectSystem, EffectWorld, EffectWorlds, MAX_EFFECTS_PER_SOURCE};
use crate::streaming::StreamingState;

use super::send_route::{SendOutput, apply_send_outputs};

#[allow(clippy::too_many_arguments)]
pub(super) fn mix_streaming(
    stream: &Arc<StreamingState>,
    src_channels: usize,
    advance: f32,
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
) -> usize {
    // ring から最大限の contiguous slice を peek (lookahead +2 frame で線形補間用)。
    let needed = ((total_frames as f32 * advance.abs()).ceil() as usize).saturating_add(2);
    let window = stream.ring.peek(needed);
    let win_frames = if src_channels == 0 {
        0
    } else {
        window.len() / src_channels
    };

    if win_frames == 0 {
        // 完全アンダーラン: 出力は無加算 (静音)。worker 進捗待ち。
        stream.mark_underrun();
        return 0;
    }

    // looping wrap は worker 責務なので静的版の `frame_count_f` 比較は不要。
    // 線形補間の上限を win_frames - 1 で守るだけ。

    let needs_mono_path = pre_count > 0 || !send_outputs.is_empty();

    if needs_mono_path && total_frames <= mono_scratch.len() {
        let mut local_offset = 0.0_f32;
        let mut underrun_at: Option<usize> = None;
        for (n, slot) in mono_scratch.iter_mut().take(total_frames).enumerate() {
            let frame_idx = local_offset as usize;
            if frame_idx + 1 >= win_frames {
                underrun_at = Some(n);
                break;
            }
            let frac = local_offset - frame_idx as f32;
            let idx1 = frame_idx + 1;
            let mut acc = 0.0_f32;
            for c in 0..src_channels {
                let s0 = window[frame_idx * src_channels + c];
                let s1 = window[idx1 * src_channels + c];
                acc += s0 + (s1 - s0) * frac;
            }
            *slot = acc / src_channels.max(1) as f32;
            local_offset += advance;
        }
        if let Some(n) = underrun_at {
            for s in mono_scratch[n..total_frames].iter_mut() {
                *s = 0.0;
            }
            stream.mark_underrun();
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
        let consumed = (local_offset.floor() as usize).min(win_frames);
        stream.ring.advance_read(consumed);
        return consumed;
    }

    // 最速経路 (Pre-Spatial chain なし)。
    let mut local_offset = 0.0_f32;
    let mut underrun = false;
    for frame in bus_buf.chunks_mut(device_channels) {
        let frame_idx = local_offset as usize;
        if frame_idx + 1 >= win_frames {
            underrun = true;
            break;
        }
        let frac = local_offset - frame_idx as f32;
        let idx0 = frame_idx;
        let idx1 = idx0 + 1;
        for (ch, out) in frame.iter_mut().enumerate() {
            let src_ch = ch % src_channels.max(1);
            let s0 = window[idx0 * src_channels + src_ch];
            let s1 = window[idx1 * src_channels + src_ch];
            let sample = s0 + (s1 - s0) * frac;
            let gain = match ch {
                0 => left_gain,
                1 => right_gain,
                _ => (left_gain + right_gain) * 0.5,
            };
            *out += sample * gain;
        }
        local_offset += advance;
    }
    if underrun {
        stream.mark_underrun();
    }
    let consumed = (local_offset.floor() as usize).min(win_frames);
    stream.ring.advance_read(consumed);
    consumed
}
