//! `stop_source_many` のバッチ停止結合テスト。
//!
//! 旧経路ではステージ終端で N 個の source を `stop_source` で個別停止すると
//! SPSC コマンドリング (容量 128) が即詰まり、`QueueFull` が頻発していた。
//! 本 API は最大 `STOP_SOURCE_BATCH_MAX` (= 32) 件を 1 コマンドに詰めて送る。
//!
//! 実機オーディオデバイスが無い環境では `SoundEngine::new()` が失敗するため
//! early return で skip する (CI 互換、既存テストと同じパターン)。

use std::thread::sleep;
use std::time::Duration;

use nezia::{SoundEngine, SpawnSpatialInit};

fn make_buffer(engine: &mut SoundEngine) -> nezia::BufferId {
    engine.load_from_pcm(vec![0.5_f32; 100], 1, 48_000)
}

#[test]
fn stop_source_many_enqueues_all_in_single_call() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let buf = make_buffer(&mut engine);
    let bus = engine.master_bus();

    // 256 voice をまとめて spawn。spawn 自体がリング 1 個ずつ消費するので、
    // 32 件ごとに小休止して audio thread (cpal callback) がリングを drain
    // する時間を作る。実機での bench フローもフレームをまたぐので等価。
    let mut ids = Vec::with_capacity(256);
    for batch in 0..8 {
        for _ in 0..32 {
            if let Some(id) =
                engine.play_with_handle(buf, 1.0, 1.0, bus, false, 128, SpawnSpatialInit::NONE)
            {
                ids.push(id);
            }
        }
        // audio callback が走るまで待つ (48kHz、典型 ~10ms 周期)。
        let _ = batch;
        sleep(Duration::from_millis(20));
    }

    // 一括 stop の前にリング drain を保証。
    sleep(Duration::from_millis(40));
    let before = engine.dropouts().command_queue_full;

    // ids.len() が 256 未満でも (MAX_SOURCES 等で取り切れない場合) 重要なのは
    // 「spawn できた N 個を stop_source_many 1 呼び出しで全部 enqueue できる」点。
    let sent = engine.stop_source_many(&ids);
    assert_eq!(
        sent,
        ids.len(),
        "spawn できた {} 件すべてが enqueue されるはず",
        ids.len()
    );

    let after = engine.dropouts().command_queue_full;
    assert_eq!(after, before, "stop_source_many は QueueFull を起こさない");

    // 旧経路と比較: ceil(N/32) コマンドしか消費しない。
    // 256 voice ≦ 8 コマンド ≦ COMMAND_RING_CAPACITY。
    assert!(
        ids.len() <= 32 * 8,
        "STOP_SOURCE_BATCH_MAX (=32) で N=256 を 8 コマンドに圧縮できる前提"
    );
}

#[test]
fn stop_source_many_handles_empty_input() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let sent = engine.stop_source_many(&[]);
    assert_eq!(sent, 0);
}
