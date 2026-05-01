use std::sync::Arc;

use crate::audio::AudioBuffer;
use crate::spatial::{SpatialSystem, SpatialWorld};

use super::world::{SourceState, SourceWorld};

/// Source ミキシングシステム。
///
/// `SourceWorld` のコンポーネントを読み出し、バスの mix_buffer に加算ミキシングする。
/// 再生が完了した Source の despawn も担当する。
pub struct SourceMixingSystem;

impl SourceMixingSystem {
    /// 毎オーディオコールバックで呼び出す update 処理。
    ///
    /// 全アクティブ Source の AudioBuffer からサンプルを読み出し、
    /// `bus_mix_buffer[output_bus * bus_stride ..]` に加算ミキシングする。
    /// 再生が完了した Source は自動的に despawn される。
    ///
    /// `bus_mix_buffer` は呼び出し前にゼロクリアされている前提。
    pub fn update(
        world: &mut SourceWorld,
        spatial: &mut SpatialWorld,
        bus_mix_buffer: &mut [f32],
        bus_stride: usize,
        sample_count: usize,
        device_channels: usize,
        device_sample_rate: f32,
        buffers: &[Option<Arc<AudioBuffer>>],
    ) {
        let source_count = world.vol.len();
        if source_count == 0 {
            return;
        }

        // Phase 1: 空間ゲインを計算（SpatialWorld に書き込む）。
        SpatialSystem::compute_gains(spatial, &world.vol, source_count);

        // Phase 2: ミキシング。
        for source_i in 0..source_count {
            if world.state[source_i] != SourceState::Playing {
                continue;
            }
            let buf_idx = world.audio_buffer_index[source_i] as usize;
            let Some(audio_buf) = buffers.get(buf_idx).and_then(|b| b.as_ref()) else {
                continue;
            };

            let pitch = world.pitch[source_i];
            let rate_ratio = audio_buf.sample_rate as f32 / device_sample_rate;
            let advance = pitch * rate_ratio;
            let src_channels = audio_buf.channels as usize;
            let src_frame_count = audio_buf.frame_count();

            let left_gain = spatial.left_gains[source_i];
            let right_gain = spatial.right_gains[source_i];

            let bus_offset = world.output_bus[source_i] as usize * bus_stride;
            let process_len = sample_count.min(bus_stride);
            let bus_buf = &mut bus_mix_buffer[bus_offset..bus_offset + process_len];

            let mut offset = world.sample_offset[source_i];

            for frame in bus_buf.chunks_mut(device_channels) {
                let frame_idx = offset as usize;
                if frame_idx >= src_frame_count {
                    break;
                }

                let frac = offset - offset.floor();
                let idx0 = frame_idx;
                let idx1 = (idx0 + 1).min(src_frame_count - 1);

                for (ch, out) in frame.iter_mut().enumerate() {
                    let src_ch = ch % src_channels;
                    let s0 = audio_buf.samples[idx0 * src_channels + src_ch];
                    let s1 = audio_buf.samples[idx1 * src_channels + src_ch];
                    let sample = s0 + (s1 - s0) * frac;
                    // ch0=L, ch1=R, それ以外は中央（L+R の平均）。
                    let gain = match ch {
                        0 => left_gain,
                        1 => right_gain,
                        _ => (left_gain + right_gain) * 0.5,
                    };
                    *out += sample * gain;
                }

                offset += advance;
            }

            world.sample_offset[source_i] = offset;
        }

    }
}
