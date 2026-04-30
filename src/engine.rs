use std::f32::consts::TAU;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::Stream;
use ringbuf::{
    HeapRb,
    traits::{Consumer, Producer, Split},
};

use crate::command::Command;

/// コマンドリングバッファの容量。
/// メインスレッドが1フレームに発行するコマンド数より十分大きくとる。
const COMMAND_RING_CAPACITY: usize = 64;

/// サウンドエンジン。メインスレッド側で保持し、コマンドを発行する。
pub struct SoundEngine {
    /// コマンドリングバッファのプロデューサ側（メインスレッドが所有）。
    command_producer: ringbuf::HeapProd<Command>,
    /// cpal のストリームハンドル。Drop 時に再生が停止される。
    _stream: Stream,
}

impl SoundEngine {
    /// サウンドエンジンを初期化し、オーディオ再生を開始する。
    ///
    /// 440 Hz のサイン波を既定の出力デバイスに出力する。
    /// 音量はコマンド経由で変更可能。
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

        let sample_rate = config.sample_rate() as f32;
        let channels = config.channels() as usize;

        // コマンドリングバッファを生成し、プロデューサとコンシューマに分割する。
        // プロデューサはメインスレッド、コンシューマはサウンドスレッドが所有する。
        let ring = HeapRb::<Command>::new(COMMAND_RING_CAPACITY);
        let (command_producer, mut command_consumer) = ring.split();

        let frequency = 440.0_f32;
        let mut volume = 0.2_f32;
        let mut phase = 0.0_f32;
        let phase_increment = frequency / sample_rate;

        let stream = device.build_output_stream(
            &config.into(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                // リングバッファからコマンドを drain して状態を更新する。
                // lock-free なのでサウンドスレッドの制約に違反しない。
                while let Some(cmd) = command_consumer.try_pop() {
                    match cmd {
                        Command::SetVolume(v) => {
                            volume = v.clamp(0.0, 1.0);
                        }
                    }
                }

                for frame in data.chunks_mut(channels) {
                    let sample = (phase * TAU).sin() * volume;
                    phase += phase_increment;
                    if phase >= 1.0 {
                        phase -= 1.0;
                    }
                    for s in frame.iter_mut() {
                        *s = sample;
                    }
                }
            },
            |err| eprintln!("stream error: {err}"),
            None,
        )?;

        stream.play()?;

        Ok(Self {
            command_producer,
            _stream: stream,
        })
    }

    /// 音量を設定する（0.0〜1.0）。
    ///
    /// コマンドをリングバッファに書き込む。リングバッファが満杯の場合は
    /// コマンドが破棄され `false` を返す。
    #[must_use]
    pub fn set_volume(&mut self, volume: f32) -> bool {
        self.command_producer
            .try_push(Command::SetVolume(volume))
            .is_ok()
    }
}
