use std::path::Path;
use std::sync::Arc;

use cpal::Stream;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::{
    HeapRb,
    traits::{Consumer, Producer, Split},
};

use crate::audio::{self, AudioBuffer};
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
    /// ロード済みの AudioBuffer 一覧。Arc でサウンドスレッドと共有。
    buffers: Vec<Arc<AudioBuffer>>,
    /// サウンドスレッドと共有するバッファリスト。
    shared_buffers: Arc<std::sync::RwLock<Vec<Arc<AudioBuffer>>>>,
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

        let shared_buffers: Arc<std::sync::RwLock<Vec<Arc<AudioBuffer>>>> =
            Arc::new(std::sync::RwLock::new(Vec::new()));
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

                // RwLock の読み取りロックを取得。
                // サウンドスレッドでのロック取得は本来避けるべきだが、
                // 書き込みはバッファ追加時のみで競合は稀。
                // 将来的にはロックフリーな仕組みに置き換える。
                let buffers = match shared_buffers_clone.read() {
                    Ok(b) => b,
                    Err(_) => return,
                };

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
            buffers: Vec::new(),
            shared_buffers,
        })
    }

    /// オーディオファイルをロードし、バッファインデックスを返す。
    ///
    /// 返されたインデックスを `play()` に渡して再生する。
    pub fn load<P: AsRef<Path>>(&mut self, path: P) -> Result<u32, Box<dyn std::error::Error>> {
        let buffer = Arc::new(audio::load(path)?);
        let index = self.buffers.len() as u32;
        self.buffers.push(Arc::clone(&buffer));

        let mut shared = self
            .shared_buffers
            .write()
            .map_err(|e| format!("lock poisoned: {e}"))?;
        shared.push(buffer);

        Ok(index)
    }

    /// ボイスを再生する。
    ///
    /// `audio_buffer_index` は `load()` で返されたインデックス。
    #[must_use]
    pub fn play(&mut self, audio_buffer_index: u32, vol: f32, pitch: f32) -> bool {
        self.command_producer
            .try_push(Command::Play {
                audio_buffer_index,
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
