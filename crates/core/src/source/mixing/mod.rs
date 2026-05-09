//! Source ミキシングシステム。
//!
//! `SourceWorld` のコンポーネントを読み出し、バスの mix_buffer に加算ミキシングする。
//! 再生が完了した Source の despawn は `SourceLifecycleSystem` 側に委譲する。
//!
//! このモジュールは frame 内のパイプラインステージで分割されている:
//! - [`scheduled`] — Phase 0.5: 予約再生 (PlayScheduled) の発音判定
//! - [`send_route`] — Source 起点 Send (User-Defined Aux Send) の宛先解決と書き出し
//! - [`mix_static`] — 静的バッファのミキシング (looping wrap あり)
//! - [`mix_streaming`] — ストリーミングバッファのミキシング (worker が wrap 吸収済み)
//!
//! # Pre-Spatial エフェクトチェーン (Phase 2-3 PR 2)
//!
//! `SourceWorld::pre_chain` に登録されたエフェクトを **resampler 後・Spatial 適用前** の
//! モノラル信号に対して適用する。チェーン空のソースは関数呼出を完全にスキップする
//! (最速経路)。スクラッチバッファは呼出側 (`AudioThread`) が事前確保したものを借用する。
//!
//! # ストリーミングバッファ (Phase 2-4)
//!
//! `AudioBuffer::Streaming` を参照するソースは、ワーカが供給する `MirrorRing` から
//! **contiguous slice** を peek してミキシングする。looping wrap や frame_count 判定は
//! ワーカ責務 (`docs/design/core/streaming.md`) なので mixing 側 inner loop には現れない。
//! 静的・streaming で per-source 1 度の cold dispatch + dense 配列 inner loop 構造は同形。

mod mix_static;
mod mix_streaming;
mod scheduled;
mod send_route;

use std::sync::Arc;

use crate::audio::AudioBuffer;
use crate::bus::{MAX_MIX_BUFFER_SIZE, SendDestKind};
use crate::effect::{EffectWorld, EffectWorlds};
use crate::spatial::{AttenuationCurve, SpatialSystem, SpatialWorld};

use super::MAX_SENDS_PER_SOURCE;
use super::virtualizer::VoiceVirtualizer;
use super::world::{SourceState, SourceWorld};

use mix_static::mix_static;
use mix_streaming::mix_streaming;
use scheduled::activate_scheduled;
use send_route::SendOutput;

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

        // Source 起点 Send 用の Compressor sidechain_buffer raw ptr を 1 度だけ取得。
        // mix_static / mix_streaming へ &mut EffectWorlds を渡すために raw ptr を別経路で
        // 持ち回る (借用衝突を回避)。bus_mix_buffer も同様。
        // SAFETY: 本ループ中 sidechain_buffer / bus_mix_buffer のサイズは変わらない
        //   (audio thread 上 spawn/despawn は前段の process_command で完了済み)。
        //   send 宛先と本線 bus_buf が同一バスを指す場合は加算ミックス (`+=`) のみで
        //   正常動作する (順序は決定的だが意味は変わらない)。
        let (sidechain_ptr, sidechain_len) = {
            let buf = effect_worlds.compressor.sidechain_buffer_mut();
            (buf.as_mut_ptr(), buf.len())
        };
        let bus_mix_ptr = bus_mix_buffer.as_mut_ptr();
        let bus_mix_len = bus_mix_buffer.len();

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

            // Source 起点 Send (User-Defined Aux Send) を解決して raw ptr 化する。
            // 宛先は Bus mix_buffer の `dest_dense` 区間か Compressor sidechain_buffer の
            // 同区間。sub_byte_offset を本線と揃えて先頭を一致させる (PlayScheduled で
            // 途中発音した場合に send 側もその offset から書き始める)。
            let send_count = world.send_count[source_i] as usize;
            let mut send_outputs: [SendOutput; MAX_SENDS_PER_SOURCE] =
                [SendOutput::NULL; MAX_SENDS_PER_SOURCE];
            let mut active_sends = 0usize;
            for slot in 0..send_count {
                let (dest_dense, gain, _pos_u8, kind_u8) = world.send_at(source_i, slot);
                let kind = match SendDestKind::from_u8(kind_u8) {
                    Some(k) => k,
                    None => continue,
                };
                let (base_ptr, base_len, area_start) = match kind {
                    SendDestKind::Bus => {
                        let start = dest_dense as usize * MAX_MIX_BUFFER_SIZE;
                        (bus_mix_ptr, bus_mix_len, start)
                    }
                    SendDestKind::CompressorSidechain => {
                        let start = dest_dense as usize * MAX_MIX_BUFFER_SIZE;
                        (sidechain_ptr, sidechain_len, start)
                    }
                };
                let area_end = area_start + MAX_MIX_BUFFER_SIZE;
                if area_end > base_len {
                    continue;
                }
                let write_start = area_start + sub_byte_offset;
                let write_end = area_start + process_len;
                if write_start >= write_end || write_end > base_len {
                    continue;
                }
                // SAFETY: write_start..write_end は base buffer 内の有効範囲で、本線
                //   bus_buf と同一区間を指す可能性はあるが、`apply_send_outputs` は
                //   `+=` のみで書き込むためアトミック性以外の挙動上の問題はない
                //   (audio thread は単一スレッドなのでアトミック性も不要)。
                let dest_ptr = unsafe { base_ptr.add(write_start) };
                let dest_len = write_end - write_start;
                send_outputs[active_sends] = SendOutput {
                    dest_ptr,
                    dest_len,
                    gain,
                };
                active_sends += 1;
            }
            let send_outputs_slice: &[SendOutput] = &send_outputs[..active_sends];

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
                    send_outputs_slice,
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
                    send_outputs_slice,
                );
                // streaming の sample_offset は累積消費フレーム数 (file-frame ではない)。
                world.sample_offset[source_i] += consumed as f32;
            }
        }
    }
}
