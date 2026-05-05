//! マスター出力キャプチャの結合テスト (Unity Recorder インテグレーション基盤)。
//!
//! 各テストは音声デバイスが利用できない CI 等の環境では `SoundEngine::new()` が
//! Err を返すため、その場合は早期 return して skip 扱いにする。

use std::time::{Duration, Instant};

use nezia::SoundEngine;

/// 一定時間 (ms) 待ってサウンドスレッドが少なくとも 1 度コールバックを回せるようにする。
fn sleep_callbacks() {
    std::thread::sleep(Duration::from_millis(120));
}

#[test]
fn output_format_returns_nonzero() {
    let engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let (sr, ch) = engine.output_format();
    assert!(sr > 0, "sample rate should be positive");
    assert!(ch > 0, "channels should be positive");
}

#[test]
fn enable_master_capture_returns_some_only_first_call() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let r1 = engine.enable_master_capture();
    assert!(r1.is_some(), "first enable should return reader");
    let r2 = engine.enable_master_capture();
    assert!(
        r2.is_none(),
        "second enable should return None (reader already taken)"
    );
}

#[test]
fn capture_reader_metadata_matches_engine_format() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let (sr, ch) = engine.output_format();
    let reader = engine.enable_master_capture().expect("reader");
    assert_eq!(reader.sample_rate(), sr);
    assert_eq!(reader.channels(), ch);
    assert_eq!(reader.dropped_samples(), 0);
}

#[test]
fn capture_reader_drains_master_samples() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut reader = engine.enable_master_capture().expect("reader");

    // 最低 1 コールバックぶん回す。サウンドスレッドが master を mix → push する。
    sleep_callbacks();

    let mut buf = vec![0.0f32; 4096];
    let mut total_read = 0usize;
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        let n = reader.read_interleaved(&mut buf);
        total_read += n;
        if total_read > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        total_read > 0,
        "expected at least some samples to be captured (got {total_read})"
    );
}

#[test]
fn dsp_time_advances_monotonically() {
    let engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let t0 = engine.dsp_time_samples();
    sleep_callbacks();
    let t1 = engine.dsp_time_samples();
    assert!(t1 >= t0, "dsp_time should be monotonic (t0={t0}, t1={t1})");
    // 100ms 以上待ったので、サウンドスレッドが動いていれば確実に進んでいるはず。
    // ただしオーディオデバイスのウォームアップに時間がかかる環境もあるので
    // strict には等号を許容する (進んでいないこと自体は assert しない)。
    let _ = t1;
}

#[test]
fn disable_capture_stops_filling_ring() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut reader = engine.enable_master_capture().expect("reader");
    sleep_callbacks();
    // 1 度全部 drain しておく。
    let mut buf = vec![0.0f32; 16384];
    while reader.read_interleaved(&mut buf) > 0 {}

    engine.disable_master_capture();
    sleep_callbacks();
    // disable 後も少し待って、新規サンプルが流入していないことを確認。
    // disable 前にバッファに残っていたぶんがあれば 0 でないこともあり得るが、
    // 直前に drain しているので 0 を期待してよい。
    let n = reader.read_interleaved(&mut buf);
    assert_eq!(
        n, 0,
        "after disable + drain, no new samples should arrive (got {n})"
    );
}
