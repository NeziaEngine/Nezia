# バスとルーティング

複数の音をまとめて制御するためのグループ。
ゲイン・ミュート・親への送り先を独立に持てる。

## 木構造のルーティング

NEZIA のバスは木構造を成す。ルートはマスターバスで、これは消せない。
すべてのバス・ソースの音は最終的にマスターバスに集約され、デバイスへ出力される。

```
        Master
        /     \
      SFX    Music
     /   \
  Voice  World
```

## 作成

```rust
// マスター直下に作る
let sfx = engine.create_bus(1.0).unwrap();

// 任意の親の下に作る
let voice = engine.create_bus_routed(1.0, sfx).unwrap();
```

- `create_bus(gain)` は `create_bus_routed(gain, master_bus())` の糖衣。
- バス数には上限があり、超えると `None` が返る。

## ゲインとミュート

```rust
engine.set_bus_gain(sfx, 0.7);   // 線形ゲイン
engine.set_bus_muted(sfx, true); // ミュート（false で解除）
```

`set_bus_gain` と `set_bus_muted` は独立。ミュートを解除すると元のゲインで鳴る。

## 繋ぎ替え

実行中のバスを別の親の下に繋ぎ替えられる。

```rust
engine.set_bus_output(voice, master); // SFX → Master 直下に移動
```

ループになる繋ぎ方（自分の祖先を子にしようとする等）は検出して拒否される（`false`）。
マスターバスを別の親に繋ぐことはできない。

## 削除

```rust
engine.destroy_bus(sfx); // bool（master を渡すと false）
```

子バスや、そのバスへ送っていたソースの扱いは内部仕様による。
詳細は設計ドキュメントの [バスルーティング](../design/core/bus.md) を参照。

## バスへ再生する

`play_to_bus` / `play_to_bus_with_callback` / `play_with_handle` は出力先バスを引数で取る。

```rust
let _ = engine.play_to_bus(buf, 1.0, 1.0, sfx);
let src = engine.play_with_handle(buf, 1.0, 1.0, sfx, false).unwrap();
```

bus に渡した `EntityId` が無効化されていると失敗する（`false` または `None`）。

## 典型的な使い方

ゲームでよくある構成例。

```rust
let master = engine.master_bus();
let bgm    = engine.create_bus_routed(0.8, master).unwrap();
let sfx    = engine.create_bus_routed(1.0, master).unwrap();
let voice  = engine.create_bus_routed(1.0, master).unwrap();
let env    = engine.create_bus_routed(0.5, sfx).unwrap(); // 環境音

// 設定画面で音量スライダ → set_bus_gain(bgm, slider_value)
```
