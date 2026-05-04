//! コマンドリングバッファ経由で受け取った `Command` をサウンドスレッド側のワールドに反映する。
//!
//! `Command::ApplySnapshot` だけは `active_snapshot` / `shared_snapshots` への
//! アクセスが必要なため呼び出し側 (`AudioThread::process`) でインターセプトしており、
//! ここでは到達不能扱い (debug_assert) にしている。

use ringbuf::traits::Producer;

use crate::bus::{BusComponent, BusWorld, SendDestKind};
use crate::command::{Command, SendDestination};
use crate::effect::{CompressorWorld, EffectKind, EffectWorld, HpfWorld, LpfWorld, ReverbWorld};
use crate::entity::EntityId;
use crate::event::Event;
use crate::source::{SourceComponent, SourceState, SourceWorld};
use crate::spatial::SpatialWorld;

use super::effect::{apply_effect_param, despawn_effect, spawn_effect};

/// 1 個のコマンドをサウンドスレッド側のワールドに反映する。
#[allow(clippy::too_many_arguments)]
pub(super) fn process_command(
    cmd: Command,
    bus_world: &mut BusWorld,
    source_world: &mut SourceWorld,
    spatial_world: &mut SpatialWorld,
    effect_world: &mut EffectWorld,
    lpf_world: &mut LpfWorld,
    hpf_world: &mut HpfWorld,
    reverb_world: &mut ReverbWorld,
    compressor_world: &mut CompressorWorld,
    event_producer: &mut ringbuf::HeapProd<Event>,
    master_bus_id: EntityId,
) {
    match cmd {
        Command::SetVolume(v) => {
            bus_world.set_gain(master_bus_id, v.clamp(0.0, 1.0));
        }
        Command::Play {
            audio_buffer_index,
            vol,
            pitch,
            token,
            looping,
        } => {
            let spawned = source_world.spawn(SourceComponent {
                vol,
                pitch,
                sample_offset: 0.0,
                audio_buffer_index,
                output_bus: 0,
                token,
                looping,
                priority: 128,
            });
            if spawned.is_some() {
                spatial_world.push_defaults();
            } else if token != 0 {
                let _ = event_producer.try_push(Event::PlayFailed { token });
            }
        }
        Command::PlayToBus {
            audio_buffer_index,
            vol,
            pitch,
            output_bus_dense,
            token,
            looping,
        } => {
            let spawned = source_world.spawn(SourceComponent {
                vol,
                pitch,
                sample_offset: 0.0,
                audio_buffer_index,
                output_bus: output_bus_dense,
                token,
                looping,
                priority: 128,
            });
            if spawned.is_some() {
                spatial_world.push_defaults();
            } else if token != 0 {
                let _ = event_producer.try_push(Event::PlayFailed { token });
            }
        }
        Command::SpawnSource {
            id,
            audio_buffer_index,
            vol,
            pitch,
            output_bus_dense,
            token,
            looping,
        } => {
            let spawned = source_world.spawn_with_id(
                id,
                SourceComponent {
                    vol,
                    pitch,
                    sample_offset: 0.0,
                    audio_buffer_index,
                    output_bus: output_bus_dense,
                    token,
                    looping,
                    priority: 128,
                },
            );
            if spawned {
                spatial_world.push_defaults();
            } else if token != 0 {
                let _ = event_producer.try_push(Event::PlayFailed { token });
            }
        }
        Command::StopAll => {
            // 既存ソースぶんの despawn 通知をメインスレッドへ送る（slot 解放のため）。
            for dense in 0..source_world.len() {
                if let Some(id) = source_world.entity_at_dense(dense) {
                    let _ = event_producer.try_push(Event::SourceDespawned { id });
                }
            }
            *source_world = SourceWorld::new();
            *spatial_world = SpatialWorld::new();
        }
        Command::SpawnBus {
            id,
            gain,
            output_bus_dense,
        } => {
            bus_world.spawn_with_id(
                id,
                BusComponent {
                    gain,
                    output_bus_dense,
                },
            );
        }
        Command::DespawnBus { id } => {
            bus_world.despawn(id);
        }
        Command::SetBusGain { id, gain } => {
            bus_world.set_gain(id, gain);
        }
        Command::SetBusMuted { id, muted } => {
            bus_world.set_muted(id, muted);
        }
        Command::SetBusOutput {
            id,
            output_bus_dense,
        } => {
            bus_world.set_output_bus_dense(id, output_bus_dense);
        }
        Command::UpdateProcessOrder { order, len } => {
            bus_world.set_process_order(&order[..len as usize]);
        }
        // ── 3D 空間コマンド ──
        Command::SetSourceSpatialParams {
            id,
            model,
            min_distance,
            max_distance,
            rolloff,
        } => {
            if let Some(dense) = source_world.resolve(id) {
                spatial_world.set_params(dense, model, min_distance, max_distance, rolloff);
            }
        }
        Command::SetListenerFocus {
            focus_point,
            distance_focus_level,
            direction_focus_level,
        } => {
            spatial_world.listener.set_focus(
                focus_point,
                distance_focus_level,
                direction_focus_level,
            );
        }
        Command::SetSourceDopplerLevel { id, level } => {
            if let Some(dense) = source_world.resolve(id) {
                spatial_world.set_doppler_level(dense, level);
            }
        }
        Command::SetSoundSpeed { speed } => {
            spatial_world.set_sound_speed(speed);
        }
        Command::SetSourceAttenuationCurve { id, curve_index } => {
            if let Some(dense) = source_world.resolve(id) {
                spatial_world.set_curve_index(dense, curve_index);
            }
        }
        Command::ApplySnapshot { .. } => {
            // process() 内で intercept されるためここには来ない (網羅性のため arm を置く)。
            debug_assert!(
                false,
                "ApplySnapshot should be handled before process_command"
            );
        }

        // ── ライブソース制御 ──
        // SetSourceVolume / SetSourcePitch / SetSourceSpatialEnabled は live_params 経由で
        // 反映するため、コマンド経路は廃止された。
        Command::SeekSource { id, frame_offset } => {
            source_world.set_sample_offset(id, frame_offset);
        }
        Command::PauseSource { id } => {
            source_world.set_state(id, SourceState::Pausing);
        }
        Command::ResumeSource { id } => {
            source_world.set_state(id, SourceState::Playing);
        }
        Command::StopSource { id } => {
            source_world.set_state(id, SourceState::Stopped);
        }
        Command::SetSourceLoop { id, looping } => {
            source_world.set_looping(id, looping);
        }
        Command::SetSourcePriority { id, priority } => {
            source_world.set_priority(id, priority);
        }
        // ── DSP エフェクト ──
        Command::SpawnEffect {
            id,
            target,
            kind,
            algo,
            position,
        } => {
            spawn_effect(
                id,
                target,
                kind,
                algo,
                position,
                bus_world,
                source_world,
                effect_world,
                lpf_world,
                hpf_world,
                reverb_world,
                compressor_world,
            );
        }
        Command::DespawnEffect { id } => {
            despawn_effect(
                id,
                bus_world,
                source_world,
                effect_world,
                lpf_world,
                hpf_world,
                reverb_world,
                compressor_world,
            );
        }
        Command::SetEffectEnabled { id, enabled } => {
            effect_world.set_enabled(id, enabled);
        }
        Command::SetEffectParam { id, param, value } => {
            apply_effect_param(
                id,
                param,
                value,
                effect_world,
                lpf_world,
                hpf_world,
                reverb_world,
                compressor_world,
            );
        }

        // ── Send (Phase 3-3) ──
        Command::AddSend {
            id,
            src_dense,
            dst,
            position,
            gain,
        } => {
            // dst を resolve: Bus はそのまま、CompressorSidechain は effect_id → state_dense。
            let (dst_dense, dest_kind) = match dst {
                SendDestination::Bus { dense } => (dense, SendDestKind::Bus),
                SendDestination::CompressorSidechain { effect } => {
                    let Some(meta_dense) = effect_world.resolve(effect) else {
                        return;
                    };
                    if effect_world.kinds()[meta_dense] != EffectKind::Compressor {
                        return;
                    }
                    let state_dense = effect_world.state_indices()[meta_dense];
                    // Sidechain mode を自動で有効化 (`bind_compressor_sidechain` の暗黙呼出)。
                    compressor_world.set_use_sidechain(state_dense, true);
                    (state_dense, SendDestKind::CompressorSidechain)
                }
            };
            bus_world.add_send(src_dense as usize, id, dst_dense, dest_kind, gain, position);
        }
        Command::RemoveSend { id } => {
            bus_world.remove_send(id);
        }
        Command::SetSendGain { id, gain } => {
            bus_world.set_send_gain(id, gain);
        }
        Command::SetSendPosition { id, position } => {
            bus_world.set_send_position(id, position);
        }
        Command::SetCompressorSidechainEnabled { id, enabled } => {
            if let Some(meta_dense) = effect_world.resolve(id)
                && effect_world.kinds()[meta_dense] == EffectKind::Compressor
            {
                let state_dense = effect_world.state_indices()[meta_dense];
                compressor_world.set_use_sidechain(state_dense, enabled);
            }
        }
    }
}
