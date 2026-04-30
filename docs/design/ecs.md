# ECS アーキテクチャ

resia はゲームエンジンの ECS（Entity-Component-System）パターンを参考にしたデータ指向設計を採用する。
ただし汎用 ECS フレームワークではなく、サウンドミドルウェアに特化した構成をとる。

## 三つの役割

### Entity

スパースセットが発行する `EntityId`（`index` + `generation`）。
個々のサウンドオブジェクト（ボイス、バス等）を一意に識別するハンドル。
データは持たず、識別子としてのみ機能する。

### Component

Entity に紐づくデータ。System 内の密配列（dense array）に SoA レイアウトで格納される。

命名規則: **`〈名前〉Component`**（例: `VoiceComponent`）

Component は純粋なデータの集合であり、ロジックを持たない。
各フィールドは System 内で独立した `Vec<T>` として格納され、
必要なフィールドだけを走査するキャッシュ効率の高い処理が可能。

```
VoiceComponent（ボイス1体あたりのデータ）
├── vol: f32              音量
├── pitch: f32            ピッチ倍率
├── sample_offset: f32    再生位置
└── audio_buffer_index: u32  再生する AudioBuffer のインデックス
```

### System

Component を保持・操作する処理単位。スパースセットの管理と、
毎フレームの `update()` による一括処理を担当する。

命名規則: **`〈名前〉System`**（例: `VoicePoolSystem`）

System の責務:

| 責務 | 説明 |
|---|---|
| Component ストレージ | SoA 密配列の管理（spawn / despawn） |
| `update()` | 毎フレーム（毎オーディオコールバック）の一括処理 |
| Component アクセス | 個別・一括の読み書き API |

## 全体構造

```
SoundEngine（ファサード）
  │
  │  コマンド経由で操作を指示
  │
  ▼
VoicePoolSystem（System）
  ├── sparse / dense 管理
  ├── VoiceComponent データ（SoA 密配列）
  │     ├── vol[]
  │     ├── pitch[]
  │     ├── sample_offset[]
  │     └── audio_buffer_index[]
  └── update()
        ├── ミキシング: AudioBuffer からサンプルを読み出し出力バッファに加算
        └── 終了チェック: 再生完了したボイスを despawn
```

- **SoundEngine** はファサードであり、System ではない。複数の System やリソースを束ねて外部にシンプルな API を公開する窓口。
- **AudioBuffer** は Component でも System でもなく、アセット（共有リソース）。複数のボイスが同一の AudioBuffer を参照できる。

## System の update() パターン

各 System は `update()` メソッドを持ち、毎フレーム呼び出される。
サウンドスレッドのオーディオコールバック内で実行されるため、
リアルタイム制約（ロック禁止・ヒープ確保禁止）に従う必要がある。

```rust
impl VoicePoolSystem {
    /// オーディオコールバックから毎フレーム呼び出す。
    /// output_buffer に全アクティブボイスのサンプルを加算ミキシングし、
    /// 再生完了したボイスを自動的に despawn する。
    pub fn update(
        &mut self,
        output_buffer: &mut [f32],
        device_channels: usize,
        device_sample_rate: f32,
        master_volume: f32,
        buffers: &[Arc<AudioBuffer>],
    );
}
```

## 命名規則まとめ

| 役割 | 接尾辞 | 例 |
|---|---|---|
| Entity | — | `EntityId` |
| Component | `Component` | `VoiceComponent` |
| System | `System` | `VoicePoolSystem` |
| ファサード | — | `SoundEngine` |
| アセット | — | `AudioBuffer` |
