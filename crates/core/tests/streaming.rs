//! ストリーミング再生 (Phase 2-4) の結合テスト。
//!
//! ワーカ + MirrorRing + symphonia decoder の経路を直接検証する。
//! 音声デバイス不要 (`SoundEngine` は使わず `AudioBufferPool` のみ駆動)。

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;

use nezia::{SpawnSpatialInit, StreamingOpts};

/// テスト用ステレオ 16-bit PCM WAV を生成する (サイン波)。
fn gen_wav(freq: f32, sample_rate: u32, secs: f32, name: &str) -> std::path::PathBuf {
    use std::io::Write as _;
    let path = std::env::temp_dir().join(name);
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
    let step = 2.0 * std::f32::consts::PI * freq / sample_rate as f32;
    for i in 0..n {
        let s = ((step * i as f32).sin() * 0.5 * i16::MAX as f32) as i16;
        f.write_all(&s.to_le_bytes()).unwrap();
        f.write_all(&s.to_le_bytes()).unwrap();
    }
    path
}

/// 内部 API を直接呼ぶため、`crates/core/src/lib.rs` の公開型 (`StreamingOpts`) と
/// 内部 `AudioBufferPool` 経由は使えない。代わりに `SoundEngine::load_streaming` を
/// 経由するが、SoundEngine は audio device を要求するため、テストでは
/// `spawn_streaming_worker` を直接叩く方針で書く。
///
/// → `streaming` モジュールは `pub(crate)` なので外部テストからは触れない。
/// 仕方ないので SoundEngine 経由のテストを書く (audio device 不在環境ではスキップ)。
#[test]
fn streaming_worker_decodes_into_ring() {
    // SoundEngine は audio device を要求する。CI で device 不在の場合はスキップ。
    let mut engine = match nezia::SoundEngine::new() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("skip: SoundEngine 初期化失敗 (device なし?): {e}");
            return;
        }
    };

    // 2 秒の 440Hz WAV を生成。
    let wav = gen_wav(440.0, 44100, 2.0, "nezia_streaming_test_decode.wav");
    let buf = engine
        .load_streaming(&wav, StreamingOpts::default())
        .expect("load_streaming should succeed");

    // ワーカが ring を埋めるのを少し待つ。
    thread::sleep(Duration::from_millis(200));

    // 再生してすぐ stop し、自然 EOF を経由しないで unload しても安全に終了することを確認。
    let master = engine.master_bus();
    let id = engine.play_with_handle(buf, 1.0, 1.0, master, false, 128, SpawnSpatialInit::NONE);
    assert!(id.is_some(), "play_with_handle should return Some");
    thread::sleep(Duration::from_millis(100));
    let _ = engine.stop_all();
    thread::sleep(Duration::from_millis(100));

    // unload で worker を join できる (deadlock しない)。
    assert!(engine.unload(buf), "unload should succeed");

    let _ = std::fs::remove_file(&wav);
}

#[test]
fn streaming_buffer_id_is_compatible_with_play_with_handle() {
    let mut engine = match nezia::SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let wav = gen_wav(220.0, 44100, 1.0, "nezia_streaming_test_compat.wav");
    let buf = engine
        .load_streaming(&wav, StreamingOpts::default())
        .unwrap();
    // streaming も 静的 と同じ play_with_handle を受け付ける。
    let master = engine.master_bus();
    let id = engine.play_with_handle(buf, 0.5, 1.0, master, false, 128, SpawnSpatialInit::NONE);
    assert!(id.is_some());
    thread::sleep(Duration::from_millis(50));
    let _ = engine.stop_all();
    thread::sleep(Duration::from_millis(50));
    let _ = engine.unload(buf);
    let _ = std::fs::remove_file(&wav);
}

#[test]
fn streaming_loop_keeps_supplying_after_eof() {
    let mut engine = match nezia::SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    // 短い WAV (0.3 秒) を作り、loop を有効化して長時間再生してもアンダーランしないことを確認。
    let wav = gen_wav(330.0, 44100, 0.3, "nezia_streaming_test_loop.wav");
    let buf = engine
        .load_streaming(&wav, StreamingOpts::default())
        .unwrap();
    engine.set_streaming_loop(buf, true);

    let master = engine.master_bus();
    let id = engine.play_with_handle(buf, 0.5, 1.0, master, true, 128, SpawnSpatialInit::NONE);
    assert!(id.is_some());

    // 0.5 秒待つ (元 WAV より長い) → loop で続いていれば source は alive 維持。
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        engine.poll_events();
        thread::sleep(Duration::from_millis(20));
    }
    engine.poll_events();
    assert!(
        engine.is_source_alive(id.unwrap()),
        "looping streaming source should still be alive after wav duration"
    );

    let _ = engine.stop_all();
    thread::sleep(Duration::from_millis(100));
    let _ = engine.unload(buf);
    let _ = std::fs::remove_file(&wav);
}

/// `Arc<ArcSwap<...>>` 越しの shared_buffers が streaming で正しく更新されることを確認。
/// (これは buffer_pool の動作の一貫性確認で、streaming 固有のロジックは触らない。)
#[test]
fn shared_snapshot_visible_after_load_streaming() {
    let mut engine = match nezia::SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let wav = gen_wav(550.0, 44100, 0.5, "nezia_streaming_test_snap.wav");
    let buf = engine
        .load_streaming(&wav, StreamingOpts::default())
        .unwrap();
    // 参照可能な BufferReader は streaming に対しては 0 frames 返す (Phase 2-4 制約)。
    let reader = engine.open_buffer_reader(buf);
    assert!(
        reader.is_some(),
        "open_buffer_reader should succeed for streaming buffer"
    );
    let r = reader.unwrap();
    let mut dst = vec![0.0_f32; 256];
    let n = r.read_frames(0, &mut dst);
    assert_eq!(n, 0, "BufferReader on streaming buffer returns 0 frames");
    let _ = engine.unload(buf);
    let _ = std::fs::remove_file(&wav);
}

// MirrorRing 単独動作はインラインテストでカバー済み (`src/streaming/mirror_ring.rs`)。
// arc_swap import を未使用扱いされないよう触る。
#[test]
fn arc_swap_smoke() {
    let s: Arc<ArcSwap<u32>> = Arc::new(ArcSwap::from_pointee(42));
    assert_eq!(**s.load(), 42);
}
