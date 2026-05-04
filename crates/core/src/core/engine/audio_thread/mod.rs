//! オーディオスレッド側の所有状態とフレーム処理。
//!
//! cpal のコールバック内に直接ロジックを書くと engine.rs が肥大化するため、
//! `AudioThread` 構造体に状態を集約し `process()` を 1 コールバック分の処理として切り出している。
//! 内部はさらに以下のサブモジュールに責務分離している:
//! - [`command`] — 受信コマンドのワールド反映
//! - [`effect`] — DSP エフェクトの spawn / despawn / param 適用
//! - [`snapshot`] — Mixer Snapshot の補間 (Phase 3-2)

mod command;
mod effect;
mod snapshot;

use std::sync::Arc;

use arc_swap::ArcSwap;
use ringbuf::traits::{Consumer, Producer};

use crate::audio::AudioBuffer;
use crate::bus::{BusSystem, BusWorld};
use crate::command::Command;
use crate::effect::{EffectWorld, HpfWorld, LpfWorld, ReverbWorld};
use crate::entity::{EntityId, SourcePositionUpdate, SourceVelocityUpdate};
use crate::event::Event;
use crate::source::{SourceLifecycleSystem, SourceMixingSystem, SourceWorld};
use crate::spatial::{AttenuationCurve, ListenerState, SpatialWorld};

use super::SourceLiveParams;
use super::SourceSnapshot;

use command::process_command;
use snapshot::{apply_snapshot, tick_snapshot_interpolation};

/// オーディオスレッド側の所有状態を一括で持つ構造体。
pub(in crate::core::engine) struct AudioThread {
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
    /// Phase 3-1: Custom Attenuation Curve のレジストリ snapshot。
    /// `compute_gains` が `AttenuationModel::Custom` 指定ソースで参照する。
    shared_curves: Arc<ArcSwap<Vec<Option<Arc<AttenuationCurve>>>>>,
    /// Phase 3-2: Mixer Snapshot のレジストリ snapshot。
    /// `Command::ApplySnapshot` 受信時に 1 度 load して `ActiveSnapshot` に展開する。
    shared_snapshots: Arc<ArcSwap<Vec<Option<Arc<crate::snapshot::Snapshot>>>>>,
    /// Phase 3-2: 進行中の Snapshot 補間状態。`fade_remaining_samples > 0` の間、
    /// 毎コールバックで lerp + BusWorld / 各 *World への書き戻しを行う。
    active_snapshot: crate::snapshot::ActiveSnapshot,
    /// メインスレッドと共有する SoA ライブパラメータ。
    /// コールバック冒頭で全アクティブソースに対して atomic load → dense 配列へ反映する。
    live_params: Arc<SourceLiveParams>,
    master_bus_id: EntityId,
    device_sample_rate: f32,
    device_channels: usize,
}

impl AudioThread {
    #[allow(clippy::too_many_arguments)]
    pub(in crate::core::engine) fn new(
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
        shared_curves: Arc<ArcSwap<Vec<Option<Arc<AttenuationCurve>>>>>,
        shared_snapshots: Arc<ArcSwap<Vec<Option<Arc<crate::snapshot::Snapshot>>>>>,
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
            shared_curves,
            shared_snapshots,
            active_snapshot: crate::snapshot::ActiveSnapshot::new(),
            live_params,
            master_bus_id,
            device_sample_rate,
            device_channels,
        }
    }

    /// cpal のコールバック 1 回分の処理。
    pub(in crate::core::engine) fn process(&mut self, data: &mut [f32]) {
        let sample_count = data.len();

        // コマンドを先に処理する。spawn 系を反映してから triple buffer の
        // 位置更新を適用しないと、spawn と同フレームで publish された位置が
        // resolve 失敗で捨てられ、初回 callback がデフォルト位置 [0,0,0] で
        // 再生されてしまう。
        while let Some(cmd) = self.command_consumer.try_pop() {
            // Phase 3-2: ApplySnapshot は active_snapshot / shared_snapshots に
            // アクセスする必要があるため、共通の process_command ではなくここで処理する。
            if let Command::ApplySnapshot {
                snapshot_index,
                fade_samples,
            } = cmd
            {
                apply_snapshot(
                    snapshot_index,
                    fade_samples,
                    &self.shared_snapshots,
                    &mut self.active_snapshot,
                    &self.bus_world,
                    &self.effect_world,
                    &self.lpf_world,
                    &self.hpf_world,
                    &self.reverb_world,
                );
                continue;
            }
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

        // Phase 3-2: 進行中の Snapshot 補間を進める (毎コールバックで sample_count 進む)。
        if self.active_snapshot.is_active() {
            tick_snapshot_interpolation(
                &mut self.active_snapshot,
                sample_count as u64,
                &mut self.bus_world,
                &mut self.lpf_world,
                &mut self.hpf_world,
                &mut self.reverb_world,
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
        // Phase 3-1: Custom Attenuation Curve のレジストリも snapshot 取得。
        let curves = self.shared_curves.load();

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
                &curves,
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
