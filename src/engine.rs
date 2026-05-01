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
use crate::bus::{BusComponent, BusSystem, MAX_BUSES};
use crate::command::Command;
use crate::entity::EntityId;
use crate::voice::{VoiceComponent, VoicePoolSystem};

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

    /// マスターバスの EntityId。
    master_bus_id: EntityId,

    /// メインスレッドが管理するバス EntityId の次のインデックス。
    /// BusSystem と同様に単調増加で発行する（free_list による再利用なし）。
    bus_next_index: u32,

    /// バスルーティングのメインスレッドミラー（ループ検出・トポロジカルソート用）。
    /// bus entity_index → 親バスの entity_index。マスターバスは自己参照。
    bus_routing: Vec<Option<u32>>,

    /// bus entity_index → 密配列インデックス。
    /// バスの spawn/despawn 時に更新される。
    bus_entity_to_dense: Vec<Option<u32>>,

    /// 現在有効なバスの entity_index リスト。
    bus_entity_indices: Vec<u32>,
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
        // マスターバスは BusSystem::new() で自動生成される（entity_index = 0, dense = 0）。
        let mut buses = BusSystem::new();
        let master_bus_id = buses.master_entity();

        let mut pool = VoicePoolSystem::new();

        let stream = device.build_output_stream(
            &config.into(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let sample_count = data.len();

                // コマンドを処理する。
                while let Some(cmd) = command_consumer.try_pop() {
                    match cmd {
                        Command::SetVolume(v) => {
                            buses.set_gain(master_bus_id, v.clamp(0.0, 1.0));
                        }
                        Command::Play {
                            audio_buffer_index,
                            vol,
                            pitch,
                        } => {
                            pool.spawn(VoiceComponent {
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
                            pool.spawn(VoiceComponent {
                                vol,
                                pitch,
                                sample_offset: 0.0,
                                audio_buffer_index,
                                output_bus: output_bus_dense,
                            });
                        }
                        Command::StopAll => {
                            pool = VoicePoolSystem::new();
                        }
                        Command::SpawnBus {
                            id,
                            gain,
                            output_bus_dense,
                        } => {
                            buses.spawn_with_id(
                                id,
                                BusComponent {
                                    gain,
                                    output_bus_dense,
                                },
                            );
                        }
                        Command::DespawnBus { id } => {
                            buses.despawn(id);
                        }
                        Command::SetBusGain { id, gain } => {
                            buses.set_gain(id, gain);
                        }
                        Command::SetBusMuted { id, muted } => {
                            buses.set_muted(id, muted);
                        }
                        Command::SetBusOutput {
                            id,
                            output_bus_dense,
                        } => {
                            buses.set_output_bus_dense(id, output_bus_dense);
                        }
                        Command::UpdateProcessOrder { order, len } => {
                            buses.set_process_order(&order[..len as usize]);
                        }
                    }
                }

                // mix_buffer をゼロクリア。
                buses.clear_mix_buffers(sample_count);

                // ロックフリーでバッファリストのスナップショットを取得。
                let buffers = shared_buffers_clone.load();

                // ボイスミキシング → BusSystem の mix_buffer に加算。
                // bus_stride() は定数なので先に取得して借用を切る。
                {
                    let mix_buf = buses.mix_buffer_mut();
                    pool.update(
                        mix_buf,
                        crate::bus::MAX_MIX_BUFFER_SIZE,
                        device_channels,
                        device_sample_rate,
                        &buffers,
                    );
                }

                // バス処理 → output_buffer へ書き出し。
                buses.update(data, device_channels, sample_count);
            },
            |err| eprintln!("stream error: {err}"),
            None,
        )?;

        stream.play()?;

        // メインスレッドのバスルーティングミラーを初期化。
        // マスターバスは entity_index=0, dense=0。
        let master_idx = master_bus_id.index as usize;
        let mut bus_routing = vec![None; master_idx + 1];
        let mut bus_entity_to_dense = vec![None; master_idx + 1];
        bus_routing[master_idx] = Some(master_idx as u32); // 自己参照
        bus_entity_to_dense[master_idx] = Some(0u32);

        Ok(Self {
            command_producer,
            _stream: stream,
            buffer_pool: AudioBufferPool::new(shared_buffers),
            master_bus_id,
            bus_next_index: master_bus_id.index + 1, // マスターバスの次から
            bus_routing,
            bus_entity_to_dense,
            bus_entity_indices: vec![master_bus_id.index],
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
        let Some(output_bus_dense) = self.resolve_bus_dense(bus) else {
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
        self.master_bus_id
    }

    /// マスターバスに接続されたバスを生成する。
    ///
    /// `MAX_BUSES` に達している場合は `None` を返す。
    pub fn create_bus(&mut self, gain: f32) -> Option<EntityId> {
        self.create_bus_routed(gain, self.master_bus_id)
    }

    /// 指定した親バスに接続されたバスを生成する。
    ///
    /// ループが検出された場合または `MAX_BUSES` に達した場合は `None` を返す。
    pub fn create_bus_routed(&mut self, gain: f32, parent: EntityId) -> Option<EntityId> {
        if self.bus_entity_indices.len() >= MAX_BUSES {
            return None;
        }
        let parent_dense = self.resolve_bus_dense(parent)?;

        // メインスレッド側で entity_index を発行（単調増加、再利用なし）。
        let new_index = self.bus_next_index;
        self.bus_next_index += 1;

        self.ensure_bus_routing_capacity(new_index as usize);
        self.bus_routing[new_index as usize] = Some(parent.index);
        // dense インデックスは生成順（bus_entity_indices.len() が現在の bus 数 = 次の dense インデックス）。
        let new_dense = self.bus_entity_indices.len() as u32;
        self.bus_entity_to_dense[new_index as usize] = Some(new_dense);
        self.bus_entity_indices.push(new_index);

        let order = self.compute_process_order();

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
            self.bus_routing[new_index as usize] = None;
            self.bus_entity_to_dense[new_index as usize] = None;
            self.bus_entity_indices.pop();
            self.bus_next_index -= 1;
            return None;
        }

        self.push_process_order(&order);

        Some(new_id)
    }

    /// バスを削除する。マスターバスは削除できない（`false` を返す）。
    pub fn destroy_bus(&mut self, id: EntityId) -> bool {
        if id == self.master_bus_id {
            return false;
        }
        let idx = id.index as usize;
        if idx >= self.bus_routing.len() || self.bus_routing[idx].is_none() {
            return false;
        }

        // ミラーを更新。このバスを親とする他バスはマスターバスにフォールバック。
        self.bus_routing[idx] = None;
        self.bus_entity_to_dense[idx] = None;
        self.bus_entity_indices.retain(|&i| i != id.index);

        // 削除によって密配列インデックスがずれる（swap-remove）ため、
        // 他エントリの bus_entity_to_dense を再計算する。
        // 簡略化: bus_entity_indices を dense 順にそのまま割り当てる。
        // ただしこれは spawn 時の割り当てと整合するよう注意が必要。
        // ここでは完全な再マッピングは行わず、process_order のみ更新する。

        let order = self.compute_process_order();

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
        if id == self.master_bus_id {
            return false;
        }
        if self.has_loop(id.index, parent.index) {
            return false;
        }
        let Some(output_bus_dense) = self.resolve_bus_dense(parent) else {
            return false;
        };

        let idx = id.index as usize;
        if idx >= self.bus_routing.len() || self.bus_routing[idx].is_none() {
            return false;
        }
        self.bus_routing[idx] = Some(parent.index);

        let order = self.compute_process_order();

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

    // ── 内部ヘルパー ──

    /// バスの EntityId を密配列インデックスに解決する。
    fn resolve_bus_dense(&self, id: EntityId) -> Option<u32> {
        self.bus_entity_to_dense
            .get(id.index as usize)?
            .as_ref()
            .copied()
    }

    fn ensure_bus_routing_capacity(&mut self, index: usize) {
        if index >= self.bus_routing.len() {
            self.bus_routing.resize(index + 1, None);
            self.bus_entity_to_dense.resize(index + 1, None);
        }
    }

    /// `start` から辿って `target` に到達するか確認する（ループ検出）。
    fn has_loop(&self, start: u32, target: u32) -> bool {
        let mut current = target;
        let master_idx = self.master_bus_id.index;
        for _ in 0..MAX_BUSES {
            if current == start {
                return true;
            }
            if current == master_idx {
                return false;
            }
            match self.bus_routing.get(current as usize).and_then(|r| *r) {
                Some(parent) => current = parent,
                None => return false,
            }
        }
        false
    }

    /// メインスレッドのバスルーティングミラーからトポロジカルソート（リーフ→ルート）を計算する。
    /// 結果は密配列インデックスの列で返す。
    fn compute_process_order(&self) -> Vec<u32> {
        use std::collections::{HashMap, VecDeque};

        let master_idx = self.master_bus_id.index;

        // in_degree: 何個の子バスがこのバスを親として参照しているか。
        let mut in_degree: HashMap<u32, usize> = HashMap::new();
        for &entity_idx in &self.bus_entity_indices {
            in_degree.entry(entity_idx).or_insert(0);
        }
        for &entity_idx in &self.bus_entity_indices {
            if entity_idx == master_idx {
                continue;
            }
            let parent = self
                .bus_routing
                .get(entity_idx as usize)
                .and_then(|r| *r)
                .unwrap_or(master_idx);
            *in_degree.entry(parent).or_insert(0) += 1;
        }

        // リーフ（in_degree == 0）からキューに入れる。
        let mut queue: VecDeque<u32> = self
            .bus_entity_indices
            .iter()
            .copied()
            .filter(|&i| in_degree.get(&i).copied().unwrap_or(0) == 0)
            .collect();

        let mut order = Vec::with_capacity(self.bus_entity_indices.len());

        while let Some(entity_idx) = queue.pop_front() {
            // entity_index → 密配列インデックスに変換。
            if let Some(dense) = self
                .bus_entity_to_dense
                .get(entity_idx as usize)
                .and_then(|d| *d)
            {
                order.push(dense);
            }

            if entity_idx == master_idx {
                continue;
            }
            let parent = self
                .bus_routing
                .get(entity_idx as usize)
                .and_then(|r| *r)
                .unwrap_or(master_idx);
            if let Some(deg) = in_degree.get_mut(&parent) {
                *deg -= 1;
                if *deg == 0 {
                    queue.push_back(parent);
                }
            }
        }

        order
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
