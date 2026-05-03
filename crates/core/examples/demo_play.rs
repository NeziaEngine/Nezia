//! シナリオ: 音の再生
//!
//! cargo run --example demo_play
//!
//! カバー範囲:
//!   - load / unload
//!   - play (fire-and-forget): 音量・ピッチ変化
//!   - set_volume (マスター音量)
//!   - stop_all
//!   - エラーパス (アンロード済みバッファ, 二重アンロード)

use std::thread;
use std::time::Duration;

use nezia::SoundEngine;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("╔══════════════════════════════════════╗");
    println!("║         demo_play: 音の再生          ║");
    println!("╚══════════════════════════════════════╝");

    let wav = gen_wav(440.0, 44100, 3.0)?;
    let mut engine = SoundEngine::new()?;
    ok("SoundEngine 起動");

    // ─── バッファロード ──────────────────────────────────
    section("バッファロード");

    let buf = engine.load(&wav)?;
    ok(format!(
        "load() => index={} gen={}",
        buf.index, buf.generation
    ));

    // ─── 通常再生 ────────────────────────────────────────
    section("fire-and-forget 再生");
    println!("  ▶ 440 Hz のサイン波が聴こえるはずです");

    check(
        "play(vol=1.0, pitch=1.0)",
        engine.play(buf, 1.0, 1.0, false),
    );
    thread::sleep(Duration::from_millis(1600));
    let _ = engine.stop_all();
    thread::sleep(Duration::from_millis(600));

    // ─── ピッチ変化 ──────────────────────────────────────
    section("ピッチ変化");
    println!("  ▶ 低い音 → 原音 → 高い音 の順で聴こえるはずです");

    for (label, pitch) in [
        ("1オクターブ下 (pitch=0.5)", 0.5_f32),
        ("原音        (pitch=1.0)", 1.0),
        ("1オクターブ上 (pitch=2.0)", 2.0),
    ] {
        println!("  ▶ {label}");
        check(
            format!("  play(pitch={pitch})"),
            engine.play(buf, 1.0, pitch, false),
        );
        thread::sleep(Duration::from_millis(1600));
        let _ = engine.stop_all();
        thread::sleep(Duration::from_millis(600));
    }

    // ─── マスター音量 ────────────────────────────────────
    section("マスター音量 (set_volume)");
    println!("  ▶ 大 → 中 → 小 → 無音 → 大 の順で変化するはずです");

    for (vol, label) in [
        (1.0_f32, "全開"),
        (0.5, "半分"),
        (0.1, "かすか"),
        (0.0, "無音"),
        (1.0, "全開に戻す"),
    ] {
        check(
            format!("  set_volume({vol:.1})  [{label}]"),
            engine.set_volume(vol),
        );
        let _ = engine.play(buf, 1.0, 1.0, false);
        thread::sleep(Duration::from_millis(1400));
        let _ = engine.stop_all();
        thread::sleep(Duration::from_millis(600));
    }

    // ─── 同時再生 ────────────────────────────────────────
    section("同時再生 (3ボイス重ね)");
    println!("  ▶ ピッチの異なる3音が重なった和音が聴こえるはずです");

    let _ = engine.play(buf, 0.6, 1.0, false);
    thread::sleep(Duration::from_millis(200));
    let _ = engine.play(buf, 0.4, 1.5, false);
    thread::sleep(Duration::from_millis(200));
    let _ = engine.play(buf, 0.3, 0.75, false);
    ok("3ボイス同時再生中...");
    thread::sleep(Duration::from_millis(3000));
    let _ = engine.stop_all();
    thread::sleep(Duration::from_millis(600));

    // ─── コールバック付き再生 ────────────────────────────
    section("コールバック付き再生 (play_with_callback)");
    println!("  ▶ 再生が自然終了したときにコールバックが呼ばれます");

    let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let finished_clone = std::sync::Arc::clone(&finished);
    check(
        "play_with_callback(vol=1.0, pitch=1.0)",
        engine.play_with_callback(buf, 1.0, 1.0, false, move || {
            finished_clone.store(true, std::sync::atomic::Ordering::Relaxed);
        }),
    );
    println!("  ... 自然終了を待機中 (最大 8s)");

    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    while std::time::Instant::now() < deadline {
        engine.poll_events();
        if finished.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    if finished.load(std::sync::atomic::Ordering::Relaxed) {
        ok("SourceFinished コールバック受信");
    } else {
        panic!("コールバックが 4s 以内に呼ばれなかった");
    }
    thread::sleep(Duration::from_millis(600));

    println!("  ▶ stop_all() で中断した場合はコールバックは呼ばれません");
    let not_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let not_called_clone = std::sync::Arc::clone(&not_called);
    check(
        "play_with_callback → stop_all() で中断",
        engine.play_with_callback(buf, 1.0, 1.0, false, move || {
            not_called_clone.store(true, std::sync::atomic::Ordering::Relaxed);
        }),
    );
    thread::sleep(Duration::from_millis(400));
    let _ = engine.stop_all();
    thread::sleep(Duration::from_millis(400));
    engine.poll_events();
    if not_called.load(std::sync::atomic::Ordering::Relaxed) {
        panic!("stop_all() 後にコールバックが呼ばれた");
    } else {
        ok("stop_all() 後: コールバック呼び出しなし (正常)");
    }
    thread::sleep(Duration::from_millis(600));

    // ─── エラーパス ──────────────────────────────────────
    section("エラーパス");

    check("unload(buf)", engine.unload(buf));
    check_false(
        "unload(buf) 2回目 => false (二重アンロード)",
        engine.unload(buf),
    );
    check_false(
        "play(unloaded buf) => false",
        engine.play(buf, 1.0, 1.0, false),
    );

    // ─── 完了 ────────────────────────────────────────────
    let _ = std::fs::remove_file(&wav);
    println!("\n✓ demo_play: 全チェック通過\n");
    Ok(())
}

// ─── ユーティリティ ──────────────────────────────────────────────────────────

fn section(name: &str) {
    println!("\n━━━ {name}");
}

fn ok(msg: impl std::fmt::Display) {
    println!("  [OK ] {msg}");
}

fn check(msg: impl std::fmt::Display, result: bool) {
    if result {
        println!("  [OK ] {msg}");
    } else {
        eprintln!("  [FAIL] {msg}");
        panic!("check failed");
    }
}

fn check_false(msg: impl std::fmt::Display, result: bool) {
    if !result {
        println!("  [OK ] {msg}");
    } else {
        eprintln!("  [FAIL] {msg} (expected false)");
        panic!("check_false failed");
    }
}

/// ステレオ 16-bit PCM WAV (サイン波) を一時ファイルへ書き出す。
fn gen_wav(
    freq: f32,
    sample_rate: u32,
    secs: f32,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    use std::io::Write as _;
    let path = std::env::temp_dir().join("nezia_demo_play.wav");
    let channels: u16 = 2;
    let bps: u16 = 16;
    let n = (sample_rate as f32 * secs) as u32;
    let data_size = n * channels as u32 * (bps / 8) as u32;
    let block_align = channels * bps / 8;
    let mut f = std::fs::File::create(&path)?;
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
    let attack = (sample_rate as f32 * 0.01) as u32;
    let release = (sample_rate as f32 * 0.05) as u32;
    for i in 0..n {
        let env = if i < attack {
            i as f32 / attack as f32
        } else if i >= n - release {
            (n - i) as f32 / release as f32
        } else {
            1.0
        };
        let s = ((step * i as f32).sin() * env * 0.5 * i16::MAX as f32) as i16;
        f.write_all(&s.to_le_bytes())?;
        f.write_all(&s.to_le_bytes())?;
    }
    Ok(path)
}
