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

2スレッド間の通信は **データの寿命と意味論** によって 3 種類の経路に振り分ける。

```
メインスレッド                              サウンドスレッド
     │                                           │
     │── コマンドリングバッファ ────────────────→│  (生成/破棄/トポロジ変更)
     │                                           │
     │←── イベントリングバッファ ────────────────│  (再生完了、despawn、エラー)
     │                                           │
     │══ triple buffer (newest-wins) ═══════════→│  (リスナー姿勢、ソース位置)
     │                                           │
     │~~ AtomicU64 per-slot 共有メモリ ~~~~~~~~~→│  (volume/pitch/spatial_enabled)
     │                                           │
```

### コマンド経路（lock-free SPSC リングバッファ）

メインスレッド → サウンドスレッド。**順序保証が必要で、すべての発行を取りこぼさず処理したい**指示に使う。

- 生成・破棄: `Play`, `SpawnSource`, `SpawnBus`, `DespawnBus`, `StopAll`
- パラメータ変更（個別・低頻度・スカラー以外）: `SetVolume`, `SetBusGain`, `SetBusMuted`, `SetBusOutput`, `SetSourceSpatialParams`
- 状態遷移: `SeekSource`, `PauseSource`, `ResumeSource`, `StopSource`, `SetSourceLoop`
- トポロジ更新: `UpdateProcessOrder`

### 共有メモリ経路（triple buffer, newest-wins）

メインスレッド → サウンドスレッド。**毎フレーム流す状態で、最新値だけ届けばよく、複数フィールドの一貫スナップショットが必要**なデータに使う。

- リスナー姿勢（`ListenerState`）
- ソース位置の一括更新（`Vec<SourcePositionUpdate>`）

### Atomic per-slot 経路（共有 SoA, AtomicU64）

メインスレッド → サウンドスレッド。**個別 API でセットされる高頻度スカラー**に使う。
スロット index = `EntityId.index`、各スロット 64bit に `(generation, value_bits)` をパックして slot 再利用時の旧値混入を防ぐ。

- `set_source_volume`
- `set_source_pitch`
- `set_source_spatial_enabled`

メインスレッド側の API は個別呼び出し（`set_source_volume(id, v)`）の感覚を保ったまま、SPSC コマンドキューを介さず該当スロットへ直接 atomic store する。サウンドスレッドはオーディオコールバック冒頭で全アクティブソースぶんを 1 ループで atomic load → dense 配列に反映する（DoD 的な連続スキャン）。

詳細は [経路の選び方](#経路の選び方) と [なぜ Atomic per-slot を加えたか](#なぜ-atomic-per-slot-を加えたか) 参照。

### イベント経路（lock-free SPSC リングバッファ）

サウンドスレッド → メインスレッド。再生完了・despawn・エラーの通知を返す。

- `SourceFinished`: 自然終了したソースの callback token を通知
- `PlayFailed`: スロット枯渇等で spawn 失敗
- `SourceDespawned`: ソース despawn を通知し、メイン側スロットアロケータが index を再利用キューに戻すために使う（自然終了 / `StopSource` / `StopAll` のいずれの経路でも emit される）

すべて lock-free な SPSC（Single Producer, Single Consumer）構造とし、サウンドスレッド側でロックが発生しないことを保証する。

## 経路の選び方

3 経路の振り分け判断基準。

| 観点 | コマンド (ring buffer) | 共有メモリ (triple buffer) | Atomic per-slot |
|---|---|---|---|
| 順序保証 | あり (FIFO) | なし (newest-wins) | なし (newest-wins) |
| 中間値の扱い | 全部届く | 古いものは捨てられる | 古いものは捨てられる |
| 容量 | N 要素 (満杯時に詰まる) | T 1 個 × 3 スロット（詰まらない） | スロット数固定（詰まらない） |
| tearing | 起きない | **起きない**（atomic ポインタ swap で全体一貫公開） | スロット内（≤8byte）は不能、複数スロット間は起きうる |
| 適している用途 | 生成・破棄・トポロジ・状態遷移・離散イベント | 一貫スナップショットが必要な複合値（姿勢・座標一括） | 個別 API で更新される高頻度スカラー |
| メイン側コスト | atomic 1〜2 回（ringbuf push） | atomic 1 回（publish）+ 全要素書き換え | atomic 1 回（store） |
| サウンド側コスト | pop + match + resolve（ランダムアクセス） | スロット切替後にシーケンシャル走査 | dense ループ内で atomic load |

判断フロー:

1. **古い値が捨てられて困るか？** 困るならコマンド経路（順序保証 + 取りこぼしなし）。
2. **複数フィールドの一貫スナップショットが必須か？** Yes なら triple buffer（tearing 完全防止）。
3. **個別 API で頻繁に呼ばれるスカラー値か？** Yes なら Atomic per-slot（個別呼び出し1回 = atomic 1命令、SPSC キュー圧迫なし）。
4. **ハンドル発行や生成・破棄を伴うか？** 必ずコマンド経路（順序が必要）。

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

## なぜ Atomic per-slot を加えたか

`set_source_volume(id, v)` のような **個別 API かつ高頻度** なスカラー値更新を、SPSC コマンドや triple buffer に乗せるとそれぞれ別の問題が出る。

### SPSC コマンドに乗せたときの問題

旧実装は `Command::SetSourceVolume { id, vol }` をリングバッファに積んでいた。256 ソースぶんの volume を毎フレーム更新すると:

- メイン側: ringbuf push を 256 回（atomic 2 回/push）
- サウンド側: pop して `match` 分岐、`resolve(id)` で sparse → dense 変換、`vol[i]` に書き込み × 256
- `vol[3]` 書いた直後に他コマンドで `pitch[7]` を書く、と **dense 配列をまたいで飛ぶ書き込みパターン** が発生し L1 prefetcher が効かない
- キュー満杯（`QueueFull`）が起きると **silent に落ちる**（`set_source_volume` が false 返すが Unity 側で気付きにくい）

### Triple buffer に乗せたときの問題

メイン側で `pending_volumes[i] = v` を蓄積し、フレーム末尾に `publish()` する設計を検討したが:

- 個別 API の挙動が「次フレーム末まで反映されない」= **1 フレーム遅延が出る**（ゲームの当たり判定で音量変えても 1 frame 遅れる）
- メイン側に「pending 蓄積 → 詰めて publish」の専用バッファロジックが要る
- 結局 `pending` を毎フレーム enumerate する必要があり、書き込みが少ないフレームでも全体走査

### Atomic per-slot で解決

- メイン側: スロットへ atomic store 1 命令。レイテンシ追加ゼロ（次オーディオコールバックで反映 = SPSC と同じ）
- サウンド側: コールバック冒頭で **dense 配列を端から舐めながら** 各スロットを atomic load して書き写す。書き込み先は連続メモリ、L1 prefetcher が効く
- atomic load は x86/ARM64 では Relaxed ordering なら通常の load 命令と同一機械語コスト
- スロット数は `MAX_SOURCES` で固定 → メモリ予測可能（vol/pitch/spatial_enabled で 6 KB 程度）
- キュー満杯失敗が原理的に発生しない

### 二層 ID と slot allocator

Atomic per-slot は **`EntityId.index` を `[0, MAX_SOURCES)` に bounded** に保つ前提で成立する（固定配列の index にするため）。

このため `play_with_handle*` はメインスレッド側の `SourceSlotAllocator` から index を発行し、despawn 時はサウンドスレッドが `Event::SourceDespawned` を返してスロット index を再利用キューに戻す。スロット再利用時の旧値混入を防ぐため、各スロットの `AtomicU64` は上位 32 bit に generation をパックし、`EntityId.generation` と一致しない load は破棄する。

### 個別フィールド一貫性は保証しない

スロット 1 個（≤ 8 byte）の内部は単一 atomic op なので tearing しないが、**複数スロット間（例: 同一 EntityId の volume と pitch を別々に store する場合）は順序保証なし**。ただし NEZIA の用途では:

- volume と pitch が時間的に1コールバックぶんズレても音響的に検知不能
- 複合値で一貫性が必要なもの（リスナー姿勢、座標一括更新）は triple buffer 経由とする

この棲み分けにより「単一フィールドは Atomic per-slot、複合値は triple buffer」という明快な分離になっている。

## ソース position の経路選択

ソース position（`[f32; 3]`）は **triple buffer の `batch_set_source_positions` 経由のみ** とし、Atomic per-slot 化はしない。判断根拠:

1. **`[f32; 3]` は 12 byte で `AtomicU64` 1 個に収まらない**。3 個に分けると同一フレームの x/y/z の間に tearing が入る余地がある。
2. ゲームオーディオ用途では位置 tearing は音響的に検知不能だが、それでも triple buffer で一貫公開できるなら downgrade する理由がない。
3. **個別 API（`set_source_position`）感覚は Integration 層（Unity ラッパ等）で吸収できる**。各ソースの desired position を C# 側に持ち、フレーム末尾に配列化して `nezia_source_batch_set_positions` を 1 回呼ぶ形にすれば、ユーザコードからは `src.position = v` の素朴な書き味になる。
4. core 側に経路を増やすほど両 API 混在時の挙動仕様（後勝ち順序など）が増える。Integration 層に押し出すことで core API は最小に保てる。
5. 既存実装は alloc 0（FFI 層で zero-copy slice cast 化済み）/ tearing 0 / レイテンシ最短で、欠点が見つからない。

これは **「個別 API 感覚は Integration 層、core は責務最小」** という棲み分けの一例で、他の同種の判断（高レベル API ラッパも core 非搭載）と整合する。

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
