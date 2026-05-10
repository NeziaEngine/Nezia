mod audio_thread;
mod buffer_api;
mod buffer_reader;
mod bus_api;
mod callback_registry;
mod capture_api;
mod container_api;
mod effect_alloc;
mod effect_api;
mod event_dispatch;
mod live_params;
mod metrics_api;
mod query_api;
mod send_alloc;
mod send_api;
mod slot_allocator;
mod snapshot_api;
mod source_api;
mod source_state_cache;
mod spatial_api;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use arc_swap::ArcSwap;
use cpal::Stream;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::{HeapRb, traits::Split};

use crate::audio::AudioBuffer;
use crate::buffer_pool::AudioBufferPool;
use crate::bus::BusWorld;
use crate::capture::{CaptureReader, CaptureShared};
use crate::command::Command;
use crate::container::ContainerWorld;
use crate::core::bus_routing::BusRoutingMirror;
use crate::effect::{EffectWorld, EffectWorlds};
use crate::entity::{EntityId, SourcePositionUpdate, SourceVelocityUpdate};
use crate::event::Event;
use crate::metrics::EngineMetrics;
use crate::snapshot::{Snapshot, SnapshotRegistry};
use crate::source::{MAX_SOURCES, SourceWorld};
use crate::spatial::{AttenuationCurve, CurveRegistry, ListenerState, SpatialWorld};

use audio_thread::AudioThread;
use callback_registry::CallbackRegistry;
use effect_alloc::EffectIdAllocator;
pub(crate) use live_params::SourceLiveParams;
use send_alloc::SendIdAllocator;
use slot_allocator::SourceSlotAllocator;
pub(crate) use source_state_cache::SourceSnapshot;
use source_state_cache::{SourceStateCache, build_source_snapshots_buffer};

pub use buffer_reader::BufferReader;

/// コマンドリングバッファの容量。
const COMMAND_RING_CAPACITY: usize = 128;

/// イベントリングバッファの容量。
const EVENT_RING_CAPACITY: usize = 64;

/// マスター出力キャプチャリングの容量 (秒)。device_sample_rate * device_channels * この値を
/// インターリーブサンプル数として確保する。1.0 秒分あれば、Unity Recorder のメインスレッド
/// drain ジッタ (フレーム落ちを含めて 〜500ms) を吸収できる余裕がある。
const CAPTURE_RING_SECONDS: f32 = 1.0;

/// サウンドエンジン。メインスレッド側で保持し、コマンドを発行する。
///
/// API は責務ごとに以下のサブモジュールへ分離されている:
/// - [`buffer_api`] — バッファのロード・アンロード
/// - [`source_api`] — Source の再生・ライブ制御
/// - [`spatial_api`] — リスナー / 3D 位置情報の publish
/// - [`bus_api`] — バスの生成・削除・ルーティング
/// - [`effect_api`] — DSP エフェクトの spawn / param
/// - [`send_api`] — Send / Sidechain ルーティング
/// - [`snapshot_api`] — Mixer Snapshot
/// - [`container_api`] — Container (Random / Switch / Sequence)
/// - [`capture_api`] — マスター出力キャプチャと出力フォーマット
/// - [`metrics_api`] — ベンチマーク用メトリクスの読み出し
/// - [`query_api`] — 生存判定 / 再生位置クエリ
/// - [`event_dispatch`] — `poll_events()` でのコールバック dispatch
pub struct SoundEngine {
    /// コマンドリングバッファのプロデューサ側（メインスレッドが所有）。
    pub(super) command_producer: ringbuf::HeapProd<Command>,
    /// イベントリングバッファのコンシューマ側（メインスレッドが所有）。
    pub(super) event_consumer: ringbuf::HeapCons<Event>,
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
    /// Phase 3-3: Send ハンドル発行。
    pub(super) send_slots: SendIdAllocator,
    /// Source 起点 Send (User-Defined Aux Send) のメインスレッドミラー。
    /// SendId.index → `(SendId, 発行元 Source EntityId)` のマップ。`Event::SourceDespawned`
    /// 受信時に該当 SendId を一括解放するために使う。`MAX_SENDS` で固定確保。
    pub(super) source_sends: Vec<Option<(crate::bus::SendId, crate::entity::EntityId)>>,
    /// Phase 4-2: Random Container のメインスレッド側ワールド (audio thread には流れない)。
    pub(super) container_world: ContainerWorld,
    /// Phase 3-3 PR2: Compressor EffectId → 所属バス EntityId のマッピング。
    /// `add_send_to_compressor` で sidechain Send を貼る際、メインスレッドが
    /// 所属バスを resolve して DAG topological sort のエッジに反映するために使う。
    pub(super) compressor_owners: HashMap<crate::effect::EffectId, EntityId>,
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
    /// デバイス出力チャンネル数 (通常 2)。`output_format()` で公開する。
    pub(super) device_channels: u16,
    /// マスター出力キャプチャの enabled フラグ + ドロップ累積。audio thread と共有。
    pub(super) capture_shared: Arc<CaptureShared>,
    /// 初回 `enable_master_capture()` 呼び出しで `take()` される唯一のリーダー。
    /// 取った後は再 enable しても `None` のまま (既存ハンドルが流量を受け続ける)。
    pub(super) capture_reader: Option<CaptureReader>,
    /// エンジン起動以降の累積処理フレーム数。`dsp_time_samples()` で公開。
    pub(super) dsp_time_frames: Arc<AtomicU64>,
    /// audio thread と共有するベンチマーク用ランタイム計測値。
    pub(super) metrics: Arc<EngineMetrics>,
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
        let effect_worlds = EffectWorlds::new();

        let live_params = Arc::new(SourceLiveParams::new());
        let live_params_audio = Arc::clone(&live_params);

        // マスター出力キャプチャ用 SPSC リングを構築。サンプル単位で 1 秒ぶん確保。
        // 例: 48000 Hz / 2ch = 96000 samples = 384 KB。device_channels が 0 の場合は
        // 後段で除算事故を起こすため、最低 1 として扱う (cpal は通常 1ch 以上を返す)。
        let capture_capacity_samples =
            ((device_sample_rate * CAPTURE_RING_SECONDS) as usize).max(1) * device_channels.max(1);
        let capture_ring = HeapRb::<f32>::new(capture_capacity_samples);
        let (capture_producer, capture_consumer) = capture_ring.split();
        let capture_shared = Arc::new(CaptureShared::new());
        let capture_shared_audio = Arc::clone(&capture_shared);
        let dsp_time_frames = Arc::new(AtomicU64::new(0));
        let dsp_time_frames_audio = Arc::clone(&dsp_time_frames);
        let metrics = Arc::new(EngineMetrics::new());
        let metrics_audio = Arc::clone(&metrics);
        let capture_reader = Some(CaptureReader::new(
            capture_consumer,
            Arc::clone(&capture_shared),
            device_sample_rate as u32,
            device_channels as u16,
        ));

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
            effect_worlds,
            shared_buffers_clone,
            shared_curves_clone,
            shared_snapshots_clone,
            live_params_audio,
            master_bus_id,
            device_sample_rate,
            device_channels,
            capture_producer,
            capture_shared_audio,
            dsp_time_frames_audio,
            metrics_audio,
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
            send_slots: SendIdAllocator::new(),
            source_sends: vec![None; crate::bus::MAX_SENDS],
            container_world: ContainerWorld::new(),
            compressor_owners: HashMap::new(),
            live_params,
            callbacks: CallbackRegistry::new(),
            source_snapshots_output,
            source_state_cache: SourceStateCache::with_capacity(MAX_SOURCES),
            device_channels: device_channels as u16,
            capture_shared,
            capture_reader,
            dsp_time_frames,
            metrics,
        })
    }

    /// SPSC コマンドリングへ 1 コマンド push する内部ヘルパ。
    ///
    /// 失敗 (リング満杯) 時に `EngineMetrics::command_queue_full` を atomic にインクリメントする
    /// ことで、サイレントなコマンド消失を観測可能にする。`command_producer.try_push(...).is_ok()`
    /// の素朴呼び出しはこのメソッド経由に統一し、計測漏れを防ぐ。
    ///
    /// 戻り値はそのまま push 成否 (true = 送信できた)。
    #[inline]
    pub(super) fn try_send_command(&mut self, cmd: Command) -> bool {
        use ringbuf::traits::Producer;
        if self.command_producer.try_push(cmd).is_ok() {
            true
        } else {
            self.metrics
                .command_queue_full
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            false
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
