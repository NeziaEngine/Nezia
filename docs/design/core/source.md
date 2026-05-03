# Source ワールド

## ライフサイクルモデル — 1 Source = 1 発音インスタンス

NEZIA コアの Source は **「生成 = 再生開始」「自然終了 or Stop = 自動 despawn」** の
ワンショット寿命モデルを採る。`spawn()` という独立操作は持たず、再生 API がそのまま
Entity 生成も兼ねる。

| API | 戻り値 | 用途 |
|---|---|---|
| `play()` / `play_to_bus()` | `bool` | fire-and-forget。EntityId を持たないので動的制御は不可 |
| `play_with_handle()` | `Option<EntityId>` | ハンドル付き再生。音量・ピッチ・位置・Stop を後から制御 |

EntityId の寿命:
- バッファ末尾到達 (`looping=false`) → 次の audio callback で despawn、Entity 無効化
- `stop_source()` → 同上（コールバックは呼ばれない）
- `looping=true` → `stop_source()` まで生存

**再生し直しは新しい EntityId を取り直す**。Unity の `AudioSource.Play()` のように
同じインスタンスで Stop→Play を繰り返すモデルではない（その層は統合層 [integration/CONCEPT.md](../integration/CONCEPT.md)
の責務）。

### 設計判断: Spawn と Play を分離しない理由

- データ指向の SoA レイアウトでは「dense 配列に並んでいるものはすべてミキシング対象」が
  最も単純。`Idle` 状態を入れると mixing system が毎フレーム skip 判定する分岐が増え、
  キャッシュ効率を損なう。
- 「3D パラメータを事前設定してから再生」したい場合は、`play_with_handle()` 直後に
  `set_source_spatial_params()` / `batch_set_source_positions()` / `seek_source()` を
  続けて呼べば triple buffer / コマンドキュー経由で同一フレーム内に反映される
  （`threading.md` 「ミキシング順序」参照）。
- FMOD の Channel、Wwise の playingID と同じ「短命ボイス」モデルで、サウンド
  ミドルウェアとしては標準的。

## SourceState

| 状態 | 説明 |
|---|---|
| `Playing` | 再生中。`update()` でミキシング対象になる |
| `Free` | 未使用。次の `update()` で despawn される |
| `Pausing` | 一時停止中。ミキシングされないが、再生位置は保持される |
| `Stopped` | 停止済み。次の `update()` で despawn される |

spawn 時は `Playing` で初期化される。

## SourceComponent

| フィールド | 型 | 説明 |
|---|---|---|
| `vol` | `f32` | 音量（0.0〜1.0） |
| `pitch` | `f32` | ピッチ倍率（1.0 = 原音） |
| `sample_offset` | `f32` | サンプル単位の再生位置 |
| `audio_buffer_index` | `u32` | 再生する AudioBuffer のインデックス |

## SourceWorld / SourceSystem

`SourceWorld` が `SourceComponent` を SoA レイアウトで管理し、
`SourceSystem::update()` がミキシング処理を担当する。
ECS における役割の詳細は [ECS アーキテクチャ](ecs.md) を参照。

## ピッチの内部表現

ピッチは **倍率** で保持する。再生処理で直接乗算でき、変換コストが不要なため。

ただし倍率は非対称な性質を持つ（1オクターブ上 = ×2.0、1オクターブ下 = ×0.5）。
オーサリングツール側ではセミトーンやセント等の対称なスケールで表示し、以下の変換を行うこと。

```
倍率 → セミトーン:  semitones = 12.0 * log2(ratio)
セミトーン → 倍率:  ratio     = 2.0.powf(semitones / 12.0)
```

この変換はオーサリングツール（UI 層）の責務であり、ミドルウェア内部では行わない。
