# 3D サウンド

ゲーム空間に配置された音源をリスナーから聴く形で再生する。
距離による音量減衰と、左右への定位（パンニング）を行う。

## 全体の流れ

```rust
use nezia::{SoundEngine, AttenuationModel};

// 1. ソースを spawn
let src = engine.spawn_source(buf, 1.0, 1.0, sfx_bus).unwrap();

// 2. 距離減衰の特性を 1 度だけ設定
engine.set_source_spatial_params(
    src,
    AttenuationModel::InverseDistance,
    /* min_distance */ 1.0,
    /* max_distance */ 50.0,
    /* rolloff      */ 1.0,
);

// 3. 毎フレーム、リスナーとソースの位置を更新
loop {
    engine.set_listener(player_pos, player_forward, player_up);
    engine.batch_set_source_positions(&[(src, enemy_pos)]);
    engine.poll_events();
}
```

## ソースの生成 — `spawn_source`

```rust
let src: EntityId = engine.spawn_source(buf, vol, pitch, output_bus)
    .ok_or("spawn 失敗")?;
```

`play` と違ってハンドルを返すので、後から位置や有効/無効を変更できる。
鳴り終わると内部で自動的に解放される。

## リスナー — `set_listener`

毎フレーム、聴き手の位置と向きを通知する。

```rust
engine.set_listener(
    /* position */ [px, py, pz],
    /* forward  */ [fx, fy, fz], // 正規化済みであること
    /* up       */ [ux, uy, uz], // 正規化済みであること
);
```

- `forward` と `up` は正規化してから渡す。
- 毎フレーム呼んで良い（負荷を気にする必要はない）。
- 右方向ベクトルは `cross(forward, up)` から自動計算される。

## 距離減衰モデル

`AttenuationModel` で 4 種類から選ぶ。

| モデル | 式（概略） | 用途 |
|---|---|---|
| `None` | 距離に依らず一定 | UI 効果音 / BGM |
| `Linear` | `1 - rolloff * (d - min)/(max - min)` | 直感的に減衰させたい |
| `InverseDistance` | `min / (min + rolloff * (d - min))` | OpenAL 互換、自然な減衰 |
| `Exponential` | `(d / min) ^ -rolloff` | より急峻な減衰 |

- `min_distance`: これより内側ではフルゲイン。
- `max_distance`: これより外側はクランプ（モデルによる）。
- `rolloff`: 減衰の強さ。1.0 を基準に調整。

設定はソースごとに 1 回だけで良い（パラメータが変わらない限り）。

## 位置の更新 — `batch_set_source_positions`

ソースが多数ある場合、位置更新は **必ずまとめて** 1 度に渡す。

```rust
let updates: Vec<(EntityId, [f32; 3])> = enemies.iter()
    .map(|e| (e.source, e.world_position))
    .collect();
engine.batch_set_source_positions(&updates);
```

- 同一フレームで 2 度呼ぶと **後の呼び出しが上書きする**（差分マージはしない）。
  必ず全ソースをまとめて渡す。
- 同時発音数の上限を超える分は切り捨てられる。

## 空間演算の有効/無効

3D ソースに対して一時的に空間演算を切ることができる（カットシーンで音だけ前面に
出したいときなど）。

```rust
engine.set_source_spatial_enabled(src, false); // 距離減衰・パンを無効化
engine.set_source_spatial_enabled(src, true);  // 戻す
```

無効時は `vol` * バスゲインのみが適用される（センター定位）。

## サンプル

`crates/core/examples/demo_spatial.rs` に実動するサンプルがある。

```bash
cargo run -p nezia --example demo_spatial
```

内部の数値計算や設計判断は [3D サウンド設計](../design/core/spatial.md) を参照。
