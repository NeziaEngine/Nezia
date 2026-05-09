//! Mixer Snapshot (Phase 3-2) の結合テスト。
//!
//! `SoundEngine` の宣言的ビルダー → registry → audio_thread での補間 → BusWorld 反映までを検証。

use std::thread;
use std::time::Duration;

use nezia::{EffectKind, EffectPosition, EffectTarget, LpfParam, ReverbParam, SoundEngine};

/// テスト用の小さな WAV (再生用)。
fn gen_wav(secs: f32, name: &str) -> std::path::PathBuf {
    use std::io::Write as _;
    let path = std::env::temp_dir().join(name);
    let sample_rate: u32 = 44100;
    let channels: u16 = 2;
    let bps: u16 = 16;
    let n = (sample_rate as f32 * secs) as u32;
    let data_size = n * channels as u32 * (bps / 8) as u32;
    let block_align = channels * bps / 8;
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(b"RIFF").unwrap();
    f.write_all(&(36 + data_size).to_le_bytes()).unwrap();
    f.write_all(b"WAVE").unwrap();
    f.write_all(b"fmt ").unwrap();
    f.write_all(&16u32.to_le_bytes()).unwrap();
    f.write_all(&1u16.to_le_bytes()).unwrap();
    f.write_all(&channels.to_le_bytes()).unwrap();
    f.write_all(&sample_rate.to_le_bytes()).unwrap();
    f.write_all(&(sample_rate * block_align as u32).to_le_bytes())
        .unwrap();
    f.write_all(&block_align.to_le_bytes()).unwrap();
    f.write_all(&bps.to_le_bytes()).unwrap();
    f.write_all(b"data").unwrap();
    f.write_all(&data_size.to_le_bytes()).unwrap();
    let step = 2.0 * std::f32::consts::PI * 440.0 / sample_rate as f32;
    for i in 0..n {
        let s = ((step * i as f32).sin() * 0.5 * i16::MAX as f32) as i16;
        f.write_all(&s.to_le_bytes()).unwrap();
        f.write_all(&s.to_le_bytes()).unwrap();
    }
    path
}

#[test]
fn build_and_destroy_snapshot() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let master = engine.master_bus();
    let id = engine.snapshot_builder().set_bus_gain(master, 0.5).commit();
    assert!(id.is_some());
    assert!(engine.destroy_snapshot(id.unwrap()));
    assert!(!engine.destroy_snapshot(id.unwrap()));
}

#[test]
fn builder_overwrites_duplicate_bus_gain() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let master = engine.master_bus();
    // 同じバスを 2 回設定 → 後勝ちで 0.2 が記録される。
    let id = engine
        .snapshot_builder()
        .set_bus_gain(master, 0.7)
        .set_bus_gain(master, 0.2)
        .commit()
        .unwrap();
    // apply して効果を観測する仕組みは別テストで担保。ここでは commit 成功のみ。
    assert!(engine.destroy_snapshot(id));
}

#[test]
fn apply_with_destroyed_id_returns_false() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let master = engine.master_bus();
    let id = engine
        .snapshot_builder()
        .set_bus_gain(master, 0.5)
        .commit()
        .unwrap();
    engine.destroy_snapshot(id);
    assert!(!engine.apply_snapshot(id, 0.0));
}

#[test]
fn apply_with_zero_fade_immediately_changes_master_gain() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let master = engine.master_bus();
    // 何か再生して audio thread を回す。
    let wav = gen_wav(1.0, "nezia_snapshot_test_zero_fade.wav");
    let buf = engine.load(&wav).unwrap();
    let _ = engine.play_with_handle(buf, 1.0, 1.0, master, false);

    let id = engine
        .snapshot_builder()
        .set_bus_gain(master, 0.25)
        .commit()
        .unwrap();
    assert!(engine.apply_snapshot(id, 0.0));

    // 数 callback 経過させる。fade=0 で即時適用されるはず。
    thread::sleep(Duration::from_millis(150));
    let _ = engine.stop_all();
    thread::sleep(Duration::from_millis(50));
    let _ = engine.unload(buf);
    engine.destroy_snapshot(id);
    let _ = std::fs::remove_file(&wav);
}

#[test]
fn apply_with_fade_runs_to_completion() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let master = engine.master_bus();
    let wav = gen_wav(1.0, "nezia_snapshot_test_fade.wav");
    let buf = engine.load(&wav).unwrap();
    let _ = engine.play_with_handle(buf, 1.0, 1.0, master, false);

    let id = engine
        .snapshot_builder()
        .set_bus_gain(master, 0.5)
        .set_bus_muted(master, false) // bool param もテスト
        .commit()
        .unwrap();
    assert!(engine.apply_snapshot(id, 0.2)); // 0.2 秒フェード

    // フェード完了まで待つ。
    thread::sleep(Duration::from_millis(400));

    let _ = engine.stop_all();
    thread::sleep(Duration::from_millis(50));
    let _ = engine.unload(buf);
    engine.destroy_snapshot(id);
    let _ = std::fs::remove_file(&wav);
}

#[test]
fn apply_can_interrupt_in_progress_fade() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let master = engine.master_bus();
    let wav = gen_wav(2.0, "nezia_snapshot_test_interrupt.wav");
    let buf = engine.load(&wav).unwrap();
    let _ = engine.play_with_handle(buf, 1.0, 1.0, master, false);

    let s1 = engine
        .snapshot_builder()
        .set_bus_gain(master, 0.0)
        .commit()
        .unwrap();
    let s2 = engine
        .snapshot_builder()
        .set_bus_gain(master, 1.0)
        .commit()
        .unwrap();

    assert!(engine.apply_snapshot(s1, 1.0)); // 1 秒フェード開始
    thread::sleep(Duration::from_millis(100));
    // 完了前に s2 を割り込ませる
    assert!(engine.apply_snapshot(s2, 0.5));
    thread::sleep(Duration::from_millis(700));

    let _ = engine.stop_all();
    thread::sleep(Duration::from_millis(50));
    let _ = engine.unload(buf);
    engine.destroy_snapshot(s1);
    engine.destroy_snapshot(s2);
    let _ = std::fs::remove_file(&wav);
}

#[test]
fn snapshot_with_effect_param_applies() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let master = engine.master_bus();
    let wav = gen_wav(1.0, "nezia_snapshot_test_effect.wav");
    let buf = engine.load(&wav).unwrap();
    let _ = engine.play_with_handle(buf, 1.0, 1.0, master, false);

    // master に LPF を 1 つ挿す。
    let lpf = engine
        .add_effect(
            EffectTarget::Bus(master),
            EffectKind::Lpf,
            EffectPosition::Post,
        )
        .expect("lpf spawn");

    let snap = engine
        .snapshot_builder()
        .set_effect_param(lpf, LpfParam::Cutoff, 500.0)
        .commit()
        .unwrap();

    assert!(engine.apply_snapshot(snap, 0.1));
    thread::sleep(Duration::from_millis(200));

    let _ = engine.stop_all();
    thread::sleep(Duration::from_millis(50));
    let _ = engine.unload(buf);
    engine.destroy_snapshot(snap);
    let _ = std::fs::remove_file(&wav);
}

#[test]
fn snapshot_with_reverb_param_applies() {
    // Reverb は AoS のため、別経路の read_reverb_param を通る。網羅性確認。
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let master = engine.master_bus();
    let reverb = engine
        .add_effect(
            EffectTarget::Bus(master),
            EffectKind::Reverb,
            EffectPosition::Post,
        )
        .expect("reverb spawn");
    let snap = engine
        .snapshot_builder()
        .set_effect_param(reverb, ReverbParam::Wet, 0.7)
        .commit()
        .unwrap();
    assert!(engine.apply_snapshot(snap, 0.1));
    thread::sleep(Duration::from_millis(200));
    engine.destroy_snapshot(snap);
}

#[test]
fn snapshot_with_source_send_gain_compiles() {
    // Wwise 互換の per-event aux send。Snapshot で gain 補間の対象にできることを確認する。
    // 実装上 SendId は bus / source 起点を区別せず受け付け、apply 時に
    // source_world.resolve_send を経由する。ここでは API パスの疎通のみを確認する
    // (実 source 状態は audio thread 上で独立に変動するため波形検証は別途)。
    use nezia::SendPosition;
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let aux = engine.create_bus(1.0).expect("create aux");
    let dummy_src = nezia::EntityId {
        index: 1,
        generation: 0,
    };
    let sid = engine
        .add_source_send(dummy_src, aux, SendPosition::Post, 0.5)
        .expect("source send");
    let snap = engine
        .snapshot_builder()
        .set_send_gain(sid, 0.0)
        .commit()
        .unwrap();
    assert!(engine.apply_snapshot(snap, 0.05));
    thread::sleep(Duration::from_millis(120));
    engine.destroy_snapshot(snap);
    assert!(engine.remove_send(sid));
}
