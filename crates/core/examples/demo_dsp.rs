//! シナリオ: DSP エフェクト (Phase 2-3 PR 1: バス LPF / HPF)
//!
//! cargo run --example demo_dsp
//!
//! カバー範囲:
//!   - add_effect: バスへの LPF / HPF 挿入
//!   - set_effect_param: cutoff / Q の動的変更
//!   - set_effect_enabled: ON/OFF トグル (A/B 比較)
//!   - remove_effect: チェーンからの削除
//!
//! 鳴らす音は 440 Hz と 5 kHz の 2 周波数を重ねたサイン波。
//! - LPF (cutoff=500Hz) を掛けると 5kHz 成分が削れて低音だけになる
//! - HPF (cutoff=2kHz) を掛けると 440Hz が削れて高音だけになる

use std::thread;
use std::time::Duration;

use nezia::{EffectKind, EffectPosition, EffectTarget, LpfParam, SoundEngine};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("╔══════════════════════════════════════╗");
    println!("║       demo_dsp: バス LPF / HPF       ║");
    println!("╚══════════════════════════════════════╝");

    let wav = gen_two_freq_wav(440.0, 5000.0, 44100, 12.0)?;
    let mut engine = SoundEngine::new()?;
    let buf = engine.load(&wav)?;
    ok("SoundEngine 起動 / バッファロード (440Hz + 5kHz, 12s)");

    let master = engine.master_bus();

    // ─── シナリオ1: LPF OFF → ON 比較 (Pre-Fader) ────────
    section("シナリオ1: バス LPF ─ OFF (2.5s) → ON (2.5s, cutoff=500Hz)");
    println!("  ▶ ON にすると 5kHz 成分が削れて低音だけになるはず");

    let src = engine
        .play_with_handle(buf, 0.5, 1.0, master, false)
        .expect("play_with_handle");
    let _ = src;

    println!("  ▶ LPF OFF (2.5 秒)");
    thread::sleep(Duration::from_millis(2500));

    let lpf = engine
        .add_effect(
            EffectTarget::Bus(master),
            EffectKind::Lpf,
            EffectPosition::Pre,
        )
        .expect("add LPF");
    let _ = engine.set_effect_param(lpf, LpfParam::Cutoff, 500.0);
    let _ = engine.set_effect_param(lpf, LpfParam::Q, 0.707);
    println!("  ▶ LPF ON  (cutoff=500Hz, Q=0.707, 2.5 秒)");
    thread::sleep(Duration::from_millis(2500));

    let _ = engine.remove_effect(lpf);
    let _ = engine.stop_all();
    thread::sleep(Duration::from_millis(700));

    // ─── シナリオ2: HPF OFF → ON 比較 ────────────────────
    section("シナリオ2: バス HPF ─ OFF (2.5s) → ON (2.5s, cutoff=2kHz)");
    println!("  ▶ ON にすると 440Hz 成分が削れて高音だけになるはず");

    let _src = engine
        .play_with_handle(buf, 0.5, 1.0, master, false)
        .expect("play_with_handle");

    println!("  ▶ HPF OFF (2.5 秒)");
    thread::sleep(Duration::from_millis(2500));

    let hpf = engine
        .add_effect(
            EffectTarget::Bus(master),
            EffectKind::Hpf,
            EffectPosition::Pre,
        )
        .expect("add HPF");
    // HPF の param インデックスは LpfParam と同レイアウト (Cutoff=0, Q=1) なので流用。
    let _ = engine.set_effect_param(hpf, nezia::HpfParam::Cutoff, 2000.0);
    let _ = engine.set_effect_param(hpf, nezia::HpfParam::Q, 0.707);
    println!("  ▶ HPF ON  (cutoff=2kHz, Q=0.707, 2.5 秒)");
    thread::sleep(Duration::from_millis(2500));

    let _ = engine.remove_effect(hpf);
    let _ = engine.stop_all();
    thread::sleep(Duration::from_millis(700));

    // ─── シナリオ3: LPF cutoff スイープ ──────────────────
    section("シナリオ3: LPF cutoff スイープ (5kHz → 200Hz)");
    println!("  ▶ 高い周波数から低い周波数へ徐々に絞る → 高音成分が段階的に消えていく");

    let _src = engine
        .play_with_handle(buf, 0.5, 1.0, master, false)
        .expect("play_with_handle");
    let lpf = engine
        .add_effect(
            EffectTarget::Bus(master),
            EffectKind::Lpf,
            EffectPosition::Pre,
        )
        .expect("add LPF");
    let _ = engine.set_effect_param(lpf, LpfParam::Q, 0.707);

    let steps = 30;
    let dt = Duration::from_millis(120);
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        // 対数スイープ: 5000 → 200
        let cutoff = 5000.0_f32 * (200.0_f32 / 5000.0).powf(t);
        let _ = engine.set_effect_param(lpf, LpfParam::Cutoff, cutoff);
        print!("\r  cutoff = {:6.0} Hz", cutoff);
        use std::io::Write as _;
        std::io::stdout().flush().ok();
        thread::sleep(dt);
    }
    println!("\r  完了                              ");
    let _ = engine.remove_effect(lpf);
    let _ = engine.stop_all();
    thread::sleep(Duration::from_millis(700));

    // ─── シナリオ4: enabled トグル (高速 A/B) ───────────
    section("シナリオ4: enabled トグル (LPF cutoff=400Hz を 0.5 秒間隔で ON/OFF × 6 回)");
    println!("  ▶ ON 時はこもった音、OFF 時は元のシャキッとした音が交互に出る");

    let _src = engine
        .play_with_handle(buf, 0.5, 1.0, master, false)
        .expect("play_with_handle");
    let lpf = engine
        .add_effect(
            EffectTarget::Bus(master),
            EffectKind::Lpf,
            EffectPosition::Pre,
        )
        .expect("add LPF");
    let _ = engine.set_effect_param(lpf, LpfParam::Cutoff, 400.0);

    for i in 0..6 {
        let on = i % 2 == 0;
        let _ = engine.set_effect_enabled(lpf, on);
        println!(
            "  ▶ LPF {}",
            if on {
                "ON  (こもる)"
            } else {
                "OFF (シャキッ)"
            }
        );
        thread::sleep(Duration::from_millis(500));
    }

    let _ = engine.remove_effect(lpf);
    let _ = engine.stop_all();
    let _ = engine.unload(buf);
    let _ = std::fs::remove_file(&wav);
    println!("\n✓ demo_dsp: 全チェック通過\n");
    Ok(())
}

// ─── ユーティリティ ──────────────────────────────────────────────────────────

fn section(name: &str) {
    println!("\n━━━ {name}");
}
fn ok(msg: impl std::fmt::Display) {
    println!("  [OK ] {msg}");
}

/// 2 周波数を重ねたサイン波を生成する。
fn gen_two_freq_wav(
    f1: f32,
    f2: f32,
    sample_rate: u32,
    secs: f32,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    use std::io::Write as _;
    let path = std::env::temp_dir().join("nezia_demo_dsp.wav");
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
    let step1 = 2.0 * std::f32::consts::PI * f1 / sample_rate as f32;
    let step2 = 2.0 * std::f32::consts::PI * f2 / sample_rate as f32;
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
        let a = (step1 * i as f32).sin();
        let b = (step2 * i as f32).sin();
        let mixed = (a * 0.5 + b * 0.5) * env * 0.45;
        let s = (mixed * i16::MAX as f32) as i16;
        f.write_all(&s.to_le_bytes())?;
        f.write_all(&s.to_le_bytes())?;
    }
    Ok(path)
}
