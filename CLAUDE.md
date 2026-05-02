# CLAUDE.md — NEZIA ENGINE

## プロジェクト概要

NEZIA ENGINE はデータ指向設計によるサウンドミドルウェアライブラリ（Rust）。
ゲームエンジンやオーサリングツールから利用されることを想定する。

## ビルド・テスト

```bash
cargo build          # ビルド
cargo test           # テスト実行
cargo clippy         # lint
cargo fmt --check    # フォーマットチェック
```

- Rust edition: 2024
- MSRV: 未定（edition 2024 対応版）

## アーキテクチャ

### データ指向設計 — スパースセット方式

すべてのサウンドオブジェクト（バス、Source、エフェクト等）はスパースセットで管理する。
データはコンポーネント種別ごとに密配列（dense array）に格納し、キャッシュ効率の高い一括処理を行う。

### 二層ID設計

ID は**論理ID（Hash ID）**と**物理ID（Entity ID）**の二層構造をとる。

#### 論理ID（Hash ID）
- オーサリングツール側で定義される文字列（例: `"MasterBus"`, `"sfx/explosion"`）をハッシュ化した `u32` 値。
- JSON 等のアセットデータに紐づく不変の識別子。同じ文字列からは常に同じ値が得られる。
- 値域が巨大（最大 ~4G）なので、配列インデックスとしては使用できない。
- 用途: アセット参照、永続化、デバッグ表示。

#### 物理ID（Entity ID）
- スパースセットが内部で発行する `(index: u32, generation: u32)` の組。
- `index` は 0 始まりの連番で、密配列への O(1) アクセスに使用する。
- `generation` はスロット再利用時にインクリメントされ、古いハンドルの無効化検出に使う。
- ゲーム再起動やオブジェクト再生成で変わり得る一時的な値。
- 用途: ランタイムでの高速アクセス、ハンドルの有効性検証。

#### ID 間のマッピング

```
論理ID (Hash ID)  ──→  HashMap  ──→  物理ID (Entity ID)
     0x3F8B2A1C                        (index: 5, generation: 2)
```

- `HashMap<HashId, EntityId>` でルックアップ。ランタイム初期化時に構築される。
- 通常のフレーム処理では物理IDのみを使い、HashMap 経由の検索は初期化・イベント発火時に限定する。

## 設計ドキュメント

詳細な設計は `docs/design/` に分離している。実装時に該当領域のドキュメントを読むこと。

- [ECS アーキテクチャ](crates/core/docs/design/ecs.md) — Entity/Component/System の役割定義、命名規則、update() パターン
- [スレッドモデル](crates/core/docs/design/threading.md) — サウンドスレッド/メインスレッドの責務分担、リアルタイム制約、スレッド間通信
- [Source ワールド](crates/core/docs/design/source.md) — コンポーネント定義、ピッチ内部表現の設計判断
- [バスルーティング](crates/core/docs/design/bus.md) — バスの木構造ルーティング、ミキシングフロー、処理順序
- [3D サウンド](crates/core/docs/design/spatial.md) — 距離減衰・パンニング・リスナー管理・ドップラー効果の設計
- [コールバック](crates/core/docs/design/callbacks.md) — イベントリングバッファ経由のコールバック設計、イベント種別と優先度、実装方針

## コーディング規約

- `unsafe` は音声バッファ操作など性能上不可避な箇所に限定し、必ず `// SAFETY:` コメントを付ける。
- パブリック API には `#[must_use]` を適切に付与する。
- エラー処理は `Result` を返す。`unwrap()` / `expect()` はテストコードに限定する。
- テストは `src/` 内にインラインで書かず、`tests/` ディレクトリに結合テストとして配置する。
