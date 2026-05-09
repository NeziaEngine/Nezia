use std::sync::Arc;

use crate::audio::AudioBuffer;
use crate::effect::{EffectSystem, EffectWorld, EffectWorlds};
use crate::spatial::{AttenuationCurve, SpatialSystem, SpatialWorld};

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
///
/// # ストリーミングバッファ (Phase 2-4)
///
/// `AudioBuffer::Streaming` を参照するソースは、ワーカが供給する `MirrorRing` から
/// **contiguous slice** を peek してミキシングする。looping wrap や frame_count 判定は
/// ワーカ責務 (`docs/design/core/streaming.md`) なので mixing 側 inner loop には現れない。
/// 静的・streaming で per-source 1 度の cold dispatch + dense 配列 inner loop 構造は同形。
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
        effect_worlds: &mut EffectWorlds,
        mono_scratch: &mut [f32],
        start_offset_scratch: &mut [u32],
        bus_mix_buffer: &mut [f32],
        bus_stride: usize,
        sample_count: usize,
        device_channels: usize,
        device_sample_rate: f32,
        clock_at_callback_start: u64,
        frames_in_callback: u64,
        buffers: &[Option<Arc<AudioBuffer>>],
        curves: &[Option<Arc<AttenuationCurve>>],
    ) {
        let source_count = world.vol.len();
        if source_count == 0 {
            return;
        }

        // Phase 0.5 (Phase 3-4): 予約再生の発音判定。Scheduled で start_dsp_frame が
        // この callback の区間内 [clock_at_callback_start, clock_at_callback_start + frames_in_callback)
        // にあるソースを Playing 化し、サブ callback offset を `start_offset_scratch` に記録する。
        // 過去指定 (start_dsp_frame <= clock_at_callback_start) は offset 0 で即時発音。
        activate_scheduled(
            world,
            start_offset_scratch,
            clock_at_callback_start,
            frames_in_callback,
        );

        // Phase 1: 空間ゲインを計算（SpatialWorld に書き込む）。
        SpatialSystem::compute_gains(spatial, &world.vol, source_count, curves);

        // Phase 1.5: Voice Virtualization。空間ゲインを使って実効可聴度をスコアリングし、
        // 上位 MAX_PHYSICAL_VOICES のみ物理化、残りを仮想化する。
        // Scheduled は state != Playing で virtualizer から自然に除外される。
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
            // streaming は frame_count=0 (worker 管理) のため looping wrap は適用されない。
            if world.is_virtual[source_i] {
                // Phase 3-4: 予約再生で当該 callback 途中に発音開始した場合、virtualizer に
                // 落ちた瞬間も「鳴っていない frame 数」だけ前進させる必要がある。
                let sub = start_offset_scratch.get(source_i).copied().unwrap_or(0) as usize;
                let virt_frames = (sample_count / device_channels.max(1)).saturating_sub(sub);
                let frames_advanced = advance * virt_frames as f32;
                let mut offset = world.sample_offset[source_i] + frames_advanced;
                let frame_count_f = audio_buf.frame_count() as f32;
                if world.looping[source_i] && frame_count_f > 0.0 {
                    offset = offset.rem_euclid(frame_count_f);
                }
                world.sample_offset[source_i] = offset;
                continue;
            }

            let src_channels = audio_buf.channels as usize;
            let left_gain = spatial.left_gains[source_i];
            let right_gain = spatial.right_gains[source_i];

            let bus_offset = world.output_bus[source_i] as usize * bus_stride;
            let process_len = sample_count.min(bus_stride);
            // Phase 3-4: 予約再生でこの callback の途中から発音を始める場合、bus_buf を
            // (sub_offset_frames * device_channels) byte 進めた先頭から書き込む。前段で
            // bus_mix_buffer 全体は clear_mix_buffers でゼロクリア済みなので、書き込まなかった
            // 区間 [0, sub_offset_frames) は無音のまま残る。
            let sub_offset_frames =
                start_offset_scratch.get(source_i).copied().unwrap_or(0) as usize;
            let total_frames_full = process_len / device_channels.max(1);
            let total_frames = total_frames_full.saturating_sub(sub_offset_frames);
            if total_frames == 0 {
                // sub_offset がこの callback 全体を覆うことは activate_scheduled の判定上
                // 起きないが、防衛的に noop。
                continue;
            }
            let sub_byte_offset = sub_offset_frames * device_channels.max(1);
            let bus_buf =
                &mut bus_mix_buffer[bus_offset + sub_byte_offset..bus_offset + process_len];

            let pre_count = world.pre_count[source_i] as usize;
            let chain_copy: [crate::effect::EffectId; crate::effect::MAX_EFFECTS_PER_SOURCE] =
                world.pre_chain[source_i];

            // ── per-source 1 度の cold dispatch: 静的か streaming か ──
            if let Some(samples) = audio_buf.static_samples() {
                let src_frame_count = audio_buf.frame_count();
                let new_offset = mix_static(
                    samples,
                    src_channels,
                    src_frame_count,
                    world.sample_offset[source_i],
                    advance,
                    world.looping[source_i],
                    left_gain,
                    right_gain,
                    bus_buf,
                    device_channels,
                    total_frames,
                    pre_count,
                    &chain_copy,
                    mono_scratch,
                    effect_world,
                    effect_worlds,
                );
                world.sample_offset[source_i] = new_offset;
            } else if let Some(stream) = audio_buf.streaming_state() {
                let consumed = mix_streaming(
                    stream,
                    src_channels,
                    advance,
                    left_gain,
                    right_gain,
                    bus_buf,
                    device_channels,
                    total_frames,
                    pre_count,
                    &chain_copy,
                    mono_scratch,
                    effect_world,
                    effect_worlds,
                );
                // streaming の sample_offset は累積消費フレーム数 (file-frame ではない)。
                world.sample_offset[source_i] += consumed as f32;
            }
        }
    }
}

/// Phase 0.5 (Phase 3-4): 予約再生の発音判定。
///
/// `state == Scheduled` のソースを走査し、`start_dsp_frame` が当該 callback 区間
/// `[clock, clock + frames_in_callback)` に到達していれば `Playing` 化する。過去指定
/// (`start_dsp_frame <= clock`) は offset 0 で即時発音、callback 区間内
/// (`clock < start_dsp_frame < clock + frames_in_callback`) は frames 単位の sub-callback
/// offset を `start_offset_scratch` に書き込む。Phase 2 mix がこれを読んで bus_buf を
/// シフトする。
///
/// `start_offset_scratch` は呼出側で長さ >= `world.len()` を保証する。書き込まれる前に
/// すべて 0 にリセットしておく必要があり、その責務もここで負う (毎 callback 冒頭で実施)。
fn activate_scheduled(
    world: &mut SourceWorld,
    start_offset_scratch: &mut [u32],
    clock: u64,
    frames_in_callback: u64,
) {
    let n = world.len();
    // Phase 2 mix が参照するため、Scheduled が無いソースも含めて全件 0 リセット。
    // MAX_SOURCES = 256 で memset O(1024 byte)。
    for slot in start_offset_scratch.iter_mut().take(n) {
        *slot = 0;
    }
    // 早期リターン: Scheduled が一つも無ければ走査しない (典型的な状態)。
    let has_scheduled = world
        .states()
        .iter()
        .take(n)
        .any(|s| *s == SourceState::Scheduled);
    if !has_scheduled {
        return;
    }
    // SourceWorld のフィールドを直接借りることで `&[u64]` と `&mut [SourceState]` の
    // 同時借用を許可する (公開 accessor 経由だと world 全体の借用になり競合する)。
    let starts = &world.start_dsp_frame;
    let states = &mut world.state;
    let callback_end = clock.saturating_add(frames_in_callback);
    for i in 0..n {
        if states[i] != SourceState::Scheduled {
            continue;
        }
        let ts = starts[i];
        if ts >= callback_end {
            // この callback では発音しない (将来 callback で再評価)。
            continue;
        }
        // ts < callback_end → 発音開始。
        states[i] = SourceState::Playing;
        if ts <= clock {
            // 過去指定 / callback 冒頭ぴったり: offset 0 で即時発音。
            start_offset_scratch[i] = 0;
        } else {
            // callback 区間内: (ts - clock) frame 進んだ位置から発音開始。
            // frames_in_callback <= u32::MAX (callback サイズは数千 frame まで) なので u32 で安全。
            start_offset_scratch[i] = (ts - clock) as u32;
        }
    }
}

/// 静的バッファのミキシング (looping wrap あり、random access)。
/// Phase 2-3 までの実装をそのまま関数化。
#[allow(clippy::too_many_arguments)]
fn mix_static(
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
    chain_copy: &[crate::effect::EffectId; crate::effect::MAX_EFFECTS_PER_SOURCE],
    mono_scratch: &mut [f32],
    effect_world: &EffectWorld,
    effect_worlds: &mut EffectWorlds,
) -> f32 {
    let frame_count_f = src_frame_count as f32;
    let mut offset = initial_offset;

    if pre_count > 0 && total_frames <= mono_scratch.len() {
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
        EffectSystem::apply_chain(
            effect_world,
            effect_worlds,
            &chain_copy[..pre_count],
            &mut mono_scratch[..total_frames],
            1,
        );
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

/// ストリーミングバッファのミキシング (worker が loop / EOF を吸収済み、wrap 不要)。
///
/// 戻り値: ring から消費したフレーム数 (sample_offset の進行量)。
#[allow(clippy::too_many_arguments)]
fn mix_streaming(
    stream: &Arc<crate::streaming::StreamingState>,
    src_channels: usize,
    advance: f32,
    left_gain: f32,
    right_gain: f32,
    bus_buf: &mut [f32],
    device_channels: usize,
    total_frames: usize,
    pre_count: usize,
    chain_copy: &[crate::effect::EffectId; crate::effect::MAX_EFFECTS_PER_SOURCE],
    mono_scratch: &mut [f32],
    effect_world: &EffectWorld,
    effect_worlds: &mut EffectWorlds,
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

    if pre_count > 0 && total_frames <= mono_scratch.len() {
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
        EffectSystem::apply_chain(
            effect_world,
            effect_worlds,
            &chain_copy[..pre_count],
            &mut mono_scratch[..total_frames],
            1,
        );
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

#[cfg(test)]
mod scheduled_tests {
    use super::*;
    use crate::source::world::{SourceComponent, SourceWorld};

    fn world_with(start_dsp_frames: &[u64]) -> SourceWorld {
        let mut w = SourceWorld::new();
        for &t in start_dsp_frames {
            w.spawn(SourceComponent {
                vol: 1.0,
                pitch: 1.0,
                start_dsp_frame: t,
                ..SourceComponent::default()
            })
            .unwrap();
        }
        w
    }

    #[test]
    fn future_schedule_stays_scheduled() {
        let mut w = world_with(&[10_000]);
        let mut scratch = [0u32; 1];
        // clock = 0, callback covers [0, 512). start = 10000 → 未来。
        activate_scheduled(&mut w, &mut scratch, 0, 512);
        assert_eq!(w.states()[0], SourceState::Scheduled);
        assert_eq!(scratch[0], 0);
    }

    #[test]
    fn past_schedule_becomes_playing_with_zero_offset() {
        let mut w = world_with(&[100]);
        let mut scratch = [0u32; 1];
        // clock = 1000 (start=100 は過去), callback covers [1000, 1512).
        activate_scheduled(&mut w, &mut scratch, 1000, 512);
        assert_eq!(w.states()[0], SourceState::Playing);
        assert_eq!(scratch[0], 0);
    }

    #[test]
    fn within_callback_sets_sub_offset() {
        let mut w = world_with(&[1200]);
        let mut scratch = [0u32; 1];
        // clock = 1000, callback covers [1000, 1512). start = 1200 → 200 frame 目。
        activate_scheduled(&mut w, &mut scratch, 1000, 512);
        assert_eq!(w.states()[0], SourceState::Playing);
        assert_eq!(scratch[0], 200);
    }

    #[test]
    fn boundary_start_equals_clock_starts_at_zero_offset() {
        let mut w = world_with(&[1000]);
        let mut scratch = [0u32; 1];
        activate_scheduled(&mut w, &mut scratch, 1000, 512);
        assert_eq!(w.states()[0], SourceState::Playing);
        assert_eq!(scratch[0], 0);
    }

    #[test]
    fn boundary_start_equals_callback_end_stays_scheduled() {
        let mut w = world_with(&[1512]);
        let mut scratch = [0u32; 1];
        activate_scheduled(&mut w, &mut scratch, 1000, 512);
        // ts >= callback_end (= clock + frames) なので Scheduled のまま。
        assert_eq!(w.states()[0], SourceState::Scheduled);
    }

    #[test]
    fn already_playing_sources_are_untouched() {
        let mut w = SourceWorld::new();
        w.spawn(SourceComponent {
            vol: 1.0,
            pitch: 1.0,
            start_dsp_frame: 0, // 即時 → spawn 時点で Playing
            ..SourceComponent::default()
        })
        .unwrap();
        let mut scratch = [0u32; 1];
        activate_scheduled(&mut w, &mut scratch, 5000, 512);
        assert_eq!(w.states()[0], SourceState::Playing);
        assert_eq!(scratch[0], 0);
    }

    #[test]
    fn scratch_is_reset_each_call() {
        // 前 callback の残骸 (99) が確実に上書きされることを確認する。
        let mut w = world_with(&[10_000, 200]);
        let mut scratch = [99u32; 2];
        activate_scheduled(&mut w, &mut scratch, 0, 512);
        // 0番: 未来 → Scheduled。scratch は 0 にリセット (99 が残らない)。
        assert_eq!(scratch[0], 0);
        // 1番: clock=0, start=200 → callback 区間内、sub_offset=200。
        assert_eq!(scratch[1], 200);
        assert_eq!(w.states()[1], SourceState::Playing);
    }

    #[test]
    fn mixed_states_handled_per_source() {
        let mut w = world_with(&[10_000, 1100, 50, 0]);
        // index 3 は start=0 (sentinel) → spawn 時点で Playing。
        let mut scratch = [0u32; 4];
        activate_scheduled(&mut w, &mut scratch, 1000, 512);
        assert_eq!(w.states()[0], SourceState::Scheduled);
        assert_eq!(w.states()[1], SourceState::Playing);
        assert_eq!(scratch[1], 100);
        assert_eq!(w.states()[2], SourceState::Playing);
        assert_eq!(scratch[2], 0); // 過去
        assert_eq!(w.states()[3], SourceState::Playing); // 既に Playing
        assert_eq!(scratch[3], 0);
    }
}
