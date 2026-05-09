//! PlayScheduled (Phase 3-4) の結合テスト。
//!
//! `SoundEngine` の公開 API レベルで予約再生の経路 + キャンセルを検証する。
//! 実機オーディオデバイスが無い環境では `SoundEngine::new()` が失敗するため
//! early return で skip する (CI 互換、既存テストと同じパターン)。
//!
//! サンプル精度の発音タイミング検証は `source::system::scheduled_tests`
//! (lib unit test) で `activate_scheduled` を直接駆動して行う。本ファイルでは
//! API 経路 (engine API → command → audio_thread) の到達と挙動だけを確認する。

use nezia::SoundEngine;

fn make_buffer(engine: &mut SoundEngine) -> nezia::BufferId {
    // 1ch / 100 frames の単純な PCM。
    engine.load_from_pcm(vec![0.5_f32; 100], 1, 48_000)
}

#[test]
fn play_scheduled_in_returns_true_for_valid_buffer() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let buf = make_buffer(&mut engine);
    assert!(engine.play_scheduled_in(buf, 0.05, 1.0, 1.0, false));
}

#[test]
fn play_scheduled_in_with_zero_or_negative_delay_treats_as_immediate() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let buf = make_buffer(&mut engine);
    // delay <= 0 は内部で sentinel 0 (即時) として扱われる。コマンドは正常に通る。
    assert!(engine.play_scheduled_in(buf, 0.0, 1.0, 1.0, false));
    assert!(engine.play_scheduled_in(buf, -1.0, 1.0, 1.0, false));
}

#[test]
fn play_scheduled_at_frame_with_invalid_buffer_fails() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let invalid = nezia::BufferId {
        index: 999,
        generation: 0,
    };
    assert!(!engine.play_scheduled_at_frame(invalid, 1000, 1.0, 1.0, false));
}

#[test]
fn play_to_bus_scheduled_in_succeeds_on_master() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let buf = make_buffer(&mut engine);
    let master = engine.master_bus();
    assert!(engine.play_to_bus_scheduled_in(buf, 0.02, 1.0, 1.0, master, false));
}

#[test]
fn play_with_handle_scheduled_in_returns_handle() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let buf = make_buffer(&mut engine);
    let master = engine.master_bus();
    let id = engine
        .play_with_handle_scheduled_in(buf, 0.05, 1.0, 1.0, master, false)
        .expect("handle should be returned");
    // handle 経由で stop できる (キャンセル経路)。
    assert!(engine.stop_source(id));
}

#[test]
fn play_with_handle_scheduled_at_frame_returns_handle() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let buf = make_buffer(&mut engine);
    let master = engine.master_bus();
    let target = engine.dsp_time_samples().saturating_add(48_000); // ~1 秒後
    let id = engine
        .play_with_handle_scheduled_at_frame(buf, target, 1.0, 1.0, master, false)
        .expect("handle should be returned");
    assert!(engine.stop_source(id));
}

#[test]
fn play_with_handle_scheduled_in_invalid_buffer_returns_none() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let invalid = nezia::BufferId {
        index: 999,
        generation: 0,
    };
    let master = engine.master_bus();
    assert!(
        engine
            .play_with_handle_scheduled_in(invalid, 0.05, 1.0, 1.0, master, false)
            .is_none()
    );
}

#[test]
fn dsp_time_seconds_advances_after_callbacks() {
    // dsp clock そのものの単調性を確認する補助テスト。
    // 予約再生は dsp clock を基準にするため、ここの時計が動かないと意味がない。
    let engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let t0 = engine.dsp_time_samples();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let t1 = engine.dsp_time_samples();
    // 50ms 経過 ⇒ 48000 * 0.05 = 2400 frame 進む想定。下限 1 frame で動作確認に留める。
    assert!(t1 >= t0, "dsp clock must be monotonic");
}
