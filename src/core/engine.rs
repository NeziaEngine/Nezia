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
use crate::source::{SourceComponent, SourceSystem, SourceWorld};

/// コマンドリングバッファの容量。
const COMMAND_RING_CAPACITY: usize = 128;

/// サウンドエンジン。メインスレッド側で保持し、コマンドを発行する。
pub struct SoundEngine {
    /// コマンドリングバッファのプロデューサ側（メインスレッドが所有）。
    command_producer: ringbuf::HeapProd<Command>,
    /// cpal のストリームハンドル。Drop 時に再生が停止される。
    _stream: Stream,
    /// AudioBuffer のスロット管理。
    buffer_pool: AudioBufferPool,
    /// バスルーティングのメインスレッドミラー（ループ検出・トポロジカルソート用）。
    bus_routing: BusRoutingMirror,
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

        let shared_buffers: Arc<ArcSwap<Vec<Option<Arc<AudioBuffer>>>>> =
            Arc::new(ArcSwap::from_pointee(Vec::new()));
        let shared_buffers_clone = Arc::clone(&shared_buffers);

        // BusSystem をオーディオコールバック前に初期化する。
        // マスターバスは BusWorld::new() で自動生成される（entity_index = 0, dense = 0）。
        let mut bus_world = BusWorld::new();
        let master_bus_id = bus_world.master_entity();

        let mut source_world = SourceWorld::new();

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
                        } => {
                            source_world.spawn(SourceComponent {
                                vol,
                                pitch,
                                sample_offset: 0.0,
                                audio_buffer_index,
                                output_bus: 0,
                            });
                        }
                        Command::PlayToBus {
                            audio_buffer_index,
                            vol,
                            pitch,
                            output_bus_dense,
                        } => {
                            source_world.spawn(SourceComponent {
                                vol,
                                pitch,
                                sample_offset: 0.0,
                                audio_buffer_index,
                                output_bus: output_bus_dense,
                            });
                        }
                        Command::StopAll => {
                            source_world = SourceWorld::new();
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
                    }
                }

                // mix_buffer をゼロクリア。
                bus_world.clear_mix_buffers(sample_count);

                // ロックフリーでバッファリストのスナップショットを取得。
                let buffers = shared_buffers_clone.load();

                // Source ミキシング → BusWorld の mix_buffer に加算。
                // bus_stride() は定数なので先に取得して借用を切る。
                {
                    let mix_buf = bus_world.mix_buffer_mut();
                    SourceSystem::update(
                        &mut source_world,
                        mix_buf,
                        crate::bus::MAX_MIX_BUFFER_SIZE,
                        sample_count,
                        device_channels,
                        device_sample_rate,
                        &buffers,
                    );
                }

                // バス処理 → output_buffer へ書き出し。
                BusSystem::update(&mut bus_world, data, device_channels, sample_count);
            },
            |err| eprintln!("stream error: {err}"),
            None,
        )?;

        stream.play()?;

        Ok(Self {
            command_producer,
            _stream: stream,
            buffer_pool: AudioBufferPool::new(shared_buffers),
            bus_routing: BusRoutingMirror::new(master_bus_id),
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

    /// ボイスをマスターバスに再生する。
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
            })
            .is_ok()
    }

    /// ボイスを指定バスに再生する。
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
            })
            .is_ok()
    }

    /// マスター音量を設定する（0.0〜1.0）。マスターバスの gain を変更する。
    #[must_use]
    pub fn set_volume(&mut self, volume: f32) -> bool {
        self.command_producer
            .try_push(Command::SetVolume(volume))
            .is_ok()
    }

    /// すべてのボイスを停止する。
    #[must_use]
    pub fn stop_all(&mut self) -> bool {
        self.command_producer.try_push(Command::StopAll).is_ok()
    }

    /// マスターバスの EntityId を返す。
    pub fn master_bus(&self) -> EntityId {
        self.bus_routing.master_bus_id
    }

    /// マスターバスに接続されたバスを生成する。
    ///
    /// `MAX_BUSES` に達している場合は `None` を返す。
    pub fn create_bus(&mut self, gain: f32) -> Option<EntityId> {
        let master = self.bus_routing.master_bus_id;
        self.create_bus_routed(gain, master)
    }

    /// 指定した親バスに接続されたバスを生成する。
    ///
    /// ループが検出された場合または `MAX_BUSES` に達した場合は `None` を返す。
    pub fn create_bus_routed(&mut self, gain: f32, parent: EntityId) -> Option<EntityId> {
        if self.bus_routing.len() >= MAX_BUSES {
            return None;
        }
        let parent_dense = self.bus_routing.resolve_dense(parent)?;

        // メインスレッド側で entity_index を発行（単調増加、再利用なし）。
        let new_index = self.bus_routing.next_index;
        self.bus_routing.next_index += 1;

        // dense インデックスは生成順（len() が現在の bus 数 = 次の dense インデックス）。
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
            // コマンド送信失敗時はミラーを元に戻す。
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

    /// process_order を UpdateProcessOrder コマンドとして送信する。
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
