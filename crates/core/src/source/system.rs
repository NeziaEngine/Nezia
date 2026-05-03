use std::sync::Arc;

use crate::audio::AudioBuffer;
use crate::effect::{EffectSystem, EffectWorld, HpfWorld, LpfWorld, ReverbWorld};
use crate::spatial::{SpatialSystem, SpatialWorld};

use super::virtualizer::VoiceVirtualizer;
use super::world::{SourceState, SourceWorld};

/// Source ミキシングシステム。
///
/// `SourceWorld` のコンポーネントを読み出し、バスの mix_buffer に加算ミキシングする。
/// 再生が完了した Source の despawn も担当する。
///
/// # Pre-Spatial エフェクトチェーン (Phase 2-3 PR 2)
///
/// `SourceWorld::pre_chain` に登録されたエフェクトを **resampler 後・Spatial 適用前** の
/// モノラル信号に対して適用する。チェーン空のソースは関数呼出を完全にスキップする
/// (最速経路)。スクラッチバッファは呼出側 (`AudioThread`) が事前確保したものを借用する。
pub struct SourceMixingSystem;

impl SourceMixingSystem {
    /// 毎オーディオコールバックで呼び出す update 処理。
    ///
    /// `bus_mix_buffer` は呼び出し前にゼロクリアされている前提。
    /// `mono_scratch` は最低 `sample_count / device_channels` フレーム分の容量を持つ
    /// 事前確保済みバッファ (Pre-Spatial chain 適用に使う)。
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        world: &mut SourceWorld,
        spatial: &mut SpatialWorld,
        effect_world: &EffectWorld,
        lpf_world: &mut LpfWorld,
        hpf_world: &mut HpfWorld,
        reverb_world: &mut ReverbWorld,
        mono_scratch: &mut [f32],
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

        // Phase 1.5: Voice Virtualization。空間ゲインを使って実効可聴度をスコアリングし、
        // 上位 MAX_PHYSICAL_VOICES のみ物理化、残りを仮想化する。
        VoiceVirtualizer::rebalance(world, spatial);

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
            // SP-10: Doppler ピッチ倍率を再生レートに反映する。
            let doppler = spatial.doppler_pitches[source_i];
            let rate_ratio = audio_buf.sample_rate as f32 / device_sample_rate;
            let advance = pitch * doppler * rate_ratio;

            // Voice Virtualization: 仮想ボイスは sample_offset だけ前進してミキシングをスキップ。
            if world.is_virtual[source_i] {
                let frames_advanced = advance * (sample_count / device_channels.max(1)) as f32;
                let mut offset = world.sample_offset[source_i] + frames_advanced;
                let frame_count_f = audio_buf.frame_count() as f32;
                if world.looping[source_i] && frame_count_f > 0.0 {
                    offset = offset.rem_euclid(frame_count_f);
                }
                world.sample_offset[source_i] = offset;
                continue;
            }
            let src_channels = audio_buf.channels as usize;
            let src_frame_count = audio_buf.frame_count();

            let left_gain = spatial.left_gains[source_i];
            let right_gain = spatial.right_gains[source_i];

            let bus_offset = world.output_bus[source_i] as usize * bus_stride;
            let process_len = sample_count.min(bus_stride);
            let bus_buf = &mut bus_mix_buffer[bus_offset..bus_offset + process_len];

            let mut offset = world.sample_offset[source_i];
            let looping = world.looping[source_i];
            let frame_count_f = src_frame_count as f32;
            let total_frames = process_len / device_channels.max(1);

            // ── Pre-Spatial チェーンが空でなければスクラッチに mono 信号を書き出してフィルタする。
            //   その後、frame ごとに scratch[n] を L/R gain で加算する。
            //   チェーン空なら従来どおり audio_buf から直接サンプリング (最速経路維持)。
            let pre_count = world.pre_count[source_i] as usize;
            if pre_count > 0 && total_frames <= mono_scratch.len() {
                // resampler 段: scratch[..total_frames] にモノラル信号 (L+R 平均) を書き出す
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
                        // 末尾到達後は 0 で埋める (フィルタ状態は維持)。
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
                    // モノラル化: 全 src_channels の平均
                    let mut acc = 0.0_f32;
                    for c in 0..src_channels {
                        let s0 = audio_buf.samples[frame_idx * src_channels + c];
                        let s1 = audio_buf.samples[idx1 * src_channels + c];
                        acc += s0 + (s1 - s0) * frac;
                    }
                    mono_scratch[n] = acc / src_channels.max(1) as f32;
                    local_offset += advance;
                }

                // Pre-Spatial chain を適用 (channels=1 のモノラル経路)。
                let chain_copy: [crate::effect::EffectId; crate::effect::MAX_EFFECTS_PER_SOURCE] =
                    world.pre_chain[source_i];
                EffectSystem::apply_chain(
                    effect_world,
                    lpf_world,
                    hpf_world,
                    reverb_world,
                    &chain_copy[..pre_count],
                    &mut mono_scratch[..total_frames],
                    1,
                );

                // Spatial gain を掛けて bus mix_buffer に加算。
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
                offset = local_offset;
            } else {
                // 既存の最速経路 (Pre-Spatial chain なし)。
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
                        let s0 = audio_buf.samples[idx0 * src_channels + src_ch];
                        let s1 = audio_buf.samples[idx1 * src_channels + src_ch];
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

            world.sample_offset[source_i] = offset;
        }
    }
}
