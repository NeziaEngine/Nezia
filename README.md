# NEZIA ENGINE

![logo](logo.png)

データ指向設計によるサウンドミドルウェアライブラリ（Rust）。
ゲームエンジン・オーサリングツールから利用されることを想定する。

## 特徴

- **スパースセット方式のデータ指向設計** — サウンドオブジェクトをコンポーネント単位で
  密配列に格納し、キャッシュフレンドリーな一括処理を実現する。
- **二層ID設計** — オーサリングツール向けの不変な論理IDと、ランタイム向けの高速な
  物理IDを分離する。
- **2 種類の統合経路** — 実機向けの C ABI（インプロセス）と、エディタ統合向けの
  gRPC デーモン（別プロセス）を併設する。

## クレート構成

ワークスペースは 3 つのクレートから成る。

| クレート | 役割 | 主な利用者 |
|---|---|---|
| [`crates/core`](crates/core) | サウンドエンジン本体（`SoundEngine`）。Rust から直接使える | Rust アプリ、上位クレート |
| [`crates/ffi`](crates/ffi) | `core` を C ABI で公開する薄いラッパー。C ヘッダと C# バインディングを自動生成 | Unity / Unreal / C++ エンジン |
| [`crates/daemon`](crates/daemon) | `core` を gRPC サービスとして公開するデーモン | エディタ統合、オーサリングツール |

`ffi` は実機ランタイムでの性能・配布サイズ最優先、`daemon` はオーサリング時の
柔軟性・多言語クライアント対応を優先する。両経路は同じ `core::SoundEngine` を駆動する。

## ドキュメント

- **[利用者ガイド](docs/guide/)** — `core` を使うための入門・概念・API 解説
- **[設計ドキュメント](docs/design/)** — 内部設計の詳細
  - [`docs/design/core/`](docs/design/core/) — ECS / スレッドモデル / バス / 3D サウンド / コールバック
  - [`docs/design/ffi/`](docs/design/ffi/) — C ABI 設計、パニック越境遮断、ABI 安定性
  - [`docs/design/daemon/`](docs/design/daemon/) — gRPC サービス設計

## ビルド

```bash
cargo build           # ワークスペース全体
cargo test            # テスト実行
cargo clippy          # lint
cargo fmt --check     # フォーマットチェック
```

クレート単位でビルドする場合は `-p` を付ける。

```bash
cargo build -p nezia          # core のみ
cargo build -p nezia-ffi      # ffi のみ
cargo build -p daemon         # daemon のみ
```

## サンプル

`core` クレートに動作確認用のサンプルを同梱している。

```bash
cargo run -p nezia --example demo_play     # 基本再生
cargo run -p nezia --example demo_bus      # バス・ルーティング
cargo run -p nezia --example demo_spatial  # 3D サウンド
```

## 動作要件

- Rust edition 2024
- macOS / Windows / Linux

## ライセンス

[MIT License](LICENSE)
