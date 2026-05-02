//! シナリオ: バス階層
//!
//! cargo run --example demo_bus
//!
//! カバー範囲:
//!   - create_bus / create_bus_routed / destroy_bus
//!   - play_to_bus: 各バスへの再生
//!   - set_bus_gain: バス音量
//!   - set_bus_muted: ミュート
//!   - set_bus_output: ルーティング変更
//!   - エラーパス (ループ検出, master 削除保護)

use std::thread;
use std::time::Duration;

use nezia::SoundEngine;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("╔══════════════════════════════════════╗");
    println!("║        demo_bus: バス階層            ║");
    println!("╚══════════════════════════════════════╝");

    let wav = gen_wav(440.0, 44100, 3.0)?;
    let mut engine = SoundEngine::new()?;
    let buf = engine.load(&wav)?;
    ok("SoundEngine 起動 / バッファロード");

    let master = engine.master_bus();

    // ─── バス作成 ────────────────────────────────────────
    section("バス作成");

    //  master
    //  ├─ sfx_bus   (gain 0.8)
    //  │   └─ sub_bus (gain 1.0)
    //  └─ music_bus (gain 0.6)

    let sfx_bus   = engine.create_bus(0.8).expect("create_bus(sfx)");
    let music_bus = engine.create_bus(0.6).expect("create_bus(music)");
    let sub_bus   = engine.create_bus_routed(1.0, sfx_bus).expect("create_bus_routed");

    println!("  master");
    println!("  ├─ sfx_bus   (gain 0.8) {:?}", sfx_bus);
    println!("  │   └─ sub_bus (gain 1.0) {:?}", sub_bus);
    println!("  └─ music_bus (gain 0.6) {:?}", music_bus);

    // ─── 各バスへの再生 ──────────────────────────────────
    section("各バスへの play_to_bus");
    println!("  ▶ どのバスに送っても音量が異なるはずです");
    println!("    master=1.0 / sfx=0.8 / music=0.6 / sub=0.8×1.0=0.8");

    for (label, expected_vol, bus) in [
        ("master   (vol=1.0 そのまま)",  "1.0",       master),
        ("sfx_bus  (gain=0.8 で減衰)",   "0.8",       sfx_bus),
        ("music_bus(gain=0.6 で減衰)",   "0.6",       music_bus),
        ("sub_bus  (sfx 0.8 × sub 1.0)", "0.8", sub_bus),
    ] {
        println!("\n  ▶ {label}  → 期待音量: {expected_vol}");
        check(format!("  play_to_bus({label})"), engine.play_to_bus(buf, 1.0, 1.0, bus));
        thread::sleep(Duration::from_millis(1600));
        let _ = engine.stop_all();
        thread::sleep(Duration::from_millis(600));
    }

    // ─── バスゲイン ──────────────────────────────────────
    section("set_bus_gain (sfx_bus を段階的に下げる)");
    println!("  ▶ 音がだんだん小さくなり、最後に戻るはずです");

    for (gain, label) in [
        (0.8_f32, "通常"),
        (0.4,     "半分"),
        (0.1,     "かすか"),
        (0.8,     "通常に戻す"),
    ] {
        println!("\n  ▶ sfx_bus gain={gain:.1}  [{label}]");
        let _ = engine.set_bus_gain(sfx_bus, gain);
        let _ = engine.play_to_bus(buf, 1.0, 1.0, sfx_bus);
        thread::sleep(Duration::from_millis(1600));
        let _ = engine.stop_all();
        thread::sleep(Duration::from_millis(600));
    }

    // ─── ミュート ────────────────────────────────────────
    section("set_bus_muted");

    println!("\n  ▶ sfx_bus をミュート → 無音になるはずです");
    let _ = engine.set_bus_muted(sfx_bus, true);
    let _ = engine.play_to_bus(buf, 1.0, 1.0, sfx_bus);
    thread::sleep(Duration::from_millis(1600));
    let _ = engine.stop_all();
    thread::sleep(Duration::from_millis(600));

    println!("\n  ▶ sfx_bus のミュートを解除 → 聴こえるはずです");
    let _ = engine.set_bus_muted(sfx_bus, false);
    let _ = engine.play_to_bus(buf, 1.0, 1.0, sfx_bus);
    thread::sleep(Duration::from_millis(1600));
    let _ = engine.stop_all();
    thread::sleep(Duration::from_millis(600));

    // ─── ルーティング変更 ────────────────────────────────
    section("set_bus_output (ルーティング変更)");

    // sub_bus を sfx_bus → music_bus へ付け替え
    check("set_bus_output(sub_bus → music_bus)", engine.set_bus_output(sub_bus, music_bus));
    println!("  ツリー変更後:");
    println!("  master");
    println!("  ├─ sfx_bus");
    println!("  └─ music_bus (gain 0.6)");
    println!("      └─ sub_bus");

    println!("\n  ▶ music_bus をミュート → sub_bus の音も消えるはずです");
    let _ = engine.set_bus_muted(music_bus, true);
    let _ = engine.play_to_bus(buf, 1.0, 1.0, sub_bus);
    thread::sleep(Duration::from_millis(1600));
    let _ = engine.set_bus_muted(music_bus, false);
    let _ = engine.stop_all();
    thread::sleep(Duration::from_millis(600));

    // ─── エラーパス ──────────────────────────────────────
    section("エラーパス (これらは失敗するのが正しい)");

    check_false("set_bus_output(master, ...) → master の出力先変更は不可", engine.set_bus_output(master, sfx_bus));
    check_false(
        "set_bus_output(music_bus, sub_bus) → 閉路検出 (music→sub→music)",
        engine.set_bus_output(music_bus, sub_bus),
    );
    check_false("destroy_bus(master) → master は削除不可", engine.destroy_bus(master));

    // ─── バス削除 ────────────────────────────────────────
    section("destroy_bus");

    check("destroy_bus(sub_bus)",   engine.destroy_bus(sub_bus));
    check_false("play_to_bus(削除済み sub_bus) → false", engine.play_to_bus(buf, 1.0, 1.0, sub_bus));
    check("destroy_bus(sfx_bus)",   engine.destroy_bus(sfx_bus));
    check("destroy_bus(music_bus)", engine.destroy_bus(music_bus));

    // ─── 完了 ────────────────────────────────────────────
    let _ = engine.stop_all();
    let _ = engine.unload(buf);
    let _ = std::fs::remove_file(&wav);
    println!("\n✓ demo_bus: 全チェック通過\n");
    Ok(())
}

// ─── ユーティリティ ──────────────────────────────────────────────────────────

fn section(name: &str) { println!("\n━━━ {name}"); }
fn ok(msg: impl std::fmt::Display) { println!("  [OK ] {msg}"); }

fn check(msg: impl std::fmt::Display, result: bool) {
    if result { println!("  [OK ] {msg}"); }
    else { eprintln!("  [FAIL] {msg}"); panic!("check failed"); }
}

fn check_false(msg: impl std::fmt::Display, result: bool) {
    if !result { println!("  [OK ] {msg}"); }
    else { eprintln!("  [FAIL] {msg} (expected false)"); panic!("check_false failed"); }
}

fn gen_wav(freq: f32, sample_rate: u32, secs: f32) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    use std::io::Write as _;
    let path = std::env::temp_dir().join("nezia_demo_bus.wav");
    let ch: u16 = 2; let bps: u16 = 16;
    let n = (sample_rate as f32 * secs) as u32;
    let data_size = n * ch as u32 * (bps / 8) as u32;
    let ba = ch * bps / 8;
    let mut f = std::fs::File::create(&path)?;
    f.write_all(b"RIFF")?; f.write_all(&(36 + data_size).to_le_bytes())?; f.write_all(b"WAVE")?;
    f.write_all(b"fmt ")?; f.write_all(&16u32.to_le_bytes())?; f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&ch.to_le_bytes())?; f.write_all(&sample_rate.to_le_bytes())?;
    f.write_all(&(sample_rate * ba as u32).to_le_bytes())?;
    f.write_all(&ba.to_le_bytes())?; f.write_all(&bps.to_le_bytes())?;
    f.write_all(b"data")?; f.write_all(&data_size.to_le_bytes())?;
    let step = 2.0 * std::f32::consts::PI * freq / sample_rate as f32;
    let atk = (sample_rate as f32 * 0.01) as u32;
    let rel = (sample_rate as f32 * 0.05) as u32;
    for i in 0..n {
        let env = if i < atk { i as f32 / atk as f32 }
                  else if i >= n - rel { (n - i) as f32 / rel as f32 } else { 1.0 };
        let s = ((step * i as f32).sin() * env * 0.5 * i16::MAX as f32) as i16;
        f.write_all(&s.to_le_bytes())?; f.write_all(&s.to_le_bytes())?;
    }
    Ok(path)
}
