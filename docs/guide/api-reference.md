# API リファレンス

`nezia` クレートが公開している型とメソッドの一覧。
シグネチャは `crates/core/src/lib.rs` を起点に再エクスポートされている。

## 公開型

| 型 | 役割 |
|---|---|
| `SoundEngine` | サウンドエンジン本体。アプリで 1 個保持する |
| `BufferId` | ロード済みオーディオへのハンドル |
| `EntityId` | バス・3D ソースの実行時ハンドル |
| `AttenuationModel` | 距離減衰モデルの列挙体（`None` / `Linear` / `InverseDistance` / `Exponential`）|

## SoundEngine の主要メソッド

### ライフサイクル

| メソッド | 戻り値 | 説明 |
|---|---|---|
| `new()` | `Result<Self, _>` | エンジンを起動する |
| `poll_events(&mut self)` | `()` | サウンドスレッドからのイベントを取り込みコールバックを発火 |

### バッファ

| メソッド | 戻り値 | 説明 |
|---|---|---|
| `load<P: AsRef<Path>>(path)` | `Result<BufferId, _>` | ファイルをロード |
| `unload(BufferId)` | `bool` | バッファを解放 |

### 再生

| メソッド | 戻り値 | 説明 |
|---|---|---|
| `play(buf, vol, pitch)` | `bool` | マスターバスへ fire-and-forget |
| `play_with_callback(buf, vol, pitch, cb)` | `bool` | 自然終了でコールバック発火 |
| `play_to_bus(buf, vol, pitch, bus)` | `bool` | 任意のバスへ再生 |
| `play_to_bus_with_callback(buf, vol, pitch, bus, cb)` | `bool` | 同上 + コールバック |
| `spawn_source(buf, vol, pitch, bus)` | `Option<EntityId>` | 制御可能な 3D ソースを生成 |
| `set_volume(v)` | `bool` | マスターバスのゲインを変更 |
| `stop_all()` | `bool` | 全ソースを停止（コールバックは呼ばずに解放）|

### バス

| メソッド | 戻り値 | 説明 |
|---|---|---|
| `master_bus()` | `EntityId` | マスターバスを取得 |
| `create_bus(gain)` | `Option<EntityId>` | マスター直下にバスを作成 |
| `create_bus_routed(gain, parent)` | `Option<EntityId>` | 任意の親の下に作成 |
| `destroy_bus(id)` | `bool` | バスを削除（master は不可）|
| `set_bus_gain(id, gain)` | `bool` | バスゲインを設定 |
| `set_bus_muted(id, muted)` | `bool` | バスをミュート |
| `set_bus_output(id, parent)` | `bool` | バスの親を変更（ループは拒否）|

### 3D サウンド

| メソッド | 戻り値 | 説明 |
|---|---|---|
| `set_listener(pos, fwd, up)` | `()` | リスナーの位置・向きを更新（毎フレーム）|
| `set_source_spatial_params(id, model, min, max, rolloff)` | `bool` | 距離減衰の特性を設定 |
| `set_source_spatial_enabled(id, enabled)` | `bool` | 空間演算を有効/無効化 |
| `batch_set_source_positions(&[(id, pos)])` | `()` | 複数ソースの位置を一括更新（毎フレーム）|

## 戻り値の意味

- `bool`: `false` は無効ハンドルや一時的な失敗。panic はしない。
- `Option<EntityId>` / `Result<_, _>`: 容量上限や I/O 失敗を表す。

`#[must_use]` が付いている API は捨てる場合 `let _ = ...` を明示する。

## 呼び出し規約

すべて `&mut self`。同じスレッドから直列に呼ぶ。複数スレッドから同時に呼ばない。

## 関連ドキュメント

- 機能別の使い方: [README](README.md) からたどれる
- 内部設計: [`docs/design/core/`](../design/core/)
