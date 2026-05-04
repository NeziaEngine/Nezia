mod audio_thread;
mod buffer_api;
mod buffer_reader;
mod bus_api;
mod callback_registry;
mod effect_alloc;
mod effect_api;
mod live_params;
mod slot_allocator;
mod snapshot_api;
mod source_api;
mod spatial_api;

use std::sync::Arc;

use arc_swap::ArcSwap;
use cpal::Stream;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::{
    HeapRb,
    traits::{Consumer, Split},
};

use crate::audio::AudioBuffer;
use crate::buffer_pool::AudioBufferPool;
use crate::bus::BusWorld;
use crate::command::Command;
use crate::core::bus_routing::BusRoutingMirror;
use crate::effect::{EffectWorld, HpfWorld, LpfWorld, ReverbWorld};
use crate::entity::{EntityId, SourcePositionUpdate, SourceVelocityUpdate};
use crate::event::Event;
use crate::snapshot::{Snapshot, SnapshotRegistry};
use crate::source::{MAX_SOURCES, SourceWorld};
use crate::spatial::{AttenuationCurve, CurveRegistry, ListenerState, SpatialWorld};

use audio_thread::AudioThread;
use callback_registry::{CallbackKind, CallbackRegistry};
use effect_alloc::EffectIdAllocator;
pub(crate) use live_params::SourceLiveParams;
use slot_allocator::SourceSlotAllocator;

pub use buffer_reader::BufferReader;

/// メインスレッドからソースの生存・再生位置を確認するためのスナップショット。
///
/// サウンドスレッドが各オーディオコールバック末尾で publish し、メインスレッドが
/// `poll_events()` で取り込む。`SourceWorld` の所有自体はサウンドスレッド側に残し、
/// クエリだけが triple buffer 経由で同期される。
///
/// triple buffer に乗せる側は AoS（フォーマットが固定で扱いやすい）、
/// メインスレッド側のクエリキャッシュは SoA（連続スキャンが速い）で持つ。
#[derive(Debug, Clone, Copy)]
pub(super) struct SourceSnapshot {
    pub(super) index: u32,
    pub(super) generation: u32,
    pub(super) sample_offset: f32,
}

/// メインスレッド側のクエリキャッシュ（SoA）。
///
/// `is_source_alive` / `source_position` の単発検索でも、`batch_*` の一括検索でも
/// 共通でこの構造を線形スキャンする。`indices` 配列だけ触れば generation 一致を
/// 確認するときまで他の配列にアクセスしないので、L1 効率が高い。
#[derive(Default)]
pub(super) struct SourceStateCache {
    pub(super) indices: Vec<u32>,
    pub(super) generations: Vec<u32>,
    pub(super) sample_offsets: Vec<f32>,
}

impl SourceStateCache {
    fn with_capacity(cap: usize) -> Self {
        Self {
            indices: Vec::with_capacity(cap),
            generations: Vec::with_capacity(cap),
            sample_offsets: Vec::with_capacity(cap),
        }
    }

    fn clear(&mut self) {
        self.indices.clear();
        self.generations.clear();
        self.sample_offsets.clear();
    }

    fn refill_from(&mut self, snapshots: &[SourceSnapshot]) {
        self.clear();
        for s in snapshots {
            self.indices.push(s.index);
            self.generations.push(s.generation);
            self.sample_offsets.push(s.sample_offset);
        }
    }

    /// `id` の dense 位置を返す（generation も一致する場合のみ）。
    /// 未存在 / stale generation なら `None`。
    #[inline]
    fn find(&self, id: EntityId) -> Option<usize> {
        // hot path: indices をシーケンシャルに走査。マッチしたら generation を確認。
        for (i, &idx) in self.indices.iter().enumerate() {
            if idx == id.index && self.generations[i] == id.generation {
                return Some(i);
            }
        }
        None
    }
}

/// コマンドリングバッファの容量。
const COMMAND_RING_CAPACITY: usize = 128;

/// イベントリングバッファの容量。
const EVENT_RING_CAPACITY: usize = 64;

/// サウンドエンジン。メインスレッド側で保持し、コマンドを発行する。
///
/// API は責務ごとに以下のサブモジュールへ分離されている:
/// - [`buffer_api`] — バッファのロード・アンロード
/// - [`source_api`] — Source の再生・ライブ制御
/// - [`spatial_api`] — リスナー / 3D 位置情報の publish
/// - [`bus_api`] — バスの生成・削除・ルーティング
pub struct SoundEngine {
    /// コマンドリングバッファのプロデューサ側（メインスレッドが所有）。
    pub(super) command_producer: ringbuf::HeapProd<Command>,
    /// イベントリングバッファのコンシューマ側（メインスレッドが所有）。
    event_consumer: ringbuf::HeapCons<Event>,
    /// リスナー姿勢の triple buffer 入力側（newest-wins, alloc 無し）。
    pub(super) listener_input: triple_buffer::Input<ListenerState>,
    /// ソース位置更新の triple buffer 入力側（newest-wins, alloc 無し）。
    /// 内部 Vec の容量は MAX_SOURCES で固定。clear + extend_from_slice で再確保なし。
    pub(super) position_updates_input: triple_buffer::Input<Vec<SourcePositionUpdate>>,
    /// SP-10: ソース速度更新の triple buffer 入力側（newest-wins, alloc 無し）。
    /// position_updates と同じ運用（容量 MAX_SOURCES 固定、clear + extend_from_slice）。
    pub(super) velocity_updates_input: triple_buffer::Input<Vec<SourceVelocityUpdate>>,
    /// cpal のストリームハンドル。Drop 時に再生が停止される。
    _stream: Stream,
    /// AudioBuffer のスロット管理。
    pub(super) buffer_pool: AudioBufferPool,
    /// Phase 3-1: Custom Attenuation Curve のレジストリ。
    pub(super) curve_registry: CurveRegistry,
    /// Phase 3-2: Mixer Snapshot のレジストリ。
    pub(super) snapshot_registry: SnapshotRegistry,
    /// デバイスのサンプルレート (`apply_snapshot` の fade 秒 → サンプル換算で使用)。
    pub(super) device_sample_rate: f32,
    /// バスルーティングのメインスレッドミラー（ループ検出・トポロジカルソート用）。
    pub(super) bus_routing: BusRoutingMirror,
    /// 3D ソース用 EntityId のスロット管理（再利用付き、上限 MAX_SOURCES）。
    pub(super) source_slots: SourceSlotAllocator,
    /// DSP エフェクト用 EntityId のスロット管理（再利用付き、上限 MAX_EFFECTS）。
    pub(super) effect_slots: EffectIdAllocator,
    /// メイン⇄サウンド両スレッドで共有する SoA ライブパラメータ。
    /// `set_source_volume` 等は SPSC コマンドではなくこちらに直接 atomic store する。
    pub(super) live_params: Arc<SourceLiveParams>,
    /// コールバック登録テーブル。
    pub(super) callbacks: CallbackRegistry,
    /// サウンドスレッドが publish する生存ソースのスナップショット出力側。
    /// `poll_events()` で update し、`source_state_cache` にコピーされる。
    pub(super) source_snapshots_output: triple_buffer::Output<Vec<SourceSnapshot>>,
    /// メインスレッド側の最新スナップショット（SoA）。
    /// `is_source_alive()` / `source_position()` / `batch_*` 系がここを参照する。
    /// SoA レイアウトにより、batch query の hot path（`indices` だけ舐める）で
    /// L1 キャッシュ親和性が高い。
    pub(super) source_state_cache: SourceStateCache,
}

impl SoundEngine {
    /// サウンドエンジンを初期化し、オーディオ再生を開始する。
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or("no output device available")?;
        let config = device.default_output_config()?;

        let device_name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        println!("Output device: {device_name}");
        println!("Sample rate: {}", config.sample_rate());
        println!("Channels: {}", config.channels());

        let device_sample_rate = config.sample_rate() as f32;
        let device_channels = config.channels() as usize;

        let ring = HeapRb::<Command>::new(COMMAND_RING_CAPACITY);
        let (command_producer, command_consumer) = ring.split();

        let event_ring = HeapRb::<Event>::new(EVENT_RING_CAPACITY);
        let (event_producer, event_consumer) = event_ring.split();

        let (listener_input, listener_output) =
            triple_buffer::triple_buffer(&ListenerState::default());
        let (position_updates_input, position_updates_output) = build_position_updates_buffer();
        let (velocity_updates_input, velocity_updates_output) = build_velocity_updates_buffer();
        let (source_snapshots_input, source_snapshots_output) = build_source_snapshots_buffer();

        let shared_buffers: Arc<ArcSwap<Vec<Option<Arc<AudioBuffer>>>>> =
            Arc::new(ArcSwap::from_pointee(Vec::new()));
        let shared_buffers_clone = Arc::clone(&shared_buffers);

        // Phase 3-1: Custom Attenuation Curve のレジストリ snapshot を作成。
        let shared_curves: Arc<ArcSwap<Vec<Option<Arc<AttenuationCurve>>>>> =
            Arc::new(ArcSwap::from_pointee(Vec::new()));
        let shared_curves_clone = Arc::clone(&shared_curves);

        // Phase 3-2: Mixer Snapshot のレジストリ snapshot を作成。
        let shared_snapshots: Arc<ArcSwap<Vec<Option<Arc<Snapshot>>>>> =
            Arc::new(ArcSwap::from_pointee(Vec::new()));
        let shared_snapshots_clone = Arc::clone(&shared_snapshots);

        let bus_world = BusWorld::new();
        let master_bus_id = bus_world.master_entity();

        let source_world = SourceWorld::new();
        let spatial_world = SpatialWorld::new();
        let effect_world = EffectWorld::new();
        let lpf_world = LpfWorld::new();
        let hpf_world = HpfWorld::new();
        let reverb_world = ReverbWorld::new();

        let live_params = Arc::new(SourceLiveParams::new());
        let live_params_audio = Arc::clone(&live_params);

        let mut audio_thread = AudioThread::new(
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
            shared_buffers_clone,
            shared_curves_clone,
            shared_snapshots_clone,
            live_params_audio,
            master_bus_id,
            device_sample_rate,
            device_channels,
        );

        let stream = device.build_output_stream(
            &config.into(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                audio_thread.process(data);
            },
            |err| eprintln!("stream error: {err}"),
            None,
        )?;

        stream.play()?;

        Ok(Self {
            command_producer,
            event_consumer,
            listener_input,
            position_updates_input,
            velocity_updates_input,
            _stream: stream,
            buffer_pool: AudioBufferPool::new(shared_buffers),
            curve_registry: CurveRegistry::new(shared_curves),
            snapshot_registry: SnapshotRegistry::new(shared_snapshots),
            device_sample_rate,
            bus_routing: BusRoutingMirror::new(master_bus_id),
            source_slots: SourceSlotAllocator::new(),
            effect_slots: EffectIdAllocator::new(),
            live_params,
            callbacks: CallbackRegistry::new(),
            source_snapshots_output,
            source_state_cache: SourceStateCache::with_capacity(MAX_SOURCES),
        })
    }

    /// ゲームループの毎フレーム末尾で呼ぶ。
    ///
    /// サウンドスレッドからのイベントをドレインし、登録済みの `on_finish` コールバックを呼び出す。
    pub fn poll_events(&mut self) {
        while let Some(ev) = self.event_consumer.try_pop() {
            match ev {
                Event::SourceFinished { token } => {
                    match self.callbacks.complete(token) {
                        CallbackKind::Native { f, user_data } => {
                            // SAFETY: 呼出側契約により f / user_data は発火時まで有効。
                            // ABI 越境を最小化するため fn ptr を直呼びする（Box ナシ）。
                            unsafe { f(user_data as *mut std::ffi::c_void) };
                        }
                        CallbackKind::Rust(closure) => closure(),
                        CallbackKind::Empty => {}
                    }
                }
                Event::PlayFailed { token } => {
                    // コールバックを解放するのみ（呼び出しは行わない）。
                    self.callbacks.cancel(token);
                }
                Event::SourceDespawned { id } => {
                    // スロット index を再利用キューに戻す。
                    self.source_slots.free(id);
                }
                Event::StreamingUnderrun { buffer } => {
                    // 現状は通知のみ。アプリ側がコールバック経由で観測するための
                    // 公開 API を Phase 2-4 後半で追加予定。
                    let _ = buffer;
                }
            }
        }

        // ソース状態スナップショットを取り込む（AoS → SoA への詰め替え）。
        if self.source_snapshots_output.update() {
            let snapshots = self.source_snapshots_output.output_buffer_mut();
            self.source_state_cache.refill_from(snapshots);
        }
    }

    /// ソースが現在 SourceWorld に存在するかを最新スナップショットで確認する。
    ///
    /// スナップショットは `poll_events()` でのみ更新されるため、最後の poll
    /// 以降の生成・終了は反映されない。フレーム末尾で poll する想定。
    #[must_use]
    pub fn is_source_alive(&self, id: EntityId) -> bool {
        self.source_state_cache.find(id).is_some()
    }

    /// ソースの再生位置（フレーム単位）を最新スナップショットから取得する。
    #[must_use]
    pub fn source_position(&self, id: EntityId) -> Option<f32> {
        self.source_state_cache
            .find(id)
            .map(|i| self.source_state_cache.sample_offsets[i])
    }

    /// 複数ソースの生存を一括判定する。
    ///
    /// `ids` と `out_alive` は同じ長さを持つ前提。`out_alive[i]` には
    /// `ids[i]` が現在の最新スナップショットに存在し generation も一致する場合 `1`、
    /// それ以外は `0` が書き込まれる。スナップショットは `poll_events()` でのみ
    /// 更新されるため、最後の poll 以降の生成・終了は反映されない。
    pub fn batch_is_source_alive(&self, ids: &[EntityId], out_alive: &mut [u8]) {
        let n = ids.len().min(out_alive.len());
        for i in 0..n {
            out_alive[i] = self.source_state_cache.find(ids[i]).is_some() as u8;
        }
    }

    /// 複数ソースの再生位置を一括取得する。
    ///
    /// `ids` / `out_positions` / `out_alive` は同じ長さを持つ前提。
    /// alive でない場合は `out_positions[i]` に `f32::NAN`、`out_alive[i]` に `0`。
    /// `out_alive` を不要なら `&mut []` を渡してもよい（その場合は alive 判定は
    /// `out_positions[i].is_nan()` で代替できる）。
    pub fn batch_source_positions(
        &self,
        ids: &[EntityId],
        out_positions: &mut [f32],
        out_alive: &mut [u8],
    ) {
        let n = ids.len().min(out_positions.len());
        for i in 0..n {
            match self.source_state_cache.find(ids[i]) {
                Some(idx) => {
                    out_positions[i] = self.source_state_cache.sample_offsets[idx];
                    if i < out_alive.len() {
                        out_alive[i] = 1;
                    }
                }
                None => {
                    out_positions[i] = f32::NAN;
                    if i < out_alive.len() {
                        out_alive[i] = 0;
                    }
                }
            }
        }
    }
}

/// ソース位置更新用の triple buffer を初期化する。
///
/// 全 3 スロットに `MAX_SOURCES` ぶんの capacity を確保しておくことで、
/// メインスレッドの `clear + extend_from_slice` で再確保が起きないようにする。
/// 入力側は publish 直後に空 Vec で 1 回 publish しておき、初回 `update()` で
/// ダミー位置データが apply されるのを防ぐ。
type PositionUpdatesIn = triple_buffer::Input<Vec<SourcePositionUpdate>>;
type PositionUpdatesOut = triple_buffer::Output<Vec<SourcePositionUpdate>>;

type VelocityUpdatesIn = triple_buffer::Input<Vec<SourceVelocityUpdate>>;
type VelocityUpdatesOut = triple_buffer::Output<Vec<SourceVelocityUpdate>>;

type SourceSnapshotsIn = triple_buffer::Input<Vec<SourceSnapshot>>;
type SourceSnapshotsOut = triple_buffer::Output<Vec<SourceSnapshot>>;

/// ソーススナップショット用の triple buffer を初期化する。
///
/// 全 3 スロットに `MAX_SOURCES` ぶんの capacity を確保しておくことで、
/// サウンドスレッドの `clear + push` で再確保が起きないようにする。
fn build_source_snapshots_buffer() -> (SourceSnapshotsIn, SourceSnapshotsOut) {
    let initial: Vec<SourceSnapshot> = vec![
        SourceSnapshot {
            index: 0,
            generation: 0,
            sample_offset: 0.0
        };
        MAX_SOURCES
    ];
    let (mut input, mut output) = triple_buffer::triple_buffer(&initial);
    input.input_buffer_mut().clear();
    input.publish();
    output.update();
    (input, output)
}

fn build_position_updates_buffer() -> (PositionUpdatesIn, PositionUpdatesOut) {
    let positions_initial: Vec<SourcePositionUpdate> = vec![
        SourcePositionUpdate {
            source: EntityId {
                index: 0,
                generation: 0,
            },
            position: [0.0; 3],
        };
        MAX_SOURCES
    ];
    let (mut input, mut output) = triple_buffer::triple_buffer(&positions_initial);
    input.input_buffer_mut().clear();
    input.publish();
    output.update();
    (input, output)
}

/// SP-10: ソース速度更新用の triple buffer を初期化する。`build_position_updates_buffer` と同パターン。
fn build_velocity_updates_buffer() -> (VelocityUpdatesIn, VelocityUpdatesOut) {
    let velocities_initial: Vec<SourceVelocityUpdate> = vec![
        SourceVelocityUpdate {
            source: EntityId {
                index: 0,
                generation: 0,
            },
            velocity: [0.0; 3],
        };
        MAX_SOURCES
    ];
    let (mut input, mut output) = triple_buffer::triple_buffer(&velocities_initial);
    input.input_buffer_mut().clear();
    input.publish();
    output.update();
    (input, output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(index: u32, generation: u32, sample_offset: f32) -> SourceSnapshot {
        SourceSnapshot {
            index,
            generation,
            sample_offset,
        }
    }

    #[test]
    fn cache_find_returns_none_when_empty() {
        let cache = SourceStateCache::default();
        assert!(
            cache
                .find(EntityId {
                    index: 0,
                    generation: 0
                })
                .is_none()
        );
    }

    #[test]
    fn cache_find_matches_index_and_generation() {
        let mut cache = SourceStateCache::with_capacity(4);
        cache.refill_from(&[snap(3, 7, 100.0), snap(5, 1, 200.0)]);
        assert_eq!(
            cache.find(EntityId {
                index: 3,
                generation: 7
            }),
            Some(0)
        );
        assert_eq!(
            cache.find(EntityId {
                index: 5,
                generation: 1
            }),
            Some(1)
        );
        assert_eq!(
            cache.find(EntityId {
                index: 3,
                generation: 8
            }),
            None
        );
        assert_eq!(
            cache.find(EntityId {
                index: 99,
                generation: 0
            }),
            None
        );
    }

    #[test]
    fn cache_refill_clears_old_entries() {
        let mut cache = SourceStateCache::with_capacity(4);
        cache.refill_from(&[snap(3, 7, 100.0)]);
        cache.refill_from(&[snap(5, 1, 200.0)]);
        assert!(
            cache
                .find(EntityId {
                    index: 3,
                    generation: 7
                })
                .is_none()
        );
        assert_eq!(
            cache.find(EntityId {
                index: 5,
                generation: 1
            }),
            Some(0)
        );
    }
}
