//! シナリオ: ストリーミング再生 (Phase 2-4)
//!
//! cargo run --example demo_streaming
//!
//! カバー範囲:
//!   - load_streaming: 長尺 BGM をフルロードせず再生開始
//!   - play_with_handle: 静的バッファと同じ API で利用可能
//!   - set_streaming_loop: 全体ループ
//!   - seek_streaming: 任意位置へジャンプ
//!   - unload: ワーカ join

use std::thread;
use std::time::Duration;

use nezia::{SoundEngine, StreamingOpts};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("╔══════════════════════════════════════╗");
    println!("║   demo_streaming: BGM ストリーミング ║");
    println!("╚══════════════════════════════════════╝");

    // sandbox/maou_bgm_orchestra25.mp3 を使う (リポジトリ同梱のフリー BGM)。
    let bgm_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("sandbox")
        .join("maou_bgm_orchestra25.mp3");
    if !bgm_path.exists() {
        eprintln!("BGM ファイルが見つかりません: {:?}", bgm_path);
        return Ok(());
    }

    let mut engine = SoundEngine::new()?;
    println!("\n━━━ ストリーミングロード");
    let bgm = engine.load_streaming(&bgm_path, StreamingOpts::default())?;
    println!(
        "  [OK] load_streaming() => index={} gen={}",
        bgm.index, bgm.generation
    );

    let master = engine.master_bus();

    println!("\n━━━ 再生 (3 秒)");
    let id = engine
        .play_with_handle(bgm, 0.6, 1.0, master, false)
        .ok_or("play_with_handle failed")?;
    thread::sleep(Duration::from_secs(3));

    println!("\n━━━ ループ有効化 + 続けて再生 (3 秒)");
    let _ = engine.stop_source(id);
    thread::sleep(Duration::from_millis(100));
    engine.set_streaming_loop(bgm, true);
    let id = engine
        .play_with_handle(bgm, 0.6, 1.0, master, true)
        .ok_or("play_with_handle failed")?;
    thread::sleep(Duration::from_secs(3));

    println!("\n━━━ シーク (10 秒地点へジャンプ)");
    engine.seek_streaming(bgm, 44100 * 10);
    thread::sleep(Duration::from_secs(3));

    println!("\n━━━ クリーンアップ");
    let _ = engine.stop_source(id);
    thread::sleep(Duration::from_millis(200));
    engine.unload(bgm);
    println!("  [OK] unload (worker join 完了)");
    println!("\n✓ demo_streaming: 完了\n");
    Ok(())
}
