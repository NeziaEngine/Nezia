use std::sync::Arc;

use arc_swap::ArcSwap;
use ringbuf::traits::{Consumer, Producer};

use crate::audio::AudioBuffer;
use crate::bus::{BusComponent, BusSystem, BusWorld};
use crate::command::Command;
use crate::effect::{
    EffectKind, EffectPosition, EffectTarget, EffectWorld, HpfParam, HpfWorld, LpfParam, LpfWorld,
    Owner, ReverbParam, ReverbWorld,
};
use crate::entity::{EntityId, SourcePositionUpdate, SourceVelocityUpdate};
use crate::event::Event;
use crate::source::{
    SourceComponent, SourceLifecycleSystem, SourceMixingSystem, SourceState, SourceWorld,
};
use crate::spatial::{ListenerState, SpatialWorld};

use super::SourceLiveParams;
use super::SourceSnapshot;

/// オーディオスレッド側の所有状態を一括で持つ構造体。
///
/// cpal のコールバック内に直接ロジックを書くと engine.rs が肥大化するため、
/// ここに状態とフレーム処理を切り出している。`process()` がコールバック 1 回分。
pub(super) struct AudioThread {
    command_consumer: ringbuf::HeapCons<Command>,
    event_producer: ringbuf::HeapProd<Event>,
    listener_output: triple_buffer::Output<ListenerState>,
    position_updates_output: triple_buffer::Output<Vec<SourcePositionUpdate>>,
    velocity_updates_output: triple_buffer::Output<Vec<SourceVelocityUpdate>>,
    source_snapshots_input: triple_buffer::Input<Vec<SourceSnapshot>>,
    bus_world: BusWorld,
    source_world: SourceWorld,
    spatial_world: SpatialWorld,
    effect_world: EffectWorld,
    lpf_world: LpfWorld,
    hpf_world: HpfWorld,
    reverb_world: ReverbWorld,
    /// Source Pre-Spatial chain 適用用の事前確保 mono スクラッチ。
    /// 容量は `MAX_MIX_BUFFER_SIZE / 1ch` でフレーム単位。サウンドスレッド alloc 0。
    mono_scratch: Vec<f32>,
    shared_buffers: Arc<ArcSwap<Vec<Option<Arc<AudioBuffer>>>>>,
    /// メインスレッドと共有する SoA ライブパラメータ。
    /// コールバック冒頭で全アクティブソースに対して atomic load → dense 配列へ反映する。
    live_params: Arc<SourceLiveParams>,
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
        position_updates_output: triple_buffer::Output<Vec<SourcePositionUpdate>>,
        velocity_updates_output: triple_buffer::Output<Vec<SourceVelocityUpdate>>,
        source_snapshots_input: triple_buffer::Input<Vec<SourceSnapshot>>,
        bus_world: BusWorld,
        source_world: SourceWorld,
        spatial_world: SpatialWorld,
        effect_world: EffectWorld,
        lpf_world: LpfWorld,
        hpf_world: HpfWorld,
        reverb_world: ReverbWorld,
        shared_buffers: Arc<ArcSwap<Vec<Option<Arc<AudioBuffer>>>>>,
        live_params: Arc<SourceLiveParams>,
        master_bus_id: EntityId,
        device_sample_rate: f32,
        device_channels: usize,
    ) -> Self {
        Self {
            command_consumer,
            event_producer,
            listener_output,
            position_updates_output,
            velocity_updates_output,
            source_snapshots_input,
            bus_world,
            source_world,
            spatial_world,
            effect_world,
            lpf_world,
            hpf_world,
            reverb_world,
            mono_scratch: vec![0.0; crate::bus::MAX_MIX_BUFFER_SIZE],
            shared_buffers,
            live_params,
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
                &mut self.effect_world,
                &mut self.lpf_world,
                &mut self.hpf_world,
                &mut self.reverb_world,
                &mut self.event_producer,
                self.master_bus_id,
            );
        }

        // DSP パラメータ変更で立った dirty フラグをフラッシュして係数を再計算する。
        self.lpf_world.flush_dirty(self.device_sample_rate);
        self.hpf_world.flush_dirty(self.device_sample_rate);
        self.reverb_world.flush_dirty();

        // triple buffer から最新の listener / source positions を取り込む。
        // commands の後にやることで、spawn と同フレームで publish された
        // 位置も resolve に成功する（順序は spawn → 位置適用）。
        if self.listener_output.update() {
            // SP-06: フォーカス系フィールドは Command で管理しているため
            // pose（位置・向き）のみを反映する。直接代入すると triple buffer
            // 入力側に残っているデフォルト focus 値で上書きされてしまう。
            // SP-10: velocity も入力側で publish される最新値をそのまま反映する。
            let pose = self.listener_output.output_buffer_mut();
            self.spatial_world
                .listener
                .update(pose.position, pose.forward, pose.up);
            self.spatial_world.listener.velocity = pose.velocity;
        }
        if self.position_updates_output.update() {
            let updates = self.position_updates_output.output_buffer_mut();
            for update in updates.iter() {
                if let Some(dense) = self.source_world.resolve(update.source) {
                    self.spatial_world.set_position(dense, update.position);
                }
            }
        }
        if self.velocity_updates_output.update() {
            let updates = self.velocity_updates_output.output_buffer_mut();
            for update in updates.iter() {
                if let Some(dense) = self.source_world.resolve(update.source) {
                    self.spatial_world.set_velocity(dense, update.velocity);
                }
            }
        }

        // ライブパラメータ（volume / pitch / spatial_enabled）を atomic から dense へ反映。
        // generation 一致しないスロット（古い設定 or 未初期化）は無視する。
        // dense 配列をシーケンシャルに走査するため L1 キャッシュ親和的。
        apply_live_params(
            &self.live_params,
            &mut self.source_world,
            &mut self.spatial_world,
        );

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
                &self.effect_world,
                &mut self.lpf_world,
                &mut self.hpf_world,
                &mut self.reverb_world,
                &mut self.mono_scratch,
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
            &self.effect_world,
            &mut self.lpf_world,
            &mut self.hpf_world,
            &mut self.reverb_world,
            data,
            self.device_channels,
            sample_count,
        );

        // streaming バッファの underrun フラグをドレインしてイベント発火。
        // BufferId は `StreamingState::buffer_id_packed` (load_streaming 時に書き込み済み)
        // から読み出すことで slot 再利用後も正しい generation 付きで通知できる。
        // 連続発火を抑える簡易抑制 (per-buffer last-emitted カウンタ) は Phase 2-4 では未実装。
        for slot in buffers.iter() {
            let Some(buf) = slot.as_ref() else { continue };
            let Some(state) = buf.streaming_state() else {
                continue;
            };
            if state
                .underrun_flag
                .swap(false, std::sync::atomic::Ordering::AcqRel)
            {
                if let Some(id) = state.buffer_id() {
                    let _ = self
                        .event_producer
                        .try_push(Event::StreamingUnderrun { buffer: id });
                }
            }
        }

        // 生存ソースのスナップショットを publish する（メインスレッドのクエリ用）。
        publish_source_snapshots(&mut self.source_snapshots_input, &self.source_world);
    }
}

/// 共有 atomic スロットから dense 配列へライブパラメータを反映する。
///
/// 全アクティブソースを 1 ループで走査し、各スロットを atomic load する。
/// generation が EntityId と一致しないスロットはスキップ（新スロット reuse 直後の
/// 古い値や、メイン側がまだ priming していないスロットを誤適用しないため）。
fn apply_live_params(
    live: &SourceLiveParams,
    source_world: &mut SourceWorld,
    spatial_world: &mut SpatialWorld,
) {
    let count = source_world.len();
    for dense in 0..count {
        let Some(id) = source_world.entity_at_dense(dense) else {
            continue;
        };
        if let Some(v) = live.load_volume(id) {
            source_world.write_vol(dense, v);
        }
        if let Some(p) = live.load_pitch(id) {
            source_world.write_pitch(dense, p);
        }
        if let Some(e) = live.load_spatial_enabled(id) {
            spatial_world.set_enabled(dense, e);
        }
    }
}

/// 生存ソースのスナップショットを triple buffer に publish する。
///
/// `clear + push` で再確保が起きないよう、入力バッファ容量は `MAX_SOURCES` で
/// 事前確保されている前提（`build_source_snapshots_buffer()` 参照）。
fn publish_source_snapshots(
    input: &mut triple_buffer::Input<Vec<SourceSnapshot>>,
    source_world: &SourceWorld,
) {
    let buf = input.input_buffer_mut();
    buf.clear();
    for (id, sample_offset) in source_world.snapshots() {
        buf.push(SourceSnapshot {
            index: id.index,
            generation: id.generation,
            sample_offset,
        });
    }
    input.publish();
}

/// 1 個のコマンドをサウンドスレッド側のワールドに反映する。
#[allow(clippy::too_many_arguments)]
fn process_command(
    cmd: Command,
    bus_world: &mut BusWorld,
    source_world: &mut SourceWorld,
    spatial_world: &mut SpatialWorld,
    effect_world: &mut EffectWorld,
    lpf_world: &mut LpfWorld,
    hpf_world: &mut HpfWorld,
    reverb_world: &mut ReverbWorld,
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
            );
        }
    }
}

/// エフェクトを生成する。
/// メインスレッドが事前発行した `EffectId` を使い、種別 World に state を確保 →
/// メタ層に登録 → owner (Bus / Source) のチェーンに slot を追加。
#[allow(clippy::too_many_arguments)]
fn spawn_effect(
    id: crate::effect::EffectId,
    target: EffectTarget,
    kind: EffectKind,
    algo: u8,
    position: EffectPosition,
    bus_world: &mut BusWorld,
    source_world: &mut SourceWorld,
    effect_world: &mut EffectWorld,
    lpf_world: &mut LpfWorld,
    hpf_world: &mut HpfWorld,
    reverb_world: &mut ReverbWorld,
) {
    // 1. owner dense を解決。
    let owner = match target {
        EffectTarget::Bus(bus_id) => match bus_world.resolve(bus_id) {
            Some(d) => Owner::Bus(d as u32),
            None => return,
        },
        EffectTarget::Source(src_id) => {
            // Source 対象 + Reverb は Phase 2-3 では非対応 (Phase 3-3 Send 経由)。
            if matches!(kind, EffectKind::Reverb) {
                return;
            }
            // Phase 2-3 では Source の Post-Spatial も未実装。
            if matches!(position, EffectPosition::Post) {
                return;
            }
            match source_world.resolve(src_id) {
                Some(d) => Owner::Source(d as u32),
                None => return,
            }
        }
    };

    // 2. 種別 World に state 確保。
    let state_index = match kind {
        EffectKind::Lpf => match lpf_world.spawn(id, 1000.0, 0.707) {
            Some(d) => d,
            None => return,
        },
        EffectKind::Hpf => match hpf_world.spawn(id, 200.0, 0.707) {
            Some(d) => d,
            None => return,
        },
        EffectKind::Reverb => match reverb_world.spawn(id) {
            Some(d) => d,
            None => return,
        },
    };

    // 3. owner のチェーンに slot を追加。失敗したら state も巻き戻す。
    let slot_ok = match owner {
        Owner::Bus(d) => bus_world.push_effect(d as usize, position, id).is_some(),
        Owner::Source(d) => source_world.push_pre_effect(d as usize, id).is_some(),
    };
    if !slot_ok {
        // チェーン満杯。state を巻き戻す。
        match kind {
            EffectKind::Lpf => {
                let _ = lpf_world.despawn(state_index);
            }
            EffectKind::Hpf => {
                let _ = hpf_world.despawn(state_index);
            }
            EffectKind::Reverb => {
                let _ = reverb_world.despawn(state_index);
            }
        }
        return;
    }
    let slot = match owner {
        Owner::Bus(d) => match position {
            EffectPosition::Pre => bus_world.pre_chain_slice(d as usize).len() as u8 - 1,
            EffectPosition::Post => bus_world.post_chain_slice(d as usize).len() as u8 - 1,
        },
        Owner::Source(d) => source_world.pre_chain_slice(d as usize).len() as u8 - 1,
    };

    // 4. メタ層に登録。
    effect_world.spawn_with_id(id, kind, algo, owner, position, slot, state_index);
}

#[allow(clippy::too_many_arguments)]
fn despawn_effect(
    id: crate::effect::EffectId,
    bus_world: &mut BusWorld,
    source_world: &mut SourceWorld,
    effect_world: &mut EffectWorld,
    lpf_world: &mut LpfWorld,
    hpf_world: &mut HpfWorld,
    reverb_world: &mut ReverbWorld,
) {
    let Some(meta_dense) = effect_world.resolve(id) else {
        return;
    };
    let owner = effect_world.owners()[meta_dense];
    let position = effect_world.positions()[meta_dense];

    // 1. owner のチェーンから slot を除去。
    match owner {
        Owner::Bus(d) => {
            let _ = bus_world.remove_effect(d as usize, position, id);
        }
        Owner::Source(d) => {
            let _ = source_world.remove_pre_effect(d as usize, id);
        }
    }

    // 2. メタ層から削除し state_index を取得。
    let Some((kind, state_index)) = effect_world.despawn(id) else {
        return;
    };

    // 3. 種別 World で state を swap-remove し、移動した state があればメタ層を再マップ。
    let moved = match kind {
        EffectKind::Lpf => lpf_world.despawn(state_index),
        EffectKind::Hpf => hpf_world.despawn(state_index),
        EffectKind::Reverb => reverb_world.despawn(state_index),
    };
    if let Some((moved_id, new_state_index)) = moved {
        let _ = moved_id;
        // 末尾要素 (元の last_dense) が state_index 位置に移動した。
        // メタ層側で「kind 種別 + state_index == 旧末尾」を新位置に書き換える。
        // 旧末尾 index は "新サイズ" (despawn 後の len)。
        let last_after = match kind {
            EffectKind::Lpf => lpf_world.len() as u32,
            EffectKind::Hpf => hpf_world.len() as u32,
            EffectKind::Reverb => reverb_world.len() as u32,
        };
        effect_world.remap_state_index(kind, last_after, new_state_index);
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_effect_param(
    id: crate::effect::EffectId,
    param: u8,
    value: f32,
    effect_world: &EffectWorld,
    lpf_world: &mut LpfWorld,
    hpf_world: &mut HpfWorld,
    reverb_world: &mut ReverbWorld,
) {
    let Some(meta_dense) = effect_world.resolve(id) else {
        return;
    };
    let kind = effect_world.kinds()[meta_dense];
    let state_index = effect_world.state_indices()[meta_dense];
    match kind {
        EffectKind::Lpf => {
            if param == LpfParam::Cutoff as u8 {
                lpf_world.set_cutoff(state_index, value);
            } else if param == LpfParam::Q as u8 {
                lpf_world.set_q(state_index, value);
            }
        }
        EffectKind::Hpf => {
            if param == HpfParam::Cutoff as u8 {
                hpf_world.set_cutoff(state_index, value);
            } else if param == HpfParam::Q as u8 {
                hpf_world.set_q(state_index, value);
            }
        }
        EffectKind::Reverb => {
            if param == ReverbParam::RoomSize as u8 {
                reverb_world.set_room_size(state_index, value);
            } else if param == ReverbParam::Damping as u8 {
                reverb_world.set_damping(state_index, value);
            } else if param == ReverbParam::Wet as u8 {
                reverb_world.set_wet(state_index, value);
            } else if param == ReverbParam::Dry as u8 {
                reverb_world.set_dry(state_index, value);
            } else if param == ReverbParam::Width as u8 {
                reverb_world.set_width(state_index, value);
            }
        }
    }
}
