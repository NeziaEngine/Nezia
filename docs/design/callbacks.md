# コールバック設計

NEZIA ENGINE はサウンドスレッドのリアルタイム制約上、**コールバックをサウンドスレッドから直接呼び出すことができない**。
代わりに「サウンドスレッドが軽量なイベントをリングバッファに push → メインスレッドが poll して登録済みコールバックを呼び出す」という二段構えを採る。

## アーキテクチャ

```
サウンドスレッド                            メインスレッド
  │                                             │
  │ Source 再生終了を検出                        │
  │ → Event::SourceFinished { token } を         │
  │   イベントリングバッファに push              │
  │──── イベントリングバッファ (SPSC) ─────────→ │
  │     (固定サイズ・lock-free)                  │ poll_events() で毎フレームドレイン
  │                                             │ callbacks[token]() を呼ぶ
```

- **イベントリングバッファ**: SPSC lock-free。サウンドスレッドはヒープ確保なしで push する。
- **コールバック管理**: メインスレッドが `HashMap<token, Box<dyn FnOnce() + Send>>` で保持。
- **トークン方式**: `play_with_callback()` 呼び出し時にメインスレッドが `u32` トークンを発行し、コマンドに埋め込む。サウンドスレッドはイベントにそのトークンをそのまま載せて返す。

## イベント種別と優先度

### 実装済み

| イベント | 発火タイミング | 優先度 |
|---|---|---|
| `SourceFinished` | Source がバッファ末尾まで再生して自然終了した | **高** |
| `PlayFailed` | `play_with_callback()` 時に `MAX_SOURCES` 上限に達し再生できなかった | **高** |

### 計画中

主要なサウンドミドルウェア（FMOD Studio・Wwise・SoLoud）の調査に基づく優先度評価。

| イベント | 発火タイミング | 優先度 | 参考 |
|---|---|---|---|
| `SourceStopped` | `stop()` / `stop_all()` で明示的に停止された | 高 | FMOD `STOPPED` |
| `PlayStarted` | Source が最初のサンプルを出力した（コマンド送信ではなく実際の発音開始） | 中 | FMOD `STARTED` |
| `LoopPoint` | ループ再生でループ境界を通過した | 中 | Wwise Marker / GameMaker Loop Points |
| `RealToVirtual` | ボイス数上限でボイスが仮想化（無音化）された | 高 | FMOD `REAL_TO_VIRTUAL` |
| `VirtualToReal` | 仮想化されたボイスが復帰して再び発音した | 高 | FMOD `VIRTUAL_TO_REAL` |
| `Starvation` | ストリーミング読み込みが間に合わずフレームを落とした | 高 | Wwise `AK_Starvation` |
| `DeviceLost` | 出力デバイスが切断された / ドライバエラー | 高 | cpal error callback |

## 制約・注意事項

### StopAll はコールバックを呼ばない

`stop_all()` は内部で `SourceWorld` ごと破棄するため、サウンドスレッドが個別の `SourceStopped` イベントを発火する機会がない。
登録済みの `on_finish` コールバックは解放されるが**呼び出されない**。

→ `stop_all()` 後に後処理が必要な場合は、呼び出し側でハンドリングすること。

### PlayFailed は on_finish を呼ばない

`PlayFailed` イベントを受け取った場合、登録済みの `on_finish` コールバックは解放されるが呼び出されない。
再生失敗をハンドリングしたい場合は `on_play_failed` コールバック（計画中）、またはイベントの生ドレイン API を使用する。

### コールバックの実行スレッドはメインスレッド

`poll_events()` を呼んだスレッドでコールバックが実行される。
コールバック内でサウンドスレッドの制約（ロック禁止・ヒープ確保禁止）は不要だが、ゲームエンジンのスレッドモデルに合わせること。

## API 設計（予定）

```rust
// コールバック付き再生（マスターバスへ）
engine.play_with_callback(buffer, vol, pitch, || {
    println!("再生終了");
});

// コールバック付き再生（指定バスへ）
engine.play_to_bus_with_callback(buffer, vol, pitch, sfx_bus, || {
    despawn_entity(entity);
});

// メインループの毎フレーム末尾で呼ぶ
engine.poll_events();
```

## 実装方針

### Event 型

```rust
// src/event.rs

/// サウンドスレッド → メインスレッド方向のイベント。
/// 固定サイズ・Copy が必須（リングバッファに積むため）。
#[derive(Debug, Clone, Copy)]
pub enum Event {
    SourceFinished { token: u32 },
    PlayFailed     { token: u32 },
}
```

### Command への token 追加

```rust
// src/command.rs（抜粋）
Command::Play {
    audio_buffer_index: u32,
    vol: f32,
    pitch: f32,
    token: u32,   // 0 = コールバックなし
},
```

### SourceSystem での発火

```rust
// SourceSystem::update() の signature に emit_event を追加
pub fn update(
    world: &mut SourceWorld,
    ...,
    emit_event: &mut dyn FnMut(Event),
)

// 再生終了時（バッファ末尾到達）のみ SourceFinished を発火し、despawn する
if natural_finish && token != 0 {
    emit_event(Event::SourceFinished { token });
}
world.despawn_by_dense_index(source_i);
```

`emit_event` は `dyn FnMut` なので ringbuf を直接 source/system.rs に import しなくて済む。
engine.rs 側でクロージャとして `|ev| { let _ = event_producer.try_push(ev); }` を渡す。

### SoundEngine の変更点

```rust
pub struct SoundEngine {
    command_producer: HeapProd<Command>,
    event_consumer:  HeapCons<Event>,       // 追加
    _stream: Stream,
    buffer_pool: AudioBufferPool,
    bus_routing: BusRoutingMirror,
    callbacks:   HashMap<u32, Box<dyn FnOnce() + Send>>,  // 追加
    next_token:  u32,                                      // 追加
}
```

```rust
/// ゲームループの毎フレーム末尾で呼ぶ。
/// 登録済みの on_finish コールバックを呼び出す。
pub fn poll_events(&mut self) {
    while let Some(ev) = self.event_consumer.try_pop() {
        match ev {
            Event::SourceFinished { token } => {
                if let Some(cb) = self.callbacks.remove(&token) {
                    cb();
                }
            }
            Event::PlayFailed { token } => {
                // コールバックを解放するのみ（呼び出しは行わない）
                self.callbacks.remove(&token);
            }
        }
    }
}
```
