# NEZIA ENGINE 利用者ガイド

NEZIA ENGINE をゲームエンジン・オーサリングツールから利用するためのガイド。
ライブラリの内部設計を知らなくても使えることを目標にしている。
（内部設計に興味がある場合は [`docs/design/`](../design/) を参照）

## ガイド一覧

1. [はじめに](getting-started.md) — インストールと最小構成での再生
2. [基本概念](concepts.md) — `BufferId` / `EntityId` / バス / ソースの関係
3. [音の再生](playback.md) — `play` / `play_with_callback` / `stop_all` / マスター音量
4. [バスとルーティング](bus.md) — バスの作成・ゲイン制御・親子付け替え
5. [3D サウンド](spatial.md) — リスナー設定・距離減衰・位置の一括更新
6. [イベントとコールバック](callbacks.md) — `poll_events` の役割と注意点
7. [API リファレンス](api-reference.md) — 公開メソッド一覧

## 動作するサンプル

リポジトリ内に動作確認できるサンプルを用意している。

```bash
cargo run -p nezia --example demo_play     # 基本再生
cargo run -p nezia --example demo_bus      # バス・ルーティング
cargo run -p nezia --example demo_spatial  # 3D サウンド
```
