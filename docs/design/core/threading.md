# スレッドモデル

NEZIA ENGINE は **メインスレッド** と **サウンドスレッド** の2スレッド構成をとる。

## サウンドスレッド

オーディオコールバックを処理するリアルタイムスレッド。
OS のオーディオドライバから一定間隔（例: 5ms ごとに 256 サンプル）で呼び出され、バッファを埋めて返す。

### 厳格な制約

サウンドスレッドでは以下の操作を **禁止** する。

| 禁止操作 | 理由 |
|---|---|
| ロック（Mutex, RwLock 等） | ロック競合時にスレッドがブロックされ、バッファアンダーランが発生する |
| ヒープメモリ確保・解放（Box, Vec::push, String 等） | アロケータ内部のロックやシステムコールでレイテンシが不確定になる |
| I/O（ファイル読み書き、ログ出力） | カーネルのスケジューリングにより遅延が発生する |
| システムコール全般 | カーネルモードへの遷移は遅延を保証できない |

これらに違反するとオーディオグリッチ（プチノイズ・途切れ）の原因となる。

### 許可される操作

- 事前確保済みバッファへの読み書き
- アトミック操作（`AtomicU32`, `AtomicBool` 等）
- lock-free なリングバッファ経由のメッセージ送受信
- 固定長配列・スタック上での演算

## メインスレッド

ゲームエンジンのメインループから呼び出されるスレッド。
リソース管理、コマンド発行、状態の同期を担当する。

### 責務

- サウンドオブジェクトの生成・破棄（スパースセットへの挿入・削除）
- アセットのロード・アンロード（デコード済みバッファの確保）
- パラメータ変更コマンドの発行
- サウンドスレッドからのイベント受信（再生完了通知等）

メインスレッドには リアルタイム制約はない。ロックやメモリ確保を自由に行える。

## スレッド間通信

2スレッド間の通信は **データの寿命と意味論** によって 2 種類の経路に振り分ける。

```
メインスレッド                              サウンドスレッド
     │                                           │
     │── コマンドリングバッファ ────────────────→│  (生成/破棄/トポロジ変更)
     │                                           │
     │←── イベントリングバッファ ────────────────│  (再生完了、エラー通知)
     │                                           │
     │══ triple buffer (newest-wins) ═══════════→│  (リスナー姿勢、ソース位置)
     │                                           │
```

### コマンド経路（lock-free SPSC リングバッファ）

メインスレッド → サウンドスレッド。**順序保証が必要で、すべての発行を取りこぼさず処理したい**指示に使う。

- 生成・破棄: `Play`, `SpawnSource`, `SpawnBus`, `DespawnBus`, `StopAll`
- パラメータ変更（個別・低頻度）: `SetVolume`, `SetBusGain`, `SetBusMuted`, `SetBusOutput`, `SetSourceSpatialParams`, `SetSourceSpatialEnabled`
- トポロジ更新: `UpdateProcessOrder`

### 共有メモリ経路（triple buffer, newest-wins）

メインスレッド → サウンドスレッド。**毎フレーム流す状態で、最新値だけ届けばよい**データに使う。

- リスナー姿勢（`ListenerState`）
- ソース位置の一括更新（`Vec<(EntityId, [f32; 3])>`）

### イベント経路（lock-free SPSC リングバッファ）

サウンドスレッド → メインスレッド。再生完了やエラーなどの通知を返す。

すべて lock-free な SPSC（Single Producer, Single Consumer）構造とし、サウンドスレッド側でロックが発生しないことを保証する。

## 経路の選び方

「コマンド」と「共有メモリ」をどう振り分けるかの判断基準。

| 観点 | コマンド (ring buffer) | 共有メモリ (triple buffer) |
|---|---|---|
| 順序保証 | あり (FIFO) | なし (newest-wins) |
| 中間値の扱い | 全部届く | 古いものは捨てられる |
| 容量 | N 要素 (満杯時に詰まる) | 常に最新 1 要素分（詰まらない） |
| 適している用途 | 生成・破棄・トポロジ・離散イベント | 連続的な状態 (姿勢、座標) |
| サイズ依存性 | enum 最大バリアント × cap で固定 | T のサイズ × 3 スロット |

判断フロー:

1. **古い値が捨てられて困るか？** 困るならコマンド経路。
2. **N 体ぶんのデータをまとめて毎フレーム送るか？** Yes なら共有メモリ。No ならコマンド経路で十分。
3. **ハンドル発行や生成・破棄を伴うか？** 必ずコマンド経路（順序が必要）。

## なぜ triple buffer なのか

毎フレーム流れる「最新値だけ要る」状態をリングバッファに乗せると以下の問題が起きる:

- **enum 最大バリアントの呪縛**: ringbuf に乗る `Command` enum は最大バリアント基準でスロット幅が決まるため、`SetListener (12 bytes)` を送るためのスロットが `BatchSetSourcePositions (~640 bytes)` の幅で確保されてしまう。リング容量 128 で **80 KB** 級の常駐になる。
- **満杯リスク**: 状態更新が頻繁だと FIFO が詰まる。詰まったら最新値が落ちる（最も損したいケースで損する）。
- **直列ループ化**: N 体の位置を `BatchSetSourcePositions` などにまとめても、サウンドスレッド側は「分割された複数コマンドを 1 個ずつ pop して match → 個別 entity に書き込み」と直列処理になる。ECS の SoA 一括処理が活きない。

triple buffer は newest-wins なので、

- 容量は常に T 1 個分 × 3 スロット（ソース位置 256 体で約 9 KB）
- 詰まらない（書き手も読み手もブロックしない）
- 1 度の `update()` で N 体ぶんの最新スナップショットをまとめて受け取れる
- alloc ゼロ（初期化時に全スロット確保、以降は in-place 上書き）

の 3 点が同時に成立する。

### パフォーマンス比較（実測値, MAX_SOURCES = 256）

毎フレーム「リスナー更新 + 全ソース位置更新」を測定:

| 経路 | time/frame | 静的メモリ | per-frame alloc |
|---|---|---|---|
| Command ring buffer | 1100 ns | ~21 KB（ring cap=32 時、cap=128 では ~82 KB） | 0 |
| ArcSwap<Vec> (alloc あり) | 520 ns | 3 KB | 3 allocs / ~3 KB |
| **triple buffer**（採用） | **280 ns** | **9 KB** | **0** |

ring buffer 比 **約 4 倍高速 + 常駐メモリ削減 + alloc ゼロ**。詳細は `crates/core/examples/bench_commands.rs`。

## 実装上の落とし穴

### 順序: コマンド処理 → 共有メモリ反映

サウンドコールバック内では **必ずコマンドを先にドレインしてから triple buffer を反映**する。

```rust
// audio callback
while let Some(cmd) = command_consumer.try_pop() {
    apply(cmd); // SpawnSource などで source_world に entity を追加
}
if listener_output.update() {
    spatial_world.listener = *listener_output.output_buffer_mut();
}
if position_updates_output.update() {
    for (id, pos) in position_updates_output.output_buffer_mut().iter() {
        if let Some(dense) = source_world.resolve(*id) {
            spatial_world.set_position(dense, *pos);
        }
    }
}
```

逆順にすると、メインスレッドが「`play_with_handle` → 直後に `batch_set_source_positions`」と発行した場合に、

1. triple buffer 反映: source 未生成 → `resolve(id)` が None → 位置更新が捨てられる
2. コマンド処理: source 生成（デフォルト位置 [0,0,0]）
3. ミキシング: 原点で再生 = リスナーと同位置 = **最大音量で鳴る**

という事故になる（newest-wins なので次フレームで main が再 publish するまで位置が反映されない）。

### Vec を triple buffer に載せるときの容量

`triple_buffer::triple_buffer(&initial)` は `initial` を 3 回 clone する。`Vec::clone()` は `len` ぶんの容量しか確保しないため、空 Vec を渡すと 3 スロットすべて capacity=0 になり、メインスレッド側で `extend_from_slice` するたびに realloc が発生する。

対策: 初期値を `vec![dummy; MAX_SOURCES]` で渡し、すべてのスロットに最大容量を持たせる。以後はメインスレッドで `clear()` + `extend_from_slice()` するだけで再確保が起きない。

```rust
let positions_initial: Vec<(EntityId, [f32; 3])> = vec![(default_id, [0.0; 3]); MAX_SOURCES];
let (mut input, output) = triple_buffer::triple_buffer(&positions_initial);
// 初期ダミーデータが apply されないよう、空 Vec で 1 度 publish して flush。
input.input_buffer_mut().clear();
input.publish();
```

## Rust での安全性担保

サウンドスレッドの制約を型システムで可能な限り強制する。

- サウンドスレッドに渡す型には `Send` を要求する。
- サウンドスレッド内で使うデータ構造は事前確保済みの固定容量型（例: `ArrayVec`, 固定長リングバッファ）を使い、実行時にヒープ確保が発生しない設計とする。
- `#[cfg(debug_assertions)]` で実行時にアロケータフックを仕込み、サウンドスレッドでの確保を検出してパニックさせるデバッグ機構を設ける。
