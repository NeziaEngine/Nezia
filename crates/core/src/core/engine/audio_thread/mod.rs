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
use std::sync::atomic::{AtomicU64, Ordering};

use arc_swap::ArcSwap;
use ringbuf::HeapProd;
use ringbuf::traits::{Consumer, Producer};

use crate::audio::AudioBuffer;
use crate::bus::{BusSystem, BusWorld};
use crate::capture::CaptureShared;
use crate::command::Command;
use crate::effect::{EffectWorld, EffectWorlds};
use crate::entity::{EntityId, SourcePositionUpdate, SourceVelocityUpdate};
use crate::event::Event;
use crate::limiter::apply_soft_clip;
use crate::metrics::{EngineMetrics, update_peak};
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
    effect_worlds: EffectWorlds,
    /// Source Pre-Spatial chain 適用用の事前確保 mono スクラッチ。
    /// 容量は `MAX_MIX_BUFFER_SIZE / 1ch` でフレーム単位。サウンドスレッド alloc 0。
    mono_scratch: Vec<f32>,
    /// Phase 3-4: 予約再生のサブコールバック開始 frame オフセット (per-source)。
    /// `activate_scheduled` が dense index ごとに「この callback の何 frame 目から鳴らすか」
    /// を書き、mix system が `bus_buf[off * channels..]` で消費する。
    /// 容量 `MAX_SOURCES`、毎 callback 冒頭で 0 リセット。
    start_offset_scratch: Vec<u32>,
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
    /// Voice Virtualization の物理ボイス上限 (`EngineConfig::max_physical_voices`)。
    max_physical_voices: usize,
    /// マスター出力キャプチャ用 SPSC リングの producer。
    /// `capture_shared.enabled` が false のときは触らない (hot path コスト 0)。
    capture_producer: HeapProd<f32>,
    /// メインスレッドと共有する制御フラグ + ドロップ累積。
    capture_shared: Arc<CaptureShared>,
    /// エンジン起動以降の累積処理フレーム数 (per-channel sample count)。
    /// 任意スレッドから `SoundEngine::dsp_time_samples()` 経由で読まれる。
    dsp_time_frames: Arc<AtomicU64>,
    /// ベンチマーク用ランタイム計測値。任意スレッドから `SoundEngine::dsp_stats()` 等で読まれる。
    metrics: Arc<EngineMetrics>,
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
        effect_worlds: EffectWorlds,
        shared_buffers: Arc<ArcSwap<Vec<Option<Arc<AudioBuffer>>>>>,
        shared_curves: Arc<ArcSwap<Vec<Option<Arc<AttenuationCurve>>>>>,
        shared_snapshots: Arc<ArcSwap<Vec<Option<Arc<crate::snapshot::Snapshot>>>>>,
        live_params: Arc<SourceLiveParams>,
        master_bus_id: EntityId,
        device_sample_rate: f32,
        device_channels: usize,
        max_physical_voices: usize,
        max_sources: usize,
        capture_producer: HeapProd<f32>,
        capture_shared: Arc<CaptureShared>,
        dsp_time_frames: Arc<AtomicU64>,
        metrics: Arc<EngineMetrics>,
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
            effect_worlds,
            mono_scratch: vec![0.0; crate::bus::MAX_MIX_BUFFER_SIZE],
            start_offset_scratch: vec![0; max_sources],
            shared_buffers,
            shared_curves,
            shared_snapshots,
            active_snapshot: crate::snapshot::ActiveSnapshot::new(),
            live_params,
            master_bus_id,
            device_sample_rate,
            device_channels,
            max_physical_voices,
            capture_producer,
            capture_shared,
            dsp_time_frames,
            metrics,
        }
    }

    /// cpal のコールバック 1 回分の処理。
    pub(in crate::core::engine) fn process(&mut self, data: &mut [f32]) {
        let sample_count = data.len();
        // ベンチマーク用に DSP 処理時間を計測。Instant::now() は macOS / Linux /
        // Windows いずれもナノ秒精度の vDSO 経由で <50 ns のオーバヘッド。
        let t_start = std::time::Instant::now();

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
                    &self.source_world,
                    &self.effect_world,
                    &self.effect_worlds,
                );
                continue;
            }
            process_command(
                cmd,
                &mut self.bus_world,
                &mut self.source_world,
                &mut self.spatial_world,
                &mut self.effect_world,
                &mut self.effect_worlds,
                &mut self.event_producer,
                self.master_bus_id,
                &self.metrics,
            );
        }

        // Phase 3-2: 進行中の Snapshot 補間を進める (毎コールバックで sample_count 進む)。
        if self.active_snapshot.is_active() {
            tick_snapshot_interpolation(
                &mut self.active_snapshot,
                sample_count as u64,
                &mut self.bus_world,
                &mut self.source_world,
                &mut self.effect_worlds,
            );
        }

        // DSP パラメータ変更で立った dirty フラグをフラッシュして係数を再計算する。
        self.effect_worlds.flush_dirty(self.device_sample_rate);

        // Phase 3-3: Compressor の sidechain buffer は、Send tap 書き込み前に
        // 必ずゼロクリアしておく必要がある (per-callback の最大値が乗らないと検波器が誤反応)。
        self.effect_worlds
            .clear_compressor_sidechain_buffers(sample_count);

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

        // Phase 3-4: 予約再生用に「この callback の冒頭での DSP clock」を読む。
        // dsp_time_frames は callback 末尾で fetch_add されるため、ここでの load は
        // 「直前 callback まで完了済みの累積 frame」= 「この callback の最初の frame の DSP 時刻」。
        let clock_at_callback_start = self.dsp_time_frames.load(Ordering::Relaxed);
        let frames_in_callback = (sample_count / self.device_channels.max(1)) as u64;

        // Source ミキシング → BusWorld の mix_buffer に加算。
        {
            let mix_buf = self.bus_world.mix_buffer_mut();
            SourceMixingSystem::update(
                &mut self.source_world,
                &mut self.spatial_world,
                &self.effect_world,
                &mut self.effect_worlds,
                &mut self.mono_scratch,
                &mut self.start_offset_scratch,
                mix_buf,
                crate::bus::MAX_MIX_BUFFER_SIZE,
                sample_count,
                self.device_channels,
                self.device_sample_rate,
                clock_at_callback_start,
                frames_in_callback,
                &buffers,
                &curves,
                self.max_physical_voices,
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
            &mut self.effect_worlds,
            data,
            self.device_channels,
            sample_count,
        );

        // マスター出力 soft limiter。多重再生時にサンプル総和が ±1.0 を超えても
        // デバイス側でハードクリップせず ±1.0 に漸近する。透過域は |x| <= 0.8 で、
        // 単発再生やゲイン段の単体テスト相当の信号には影響しない。capture も
        // この後で取るため、外部録音にもリミッタ後の信号が反映される。
        apply_soft_clip(data);

        // マスター出力キャプチャ。enabled でない限り capture_producer には触らない。
        // SPSC リングは事前確保済みで alloc 0、push_slice は memcpy + atomic 1 回。
        if self.capture_shared.enabled.load(Ordering::Relaxed) {
            let pushed = self.capture_producer.push_slice(data);
            if pushed < data.len() {
                let dropped = (data.len() - pushed) as u64;
                self.capture_shared
                    .dropped_samples
                    .fetch_add(dropped, Ordering::Relaxed);
                // CaptureOverflow イベントは「同一 callback で取りこぼしが発生した」場合のみ
                // 1 度だけ発火する。連続発火時は「何 sample 落ちたか」が累積で
                // dropped_samples() 経由で取れるので、イベント側は U32_MAX で頭打ち。
                let dropped_u32 = dropped.min(u32::MAX as u64) as u32;
                let _ = self.event_producer.try_push(Event::CaptureOverflow {
                    dropped_samples: dropped_u32,
                });
            }
        }

        // DSP クロックを進める。frames = サンプル数 / チャンネル数 (per-channel)。
        // device_channels == 0 は new() で弾いているので除算は安全。
        let frames_advanced = (sample_count / self.device_channels) as u64;
        self.dsp_time_frames
            .fetch_add(frames_advanced, Ordering::Relaxed);

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
                self.metrics
                    .streaming_underrun_count
                    .fetch_add(1, Ordering::Relaxed);
                if let Some(id) = state.buffer_id() {
                    let _ = self
                        .event_producer
                        .try_push(Event::StreamingUnderrun { buffer: id });
                }
            }
        }

        // 生存ソースのスナップショットを publish する（メインスレッドのクエリ用）。
        publish_source_snapshots(&mut self.source_snapshots_input, &self.source_world);

        // ベンチマーク用カウンタを更新する。
        // - Playing 数: ベンチで「実際に鳴っているボイス本数」として読まれる。
        // - virtualized 数: mix スキップされた本数 (現状の Nezia の voice steal 相当)。
        // 線形スキャン (高々 MAX_SOURCES = 256) で SoA を 1 度だけ走る。
        let mut playing = 0u32;
        let mut virt = 0u32;
        let states = self.source_world.states();
        let virts = self.source_world.is_virtuals();
        let n = states.len();
        for i in 0..n {
            if states[i] == crate::source::SourceState::Playing {
                playing += 1;
                if virts[i] {
                    virt += 1;
                }
            }
        }
        self.metrics
            .active_source_count
            .store(playing, Ordering::Relaxed);
        self.metrics
            .virtualized_voice_count
            .store(virt, Ordering::Relaxed);
        self.metrics
            .voice_steal_count
            .fetch_add(virt as u64, Ordering::Relaxed);

        // DSP 処理時間と予算を atomic に publish。
        let elapsed_ns = t_start.elapsed().as_nanos() as u64;
        // 予算 = (per-channel sample 数 / sample_rate) * 1e9。device_channels == 0 は
        // new() で除外済みのため安全。sample_rate <= 0 のときは 0 を入れる。
        let budget_ns = if self.device_sample_rate > 0.0 {
            let per_channel = (sample_count / self.device_channels) as f64;
            ((per_channel / self.device_sample_rate as f64) * 1.0e9) as u64
        } else {
            0
        };
        self.metrics
            .last_callback_ns
            .store(elapsed_ns, Ordering::Relaxed);
        self.metrics
            .last_callback_budget_ns
            .store(budget_ns, Ordering::Relaxed);
        self.metrics.callback_count.fetch_add(1, Ordering::Relaxed);
        self.metrics
            .callback_total_ns
            .fetch_add(elapsed_ns, Ordering::Relaxed);
        update_peak(&self.metrics.peak_callback_ns, elapsed_ns);
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
