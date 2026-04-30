# ボイスプール

## VoiceState

| 状態 | 説明 |
|---|---|
| `Playing` | 再生中。`update()` でミキシング対象になる |
| `Free` | 未使用。次の `update()` で despawn される |
| `Pausing` | 一時停止中。ミキシングされないが、再生位置は保持される |
| `Stopped` | 停止済み。次の `update()` で despawn される |

spawn 時は `Playing` で初期化される。

## VoiceComponent

| フィールド | 型 | 説明 |
|---|---|---|
| `vol` | `f32` | 音量（0.0〜1.0） |
| `pitch` | `f32` | ピッチ倍率（1.0 = 原音） |
| `sample_offset` | `f32` | サンプル単位の再生位置 |
| `audio_buffer_index` | `u32` | 再生する AudioBuffer のインデックス |

## VoicePoolSystem

`VoiceComponent` を SoA レイアウトで管理し、`update()` でミキシングを行う System。
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
