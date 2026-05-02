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
use crate::command::{Command, SPATIAL_BATCH_SIZE};
use crate::core::bus_routing::BusRoutingMirror;
use crate::entity::EntityId;
use crate::event::Event;
use crate::source::{SourceComponent, SourceLifecycleSystem, SourceMixingSystem, SourceWorld};
use crate::spatial::{AttenuationModel, SpatialWorld};

/// コマンドリングバッファの容量。
const COMMAND_RING_CAPACITY: usize = 128;

/// イベントリングバッファの容量。
const EVENT_RING_CAPACITY: usize = 64;

/// サウンドエンジン。メインスレッド側で保持し、コマンドを発行する。
pub struct SoundEngine {
    /// コマンドリングバッファのプロデューサ側（メインスレッドが所有）。
    command_producer: ringbuf::HeapProd<Command>,
    /// イベントリングバッファのコンシューマ側（メインスレッドが所有）。
    event_consumer: ringbuf::HeapCons<Event>,
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

        let shared_buffers: Arc<ArcSwap<Vec<Option<Arc<AudioBuffer>>>>> =
            Arc::new(ArcSwap::from_pointee(Vec::new()));
        let shared_buffers_clone = Arc::clone(&shared_buffers);

        let mut bus_world = BusWorld::new();
        let master_bus_id = bus_world.master_entity();

        let mut source_world = SourceWorld::new();
        let mut spatial_world = SpatialWorld::new();

        let stream = device.build_output_stream(
            &config.into(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let sample_count = data.len();

                // コマンドを処理する。
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
                                spatial_world.set_params(dense, model, min_distance, max_distance, rolloff);
                            }
                        }
                        Command::SetSourceSpatialEnabled { id, enabled } => {
                            if let Some(dense) = source_world.resolve(id) {
                                spatial_world.set_enabled(dense, enabled);
                            }
                        }
                        Command::BatchSetSourcePositions { count, updates } => {
                            for (id, pos) in &updates[..count as usize] {
                                if let Some(dense) = source_world.resolve(*id) {
                                    spatial_world.set_position(dense, *pos);
                                }
                            }
                        }
                        Command::SetListener {
                            position,
                            forward,
                            up,
                        } => {
                            spatial_world.listener.update(position, forward, up);
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

    /// バッファをアンロードする。
    pub fn unload(&mut self, id: BufferId) -> bool {
        self.buffer_pool.unload(id)
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
    pub fn spawn_source(&mut self, buffer: BufferId, vol: f32, pitch: f32, bus: EntityId) -> Option<EntityId> {
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
    #[must_use]
    pub fn set_listener(&mut self, position: [f32; 3], forward: [f32; 3], up: [f32; 3]) -> bool {
        self.command_producer
            .try_push(Command::SetListener { position, forward, up })
            .is_ok()
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

    /// 複数ソースの位置を一括更新する（毎フレーム用）。
    ///
    /// `updates` が `SPATIAL_BATCH_SIZE` を超える場合は複数コマンドに分割して送信する。
    #[must_use]
    pub fn batch_set_source_positions(&mut self, updates: &[(EntityId, [f32; 3])]) -> bool {
        let dummy = (EntityId { index: 0, generation: 0 }, [0.0f32; 3]);
        for chunk in updates.chunks(SPATIAL_BATCH_SIZE) {
            let count = chunk.len() as u8;
            let mut buf = [dummy; SPATIAL_BATCH_SIZE];
            buf[..chunk.len()].copy_from_slice(chunk);
            if self
                .command_producer
                .try_push(Command::BatchSetSourcePositions { count, updates: buf })
                .is_err()
            {
                return false;
            }
        }
        true
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
