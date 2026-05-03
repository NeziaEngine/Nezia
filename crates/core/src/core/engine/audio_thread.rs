use std::sync::Arc;

use arc_swap::ArcSwap;
use ringbuf::traits::{Consumer, Producer};

use crate::audio::AudioBuffer;
use crate::bus::{BusComponent, BusSystem, BusWorld};
use crate::command::Command;
use crate::entity::EntityId;
use crate::event::Event;
use crate::source::{
    SourceComponent, SourceLifecycleSystem, SourceMixingSystem, SourceState, SourceWorld,
};
use crate::spatial::{ListenerState, SpatialWorld};

/// オーディオスレッド側の所有状態を一括で持つ構造体。
///
/// cpal のコールバック内に直接ロジックを書くと engine.rs が肥大化するため、
/// ここに状態とフレーム処理を切り出している。`process()` がコールバック 1 回分。
pub(super) struct AudioThread {
    command_consumer: ringbuf::HeapCons<Command>,
    event_producer: ringbuf::HeapProd<Event>,
    listener_output: triple_buffer::Output<ListenerState>,
    position_updates_output: triple_buffer::Output<Vec<(EntityId, [f32; 3])>>,
    bus_world: BusWorld,
    source_world: SourceWorld,
    spatial_world: SpatialWorld,
    shared_buffers: Arc<ArcSwap<Vec<Option<Arc<AudioBuffer>>>>>,
    master_bus_id: EntityId,
    device_sample_rate: f32,
    device_channels: usize,
}

impl AudioThread {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        command_consumer: ringbuf::HeapCons<Command>,
        event_producer: ringbuf::HeapProd<Event>,
        listener_output: triple_buffer::Output<ListenerState>,
        position_updates_output: triple_buffer::Output<Vec<(EntityId, [f32; 3])>>,
        bus_world: BusWorld,
        source_world: SourceWorld,
        spatial_world: SpatialWorld,
        shared_buffers: Arc<ArcSwap<Vec<Option<Arc<AudioBuffer>>>>>,
        master_bus_id: EntityId,
        device_sample_rate: f32,
        device_channels: usize,
    ) -> Self {
        Self {
            command_consumer,
            event_producer,
            listener_output,
            position_updates_output,
            bus_world,
            source_world,
            spatial_world,
            shared_buffers,
            master_bus_id,
            device_sample_rate,
            device_channels,
        }
    }

    /// cpal のコールバック 1 回分の処理。
    pub(super) fn process(&mut self, data: &mut [f32]) {
        let sample_count = data.len();

        // コマンドを先に処理する。spawn 系を反映してから triple buffer の
        // 位置更新を適用しないと、spawn と同フレームで publish された位置が
        // resolve 失敗で捨てられ、初回 callback がデフォルト位置 [0,0,0] で
        // 再生されてしまう。
        while let Some(cmd) = self.command_consumer.try_pop() {
            process_command(
                cmd,
                &mut self.bus_world,
                &mut self.source_world,
                &mut self.spatial_world,
                &mut self.event_producer,
                self.master_bus_id,
            );
        }

        // triple buffer から最新の listener / source positions を取り込む。
        // commands の後にやることで、spawn と同フレームで publish された
        // 位置も resolve に成功する（順序は spawn → 位置適用）。
        if self.listener_output.update() {
            // SP-06: フォーカス系フィールドは Command で管理しているため
            // pose（位置・向き）のみを反映する。直接代入すると triple buffer
            // 入力側に残っているデフォルト focus 値で上書きされてしまう。
            let pose = self.listener_output.output_buffer_mut();
            self.spatial_world
                .listener
                .update(pose.position, pose.forward, pose.up);
        }
        if self.position_updates_output.update() {
            let updates = self.position_updates_output.output_buffer_mut();
            for (id, pos) in updates.iter() {
                if let Some(dense) = self.source_world.resolve(*id) {
                    self.spatial_world.set_position(dense, *pos);
                }
            }
        }

        // mix_buffer をゼロクリア。
        self.bus_world.clear_mix_buffers(sample_count);

        // ロックフリーでバッファリストのスナップショットを取得。
        let buffers = self.shared_buffers.load();

        // Source ミキシング → BusWorld の mix_buffer に加算。
        {
            let mix_buf = self.bus_world.mix_buffer_mut();
            SourceMixingSystem::update(
                &mut self.source_world,
                &mut self.spatial_world,
                mix_buf,
                crate::bus::MAX_MIX_BUFFER_SIZE,
                sample_count,
                self.device_channels,
                self.device_sample_rate,
                &buffers,
            );
        }

        // 再生終了 Source の despawn。SourceFinished イベントを push する。
        let event_producer = &mut self.event_producer;
        SourceLifecycleSystem::update(
            &mut self.source_world,
            &mut self.spatial_world,
            &buffers,
            &mut |ev| {
                let _ = event_producer.try_push(ev);
            },
        );

        // バス処理 → output_buffer へ書き出し。
        BusSystem::update(
            &mut self.bus_world,
            data,
            self.device_channels,
            sample_count,
        );
    }
}

/// 1 個のコマンドをサウンドスレッド側のワールドに反映する。
fn process_command(
    cmd: Command,
    bus_world: &mut BusWorld,
    source_world: &mut SourceWorld,
    spatial_world: &mut SpatialWorld,
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
        } => {
            let spawned = source_world.spawn(SourceComponent {
                vol,
                pitch,
                sample_offset: 0.0,
                audio_buffer_index,
                output_bus: 0,
                token,
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
        } => {
            let spawned = source_world.spawn(SourceComponent {
                vol,
                pitch,
                sample_offset: 0.0,
                audio_buffer_index,
                output_bus: output_bus_dense,
                token,
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
                },
            );
            if spawned {
                spatial_world.push_defaults();
            } else if token != 0 {
                let _ = event_producer.try_push(Event::PlayFailed { token });
            }
        }
        Command::StopAll => {
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
        Command::SetSourceSpatialEnabled { id, enabled } => {
            if let Some(dense) = source_world.resolve(id) {
                spatial_world.set_enabled(dense, enabled);
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

        // ── ライブソース制御 ──
        Command::SetSourceVolume { id, vol } => {
            source_world.set_vol(id, vol);
        }
        Command::SetSourcePitch { id, pitch } => {
            source_world.set_pitch(id, pitch);
        }
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
    }
}
