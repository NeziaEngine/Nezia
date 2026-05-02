# 基本概念

NEZIA ENGINE を使う上で押さえておきたい 4 つの概念。

## BufferId — ロード済みオーディオの参照

`engine.load()` で得られるハンドル。デコード済み PCM データへの参照。

```rust
let buf = engine.load("assets/footstep.wav")?;
```

- 同じファイルを何度ロードしても別の `BufferId` が返る（重複排除はしない）。
- 不要になったら `engine.unload(buf)` で解放する。アンロード後に `play(buf, ..)` を
  呼ぶと `false` が返る（panic しない）。

## EntityId — バス・ソースの実行時ハンドル

ランタイムで生成されるバスや 3D ソースは `EntityId` で識別する。

```rust
let bus: EntityId = engine.create_bus(1.0).unwrap();
let src: EntityId = engine.spawn_source(buf, 1.0, 1.0, bus).unwrap();
```

- 解放されたあとの `EntityId` を使って操作しても `false` が返るだけで安全。

`BufferId` は「どの音か」、`EntityId` は「実行中のどのオブジェクトか」を表す。

## バス（Bus）

バスは音をまとめて扱うグループ。ゲイン・ミュートを掛けたり、別のバスに送ったりできる。

```
        Master
        /     \
      SFX    Music
     /   \
  Voice  World
```

- `engine.master_bus()` で常に存在するルートバスを取得できる。
- `engine.create_bus(gain)` でマスター直下にバスを作る。
- `engine.create_bus_routed(gain, parent)` で任意の親に繋いで作る。
- `engine.set_bus_output(bus, new_parent)` で繋ぎ替えられる。ループになる繋ぎ方は
  検出して拒否される（`false` を返す）。

詳細は [バスとルーティング](bus.md) を参照。

## ソース（Source）— 鳴っている音 1 つ

実際に音を出している実体を「ソース」と呼ぶ。
ソースには 2 通りの作り方がある。

### fire-and-forget な再生

`play` / `play_to_bus` で作るソースはハンドルを返さない。
鳴り終わったら自動で消える。SE のように使い捨てる音に向く。

```rust
let _ = engine.play(buf, 1.0, 1.0);
```

### 制御可能な 3D ソース

`spawn_source` で作るソースは `EntityId` を返し、毎フレーム位置などを更新できる。
プレイヤーや敵の足音など、空間内を動く音に使う。

```rust
let src = engine.spawn_source(buf, 1.0, 1.0, bus).unwrap();
engine.set_source_spatial_params(src, AttenuationModel::InverseDistance, 1.0, 50.0, 1.0);
engine.batch_set_source_positions(&[(src, [10.0, 0.0, 5.0])]);
```

詳細は [3D サウンド](spatial.md) を参照。

## 呼び出し規約

`SoundEngine` のメソッドはすべてメインスレッド（同じスレッド）から呼ぶ。
複数スレッドから同時に触らない。
