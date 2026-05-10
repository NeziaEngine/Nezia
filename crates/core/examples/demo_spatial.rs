//! シナリオ: 3D 空間サウンド
//!
//! cargo run --example demo_spatial
//!
//! ヘッドフォン推奨。
//!
//! カバー範囲:
//!   - play_with_handle / set_source_spatial_enabled
//!   - set_source_spatial_params: 全4減衰モデル
//!   - set_listener: 位置・向き
//!   - batch_set_source_positions: フレームごとの移動
//!   - シナリオ1: 音源がリスナーの左→右を横断 (パンニング確認)
//!   - シナリオ2: 音源がリスナーに接近 (距離減衰確認)
//!   - シナリオ3: 4 減衰モデルの比較
//!   - シナリオ4: リスナー回転
//!   - シナリオ5: SP-10 Doppler — OFF / ON の聴き比べ + リアルタイムピッチ表示
//!   - シナリオ6: dopplerLevel 0.0 / 0.5 / 1.0 段階比較
//!   - シナリオ7: リスナーが走って Doppler 発生 (Phase A 接近 / Phase B 離反)

use std::thread;
use std::time::Duration;

use nezia::{
    AttenuationModel, EntityId, SoundEngine, SourcePositionUpdate, SourceVelocityUpdate,
    SpawnSpatialInit,
};

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

    // リスナーを原点、+Z 方向に向ける (左手系 Y-up: 右 = +X, 前 = +Z)
    engine.set_listener([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]);

    // ─── シナリオ1: 左→右 横断 (パンニング確認) ──────────────
    section("シナリオ1: 左→右 横断 (パンニング)");
    println!("  ▶ 音が左耳 → 中央 → 右耳へと移動するはずです");
    println!("  ▶ 音源: x=-12m → x=+12m  (正面 z=+5m, 20ステップ × 400ms)");

    let src = engine
        .play_with_handle(buf, 1.0, 1.0, master, false, 128, SpawnSpatialInit::NONE)
        .expect("play_with_handle");
    let _ =
        engine.set_source_spatial_params(src, AttenuationModel::InverseDistance, 1.0, 30.0, 1.0);
    let _ = engine.set_source_spatial_enabled(src, true);

    for step in 0..=20 {
        let x = step as f32 * 1.2 - 12.0;
        engine.batch_set_source_positions(&[SourcePositionUpdate {
            source: src,
            position: [x, 0.0, 5.0],
        }]);
        print!(
            "\r  x={x:+5.1}m  [{}{}]",
            "█".repeat(step),
            "░".repeat(20 - step)
        );
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
    println!("  ▶ 音源: z=+40m → z=-5m  (18ステップ × 500ms)");

    let src = engine
        .play_with_handle(buf, 1.0, 1.0, master, false, 128, SpawnSpatialInit::NONE)
        .expect("play_with_handle");
    let _ =
        engine.set_source_spatial_params(src, AttenuationModel::InverseDistance, 1.0, 40.0, 1.0);
    let _ = engine.set_source_spatial_enabled(src, true);

    for step in 0..=18 {
        let z = 40.0 - step as f32 * 2.5;
        let dist = z.abs();
        engine.batch_set_source_positions(&[SourcePositionUpdate {
            source: src,
            position: [0.0, 0.0, z],
        }]);
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
        (
            "InverseDistance (自然な減衰)",
            AttenuationModel::InverseDistance,
            1.0_f32,
            50.0_f32,
            1.0_f32,
        ),
        (
            "Linear          (線形減衰)",
            AttenuationModel::Linear,
            1.0,
            50.0,
            1.0,
        ),
        (
            "Exponential     (急激な減衰)",
            AttenuationModel::Exponential,
            1.0,
            50.0,
            2.0,
        ),
        (
            "None            (減衰なし)",
            AttenuationModel::None,
            1.0,
            50.0,
            1.0,
        ),
    ];

    for (label, model, min, max, rolloff) in models {
        let src = engine
            .play_with_handle(buf, 1.0, 1.0, master, false, 128, SpawnSpatialInit::NONE)
            .expect("play_with_handle");
        let _ = engine.set_source_spatial_params(src, model, min, max, rolloff);
        let _ = engine.set_source_spatial_enabled(src, true);
        engine.batch_set_source_positions(&[SourcePositionUpdate {
            source: src,
            position: [0.0, 0.0, 10.0],
        }]);
        println!("\n  ▶ {label}");
        thread::sleep(Duration::from_millis(2400));
        let _ = engine.stop_all();
        thread::sleep(Duration::from_millis(800));
    }

    // ─── シナリオ4: リスナー回転 + ソース固定 ────────────────
    section("シナリオ4: リスナー回転 (音源は右 +X=8m に固定)");
    println!("  ▶ 最初は右から聴こえ、リスナーが右を向くと正面になるはずです");
    println!("  ▶ 13ステップ × 400ms でリスナーが 90° 回転");

    let src = engine
        .play_with_handle(buf, 1.0, 1.0, master, false, 128, SpawnSpatialInit::NONE)
        .expect("play_with_handle");
    let _ =
        engine.set_source_spatial_params(src, AttenuationModel::InverseDistance, 1.0, 30.0, 1.0);
    let _ = engine.set_source_spatial_enabled(src, true);
    engine.batch_set_source_positions(&[SourcePositionUpdate {
        source: src,
        position: [8.0, 0.0, 0.0],
    }]);

    // リスナーが +Z → +X へ 90° 回転
    for step in 0..=12 {
        let angle = step as f32 * std::f32::consts::FRAC_PI_2 / 12.0; // 0 → π/2
        let fwd = [angle.sin(), 0.0, angle.cos()];
        let deg = (angle * 180.0 / std::f32::consts::PI) as u32;
        engine.set_listener([0.0, 0.0, 0.0], fwd, [0.0, 1.0, 0.0]);
        print!(
            "\r  リスナー向き: {deg:3}°  [{}{}]",
            "█".repeat(step),
            "░".repeat(12 - step)
        );
        use std::io::Write as _;
        std::io::stdout().flush().ok();
        thread::sleep(Duration::from_millis(400));
    }
    println!("\r  完了 (90° 回転)                  ");
    let _ = engine.stop_all();
    thread::sleep(Duration::from_millis(800));

    // ─── シナリオ5: Doppler A/B 比較 (OFF → ON) ──────────────
    section("シナリオ5: SP-10 Doppler — OFF / ON の聴き比べ");
    println!("  ▶ 設定: 音源が右側 x=+8m に固定されたまま、奥 z=+40m から手前 z=-40m へ");
    println!("    -25 m/s (時速 90km 相当) で通過。リスナーは原点に静止。");
    println!("  ▶ 1 回目は Doppler **OFF** (level=0.0) → ピッチ一定で通過");
    println!("  ▶ 2 回目は Doppler **ON**  (level=1.0) → 接近時↑、離反時↓");
    println!();

    // リスナー姿勢を初期化（前のシナリオで回転していたため）
    engine.set_listener([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]);
    engine.set_listener_velocity([0.0, 0.0, 0.0]);

    let pass_speed = -25.0_f32; // m/s, -Z 方向
    let pass_x = 8.0_f32; // 右側オフセット (頭を貫通させない)
    let z_start = 40.0_f32;
    let z_end = -40.0_f32;
    let pass_secs = (z_start - z_end) / pass_speed.abs(); // 3.2 秒
    let pass_steps = 32;
    let pass_dt = pass_secs / pass_steps as f32;
    let sound_speed = 343.0_f32;

    for (label, level) in [("1/2 Doppler OFF", 0.0_f32), ("2/2 Doppler ON ", 1.0_f32)] {
        println!("  ▶ {label}");
        let src = engine
            .play_with_handle(buf, 1.0, 1.0, master, false, 128, SpawnSpatialInit::NONE)
            .expect("play_with_handle");
        let _ = engine.set_source_spatial_params(
            src,
            AttenuationModel::InverseDistance,
            2.0,
            80.0,
            1.0,
        );
        let _ = engine.set_source_spatial_enabled(src, true);
        let _ = engine.set_source_doppler_level(src, level);

        for step in 0..=pass_steps {
            let t = step as f32 * pass_dt;
            let z = z_start + pass_speed * t;
            engine.batch_set_source_positions(&[SourcePositionUpdate {
                source: src,
                position: [pass_x, 0.0, z],
            }]);
            engine.batch_set_source_velocities(&[SourceVelocityUpdate {
                source: src,
                velocity: [0.0, 0.0, pass_speed],
            }]);

            // 計算上のピッチ倍率（v_s_toward = -dot(v_s, listener→source)）
            let dist = (pass_x * pass_x + z * z).sqrt();
            let nz = z / dist;
            let v_s_toward = -(pass_speed * nz);
            let raw_ratio = sound_speed / (sound_speed - v_s_toward * level);
            let ratio = raw_ratio.clamp(0.5, 4.0);
            let bar_len = 20;
            let center = bar_len / 2;
            // 0.5..2.0 を bar 上にマップ（1.0 を中央）
            let pos = ((ratio.log2() * center as f32) + center as f32)
                .clamp(0.0, (bar_len - 1) as f32) as usize;
            let mut bar = vec![' '; bar_len];
            bar[center] = '|';
            bar[pos] = if ratio > 1.0 {
                '↑'
            } else if ratio < 1.0 {
                '↓'
            } else {
                '●'
            };
            let bar_str: String = bar.into_iter().collect();
            let side = if z > 0.0 { "前→" } else { "←後" };
            print!("\r    z={z:+5.1}m {side}  ピッチ x{ratio:.2}  低[{bar_str}]高");
            use std::io::Write as _;
            std::io::stdout().flush().ok();
            thread::sleep(Duration::from_secs_f32(pass_dt));
        }
        println!(
            "\r    完了                                                                        "
        );
        let _ = engine.stop_all();
        thread::sleep(Duration::from_millis(700));
    }

    // ─── シナリオ6: dopplerLevel 段階比較 ────────────────────────
    section("シナリオ6: SP-10 dopplerLevel 段階比較");
    println!("  ▶ 同じ通過を level=0.0 / 0.5 / 1.0 で 3 回繰り返し");
    println!("    0.0 < 0.5 < 1.0 の順で効果が強まる (ピッチ変化幅が広がる)");
    println!();

    for (label, level) in [
        ("level=0.0 (無効)    ", 0.0_f32),
        ("level=0.5 (半量)    ", 0.5),
        ("level=1.0 (物理通り)", 1.0),
    ] {
        println!("  ▶ {label}");
        let src = engine
            .play_with_handle(buf, 1.0, 1.0, master, false, 128, SpawnSpatialInit::NONE)
            .expect("play_with_handle");
        let _ = engine.set_source_spatial_params(
            src,
            AttenuationModel::InverseDistance,
            2.0,
            80.0,
            1.0,
        );
        let _ = engine.set_source_spatial_enabled(src, true);
        let _ = engine.set_source_doppler_level(src, level);

        for step in 0..=pass_steps {
            let t = step as f32 * pass_dt;
            let z = z_start + pass_speed * t;
            engine.batch_set_source_positions(&[SourcePositionUpdate {
                source: src,
                position: [pass_x, 0.0, z],
            }]);
            engine.batch_set_source_velocities(&[SourceVelocityUpdate {
                source: src,
                velocity: [0.0, 0.0, pass_speed],
            }]);
            thread::sleep(Duration::from_secs_f32(pass_dt));
        }
        let _ = engine.stop_all();
        thread::sleep(Duration::from_millis(500));
    }

    // ─── シナリオ7: リスナー移動による Doppler (接近 → 離反 を分離) ──
    section("シナリオ7: SP-10 リスナーが走る (接近フェーズ → 離反フェーズ)");
    println!("  ▶ ソースは右前方 [8, 0, 15] に静止");
    println!("  ▶ Phase A: リスナーが +Z 方向に 25 m/s で接近 (ピッチ↑)");
    println!("  ▶ Phase B: リスナーがそのまま走り続けて通過後 → 離反 (ピッチ↓)");
    println!();

    let src = engine
        .play_with_handle(buf, 1.0, 1.0, master, false, 128, SpawnSpatialInit::NONE)
        .expect("play_with_handle");
    let _ =
        engine.set_source_spatial_params(src, AttenuationModel::InverseDistance, 2.0, 80.0, 1.0);
    let _ = engine.set_source_spatial_enabled(src, true);
    let _ = engine.set_source_doppler_level(src, 1.0);
    engine.batch_set_source_positions(&[SourcePositionUpdate {
        source: src,
        position: [8.0, 0.0, 15.0],
    }]);
    engine.batch_set_source_velocities(&[SourceVelocityUpdate {
        source: src,
        velocity: [0.0, 0.0, 0.0],
    }]);

    let listener_speed = 25.0_f32;
    let listener_total_secs = 3.0_f32;
    let listener_steps = 30;
    let listener_dt = listener_total_secs / listener_steps as f32;
    for step in 0..=listener_steps {
        let t = step as f32 * listener_dt;
        let lz = -25.0 + listener_speed * t; // -25 → +50 を通過
        engine.set_listener([0.0, 0.0, lz], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]);
        engine.set_listener_velocity([0.0, 0.0, listener_speed]);
        let phase = if lz < 15.0 {
            "Phase A 接近"
        } else {
            "Phase B 離反"
        };
        print!("\r  リスナー z={lz:+6.1}m  →  {phase}                  ");
        use std::io::Write as _;
        std::io::stdout().flush().ok();
        thread::sleep(Duration::from_secs_f32(listener_dt));
    }
    println!("\r  完了                                                  ");
    let _ = engine.stop_all();
    // リスナー速度・位置を 0 に戻す（後続シナリオに影響しないよう）
    engine.set_listener_velocity([0.0, 0.0, 0.0]);
    engine.set_listener([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]);
    thread::sleep(Duration::from_millis(800));

    // ─── シナリオ8: 大量バッチ ──────────────────
    section("シナリオ8: batch_set_source_positions 33件 (triple buffer 経由)");
    println!("  ▶ triple buffer に snapshot 公開 (音には影響なし)");

    let big: Vec<SourcePositionUpdate> = (0..33)
        .map(|i| SourcePositionUpdate {
            source: EntityId {
                index: i,
                generation: 0,
            },
            position: [i as f32 * 0.3, 0.0, 5.0],
        })
        .collect();
    engine.batch_set_source_positions(&big);
    ok("batch 33件 → triple buffer publish");
    engine.batch_set_source_positions(&[]);
    ok("batch 空配列 → triple buffer publish");

    // ─── 完了 ────────────────────────────────────────────────
    let _ = engine.stop_all();
    let _ = engine.unload(buf);
    let _ = std::fs::remove_file(&wav);
    println!("\n✓ demo_spatial: 全チェック通過\n");
    Ok(())
}

// ─── ユーティリティ ──────────────────────────────────────────────────────────

fn section(name: &str) {
    println!("\n━━━ {name}");
}
fn ok(msg: impl std::fmt::Display) {
    println!("  [OK ] {msg}");
}

fn gen_wav(
    freq: f32,
    sample_rate: u32,
    secs: f32,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    use std::io::Write as _;
    let path = std::env::temp_dir().join("nezia_demo_spatial.wav");
    let ch: u16 = 2;
    let bps: u16 = 16;
    let n = (sample_rate as f32 * secs) as u32;
    let data_size = n * ch as u32 * (bps / 8) as u32;
    let ba = ch * bps / 8;
    let mut f = std::fs::File::create(&path)?;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_size).to_le_bytes())?;
    f.write_all(b"WAVE")?;
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&ch.to_le_bytes())?;
    f.write_all(&sample_rate.to_le_bytes())?;
    f.write_all(&(sample_rate * ba as u32).to_le_bytes())?;
    f.write_all(&ba.to_le_bytes())?;
    f.write_all(&bps.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_size.to_le_bytes())?;
    let step = 2.0 * std::f32::consts::PI * freq / sample_rate as f32;
    let atk = (sample_rate as f32 * 0.01) as u32;
    let rel = (sample_rate as f32 * 0.05) as u32;
    for i in 0..n {
        let env = if i < atk {
            i as f32 / atk as f32
        } else if i >= n - rel {
            (n - i) as f32 / rel as f32
        } else {
            1.0
        };
        let s = ((step * i as f32).sin() * env * 0.5 * i16::MAX as f32) as i16;
        f.write_all(&s.to_le_bytes())?;
        f.write_all(&s.to_le_bytes())?;
    }
    Ok(path)
}
