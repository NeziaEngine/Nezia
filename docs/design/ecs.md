# ECS アーキテクチャ

NEZIA ENGINE はゲームエンジンの ECS（Entity-Component-System）パターンを参考にしたデータ指向設計を採用する。
ただし汎用 ECS フレームワークではなく、サウンドミドルウェアに特化した構成をとる。
各 System はドメイン固有の Entity 空間・Component ストア・処理ロジックを自己完結して管理する。

## 三つの役割

### Entity

スパースセットが発行する `EntityId`（`index` + `generation`）。
個々のサウンドオブジェクト（Source、バス等）を一意に識別するハンドル。
データは持たず、識別子としてのみ機能する。

各 System が独自に EntityId を発行・管理する。Source の EntityId と Bus の EntityId は
異なる System が管理する独立した空間に属する。

### Component

Entity に紐づくデータ。System 内の密配列（dense array）に SoA レイアウトで格納される。

命名規則: **`〈名前〉Component`**（例: `SourceComponent`）

Component は純粋なデータの集合であり、ロジックを持たない。
各フィールドは System 内で独立した `Vec<T>` として格納され、
必要なフィールドだけを走査するキャッシュ効率の高い処理が可能。

```
SourceComponent（Source 1体あたりのデータ）
├── vol: f32              音量
├── pitch: f32            ピッチ倍率
├── sample_offset: f32    再生位置
├── audio_buffer_index: u32  再生する AudioBuffer のインデックス
└── output_bus: u32       出力先バスの密配列インデックス

BusComponent（バス1本あたりの初期パラメータ）
├── gain: f32             音量倍率
└── output_bus: EntityId  出力先バスの EntityId
```

#### SoA タグコンポーネント

`bool` 型の SoA 密配列は「タグコンポーネント」に相当する役割を果たす。
例として `BusSystem` の `muted: Vec<bool>` は `BusComponent` の外に独立したフィールドとして管理され、
バス生成時は常に `false` で初期化される。

これにより:
- `BusComponent`（初期化パラメータ）に mute 状態を混在させずに済む
- 生成後に `set_muted()` でトグルでき、`gain` の値を保持したままミュートできる
- ホットループでは `muted[i]` を参照するだけで分岐が確定する

### System

Component を保持・操作する処理単位。スパースセットの管理と、
毎フレームの `update()` による一括処理を担当する。

命名規則: World（データ）は **`〈名前〉World`**、System（処理）は **`〈名前〉System`**（例: `SourceWorld` / `SourceSystem`, `BusSystem`）

System の責務:

| 責務 | 説明 |
|---|---|
| Entity 管理 | 自身のスパースセットによる EntityId の発行・回収 |
| Component ストレージ | SoA 密配列の管理（spawn / despawn） |
| `update()` | 毎フレーム（毎オーディオコールバック）の一括処理 |
| Component アクセス | 個別・一括の読み書き API |

## 全体構造

```
SoundEngine（ファサード）
  │
  │  コマンド経由で操作を指示
  │
  ├─▶ SourceWorld（データ）
  │     ├── sparse / dense 管理（EntityId 発行）
  │     └── SourceComponent データ（SoA 密配列）
  │           ├── vol[]
  │           ├── pitch[]
  │           ├── sample_offset[]
  │           ├── audio_buffer_index[]
  │           └── output_bus[]       ← BusSystem の密配列インデックス
  ├─▶ SourceSystem（処理）
  │     └── update(&mut SourceWorld, ...)
  │           └── ミキシング: AudioBuffer からサンプルを読み出し BusSystem の
  │                           mix_buffer[output_bus] に加算
  │
  └─▶ BusSystem（System）
        ├── sparse / dense 管理（EntityId 発行）
        ├── BusComponent データ（SoA 密配列）
        │     ├── gain[]
        │     ├── muted[]            ← SoA タグコンポーネント（BusComponent 外）
        │     └── output_bus_dense[] ← 親バスの密配列インデックス
        ├── mix_buffer[]  ← フラット中間バッファ（MAX_BUSES × MAX_MIX_BUFFER_SIZE）
        ├── process_order[]  ← リーフ→ルート順の処理順序
        └── update()
              ├── process_order 順に gain 適用・mute 処理・親バスへ加算
              └── マスターバス mix_buffer → output_buffer コピー
```

- **SoundEngine** はファサードであり、System ではない。複数の System やリソースを束ねて外部にシンプルな API を公開する窓口。
- **AudioBuffer** は Component でも System でもなく、アセット（共有リソース）。複数の Source が同一の AudioBuffer を参照できる。

## System の update() パターン

各 System は `update()` メソッドを持ち、毎フレーム呼び出される。
サウンドスレッドのオーディオコールバック内で実行されるため、
リアルタイム制約（ロック禁止・ヒープ確保禁止）に従う必要がある。

オーディオコールバック内の呼び出し順序:

```rust
// 1. mix_buffer をゼロクリア
bus_system.clear_mix_buffers(sample_count);

// 2. 各 Source のサンプルをバスの mix_buffer に加算
{
    let mix_buf = bus_system.mix_buffer_mut();
    let stride = bus_system.bus_stride();
    SourceSystem::update(&mut source_world, mix_buf, stride, device_channels, device_sample_rate, &buffers);
}

// 3. バス処理 → output_buffer
bus_system.update(data, device_channels, sample_count);
```

```rust
impl SourceSystem {
    /// BusSystem の mix_buffer に全アクティブ Source のサンプルを加算ミキシングし、
    /// 再生完了した Source を自動的に despawn する。
    pub fn update(
        world: &mut SourceWorld,
        bus_mix_buffer: &mut [f32],   // BusSystem のフラットバッファ
        bus_stride: usize,            // バスあたりのスライド幅
        device_channels: usize,
        device_sample_rate: f32,
        buffers: &[Option<Arc<AudioBuffer>>],
    );
}

impl BusSystem {
    /// process_order 順に gain 適用・mute 処理・親バスへの加算を行い、
    /// マスターバスの mix_buffer を output_buffer にコピーする。
    pub fn update(
        &mut self,
        output_buffer: &mut [f32],
        device_channels: usize,
        sample_count: usize,
    );
}
```

## 命名規則まとめ

| 役割 | 接尾辞 | 例 |
|---|---|---|
| Entity | — | `EntityId` |
| Component | `Component` | `SourceComponent`, `BusComponent` |
| World（データ） | `World` | `SourceWorld` |
| System（処理） | `System` | `SourceSystem`, `BusSystem` |
| ファサード | — | `SoundEngine` |
| アセット | — | `AudioBuffer` |
