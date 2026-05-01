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
use crate::command::Command;
use crate::voice::{VoiceComponent, VoicePoolSystem};

/// コマンドリングバッファの容量。
/// メインスレッドが1フレームに発行するコマンド数より十分大きくとる。
const COMMAND_RING_CAPACITY: usize = 64;

/// サウンドエンジン。メインスレッド側で保持し、コマンドを発行する。
pub struct SoundEngine {
    /// コマンドリングバッファのプロデューサ側（メインスレッドが所有）。
    command_producer: ringbuf::HeapProd<Command>,
    /// cpal のストリームハンドル。Drop 時に再生が停止される。
    _stream: Stream,
    /// AudioBuffer のスロット管理。
    buffer_pool: AudioBufferPool,
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

        let mut master_volume = 1.0_f32;
        let mut pool = VoicePoolSystem::new();

        let stream = device.build_output_stream(
            &config.into(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                // コマンドを処理する。
                while let Some(cmd) = command_consumer.try_pop() {
                    match cmd {
                        Command::SetVolume(v) => {
                            master_volume = v.clamp(0.0, 1.0);
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
                            });
                        }
                        Command::StopAll => {
                            pool = VoicePoolSystem::new();
                        }
                    }
                }

                // 出力バッファをゼロクリア。
                for sample in data.iter_mut() {
                    *sample = 0.0;
                }

                // ロックフリーでバッファリストのスナップショットを取得。
                let buffers = shared_buffers_clone.load();

                pool.update(
                    data,
                    device_channels,
                    device_sample_rate,
                    master_volume,
                    &buffers,
                );
            },
            |err| eprintln!("stream error: {err}"),
            None,
        )?;

        stream.play()?;

        Ok(Self {
            command_producer,
            _stream: stream,
            buffer_pool: AudioBufferPool::new(shared_buffers),
        })
    }

    /// オーディオファイルをロードし、ハンドルを返す。
    ///
    /// 返されたハンドルを `play()` や `unload()` に渡す。
    pub fn load<P: AsRef<Path>>(
        &mut self,
        path: P,
    ) -> Result<BufferId, Box<dyn std::error::Error>> {
        self.buffer_pool.load(path)
    }

    /// バッファをアンロードする。
    ///
    /// 再生中のボイスがこのバッファを参照していた場合、
    /// 次の update で自動的に despawn される。
    pub fn unload(&mut self, id: BufferId) -> bool {
        self.buffer_pool.unload(id)
    }

    /// ボイスを再生する。
    ///
    /// `buffer` は `load()` で返されたハンドル。
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

    /// マスター音量を設定する（0.0〜1.0）。
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
}
