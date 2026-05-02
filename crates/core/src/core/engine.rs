use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use arc_swap::ArcSwap;
use cpal::Stream;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::{
    HeapRb,
    traits::{Consumer, Producer, Split},
};

use crate::audio::AudioBuffer;
use crate::buffer_pool::{AudioBufferPool, BufferId};
use crate::bus::{BusComponent, BusSystem, BusWorld, MAX_BUSES};
use crate::command::Command;
use crate::core::bus_routing::BusRoutingMirror;
use crate::entity::EntityId;
use crate::event::Event;
use crate::source::{
    MAX_SOURCES, SourceComponent, SourceLifecycleSystem, SourceMixingSystem, SourceState,
    SourceWorld,
};
use crate::spatial::{AttenuationModel, ListenerState, SpatialWorld};

/// コマンドリングバッファの容量。
const COMMAND_RING_CAPACITY: usize = 128;

/// イベントリングバッファの容量。
const EVENT_RING_CAPACITY: usize = 64;

/// audio thread が保持する、スケジュール待ちの再生エントリ。
struct PendingScheduled {
    target_tick: u64,
    audio_buffer_index: u32,
    vol: f32,
    pitch: f32,
    output_bus_dense: u32,
    token: u32,
}

/// 任意スレッドから読める PCM 読み取りハンドル。
///
/// `SoundEngine::open_buffer_reader` で生成し、ハンドル経由で `Arc<AudioBuffer>` を
/// 保持する。これにより:
/// - 任意スレッドから `read_frames()` を呼べる（lock-free）
/// - ハンドル生存中は `unload()` してもメモリが解放されない（reader 側で安全に読み続けられる）
pub struct BufferReader {
    buffer: Arc<AudioBuffer>,
}

impl BufferReader {
    /// チャンネル数。
    pub fn channels(&self) -> u16 {
        self.buffer.channels
    }

    /// サンプルレート（Hz）。
    pub fn sample_rate(&self) -> u32 {
        self.buffer.sample_rate
    }

    /// 総フレーム数（チャンネルあたりのサンプル数）。
    pub fn total_frames(&self) -> usize {
        self.buffer.frame_count()
    }

    /// `frame_offset` 位置から `dst` を埋めるだけのインターリーブ PCM を書き込む。
    ///
    /// 戻り値は実際に書き込んだフレーム数（`dst.len() / channels` 以下）。EOF に達した
    /// 場合は要求より少ないフレーム数を返す。`dst.len()` は `channels` の倍数である必要が
    /// ある（そうでない場合は端数を切り捨てる）。
    pub fn read_frames(&self, frame_offset: usize, dst: &mut [f32]) -> usize {
        let channels = self.buffer.channels as usize;
        if channels == 0 {
            return 0;
        }
        let requested_frames = dst.len() / channels;
        let total_frames = self.buffer.frame_count();
        let available = total_frames.saturating_sub(frame_offset);
        let frames = requested_frames.min(available);
        let sample_offset = frame_offset * channels;
        let sample_count = frames * channels;
        dst[..sample_count]
            .copy_from_slice(&self.buffer.samples[sample_offset..sample_offset + sample_count]);
        frames
    }
}

/// サウンドエンジン。メインスレッド側で保持し、コマンドを発行する。
pub struct SoundEngine {
    /// コマンドリングバッファのプロデューサ側（メインスレッドが所有）。
    command_producer: ringbuf::HeapProd<Command>,
    /// イベントリングバッファのコンシューマ側（メインスレッドが所有）。
    event_consumer: ringbuf::HeapCons<Event>,
    /// リスナー姿勢の triple buffer 入力側（newest-wins, alloc 無し）。
    listener_input: triple_buffer::Input<ListenerState>,
    /// ソース位置更新の triple buffer 入力側（newest-wins, alloc 無し）。
    /// 内部 Vec の容量は MAX_SOURCES で固定。clear + extend_from_slice で再確保なし。
    position_updates_input: triple_buffer::Input<Vec<(EntityId, [f32; 3])>>,
    /// cpal のストリームハンドル。Drop 時に再生が停止される。
    _stream: Stream,
    /// AudioBuffer のスロット管理。
    buffer_pool: AudioBufferPool,
    /// バスルーティングのメインスレッドミラー（ループ検出・トポロジカルソート用）。
    bus_routing: BusRoutingMirror,
    /// 3D ソース用 EntityId の単調増加カウンタ。
    next_source_index: u32,
    /// コールバック登録テーブル。token → on_finish クロージャ。
    callbacks: HashMap<u32, Box<dyn FnOnce() + Send>>,
    /// 次に発行するコールバックトークン（0 はコールバックなしの予約値）。
    next_token: u32,
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
        let (command_producer, mut command_consumer) = ring.split();

        let event_ring = HeapRb::<Event>::new(EVENT_RING_CAPACITY);
        let (mut event_producer, event_consumer) = event_ring.split();

        // 毎フレーム送る newest-wins 状態は triple buffer 経由で受け渡す。
        // alloc 無し・lock-free・順序保証なし（最新値だけ届けばよい用途）。
        let (listener_input, mut listener_output) =
            triple_buffer::triple_buffer(&ListenerState::default());
        // Vec の初期 len と capacity を MAX_SOURCES に揃えておく。Vec::clone は
        // len ぶんの容量を確保するため、3 スロット全てが MAX_SOURCES ぶんの
        // capacity を持ち、メインスレッドで clear + extend_from_slice しても
        // 再確保が起きない。
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
        let (mut position_updates_input, mut position_updates_output) =
            triple_buffer::triple_buffer(&positions_initial);
        // 初期状態は「未公開」にしたいので、入力側を空 Vec に reset して publish。
        // これでサウンドスレッド側 `update()` は「変更なし」を返し、初回 callback で
        // ダミー位置データを apply してしまうのを防ぐ。
        position_updates_input.input_buffer_mut().clear();
        position_updates_input.publish();
        // 初回 update() で空 Vec を吸収しておく（以降は新しい publish のみが届く）。
        position_updates_output.update();

        let shared_buffers: Arc<ArcSwap<Vec<Option<Arc<AudioBuffer>>>>> =
            Arc::new(ArcSwap::from_pointee(Vec::new()));
        let shared_buffers_clone = Arc::clone(&shared_buffers);

        let mut bus_world = BusWorld::new();
        let master_bus_id = bus_world.master_entity();

        let mut source_world = SourceWorld::new();
        let mut spatial_world = SpatialWorld::new();

        // スケジュール再生の保留キュー（audio thread 専有）。
        // 容量超過時は新規スケジュールが拒否されるため、十分大きく取る。
        // Vec の事前 with_capacity により以後 push しても再確保は起きない。
        const PENDING_SCHEDULED_CAPACITY: usize = MAX_SOURCES;
        let mut pending_scheduled: Vec<PendingScheduled> =
            Vec::with_capacity(PENDING_SCHEDULED_CAPACITY);
        // エンジン起動からの累積フレーム tick（audio callback 単位で増加）。
        let mut current_frame_tick: u64 = 0;

        let stream = device.build_output_stream(
            &config.into(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let sample_count = data.len();

                // コマンドを先に処理する。spawn 系を反映してから triple buffer の
                // 位置更新を適用しないと、spawn と同フレームで publish された位置が
                // resolve 失敗で捨てられ、初回 callback がデフォルト位置 [0,0,0] で
                // 再生されてしまう。
                while let Some(cmd) = command_consumer.try_pop() {
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
                            source_world = SourceWorld::new();
                            spatial_world = SpatialWorld::new();
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
                                spatial_world.set_params(
                                    dense,
                                    model,
                                    min_distance,
                                    max_distance,
                                    rolloff,
                                );
                            }
                        }
                        Command::SetSourceSpatialEnabled { id, enabled } => {
                            if let Some(dense) = source_world.resolve(id) {
                                spatial_world.set_enabled(dense, enabled);
                            }
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

                        // ── スケジュール再生（保留キューに積む） ──
                        Command::PlayScheduled {
                            audio_buffer_index,
                            vol,
                            pitch,
                            output_bus_dense,
                            delay_seconds,
                            token,
                        } => {
                            if pending_scheduled.len() < pending_scheduled.capacity() {
                                let delay_frames =
                                    (delay_seconds.max(0.0) * device_sample_rate) as u64;
                                pending_scheduled.push(PendingScheduled {
                                    target_tick: current_frame_tick + delay_frames,
                                    audio_buffer_index,
                                    vol,
                                    pitch,
                                    output_bus_dense,
                                    token,
                                });
                            } else if token != 0 {
                                let _ = event_producer.try_push(Event::PlayFailed { token });
                            }
                        }
                    }
                }

                // ── pending scheduled の消化 ──
                // この callback で再生される範囲（current_frame_tick ..
                // current_frame_tick + frame_count）に target_tick が入っているものを
                // spawn する。jitter は最大 1 callback ぶん（数 ms）。
                let frame_count = sample_count / device_channels;
                let next_frame_tick = current_frame_tick + frame_count as u64;
                let mut i = 0;
                while i < pending_scheduled.len() {
                    if pending_scheduled[i].target_tick <= next_frame_tick {
                        let entry = pending_scheduled.swap_remove(i);
                        let spawned = source_world.spawn(SourceComponent {
                            vol: entry.vol,
                            pitch: entry.pitch,
                            sample_offset: 0.0,
                            audio_buffer_index: entry.audio_buffer_index,
                            output_bus: entry.output_bus_dense,
                            token: entry.token,
                        });
                        if spawned.is_some() {
                            spatial_world.push_defaults();
                        } else if entry.token != 0 {
                            let _ =
                                event_producer.try_push(Event::PlayFailed { token: entry.token });
                        }
                    } else {
                        i += 1;
                    }
                }
                current_frame_tick = next_frame_tick;

                // triple buffer から最新の listener / source positions を取り込む。
                // commands の後にやることで、spawn と同フレームで publish された
                // 位置も resolve に成功する（順序は spawn → 位置適用）。
                if listener_output.update() {
                    spatial_world.listener = *listener_output.output_buffer_mut();
                }
                if position_updates_output.update() {
                    let updates = position_updates_output.output_buffer_mut();
                    for (id, pos) in updates.iter() {
                        if let Some(dense) = source_world.resolve(*id) {
                            spatial_world.set_position(dense, *pos);
                        }
                    }
                }

                // mix_buffer をゼロクリア。
                bus_world.clear_mix_buffers(sample_count);

                // ロックフリーでバッファリストのスナップショットを取得。
                let buffers = shared_buffers_clone.load();

                // Source ミキシング → BusWorld の mix_buffer に加算。
                {
                    let mix_buf = bus_world.mix_buffer_mut();
                    SourceMixingSystem::update(
                        &mut source_world,
                        &mut spatial_world,
                        mix_buf,
                        crate::bus::MAX_MIX_BUFFER_SIZE,
                        sample_count,
                        device_channels,
                        device_sample_rate,
                        &buffers,
                    );
                }

                // 再生終了 Source の despawn。SourceFinished イベントを push する。
                SourceLifecycleSystem::update(
                    &mut source_world,
                    &mut spatial_world,
                    &buffers,
                    &mut |ev| {
                        let _ = event_producer.try_push(ev);
                    },
                );

                // バス処理 → output_buffer へ書き出し。
                BusSystem::update(&mut bus_world, data, device_channels, sample_count);
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
            callbacks: HashMap::new(),
            next_token: 1,
        })
    }

    /// オーディオファイルをロードし、ハンドルを返す。
    pub fn load<P: AsRef<Path>>(
        &mut self,
        path: P,
    ) -> Result<BufferId, Box<dyn std::error::Error>> {
        self.buffer_pool.load(path)
    }

    /// メモリ上のエンコード済みバイト列からロードし、ハンドルを返す。
    ///
    /// 統合層からの主要ロード経路。`NeziaAudioClip` の保持バイト列、Addressables、
    /// `UnityWebRequest` などホスト側で取得したバイト列をそのままデコードする。
    pub fn load_from_memory(
        &mut self,
        bytes: &[u8],
    ) -> Result<BufferId, Box<dyn std::error::Error>> {
        self.buffer_pool.load_from_memory(bytes)
    }

    /// 既にデコード済みの PCM サンプル列からロードし、ハンドルを返す。
    ///
    /// Unity 標準 `AudioClip.GetData()` 結果のような、ホスト側で既に展開済みの
    /// PCM を Nezia バッファに取り込む経路（移行期間用ブリッジ）。
    /// `samples` はインターリーブ形式（ステレオなら `[L0, R0, L1, R1, ...]`）。
    pub fn load_from_pcm(
        &mut self,
        samples: Vec<f32>,
        channels: u16,
        sample_rate: u32,
    ) -> BufferId {
        self.buffer_pool
            .load_from_pcm(samples, channels, sample_rate)
    }

    /// バッファをアンロードする。
    pub fn unload(&mut self, id: BufferId) -> bool {
        self.buffer_pool.unload(id)
    }

    /// 指定バッファに対する読み取り専用ハンドルを開く。
    ///
    /// `BufferReader` は内部で `Arc<AudioBuffer>` を保持するため、main thread が
    /// `unload(id)` してもハンドルが生きている間はバッファのメモリは解放されない。
    /// **任意のスレッドから `read_frames` を呼べる** のが特徴で、Unity の
    /// `AudioClip.Create(stream: true, pcmReadCallback)` のように、main thread と
    /// 別のスレッドから PCM をストリーム供給したいケース向け。
    pub fn open_buffer_reader(&self, id: BufferId) -> Option<BufferReader> {
        let index = self.buffer_pool.resolve(id)? as usize;
        let snapshot = self.buffer_pool.shared_snapshot();
        let buf = snapshot.get(index).and_then(|slot| slot.clone())?;
        Some(BufferReader { buffer: buf })
    }

    /// ボイスをマスターバスに再生する（fire-and-forget）。
    #[must_use]
    pub fn play(&mut self, buffer: BufferId, vol: f32, pitch: f32) -> bool {
        let Some(index) = self.buffer_pool.resolve(buffer) else {
            return false;
        };
        self.command_producer
            .try_push(Command::Play {
                audio_buffer_index: index,
                vol,
                pitch,
                token: 0,
            })
            .is_ok()
    }

    /// ボイスをマスターバスにコールバック付きで再生する。
    ///
    /// 再生が自然終了したとき、次の `poll_events()` で `callback` が呼ばれる。
    /// `MAX_SOURCES` 上限に達していた場合はコールバックは呼ばれない。
    #[must_use]
    pub fn play_with_callback(
        &mut self,
        buffer: BufferId,
        vol: f32,
        pitch: f32,
        callback: impl FnOnce() + Send + 'static,
    ) -> bool {
        let Some(index) = self.buffer_pool.resolve(buffer) else {
            return false;
        };
        let token = self.next_token;
        self.next_token = self.next_token.wrapping_add(1).max(1);
        self.callbacks.insert(token, Box::new(callback));
        let ok = self
            .command_producer
            .try_push(Command::Play {
                audio_buffer_index: index,
                vol,
                pitch,
                token,
            })
            .is_ok();
        if !ok {
            self.callbacks.remove(&token);
        }
        ok
    }

    /// ボイスを指定バスに再生する（fire-and-forget）。
    #[must_use]
    pub fn play_to_bus(&mut self, buffer: BufferId, vol: f32, pitch: f32, bus: EntityId) -> bool {
        let Some(index) = self.buffer_pool.resolve(buffer) else {
            return false;
        };
        let Some(output_bus_dense) = self.bus_routing.resolve_dense(bus) else {
            return false;
        };
        self.command_producer
            .try_push(Command::PlayToBus {
                audio_buffer_index: index,
                vol,
                pitch,
                output_bus_dense,
                token: 0,
            })
            .is_ok()
    }

    /// ボイスを指定バスにコールバック付きで再生する。
    ///
    /// 再生が自然終了したとき、次の `poll_events()` で `callback` が呼ばれる。
    /// `MAX_SOURCES` 上限に達していた場合はコールバックは呼ばれない。
    #[must_use]
    pub fn play_to_bus_with_callback(
        &mut self,
        buffer: BufferId,
        vol: f32,
        pitch: f32,
        bus: EntityId,
        callback: impl FnOnce() + Send + 'static,
    ) -> bool {
        let Some(index) = self.buffer_pool.resolve(buffer) else {
            return false;
        };
        let Some(output_bus_dense) = self.bus_routing.resolve_dense(bus) else {
            return false;
        };
        let token = self.next_token;
        self.next_token = self.next_token.wrapping_add(1).max(1);
        self.callbacks.insert(token, Box::new(callback));
        let ok = self
            .command_producer
            .try_push(Command::PlayToBus {
                audio_buffer_index: index,
                vol,
                pitch,
                output_bus_dense,
                token,
            })
            .is_ok();
        if !ok {
            self.callbacks.remove(&token);
        }
        ok
    }

    /// 3D ソースをスポーンし、EntityId を返す。
    ///
    /// 返った EntityId を使って `set_source_spatial_params()` / `set_source_spatial_enabled()` /
    /// `batch_set_source_positions()` で空間パラメータを更新する。
    #[must_use]
    pub fn spawn_source(
        &mut self,
        buffer: BufferId,
        vol: f32,
        pitch: f32,
        bus: EntityId,
    ) -> Option<EntityId> {
        let index = self.buffer_pool.resolve(buffer)?;
        let output_bus_dense = self.bus_routing.resolve_dense(bus)?;

        let id = EntityId {
            index: self.next_source_index,
            generation: 0,
        };

        self.command_producer
            .try_push(Command::SpawnSource {
                id,
                audio_buffer_index: index,
                vol,
                pitch,
                output_bus_dense,
                token: 0,
            })
            .ok()?;

        self.next_source_index += 1;
        Some(id)
    }

    /// マスター音量を設定する（0.0〜1.0）。マスターバスの gain を変更する。
    #[must_use]
    pub fn set_volume(&mut self, volume: f32) -> bool {
        self.command_producer
            .try_push(Command::SetVolume(volume))
            .is_ok()
    }

    /// すべてのボイスを停止する。
    ///
    /// 登録済みのコールバックは解放されるが呼び出されない。
    #[must_use]
    pub fn stop_all(&mut self) -> bool {
        self.callbacks.clear();
        self.command_producer.try_push(Command::StopAll).is_ok()
    }

    /// ゲームループの毎フレーム末尾で呼ぶ。
    ///
    /// サウンドスレッドからのイベントをドレインし、登録済みの `on_finish` コールバックを呼び出す。
    pub fn poll_events(&mut self) {
        while let Some(ev) = self.event_consumer.try_pop() {
            match ev {
                Event::SourceFinished { token } => {
                    if let Some(cb) = self.callbacks.remove(&token) {
                        cb();
                    }
                }
                Event::PlayFailed { token } => {
                    // コールバックを解放するのみ（呼び出しは行わない）。
                    self.callbacks.remove(&token);
                }
            }
        }
    }

    /// リスナーの位置・向きを更新する（毎フレーム呼び出す）。
    ///
    /// triple buffer 経由で publish するため、リングバッファ詰まりで失敗しない。
    /// `forward` / `up` はメインスレッドで正規化してから受け渡す。
    pub fn set_listener(&mut self, position: [f32; 3], forward: [f32; 3], up: [f32; 3]) {
        let buf = self.listener_input.input_buffer_mut();
        buf.update(position, forward, up);
        self.listener_input.publish();
    }

    /// ソースの距離減衰パラメータを設定する（初期化・変更時のみ）。
    #[must_use]
    pub fn set_source_spatial_params(
        &mut self,
        id: EntityId,
        model: AttenuationModel,
        min_distance: f32,
        max_distance: f32,
        rolloff: f32,
    ) -> bool {
        self.command_producer
            .try_push(Command::SetSourceSpatialParams {
                id,
                model,
                min_distance,
                max_distance,
                rolloff,
            })
            .is_ok()
    }

    /// ソースの空間演算を有効化・無効化する。
    #[must_use]
    pub fn set_source_spatial_enabled(&mut self, id: EntityId, enabled: bool) -> bool {
        self.command_producer
            .try_push(Command::SetSourceSpatialEnabled { id, enabled })
            .is_ok()
    }

    // ── ライブソース制御 ──

    /// ソースの音量を設定する（spawn 後の動的変更）。
    #[must_use]
    pub fn set_source_volume(&mut self, id: EntityId, vol: f32) -> bool {
        self.command_producer
            .try_push(Command::SetSourceVolume { id, vol })
            .is_ok()
    }

    /// ソースのピッチを設定する（spawn 後の動的変更）。
    #[must_use]
    pub fn set_source_pitch(&mut self, id: EntityId, pitch: f32) -> bool {
        self.command_producer
            .try_push(Command::SetSourcePitch { id, pitch })
            .is_ok()
    }

    /// ソースの再生位置（フレーム単位）をシークする。
    #[must_use]
    pub fn seek_source(&mut self, id: EntityId, frame_offset: f32) -> bool {
        self.command_producer
            .try_push(Command::SeekSource { id, frame_offset })
            .is_ok()
    }

    /// ソースを一時停止する。再生位置は保持される。
    #[must_use]
    pub fn pause_source(&mut self, id: EntityId) -> bool {
        self.command_producer
            .try_push(Command::PauseSource { id })
            .is_ok()
    }

    /// 一時停止中のソースを再開する。
    #[must_use]
    pub fn resume_source(&mut self, id: EntityId) -> bool {
        self.command_producer
            .try_push(Command::ResumeSource { id })
            .is_ok()
    }

    /// ソースを停止する。次の audio callback で despawn される。
    #[must_use]
    pub fn stop_source(&mut self, id: EntityId) -> bool {
        self.command_producer
            .try_push(Command::StopSource { id })
            .is_ok()
    }

    /// 指定秒数だけ遅らせてマスターバスに再生する。
    ///
    /// `delay_seconds` はサウンドスレッドがコマンドを受け取った時点を基準とする。
    /// メインスレッドが累積 tick を知らないため、相対遅延としてサウンドスレッドへ送る。
    /// jitter は最大 1 audio callback ぶん（数 ms）。
    #[must_use]
    pub fn play_delayed(
        &mut self,
        buffer: BufferId,
        vol: f32,
        pitch: f32,
        delay_seconds: f32,
    ) -> bool {
        let Some(index) = self.buffer_pool.resolve(buffer) else {
            return false;
        };
        self.command_producer
            .try_push(Command::PlayScheduled {
                audio_buffer_index: index,
                vol,
                pitch,
                output_bus_dense: 0,
                delay_seconds,
                token: 0,
            })
            .is_ok()
    }

    /// 指定秒数だけ遅らせて指定バスに再生する。
    #[must_use]
    pub fn play_delayed_to_bus(
        &mut self,
        buffer: BufferId,
        vol: f32,
        pitch: f32,
        bus: EntityId,
        delay_seconds: f32,
    ) -> bool {
        let Some(index) = self.buffer_pool.resolve(buffer) else {
            return false;
        };
        let Some(output_bus_dense) = self.bus_routing.resolve_dense(bus) else {
            return false;
        };
        self.command_producer
            .try_push(Command::PlayScheduled {
                audio_buffer_index: index,
                vol,
                pitch,
                output_bus_dense,
                delay_seconds,
                token: 0,
            })
            .is_ok()
    }

    /// 複数ソースの位置を一括更新する（毎フレーム用）。
    ///
    /// triple buffer 経由で publish するため、リングバッファ詰まりで失敗しない。
    /// `MAX_SOURCES` を超える分は切り捨てる（事前確保された容量を超えると
    /// メインスレッド側で realloc が発生し、リアルタイム制約とは関係ないが
    /// alloc コストが上がるため）。
    pub fn batch_set_source_positions(&mut self, updates: &[(EntityId, [f32; 3])]) {
        let buf = self.position_updates_input.input_buffer_mut();
        buf.clear();
        let take = updates.len().min(MAX_SOURCES);
        buf.extend_from_slice(&updates[..take]);
        self.position_updates_input.publish();
    }

    /// マスターバスの EntityId を返す。
    pub fn master_bus(&self) -> EntityId {
        self.bus_routing.master_bus_id
    }

    /// マスターバスに接続されたバスを生成する。
    pub fn create_bus(&mut self, gain: f32) -> Option<EntityId> {
        let master = self.bus_routing.master_bus_id;
        self.create_bus_routed(gain, master)
    }

    /// 指定した親バスに接続されたバスを生成する。
    pub fn create_bus_routed(&mut self, gain: f32, parent: EntityId) -> Option<EntityId> {
        if self.bus_routing.len() >= MAX_BUSES {
            return None;
        }
        let parent_dense = self.bus_routing.resolve_dense(parent)?;

        let new_index = self.bus_routing.next_index;
        self.bus_routing.next_index += 1;

        let new_dense = self.bus_routing.len() as u32;
        self.bus_routing.insert(new_index, parent.index, new_dense);

        let order = self.bus_routing.compute_process_order();

        let new_id = EntityId {
            index: new_index,
            generation: 0,
        };

        if self
            .command_producer
            .try_push(Command::SpawnBus {
                id: new_id,
                gain,
                output_bus_dense: parent_dense,
            })
            .is_err()
        {
            self.bus_routing.remove(new_index);
            self.bus_routing.next_index -= 1;
            return None;
        }

        self.push_process_order(&order);

        Some(new_id)
    }

    /// バスを削除する。マスターバスは削除できない（`false` を返す）。
    pub fn destroy_bus(&mut self, id: EntityId) -> bool {
        if id == self.bus_routing.master_bus_id {
            return false;
        }
        if self.bus_routing.resolve_dense(id).is_none() {
            return false;
        }

        self.bus_routing.remove(id.index);

        let order = self.bus_routing.compute_process_order();

        if self
            .command_producer
            .try_push(Command::DespawnBus { id })
            .is_err()
        {
            return false;
        }
        self.push_process_order(&order);
        true
    }

    /// バスのゲインを設定する。
    #[must_use]
    pub fn set_bus_gain(&mut self, id: EntityId, gain: f32) -> bool {
        self.command_producer
            .try_push(Command::SetBusGain { id, gain })
            .is_ok()
    }

    /// バスのミュートを設定する。
    #[must_use]
    pub fn set_bus_muted(&mut self, id: EntityId, muted: bool) -> bool {
        self.command_producer
            .try_push(Command::SetBusMuted { id, muted })
            .is_ok()
    }

    /// バスの出力先を変更する。ループが検出された場合は `false` を返す。
    #[must_use]
    pub fn set_bus_output(&mut self, id: EntityId, parent: EntityId) -> bool {
        if id == self.bus_routing.master_bus_id {
            return false;
        }
        if self.bus_routing.has_loop(id.index, parent.index) {
            return false;
        }
        let Some(output_bus_dense) = self.bus_routing.resolve_dense(parent) else {
            return false;
        };
        if self.bus_routing.resolve_dense(id).is_none() {
            return false;
        }

        self.bus_routing.set_parent(id.index, parent.index);
        let order = self.bus_routing.compute_process_order();

        if self
            .command_producer
            .try_push(Command::SetBusOutput {
                id,
                output_bus_dense,
            })
            .is_err()
        {
            return false;
        }
        self.push_process_order(&order);
        true
    }

    fn push_process_order(&mut self, order: &[u32]) {
        let mut arr = [0u32; MAX_BUSES];
        let len = order.len().min(MAX_BUSES);
        arr[..len].copy_from_slice(&order[..len]);
        let _ = self.command_producer.try_push(Command::UpdateProcessOrder {
            order: arr,
            len: len as u8,
        });
    }
}
