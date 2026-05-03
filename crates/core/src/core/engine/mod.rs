mod audio_thread;
mod buffer_api;
mod buffer_reader;
mod bus_api;
mod callback_registry;
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
use crate::entity::EntityId;
use crate::event::Event;
use crate::source::{MAX_SOURCES, SourceWorld};
use crate::spatial::{ListenerState, SpatialWorld};

use audio_thread::AudioThread;
use callback_registry::CallbackRegistry;

pub use buffer_reader::BufferReader;

/// メインスレッドからソースの生存・再生位置を確認するためのスナップショット。
///
/// サウンドスレッドが各オーディオコールバック末尾で publish し、メインスレッドが
/// `poll_events()` で取り込む。`SourceWorld` の所有自体はサウンドスレッド側に残し、
/// クエリだけが triple buffer 経由で同期される。
#[derive(Debug, Clone, Copy)]
pub(super) struct SourceSnapshot {
    pub(super) index: u32,
    pub(super) generation: u32,
    pub(super) sample_offset: f32,
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
    pub(super) position_updates_input: triple_buffer::Input<Vec<(EntityId, [f32; 3])>>,
    /// cpal のストリームハンドル。Drop 時に再生が停止される。
    _stream: Stream,
    /// AudioBuffer のスロット管理。
    pub(super) buffer_pool: AudioBufferPool,
    /// バスルーティングのメインスレッドミラー（ループ検出・トポロジカルソート用）。
    pub(super) bus_routing: BusRoutingMirror,
    /// 3D ソース用 EntityId の単調増加カウンタ。
    pub(super) next_source_index: u32,
    /// コールバック登録テーブル。
    pub(super) callbacks: CallbackRegistry,
    /// サウンドスレッドが publish する生存ソースのスナップショット出力側。
    /// `poll_events()` で update し、`source_state_cache` にコピーされる。
    pub(super) source_snapshots_output: triple_buffer::Output<Vec<SourceSnapshot>>,
    /// メインスレッド側の最新スナップショット。
    /// `is_source_alive()` / `source_position()` がここを参照する。
    pub(super) source_state_cache: Vec<SourceSnapshot>,
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
        let (source_snapshots_input, source_snapshots_output) = build_source_snapshots_buffer();

        let shared_buffers: Arc<ArcSwap<Vec<Option<Arc<AudioBuffer>>>>> =
            Arc::new(ArcSwap::from_pointee(Vec::new()));
        let shared_buffers_clone = Arc::clone(&shared_buffers);

        let bus_world = BusWorld::new();
        let master_bus_id = bus_world.master_entity();

        let source_world = SourceWorld::new();
        let spatial_world = SpatialWorld::new();

        let mut audio_thread = AudioThread::new(
            command_consumer,
            event_producer,
            listener_output,
            position_updates_output,
            source_snapshots_input,
            bus_world,
            source_world,
            spatial_world,
            shared_buffers_clone,
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
            _stream: stream,
            buffer_pool: AudioBufferPool::new(shared_buffers),
            bus_routing: BusRoutingMirror::new(master_bus_id),
            next_source_index: 0,
            callbacks: CallbackRegistry::new(),
            source_snapshots_output,
            source_state_cache: Vec::with_capacity(MAX_SOURCES),
        })
    }

    /// ゲームループの毎フレーム末尾で呼ぶ。
    ///
    /// サウンドスレッドからのイベントをドレインし、登録済みの `on_finish` コールバックを呼び出す。
    pub fn poll_events(&mut self) {
        while let Some(ev) = self.event_consumer.try_pop() {
            match ev {
                Event::SourceFinished { token } => {
                    if let Some(cb) = self.callbacks.complete(token) {
                        cb();
                    }
                }
                Event::PlayFailed { token } => {
                    // コールバックを解放するのみ（呼び出しは行わない）。
                    self.callbacks.cancel(token);
                }
            }
        }

        // ソース状態スナップショットを取り込む。
        if self.source_snapshots_output.update() {
            self.source_state_cache.clear();
            self.source_state_cache
                .extend_from_slice(self.source_snapshots_output.output_buffer_mut());
        }
    }

    /// ソースが現在 SourceWorld に存在するかを最新スナップショットで確認する。
    ///
    /// スナップショットは `poll_events()` でのみ更新されるため、最後の poll
    /// 以降の生成・終了は反映されない。フレーム末尾で poll する想定。
    #[must_use]
    pub fn is_source_alive(&self, id: EntityId) -> bool {
        self.source_state_cache
            .iter()
            .any(|s| s.index == id.index && s.generation == id.generation)
    }

    /// ソースの再生位置（フレーム単位）を最新スナップショットから取得する。
    #[must_use]
    pub fn source_position(&self, id: EntityId) -> Option<f32> {
        self.source_state_cache
            .iter()
            .find(|s| s.index == id.index && s.generation == id.generation)
            .map(|s| s.sample_offset)
    }
}

/// ソース位置更新用の triple buffer を初期化する。
///
/// 全 3 スロットに `MAX_SOURCES` ぶんの capacity を確保しておくことで、
/// メインスレッドの `clear + extend_from_slice` で再確保が起きないようにする。
/// 入力側は publish 直後に空 Vec で 1 回 publish しておき、初回 `update()` で
/// ダミー位置データが apply されるのを防ぐ。
type PositionUpdatesIn = triple_buffer::Input<Vec<(EntityId, [f32; 3])>>;
type PositionUpdatesOut = triple_buffer::Output<Vec<(EntityId, [f32; 3])>>;

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
    let positions_initial: Vec<(EntityId, [f32; 3])> = vec![
        (
            EntityId {
                index: 0,
                generation: 0,
            },
            [0.0; 3],
        );
        MAX_SOURCES
    ];
    let (mut input, mut output) = triple_buffer::triple_buffer(&positions_initial);
    input.input_buffer_mut().clear();
    input.publish();
    output.update();
    (input, output)
}
