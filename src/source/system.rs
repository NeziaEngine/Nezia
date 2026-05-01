use std::sync::Arc;

use crate::audio::AudioBuffer;

use super::world::{SourceState, SourceWorld};

/// Source ミキシングシステム。
///
/// `SourceWorld` のコンポーネントを読み出し、バスの mix_buffer に加算ミキシングする。
/// 再生が完了した Source の despawn も担当する。
pub struct SourceSystem;

impl SourceSystem {
    /// 毎オーディオコールバックで呼び出す update 処理。
    ///
    /// 全アクティブ Source の AudioBuffer からサンプルを読み出し、
    /// `bus_mix_buffer[output_bus * bus_stride ..]` に加算ミキシングする。
    /// 再生が完了した Source は自動的に despawn される。
    ///
    /// `bus_mix_buffer` は呼び出し前にゼロクリアされている前提。
    pub fn update(
        world: &mut SourceWorld,
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

        let (vols, pitches, offsets, buf_indices, states, output_buses) = (
            &world.vol,
            &world.pitch,
            &mut world.sample_offset,
            &world.audio_buffer_index,
            &world.state,
            &world.output_bus,
        );

        // 各 Source からサンプルを読み出し、出力先バスの mix_buffer に加算ミキシングする。
        // Playing 状態の Source のみミキシング対象。
        for source_i in 0..source_count {
            if states[source_i] != SourceState::Playing {
                continue;
            }
            let buf_idx = buf_indices[source_i] as usize;
            let Some(audio_buf) = buffers.get(buf_idx).and_then(|b| b.as_ref()) else {
                continue;
            };

            let vol = vols[source_i];
            let pitch = pitches[source_i];
            let rate_ratio = audio_buf.sample_rate as f32 / device_sample_rate;
            let advance = pitch * rate_ratio;
            let src_channels = audio_buf.channels as usize;
            let src_frame_count = audio_buf.frame_count();

            let bus_offset = output_buses[source_i] as usize * bus_stride;
            let process_len = sample_count.min(bus_stride);
            let bus_buf = &mut bus_mix_buffer[bus_offset..bus_offset + process_len];

            let mut offset = offsets[source_i];

            for frame in bus_buf.chunks_mut(device_channels) {
                let frame_idx = offset as usize;
                if frame_idx >= src_frame_count {
                    break;
                }

                // 線形補間でサブサンプル精度の再生位置をサポート。
                let frac = offset - offset.floor();
                let idx0 = frame_idx;
                let idx1 = (idx0 + 1).min(src_frame_count - 1);

                for (ch, out) in frame.iter_mut().enumerate() {
                    let src_ch = ch % src_channels;
                    let s0 = audio_buf.samples[idx0 * src_channels + src_ch];
                    let s1 = audio_buf.samples[idx1 * src_channels + src_ch];
                    let sample = s0 + (s1 - s0) * frac;
                    *out += sample * vol;
                }

                offset += advance;
            }

            offsets[source_i] = offset;
        }

        // 再生が終了した / 停止済み / Free の Source を逆順で despawn。
        for source_i in (0..world.vol.len()).rev() {
            let should_despawn = match world.state[source_i] {
                SourceState::Stopped | SourceState::Free => true,
                SourceState::Playing => {
                    let buf_idx = world.audio_buffer_index[source_i] as usize;
                    match buffers.get(buf_idx).and_then(|b| b.as_ref()) {
                        Some(ab) => world.sample_offset[source_i] as usize >= ab.frame_count(),
                        None => true,
                    }
                }
                SourceState::Pausing => false,
            };
            if should_despawn {
                world.despawn_by_dense_index(source_i);
            }
        }
    }
}
