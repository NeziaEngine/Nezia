//! シナリオ: 3D 空間サウンド
//!
//! cargo run --example demo_spatial
//!
//! ヘッドフォン推奨。
//!
//! カバー範囲:
//!   - spawn_source / set_source_spatial_enabled
//!   - set_source_spatial_params: 全4減衰モデル
//!   - set_listener: 位置・向き
//!   - batch_set_source_positions: フレームごとの移動
//!   - シナリオ1: 音源がリスナーの左→右を横断 (パンニング確認)
//!   - シナリオ2: 音源がリスナーに接近 (距離減衰確認)
//!   - シナリオ3: 4 減衰モデルの比較
//!   - シナリオ4: リスナー回転

use std::thread;
use std::time::Duration;

use nezia::{AttenuationModel, EntityId, SoundEngine};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("╔══════════════════════════════════════╗");
    println!("║      demo_spatial: 3D サウンド       ║");
    println!("║      ヘッドフォン推奨               ║");
    println!("╚══════════════════════════════════════╝");

    let wav = gen_wav(880.0, 44100, 12.0)?;
    let mut engine = SoundEngine::new()?;
    let buf = engine.load(&wav)?;
    ok("SoundEngine 起動 / バッファロード (880 Hz, 12s)");

    let master = engine.master_bus();

    // リスナーを原点、-Z 方向に向ける (右 = +X)
    let _ = engine.set_listener([0.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]);

    // ─── シナリオ1: 左→右 横断 (パンニング確認) ──────────────
    section("シナリオ1: 左→右 横断 (パンニング)");
    println!("  ▶ 音が左耳 → 中央 → 右耳へと移動するはずです");
    println!("  ▶ 音源: x=-12m → x=+12m  (正面 z=-5m, 20ステップ × 400ms)");

    let src = engine.spawn_source(buf, 1.0, 1.0, master).expect("spawn_source");
    let _ = engine.set_source_spatial_params(src, AttenuationModel::InverseDistance, 1.0, 30.0, 1.0);
    let _ = engine.set_source_spatial_enabled(src, true);

    for step in 0..=20 {
        let x = step as f32 * 1.2 - 12.0;
        let _ = engine.batch_set_source_positions(&[(src, [x, 0.0, -5.0])]);
        print!("\r  x={x:+5.1}m  [{}{}]",
            "█".repeat(step),
            "░".repeat(20 - step));
        use std::io::Write as _;
        std::io::stdout().flush().ok();
        thread::sleep(Duration::from_millis(400));
    }
    println!("\r  完了                              ");
    let _ = engine.stop_all();
    thread::sleep(Duration::from_millis(800));

    // ─── シナリオ2: 接近 (距離減衰確認) ──────────────────────
    section("シナリオ2: 正面から接近 (距離減衰)");
    println!("  ▶ 音が遠くから近づき、通り過ぎるにつれて大きくなるはずです");
    println!("  ▶ 音源: z=-40m → z=+5m  (18ステップ × 500ms)");

    let src = engine.spawn_source(buf, 1.0, 1.0, master).expect("spawn_source");
    let _ = engine.set_source_spatial_params(src, AttenuationModel::InverseDistance, 1.0, 40.0, 1.0);
    let _ = engine.set_source_spatial_enabled(src, true);

    for step in 0..=18 {
        let z = step as f32 * 2.5 - 40.0;
        let dist = z.abs();
        let _ = engine.batch_set_source_positions(&[(src, [0.0, 0.0, z])]);
        print!("\r  z={z:+5.1}m  距離={dist:.1}m");
        use std::io::Write as _;
        std::io::stdout().flush().ok();
        thread::sleep(Duration::from_millis(500));
    }
    println!("\r  完了                              ");
    let _ = engine.stop_all();
    thread::sleep(Duration::from_millis(800));

    // ─── シナリオ3: 4 減衰モデル比較 ─────────────────────────
    section("シナリオ3: 減衰モデル比較 (距離 10m に固定, 各 2.4s)");
    println!("  ▶ 距離 10m 固定での各モデルの減衰量を比較します (音量は変化しません)");
    println!("    None は減衰なし (距離に関係なく最大音量)");

    let models = [
        ("InverseDistance (自然な減衰)", AttenuationModel::InverseDistance, 1.0_f32, 50.0_f32, 1.0_f32),
        ("Linear          (線形減衰)",   AttenuationModel::Linear,          1.0,     50.0,     1.0),
        ("Exponential     (急激な減衰)", AttenuationModel::Exponential,     1.0,     50.0,     2.0),
        ("None            (減衰なし)",   AttenuationModel::None,            1.0,     50.0,     1.0),
    ];

    for (label, model, min, max, rolloff) in models {
        let src = engine.spawn_source(buf, 1.0, 1.0, master).expect("spawn_source");
        let _ = engine.set_source_spatial_params(src, model, min, max, rolloff);
        let _ = engine.set_source_spatial_enabled(src, true);
        let _ = engine.batch_set_source_positions(&[(src, [0.0, 0.0, -10.0])]);
        println!("\n  ▶ {label}");
        thread::sleep(Duration::from_millis(2400));
        let _ = engine.stop_all();
        thread::sleep(Duration::from_millis(800));
    }

    // ─── シナリオ4: リスナー回転 + ソース固定 ────────────────
    section("シナリオ4: リスナー回転 (音源は右 +X=8m に固定)");
    println!("  ▶ 最初は右から聴こえ、リスナーが右を向くと正面になるはずです");
    println!("  ▶ 13ステップ × 400ms でリスナーが 90° 回転");

    let src = engine.spawn_source(buf, 1.0, 1.0, master).expect("spawn_source");
    let _ = engine.set_source_spatial_params(src, AttenuationModel::InverseDistance, 1.0, 30.0, 1.0);
    let _ = engine.set_source_spatial_enabled(src, true);
    let _ = engine.batch_set_source_positions(&[(src, [8.0, 0.0, 0.0])]);

    // リスナーが -Z → +X へ 90° 回転
    for step in 0..=12 {
        let angle = step as f32 * std::f32::consts::FRAC_PI_2 / 12.0; // 0 → π/2
        let fwd = [angle.sin(), 0.0, -angle.cos()];
        let deg = (angle * 180.0 / std::f32::consts::PI) as u32;
        let _ = engine.set_listener([0.0, 0.0, 0.0], fwd, [0.0, 1.0, 0.0]);
        print!("\r  リスナー向き: {deg:3}°  [{}{}]",
            "█".repeat(step),
            "░".repeat(12 - step));
        use std::io::Write as _;
        std::io::stdout().flush().ok();
        thread::sleep(Duration::from_millis(400));
    }
    println!("\r  完了 (90° 回転)                  ");
    let _ = engine.stop_all();
    thread::sleep(Duration::from_millis(800));

    // ─── シナリオ5: バッチ上限超え 自動分割 ──────────────────
    section("シナリオ5: batch_set_source_positions 33件 (SPATIAL_BATCH_SIZE=32 超)");
    println!("  ▶ 内部でチャンクに分割されて送信されます (音には影響なし)");

    let big: Vec<(EntityId, [f32; 3])> = (0..33)
        .map(|i| (EntityId { index: i, generation: 0 }, [i as f32 * 0.3, 0.0, -5.0]))
        .collect();
    check("batch 33件 → 自動2チャンク分割", engine.batch_set_source_positions(&big));
    check("batch 空配列",                   engine.batch_set_source_positions(&[]));

    // ─── 完了 ────────────────────────────────────────────────
    let _ = engine.stop_all();
    let _ = engine.unload(buf);
    let _ = std::fs::remove_file(&wav);
    println!("\n✓ demo_spatial: 全チェック通過\n");
    Ok(())
}

// ─── ユーティリティ ──────────────────────────────────────────────────────────

fn section(name: &str) { println!("\n━━━ {name}"); }
fn ok(msg: impl std::fmt::Display) { println!("  [OK ] {msg}"); }

fn check(msg: impl std::fmt::Display, result: bool) {
    if result { println!("  [OK ] {msg}"); }
    else { eprintln!("  [FAIL] {msg}"); panic!("check failed"); }
}

fn gen_wav(freq: f32, sample_rate: u32, secs: f32) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    use std::io::Write as _;
    let path = std::env::temp_dir().join("nezia_demo_spatial.wav");
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
