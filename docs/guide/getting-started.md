# はじめに

NEZIA ENGINE をプロジェクトに組み込み、最初の音を鳴らすまでの手順。

## 動作要件

- Rust edition 2024 が利用可能なツールチェイン
- macOS / Windows / Linux

## 依存に追加する

```toml
[dependencies]
nezia = { path = "crates/core" }
```

外部プロジェクトから使う場合は git 依存などに置き換える。

## 最小構成: 音を鳴らす

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use nezia::SoundEngine;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut engine = SoundEngine::new()?;
    let buf = engine.load("assets/explosion.wav")?;

    let finished = Arc::new(AtomicBool::new(false));
    let f = finished.clone();
    engine.play_with_callback(buf, 1.0, 1.0, move || {
        f.store(true, Ordering::Relaxed);
    });

    while !finished.load(Ordering::Relaxed) {
        engine.poll_events();
        std::thread::sleep(Duration::from_millis(16));
    }
    Ok(())
}
```

対応フォーマットは WAV / MP3 / FLAC / Ogg Vorbis。

## ゲームループでの使い方

毎フレームの末尾で `poll_events()` を呼ぶ。
これを忘れるとコールバックが呼ばれない
（公式 Integration を使う場合は内部で吸収されるため不要）。

```rust
loop {
    // ... ゲームの更新処理 ...

    engine.set_listener(player_pos, player_forward, player_up);
    engine.batch_set_source_positions(&source_positions);

    engine.poll_events(); // ← 毎フレーム必須
}
```

`SoundEngine` はアプリ起動時に 1 つ生成し、終了まで保持する。drop すると音が止まる。

## 戻り値の扱い

多くのメソッドは `bool` を返す。`false` のときは無効なハンドルや一時的な失敗を意味し、
panic はしない。安全に無視できる。

```rust
if !engine.play(buf, 1.0, 1.0) {
    // 失敗した（buf が無効など）
}
```

`#[must_use]` が付いているので、捨てたいときは `let _ = ...` を明示する。

## 次に読むもの

- [基本概念](concepts.md) — バッファ・ソース・バスの関係と ID の意味
- [音の再生](playback.md) — 再生 API の詳細
