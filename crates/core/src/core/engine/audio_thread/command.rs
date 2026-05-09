//! コマンドリングバッファ経由で受け取った `Command` をサウンドスレッド側のワールドに反映する。
//!
//! `Command::ApplySnapshot` だけは `active_snapshot` / `shared_snapshots` への
//! アクセスが必要なため呼び出し側 (`AudioThread::process`) でインターセプトしており、
//! ここでは到達不能扱い (debug_assert) にしている。

use std::sync::atomic::Ordering;

use ringbuf::traits::Producer;

use crate::bus::{BusComponent, BusWorld, SendDestKind};
use crate::command::{Command, SendDestination};
use crate::effect::{CompressorWorld, EffectKind, EffectWorld, EffectWorlds};
use crate::entity::EntityId;
use crate::event::Event;
use crate::metrics::EngineMetrics;
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
    effect_worlds: &mut EffectWorlds,
    event_producer: &mut ringbuf::HeapProd<Event>,
    master_bus_id: EntityId,
    metrics: &EngineMetrics,
) {
    // 大半のセッタは bool (= 解決成功か) を返すが、コマンド経路では戻り値を使わない。
    // 各 arm を `{ ...; }` で `()` に揃える。
    match cmd {
        // ── マスター / 全体 ──────────────────────────────────────────────
        Command::SetVolume(v) => {
            bus_world.set_gain(master_bus_id, v.clamp(0.0, 1.0));
        }
        Command::StopAll => stop_all(source_world, spatial_world, event_producer),
        Command::UpdateProcessOrder { order, len } => {
            bus_world.set_process_order(&order[..len as usize]);
        }
        Command::ApplySnapshot { .. } => {
            // process() 内で intercept されるためここには来ない (網羅性のため arm を置く)。
            debug_assert!(
                false,
                "ApplySnapshot should be handled before process_command"
            );
        }

        // ── ソース再生 (Play 系) ──────────────────────────────────────────
        Command::Play {
            audio_buffer_index,
            vol,
            pitch,
            token,
            looping,
        } => {
            try_spawn_source(
                source_world,
                spatial_world,
                event_producer,
                metrics,
                None,
                build_source_component(audio_buffer_index, vol, pitch, 0, token, looping),
            );
        }
        Command::PlayToBus {
            audio_buffer_index,
            vol,
            pitch,
            output_bus_dense,
            token,
            looping,
        } => {
            try_spawn_source(
                source_world,
                spatial_world,
                event_producer,
                metrics,
                None,
                build_source_component(
                    audio_buffer_index,
                    vol,
                    pitch,
                    output_bus_dense,
                    token,
                    looping,
                ),
            );
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
            try_spawn_source(
                source_world,
                spatial_world,
                event_producer,
                metrics,
                Some(id),
                build_source_component(
                    audio_buffer_index,
                    vol,
                    pitch,
                    output_bus_dense,
                    token,
                    looping,
                ),
            );
        }

        // ── ライブソース制御 ──────────────────────────────────────────────
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

        // ── バス ──────────────────────────────────────────────────────────
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

        // ── 3D 空間 ────────────────────────────────────────────────────
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

        // ── DSP エフェクト ──────────────────────────────────────────────
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
                effect_worlds,
            );
        }
        Command::DespawnEffect { id } => {
            despawn_effect(id, bus_world, source_world, effect_world, effect_worlds);
        }
        Command::SetEffectEnabled { id, enabled } => {
            effect_world.set_enabled(id, enabled);
        }
        Command::SetEffectParam { id, param, value } => {
            apply_effect_param(id, param, value, effect_world, effect_worlds);
        }

        // ── Send (Phase 3-3) ────────────────────────────────────────────
        Command::AddSend {
            id,
            src_dense,
            dst,
            position,
            gain,
        } => handle_add_send(
            id,
            src_dense,
            dst,
            position,
            gain,
            bus_world,
            effect_world,
            &mut effect_worlds.compressor,
        ),
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
            handle_set_compressor_sidechain(
                id,
                enabled,
                effect_world,
                &mut effect_worlds.compressor,
            );
        }
    }
}

/// Play / PlayToBus / SpawnSource の SoA フィールド組み立てを共通化する。
/// `priority` のデフォルト 128、`sample_offset` 0 はここで集約する。
#[inline]
fn build_source_component(
    audio_buffer_index: u32,
    vol: f32,
    pitch: f32,
    output_bus: u32,
    token: u32,
    looping: bool,
) -> SourceComponent {
    SourceComponent {
        vol,
        pitch,
        sample_offset: 0.0,
        audio_buffer_index,
        output_bus,
        token,
        looping,
        priority: 128,
    }
}

/// Play / PlayToBus / SpawnSource の共通本体。
/// `id = Some(_)` で `spawn_with_id`、`None` で `spawn` (auto allocate)。
/// 失敗時は `dropped_play_calls` をインクリメントし、token 付きなら `PlayFailed` を発火する。
fn try_spawn_source(
    source_world: &mut SourceWorld,
    spatial_world: &mut SpatialWorld,
    event_producer: &mut ringbuf::HeapProd<Event>,
    metrics: &EngineMetrics,
    id: Option<EntityId>,
    component: SourceComponent,
) {
    let token = component.token;
    let spawned = match id {
        Some(id) => source_world.spawn_with_id(id, component),
        None => source_world.spawn(component).is_some(),
    };
    if spawned {
        spatial_world.push_defaults();
    } else {
        metrics.dropped_play_calls.fetch_add(1, Ordering::Relaxed);
        if token != 0 {
            let _ = event_producer.try_push(Event::PlayFailed { token });
        }
    }
}

/// `Command::StopAll` 本体。既存ソースぶんの despawn 通知を発行してから world を作り直す。
fn stop_all(
    source_world: &mut SourceWorld,
    spatial_world: &mut SpatialWorld,
    event_producer: &mut ringbuf::HeapProd<Event>,
) {
    for dense in 0..source_world.len() {
        if let Some(id) = source_world.entity_at_dense(dense) {
            let _ = event_producer.try_push(Event::SourceDespawned { id });
        }
    }
    *source_world = SourceWorld::new();
    *spatial_world = SpatialWorld::new();
}

/// `Command::AddSend` 本体。dst が CompressorSidechain の場合は effect_id → state_dense
/// 解決と sidechain mode 自動有効化を行う。解決失敗 / 種別不一致は no-op。
#[allow(clippy::too_many_arguments)]
fn handle_add_send(
    id: crate::bus::SendId,
    src_dense: u32,
    dst: SendDestination,
    position: crate::bus::SendPosition,
    gain: f32,
    bus_world: &mut BusWorld,
    effect_world: &EffectWorld,
    compressor_world: &mut CompressorWorld,
) {
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

/// `Command::SetCompressorSidechainEnabled` 本体。Compressor 以外の effect_id は no-op。
fn handle_set_compressor_sidechain(
    id: crate::effect::EffectId,
    enabled: bool,
    effect_world: &EffectWorld,
    compressor_world: &mut CompressorWorld,
) {
    if let Some(meta_dense) = effect_world.resolve(id)
        && effect_world.kinds()[meta_dense] == EffectKind::Compressor
    {
        let state_dense = effect_world.state_indices()[meta_dense];
        compressor_world.set_use_sidechain(state_dense, enabled);
    }
}
