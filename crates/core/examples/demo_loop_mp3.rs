//! シナリオ: MP3 ループ再生のデバッグ
//!
//! cargo run --example demo_loop_mp3
//!
//! `crates/core/sandbox/ambience-ocean-rough-wave-loop.mp3` をループ再生し、
//! ループ境界でクリック音 (ぶつっ) が発生していないかを耳で確認する。
//! priming/padding trim 修正の効果検証用デモ。
//!
//! 併せて、デコード後の総フレーム数と先頭/末尾サンプル数フレームをダンプし、
//! 無音区間 (priming/padding 残り) が残っていないかを観測できるようにする。

use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use nezia::{SoundEngine, peek_metadata};

const MP3_RELATIVE: &str = "crates/core/sandbox/ambience-ocean-rough-wave-loop.mp3";
const PLAY_SECONDS: u64 = 30;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("╔══════════════════════════════════════════════════╗");
    println!("║   demo_loop_mp3: MP3 ループ再生デバッグ          ║");
    println!("╚══════════════════════════════════════════════════╝");

    // ─── ファイル解決 ────────────────────────────────────
    let path = resolve_mp3_path()?;
    println!("\n  MP3: {}", path.display());

    // ─── メタデータ (デコード前) ─────────────────────────
    let bytes = std::fs::read(&path)?;
    let meta = peek_metadata(&bytes)?;
    section("メタデータ (peek_metadata)");
    println!("  sample_rate : {} Hz", meta.sample_rate);
    println!("  channels    : {}", meta.channels);
    println!(
        "  total_frames: {} ({:.3} s)",
        meta.total_frames,
        meta.total_frames as f32 / meta.sample_rate.max(1) as f32
    );

    // ─── ロード (priming/padding trim 適用後) ────────────
    let mut engine = SoundEngine::new()?;
    ok("SoundEngine 起動");

    let buf = engine.load(&path)?;
    ok(format!(
        "load() => index={} gen={}",
        buf.index, buf.generation
    ));

    let reader = engine.open_buffer_reader(buf).expect("buffer reader");
    let total_frames = reader.total_frames();
    let channels = reader.channels() as usize;
    println!(
        "  decoded_frames (after trim): {} ({:.3} s)",
        total_frames,
        total_frames as f32 / reader.sample_rate().max(1) as f32
    );

    let trimmed_off = meta.total_frames as i64 - total_frames as i64;
    println!(
        "  priming+padding trim 推定値 : {} frames",
        trimmed_off.max(0)
    );

    // ─── 先頭/末尾サンプル観察 ───────────────────────────
    section("先頭/末尾サンプル (絶対値)");
    let head_frames = 8usize;
    let tail_frames = 8usize;
    let mut head = vec![0.0f32; head_frames * channels];
    reader.read_frames(0, &mut head);
    print!("  head[0..8] :");
    for f in 0..head_frames {
        let mut peak = 0.0f32;
        for c in 0..channels {
            peak = peak.max(head[f * channels + c].abs());
        }
        print!(" {peak:.6}");
    }
    println!();

    let mut tail = vec![0.0f32; tail_frames * channels];
    let tail_start = total_frames.saturating_sub(tail_frames);
    reader.read_frames(tail_start, &mut tail);
    print!("  tail[-8..] :");
    for f in 0..tail_frames {
        let mut peak = 0.0f32;
        for c in 0..channels {
            peak = peak.max(tail[f * channels + c].abs());
        }
        print!(" {peak:.6}");
    }
    println!();
    println!("  ▶ どちらも 0.0000 が並んでいたら trim 漏れ (= ループでクリック発生)");

    // ─── ループ再生 ──────────────────────────────────────
    section(&format!("ループ再生 ({PLAY_SECONDS}s)"));
    println!(
        "  ▶ ループ境界 ({:.3}s 周期) でクリック音が出るかを聴く",
        { total_frames as f32 / reader.sample_rate().max(1) as f32 }
    );

    let master = engine.master_bus();
    let id = engine
        .play_with_handle(buf, 1.0, 1.0, master, true)
        .expect("play_with_handle should succeed");
    ok(format!("play_with_handle(looping=true) => entity={id:?}"));

    let deadline = Instant::now() + Duration::from_secs(PLAY_SECONDS);
    let loop_period =
        Duration::from_secs_f32(total_frames as f32 / reader.sample_rate().max(1) as f32);
    let mut next_mark = Instant::now() + loop_period;
    while Instant::now() < deadline {
        engine.poll_events();
        if Instant::now() >= next_mark {
            println!("  ▶ ~ループ境界通過");
            next_mark += loop_period;
        }
        thread::sleep(Duration::from_millis(20));
    }

    let _ = engine.stop_source(id);
    thread::sleep(Duration::from_millis(200));
    let _ = engine.unload(buf);

    println!("\n✓ demo_loop_mp3: 完了\n");
    Ok(())
}

fn resolve_mp3_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    // workspace ルートからの相対 / crates/core からの相対 のどちらでも動くようにする。
    let candidates = [
        PathBuf::from(MP3_RELATIVE),
        PathBuf::from("../../").join(MP3_RELATIVE),
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join(MP3_RELATIVE),
    ];
    for p in candidates {
        if p.exists() {
            return Ok(p);
        }
    }
    Err(format!("MP3 が見つかりません: {MP3_RELATIVE}").into())
}

fn section(name: &str) {
    println!("\n━━━ {name}");
}

fn ok(msg: impl std::fmt::Display) {
    println!("  [OK ] {msg}");
}
