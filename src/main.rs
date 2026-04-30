use std::f32::consts::TAU;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

fn main() {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .expect("no output device available");

    let config = device
        .default_output_config()
        .expect("failed to get default output config");

    let device_name = device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    println!("Output device: {device_name}");
    println!("Sample format: {:?}", config.sample_format());
    println!("Sample rate: {}", config.sample_rate());
    println!("Channels: {}", config.channels());

    let sample_rate = config.sample_rate() as f32;
    let channels = config.channels() as usize;
    let frequency = 440.0_f32; // A4
    let volume = 0.2_f32;

    let mut phase = 0.0_f32;
    let phase_increment = frequency / sample_rate;

    let stream = device
        .build_output_stream(
            &config.into(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
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
        )
        .expect("failed to build output stream");

    stream.play().expect("failed to play stream");

    println!("Playing 440 Hz sine wave. Press Enter to stop.");
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .expect("failed to read line");
}
