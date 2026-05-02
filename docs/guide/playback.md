# 音の再生

最も基本的なユースケース。SE やボイスを 1 回鳴らす。

## ロードとアンロード

```rust
let buf = engine.load("assets/explosion.wav")?;
// ...
engine.unload(buf); // bool（既にアンロード済みなら false）
```

ロードは I/O とデコードを伴うため、可能なら起動時にまとめて行う。

## fire-and-forget 再生

```rust
let ok = engine.play(buf, /* vol */ 1.0, /* pitch */ 1.0);
```

- `vol`: 線形ゲイン（0.0〜）。1.0 で原音、0.5 で約 -6 dB。
- `pitch`: 1.0 で原音、0.5 で 1 オクターブ下、2.0 で 1 オクターブ上。
- 戻り値が `false` のときは `buf` がアンロード済みなどで失敗。

マスターバス以外に送るには `play_to_bus` を使う。

```rust
let sfx_bus = engine.create_bus(1.0).unwrap();
let _ = engine.play_to_bus(buf, 1.0, 1.0, sfx_bus);
```

## 同時発音

`play` を続けて呼ぶだけでよい。同時発音数には上限があり、超えた呼び出しは `false` を
返す。古い音を間引くような優先度制御は行わないので、上限管理は呼び出し側の責務。

```rust
let _ = engine.play(buf, 0.6, 1.0);
let _ = engine.play(buf, 0.4, 1.5);
let _ = engine.play(buf, 0.3, 0.75);
```

## 再生終了を知る — コールバック

自然終了を検知したい場合は `play_with_callback` を使う。

```rust
let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
let done2 = done.clone();
engine.play_with_callback(buf, 1.0, 1.0, move || {
    done2.store(true, std::sync::atomic::Ordering::Relaxed);
});

// ゲームループで…
engine.poll_events(); // コールバックはこの中で呼ばれる
```

注意点:

- コールバックは `poll_events()` を呼んだスレッドで実行される。
- `stop_all()` で打ち切られた音や、同時発音数の上限で失敗した場合は
  コールバックは呼ばれずに解放される。
- 「鳴り終わったかどうか」を知る他の手段はない。チェックしたければコールバックを使う。

バス指定版は `play_to_bus_with_callback`。

## マスター音量

```rust
engine.set_volume(0.5); // マスターバスの gain を変更
```

`set_volume(v)` は内部的に `set_bus_gain(master_bus, v)` と等価。
バス側の API（[バスとルーティング](bus.md)）でも同じことができる。

## 全停止

```rust
engine.stop_all();
```

すべてのソースを即座に停止する。登録済みのコールバックは呼ばれずに解放される。
シーン遷移やポーズ画面の切り替えに使う。
