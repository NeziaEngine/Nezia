//! シナリオ: 数学的に完璧なループ素材でエンジン wrap 動作を検証
//!
//! cargo run --example demo_loop_perfect
//!
//! 整数周期のサイン波 WAV を生成 (`sample[0]` と `sample[N]` が完全一致するよう
//! 周波数とフレーム数を選ぶ) し、ループ再生する。これでクリックが聞こえれば
//! `mix_static` の wrap ロジック自体に問題があるということになる。

use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use nezia::{SoundEngine, SpawnSpatialInit};

const PLAY_SECONDS: u64 = 15;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("╔══════════════════════════════════════════════════╗");
    println!("║   demo_loop_perfect: 完璧ループでエンジン検証    ║");
    println!("╚══════════════════════════════════════════════════╝");

    // 440 Hz @ 44100 Hz で 100 周期分 = 10025 samples
    // (44100 / 440 = 100.227... なので、10025 samples で整数 100 周期 ぴったりではないが、
    //  loop_frames = 44100 として 1 秒丸ごと使うと 440 周期分 → 完全一致)
    let sample_rate = 44100;
    let freq = 440.0_f32;
    // freq * loop_frames / sample_rate が整数になるよう調整。
    // 1 秒 = 440 周期 (整数) なので loop_frames = 44100 が完璧。
    let loop_frames = sample_rate as usize;

    let wav_path = std::env::temp_dir().join("nezia_loop_perfect.wav");
    write_sine_wav(&wav_path, freq, sample_rate, loop_frames)?;
    println!(
        "\n  生成: {} ({} frames, {:.3}s, {} Hz × {} 周期)",
        wav_path.display(),
        loop_frames,
        loop_frames as f32 / sample_rate as f32,
        freq,
        (freq * loop_frames as f32 / sample_rate as f32) as i32,
    );

    let mut engine = SoundEngine::new()?;
    let buf = engine.load(&wav_path)?;

    let reader = engine.open_buffer_reader(buf).expect("reader");
    let total = reader.total_frames();
    let ch = reader.channels() as usize;
    println!("  decoded_frames: {total}");

    // 端点確認
    let mut head = vec![0.0f32; ch];
    let mut tail = vec![0.0f32; ch];
    reader.read_frames(0, &mut head);
    reader.read_frames(total - 1, &mut tail);
    println!("  sample[0]   : {:+.6} (左ch)", head[0]);
    println!("  sample[N-1] : {:+.6} (左ch)", tail[0]);
    println!("  ▶ sample[N-1] と sample[0] が極めて近ければ wrap は数学的に滑らか");

    // ─── ループ再生 ──────────────────────────────────────
    let master = engine.master_bus();
    let id = engine
        .play_with_handle(buf, 0.5, 1.0, master, true, 128, SpawnSpatialInit::NONE)
        .expect("play");

    println!(
        "\n  ▶ {PLAY_SECONDS}s ループ再生 (1 秒周期で wrap)。クリックが聞こえなければ engine OK"
    );

    let deadline = Instant::now() + Duration::from_secs(PLAY_SECONDS);
    while Instant::now() < deadline {
        engine.poll_events();
        thread::sleep(Duration::from_millis(50));
    }

    let _ = engine.stop_source(id);
    thread::sleep(Duration::from_millis(200));
    let _ = engine.unload(buf);
    let _ = std::fs::remove_file(&wav_path);

    println!("\n✓ 完了\n");
    Ok(())
}

/// 整数周期サイン波 (ステレオ 16-bit PCM) を WAV で書き出す。
fn write_sine_wav(
    path: &PathBuf,
    freq: f32,
    sample_rate: u32,
    frames: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write as _;
    let channels: u16 = 2;
    let bps: u16 = 16;
    let data_size = frames as u32 * channels as u32 * (bps / 8) as u32;
    let block_align = channels * bps / 8;
    let mut f = std::fs::File::create(path)?;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_size).to_le_bytes())?;
    f.write_all(b"WAVE")?;
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&channels.to_le_bytes())?;
    f.write_all(&sample_rate.to_le_bytes())?;
    f.write_all(&(sample_rate * block_align as u32).to_le_bytes())?;
    f.write_all(&block_align.to_le_bytes())?;
    f.write_all(&bps.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_size.to_le_bytes())?;
    let step = 2.0 * std::f32::consts::PI * freq / sample_rate as f32;
    for i in 0..frames {
        // フェード一切なし。整数周期なので sample[frames] = sample[0] が成立。
        let s = ((step * i as f32).sin() * 0.5 * i16::MAX as f32) as i16;
        f.write_all(&s.to_le_bytes())?;
        f.write_all(&s.to_le_bytes())?;
    }
    Ok(())
}
