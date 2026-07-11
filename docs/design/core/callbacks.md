# コールバック / イベント設計

NEZIA ENGINE はサウンドスレッドのリアルタイム制約上、**コールバックをサウンドスレッドから直接呼び出すことができない**。
代わりに「サウンドスレッドが軽量なイベントをリングバッファに push → メインスレッドが poll して登録済みコールバックを呼び出す」という二段構えを採る。

## アーキテクチャ

```
サウンドスレッド                            メインスレッド
  │                                             │
  │ 発生事象を Event (Copy, 16B) として          │
  │ emit_event() → SPSC リングに try_push       │
  │ (失敗は event_queue_full で計数)             │
  │──── イベントリングバッファ (SPSC) ─────────→ │ poll_events() で毎フレームドレイン
  │     (容量 = event_ring_capacity())           │  ├─ event_sink があれば全イベントを横流し
  │                                             │  ├─ SourceFinished → callbacks[token] 発火
  │                                             │  ├─ SourceDespawned → スロット回収 (内部)
  │                                             │  └─ 最後にソース状態スナップショット取込
```

- **イベントリングバッファ**: SPSC lock-free。サウンドスレッドはヒープ確保なしで push する。
  push は必ず `emit_event()` ヘルパを経由し、満杯による失敗を
  `EngineMetrics::event_queue_full` で計数する (`let _ = try_push` の黙殺は禁止)。
- **コールバック管理**: `CallbackRegistry` — 固定サイズのスロット配列 (容量 = `max_sources`)。
  HashMap ではない (旧設計から変更、後述)。
- **トークン方式**: `play_*_with_callback()` 呼び出し時にメインスレッドが `u32` トークンを
  発行してコマンドに埋め込む。サウンドスレッドはイベントにそのトークンを載せて返す。
- **観測用 sink**: `set_event_sink()` で登録した closure に、drain した**全イベント**が
  種別を問わず流れる (daemon の `SubscribeEvents` 転送用)。コールバック機構とは独立。

## イベント種別 (実装済み)

`src/event.rs` の `Event` enum。リングに積むため固定サイズ・`Copy` 必須。

| イベント | ペイロード | 発火タイミング | poll_events での処理 |
|---|---|---|---|
| `SourceFinished` | `token: u32` | Source がバッファ末尾まで再生して自然終了 | token のコールバックを発火 |
| `PlayFailed` | `token: u32` | ボイス上限到達で spawn 拒否 | コールバックを解放のみ (呼ばない) |
| `SourceDespawned` | `id: EntityId` | despawn 全般 (自然終了 / Stop / StopAll) | source/send スロット回収 (内部処理) |
| `StreamingUnderrun` | `buffer: BufferId` | ストリーミングのデコードが再生に未達 | no-op (観測は event_sink 経由のみ) |
| `CaptureOverflow` | `dropped_samples: u32` | キャプチャリング満杯でサンプル破棄 | no-op (累積は `CaptureReader` 側) |

- `SourceFinished` と `SourceDespawned` は自然終了時に**両方**発火する
  (前者はコールバック用、後者はスロット回収用)。
- `token` はコールバックレジストリの内部表現であり、外部プロトコル (daemon proto) には
  露出しない。daemon はソースの終了を `SourceDespawned` → `SourceStopped` として配信する。

## CallbackRegistry (スロット配列 + 世代トークン)

`src/core/engine/callback_registry.rs`。

- **固定サイズのスロット配列** (容量 = `EngineConfig::max_sources`)。旧設計の
  `HashMap<u32, Box<dyn FnOnce()>>` は spawn ごとの alloc + hash lookup が
  かかるため置き換えた。
- **トークン形式**: `token: u32 = (generation: u16) << 16 | (slot index: u16)`
  - generation はスロット再利用ごとに +1 (0 は欠番)。stale token の誤発火を防ぐ。
  - 全 generation の初期値を 1 にすることで有効 token は `>> 16 != 0` が保証され、
    `token == 0` を「コールバックなし」の予約値にできる。
- **2 種類のコールバック実体** (`CallbackKind`):
  - `Rust(Box<dyn FnOnce() + Send>)` — Rust API 用。Box 1 個の alloc (dyn の原理上不可避)
  - `Native { fn ptr, user_data }` — FFI 用。**spawn ごとの alloc ゼロ**。
    呼出側 (C#) が発火時まで `user_data` を有効に保つ契約

## 公開 API (実装済み)

```rust
// Rust API — closure 登録
engine.play_with_callback(buffer, vol, pitch, looping, || println!("再生終了"));
engine.play_to_bus_with_callback(buffer, vol, pitch, bus, looping, cb);
engine.play_with_handle_and_callback(buffer, vol, pitch, bus, looping, priority, spatial, cb);

// FFI — 関数ポインタ登録 (alloc ゼロ)。csbindgen で C# へ露出
// nezia_play_with_callback(engine, buffer, vol, pitch, looping, fn_ptr, user_data)

// 観測用 sink — 全イベントの横流し (daemon の SubscribeEvents が使用)
engine.set_event_sink(|ev| { let _ = tx.send(ev); });
engine.clear_event_sink();

// メインループの毎フレーム末尾で呼ぶ
engine.poll_events();
```

## リング容量とオーバーフロー方針

**方針: 「溢れたら捨てる」ではなく「構造上溢れない容量を確保し、万一の溢れは計数する」。**

- 容量は `event_ring_capacity(max_sources) = max(2 × max_sources + 32, 64)`。
  - worst-case バーストは `stop_all` が 1 audio callback 内で全生存ソースの
    `SourceDespawned` を一括発行するケース (= max_sources 件)。
  - 自然終了は `SourceFinished` + `SourceDespawned` の 2 件/ソースになり得るため 2 倍。
  - **スロット再利用はメインスレッドの `poll_events()` を経由しないと起きない**ため、
    未処理イベント数はこの上限を構造的に超えない。
  - イベントは 1 件 16B。max_sources = 1024 でも約 33KB と安価。
- それでも push に失敗した場合は `EngineMetrics::event_queue_full` に計数され、
  `SoundEngine::dropouts()` (`DropoutStats::event_queue_full`) で観測できる。
  **この値が 0 以外 = イベントロスが起きている**。`SourceDespawned` のロスは
  スロットリーク (ソース上限の恒久的減少)、`SourceFinished` のロスはコールバック不発 +
  レジストリスロットのリークに直結するため、容量式か `poll_events()` の呼び出し頻度を
  見直すこと。

## イベント追加時のレート予算ガイドライン

この経路は**制御プレーン** (ライフサイクル頻度、高々数百件/秒) を前提に設計されている。
新しいイベント種別を追加するときは発火レートを見積もり、次の基準で経路を選ぶ:

| レート | 経路 |
|---|---|
| ライフサイクル頻度 (play/stop 単位) | この Event リング |
| 高頻度・周期的 (例: `LoopPoint` の短ループ、毎フレームのメーター値) | **リングに乗せない**。共有メモリ side-channel ([capture.md](capture.md) の SPSC パターン、daemon CONCEPT の Phase 4-3 予約) を使う |

例: `LoopPoint` を 50ms ループ × 64 ソースで発火させると 1,280 件/秒になり、
リング容量とメインスレッド drain の前提を壊す。導入時はレート制限 (per-source
間引き) か side-channel を検討する。

## 制約・注意事項

### StopAll はコールバックを呼ばない

`stop_all()` は `SourceWorld` ごと破棄するため、個別の「停止による終了」通知は
`SourceDespawned` (スロット回収用) しか流れない。登録済みの `on_finish` コールバックは
`CallbackRegistry::clear()` で解放されるが**呼び出されない**。

→ `stop_all()` 後に後処理が必要な場合は呼び出し側でハンドリングする。
   daemon 経由の購読者は `SourceStopped` イベント (SourceDespawned 由来) で観測できる。

### PlayFailed は on_finish を呼ばない

再生失敗時、登録済みコールバックは解放されるが呼び出されない。失敗のハンドリングは
`event_sink` (全イベント横流し) で `PlayFailed` を拾うか、`dropouts().dropped_play_calls`
を監視する。専用の `on_play_failed` コールバックは未実装 (計画中)。

### コールバックの実行スレッドとコスト

- コールバックと event_sink は **`poll_events()` を呼んだスレッドで同期実行**される。
  daemon ではエンジン専用スレッド、ゲーム統合ではメインスレッド。
- 重い処理は poll ループ (daemon なら RPC 処理、ゲームならフレーム) を止める。
  **コールバック / sink はチャネル送信程度の軽量処理に留める**こと。
- サウンドスレッドの制約 (ロック禁止等) はここでは適用されないが、
  ゲームエンジンのスレッドモデルには合わせること。

## 計画中のイベント

主要ミドルウェア (FMOD Studio / Wwise / SoLoud) の調査に基づく優先度評価。
追加時は上記レート予算ガイドラインに従うこと。

| イベント | 発火タイミング | 優先度 | 参考 | レート懸念 |
|---|---|---|---|---|
| `SourceStopped` | `stop()` で明示停止された (despawn と区別した通知) | 高 | FMOD `STOPPED` | なし |
| `PlayStarted` | 実際に最初のサンプルを出力した | 中 | FMOD `STARTED` | なし |
| `LoopPoint` | ループ境界を通過した | 中 | Wwise Marker | **あり** (短ループ × 多ソース) |
| `RealToVirtual` / `VirtualToReal` | ボイス仮想化 / 復帰 | 高 | FMOD | 中 (voice steal 頻発時) |
| `Starvation` | ストリーミング読み込み未達でフレーム落ち | 高 | Wwise `AK_Starvation` | なし |
| `DeviceLost` | 出力デバイス切断 / ドライバエラー | 高 | cpal error callback | なし |

## 関連ドキュメント

- [スレッドモデル](threading.md) — イベントリングは「イベント経路」の実体
- [マスター出力キャプチャ](capture.md) — 高頻度データ用 SPSC の参考実装 (side-channel の雛形)
- [daemon コンセプト](../daemon/CONCEPT.md) — `SubscribeEvents` (event_sink の下流) と
  Phase 4-3 共有メモリ side-channel の予約
