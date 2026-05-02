# バスルーティング

## 概要

バスは Source の出力を集約し、音量・ミュートなどのパラメータをグループ単位で制御するためのミキシング経路である。
Source は必ずいずれかのバスに出力し、バスは別のバスへ出力することで木構造のルーティンググラフを形成する。
最終的にすべての信号はマスターバスに集約され、デバイスへ出力される。

```
Source A ──→ SFX Bus ──→ Master Bus ──→ デバイス出力
Source B ──→ SFX Bus ──┘       ↑
Source C ──→ BGM Bus ──────────┘
Source D ──→ BGM Bus ──┘
```

## マスターバス

マスターバスはルーティンググラフのルート（根）であり、以下の特性を持つ。

- `BusSystem` 初期化時に自動生成される。EntityId は常に `(index: 0, generation: 0)`。
- 削除できない。
- 出力先（`output_bus`）を持たない。マスターバスの出力が最終出力となる。
- 新規作成されたバスのデフォルトの出力先はマスターバスである。

## BusComponent

`BusComponent` はバス生成時の初期パラメータを表す。mute は含まない。

| フィールド | 型 | デフォルト | 説明 |
|---|---|---|---|
| `gain` | `f32` | `1.0` | 音量倍率（0.0〜） |
| `output_bus` | `EntityId` | master | 出力先バスの EntityId |

### mute の設計判断

mute は `BusComponent` の外に置き、`BusSystem` 内の独立した密配列として管理する。

```rust
pub struct BusSystem {
    // ...
    muted: Vec<bool>,  // BusComponent とは独立した SoA フィールド
}
```

BusComponent に含めない理由:

- ミュート前の `gain` 値を保持できる。`gain = 0.0` にすると元の値が失われる。
- SoA の文脈では、独立した `Vec<bool>` はタグコンポーネントと等価な役割を果たす。
  バスが生成されたとき `muted = false` で初期化され、`set_muted()` でトグルする。
- 生成時にミュート状態を指定するユースケースはほぼないため、`BusComponent` に持たせる必要がない。

### gain の設計判断

Source の `vol` と異なり、バスの `gain` は上限を `1.0` に制限しない。
バス単位での信号増幅（ブースト）はミキシングにおいて一般的な操作であり、制限する理由がない。
クリッピングの防止はマスターバス出力段のリミッター（将来実装）の責務とする。

## BusSystem

`SourceWorld` と同じスパースセット方式で管理する System。
EntityId の発行・回収も BusSystem が自身で行う（SourceWorld と同パターン）。

```
BusSystem
  ├── sparse / dense 管理（SourceWorld と同構造）
  ├── BusComponent データ（SoA 密配列）
  │     ├── gain[]
  │     ├── muted[]          ── BusComponent 外のタグ相当フィールド
  │     └── output_bus_dense[]  ── 密配列インデックスで格納
  ├── mix_buffer[]   ── フラット配列（MAX_BUSES × MAX_MIX_BUFFER_SIZE）
  └── process_order[]  ── 処理順序（トポロジカル順）
```

### 定数

```rust
pub const MAX_BUSES: usize = 64;
const MAX_MIX_BUFFER_SIZE: usize = 8192;  // 4096フレーム × 2ch
```

### フラット mix_buffer

各バスの中間ミキシングバッファは単一のフラット `Vec<f32>` として管理する。

```
mix_buffer: [bus0_sample0, bus0_sample1, ..., bus0_sampleN,
             bus1_sample0, bus1_sample1, ..., bus1_sampleN,
             ...]

stride = MAX_MIX_BUFFER_SIZE
bus d のスライス = mix_buffer[d * stride .. d * stride + sample_count]
```

フラット配列にする理由:
- `SourceWorld::update()` に `&mut [f32]` を渡す際に、複数の可変参照が必要になる問題を回避できる。
  フラット配列であれば `BusSystem` から一度だけ `&mut [f32]` を借用し、ストライドで各バスのスライスに分割できる。
- `BusSystem::new()` で `MAX_BUSES * MAX_MIX_BUFFER_SIZE` 要素を事前確保する。
  サウンドスレッドでのヒープ確保を避けるため、実行時はスライスとして使用する。

### output_bus_dense の設計判断

バス間のルーティング参照は内部で密配列インデックス（`u32`）を使用する。
ミキシングループ内で毎フレーム EntityId を resolve するコストを避けるためである。

コマンド処理時に EntityId → 密配列インデックスの解決を行い、以降のフレームでは密配列インデックスのみで参照する。

ルーティング変更やバス削除時には、影響を受けるエントリの再マッピングをコマンド処理段階で行う。

### 処理順序

バスはルーティンググラフの**リーフ（末端）からルート（マスター）**に向かって処理する必要がある。
子バスの出力が確定してから親バスに加算するためである。

```rust
process_order: Vec<u32>  // 密配列インデックスの列。リーフ → ルートの順。
```

- ルーティング変更時にメインスレッドで再計算し、`UpdateProcessOrder` コマンド経由でサウンドスレッドに送る。
- サウンドスレッドでは `process_order` を順に走査するだけであり、グラフ探索は行わない。

## SourceWorld との統合

### Source の出力先バス

`SourceComponent` に `output_bus: u32` フィールドを追加する（密配列インデックス）。
デフォルト値は `0`（マスターバスの密配列インデックス）。

### ミキシングフロー

```
1. 全バスの mix_buffer をゼロクリア
2. SourceSystem::update(&mut source_world, ...):
   各 Source のサンプルを mix_buffer[source.output_bus * stride ..] に加算
3. BusSystem::update():
   process_order 順に:
     a. muted[d] = true なら当該バスのスライスをゼロ埋め
     b. muted[d] = false なら gain[d] を乗算
     c. マスターバス以外は output_bus_dense[d] のバッファに加算
4. マスターバスの mix_buffer を output_buffer にコピー
```

### update() シグネチャ

```rust
impl SourceSystem {
    pub fn update(
        world: &mut SourceWorld,
        bus_mix_buffer: &mut [f32],   // BusSystem のフラットバッファ
        bus_stride: usize,            // = MAX_MIX_BUFFER_SIZE
        device_channels: usize,
        device_sample_rate: f32,
        buffers: &[Option<Arc<AudioBuffer>>],
    );
}
```

- `master_volume` 引数は削除する。マスターバスの `gain` がその役割を担う。
- Source の `vol` のみが個別音量として適用される。バスの `gain` はバス処理段で適用される。

## コマンド

バス操作のために `Command` に以下のバリアントを追加する。

```rust
enum Command {
    // 既存
    Play { audio_buffer_index: u32, vol: f32, pitch: f32 },
    SetVolume(f32),  // マスターバスの gain として処理
    StopAll,

    // バス関連（新規）
    PlayToBus { audio_buffer_index: u32, vol: f32, pitch: f32, output_bus_dense: u32 },
    SpawnBus { id: EntityId, gain: f32, output_bus_dense: u32 },
    DespawnBus { id: EntityId },
    SetBusGain { id: EntityId, gain: f32 },
    SetBusMuted { id: EntityId, muted: bool },
    SetBusOutput { id: EntityId, output_bus_dense: u32 },
    UpdateProcessOrder { order: [u32; MAX_BUSES], len: u8 },
}
```

### UpdateProcessOrder の設計判断

`process_order` は可変長だが、コマンドは固定サイズでなければならない（リングバッファ制約）。
`MAX_BUSES = 64` なので `[u32; 64]` + `len: u8` で固定サイズに収まる。

## 公開 API

`SoundEngine` に以下のメソッドを追加する。

```rust
impl SoundEngine {
    /// マスターバスの EntityId を返す。
    pub fn master_bus(&self) -> EntityId;

    /// マスターバスに接続されたバスを作成する。
    pub fn create_bus(&mut self, gain: f32) -> Option<EntityId>;

    /// 指定した親バスに接続されたバスを作成する。ループが検出された場合は None。
    pub fn create_bus_routed(&mut self, gain: f32, parent: EntityId) -> Option<EntityId>;

    /// バスを削除する。マスターバスは削除できない（false を返す）。
    pub fn destroy_bus(&mut self, id: EntityId) -> bool;

    /// バスのゲインを設定する。
    pub fn set_bus_gain(&mut self, id: EntityId, gain: f32) -> bool;

    /// バスのミュートを設定する。
    pub fn set_bus_muted(&mut self, id: EntityId, muted: bool) -> bool;

    /// バスの出力先を変更する。ループが検出された場合は false。
    pub fn set_bus_output(&mut self, id: EntityId, parent: EntityId) -> bool;

    /// 出力先バスを指定して Source を再生する。
    pub fn play_to_bus(&mut self, buffer: BufferId, vol: f32, pitch: f32, bus: EntityId) -> bool;
}
```

- ルーティング変更（`set_bus_output`, `create_bus_routed`）ではメインスレッド側でループ検出とトポロジカルソートを行い、`SetBusOutput` + `UpdateProcessOrder` の2コマンドを送信する。
- 既存の `set_volume()` はマスターバスの gain を変更する操作として機能する。

## 制約と設計上の決定

### 木構造（1出力）に限定する理由

バスの出力先は1つに限定し、DAG（有向非巡回グラフ）やセンド/リターンは初期実装に含めない。

- 木構造は処理順序の計算が単純で、ループ検出も容易。
- ゲームサウンドの主要なユースケース（BGM/SE/Source/UI の分類制御）は木構造で十分カバーできる。
- 将来的にセンド/リターン（Aux バス）が必要になった場合は拡張として追加する。

### バスの動的生成・削除

バスの生成・削除はランタイムで可能とする。ただし実運用では、バスの大半はアセットロード時に一括生成され、ゲームプレイ中に頻繁に増減することは想定しない。
