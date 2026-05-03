# ffi クレート — コンセプト

## 位置づけ

`ffi` は `core` クレートが提供する `SoundEngine` を **C ABI** 経由で外部言語から
利用可能にする薄いラッパークレートである。Unity / Unreal / 自社製 C++ エンジン等の
ゲームエンジンに **インプロセスで組み込む** ための境界面となる。

ビルド時に [csbindgen](https://github.com/Cysharp/csbindgen)（Cysharp 製）を
`build.rs` から走らせ、Rust の `extern "C"` 宣言から **C ヘッダ `nezia.h`** と
**C# の `[DllImport]` バインディング** を同時に生成する。C ヘッダは Unreal / C++
統合に、C# バインディングは Unity 統合にそれぞれ用いる。当面の動作確認は Unity を
主ターゲットとして進めるが、API 設計はエンジン非依存とする。

## ドロップイン互換戦略との関係

NEZIA ENGINE の差別化方針として、各エンジンの標準サウンド API（Unity の
`AudioSource` 等）と互換のラッパを提供することでモック開発から導入できる
ミドルウェアを目指す。詳細は [integration/CONCEPT.md](../integration/CONCEPT.md)
を参照。`ffi` 側ではこの戦略を成立させるため、以下の追加 API を提供する必要がある。

- `nezia_buffer_load_from_memory` — Resources / Addressables / WebRequest 経由で
  得たバイト列を直接ロード
- `nezia_buffer_load_from_pcm` — `AudioClip.GetData()` で取り出した float PCM を
  直接アップロード（レベル 2 互換用）

これらは現行 API（`nezia_buffer_load`）と並列に提供し、用途に応じて使い分ける。

## 統合モデルにおける位置づけ

NEZIA ENGINE は用途に応じて 2 種類の統合経路を提供する。`ffi` はそのうちの一方である。

| 経路 | 想定ユースケース | プロセスモデル | レイテンシ | 接続性 |
|---|---|---|---|---|
| **`ffi`（本クレート）** | プレイモード・実機ビルド・製品出荷 | インプロセス（共有/静的ライブラリ） | 関数呼出のみ | 同一プロセス |
| **`daemon`（gRPC）** | エディタ統合・オーサリングツール・リモートデバッグ | 別プロセス | ネットワーク往復 | 任意のエンジン / 言語 |

両経路は同じ `core::SoundEngine` を駆動する。`ffi` は性能とフットプリントを優先し、
`daemon` は柔軟性と多言語クライアント対応を優先する。本クレートは前者を担い、
**実機ランタイム性能** と **配布バイナリの最小化** を最優先する。

## 設計思想

### 薄く保つ

`ffi` 層では **状態を持たない**。すべての状態は `core::SoundEngine` に閉じ込め、
`ffi` は引数の型変換と不正値チェック、パニック越境の遮断のみを行う。
ロジックを書かないことで、Rust 側のテストが ffi 層の仕様を実質的に保証する。

### ABI 安定性最優先

C ABI は一度公開すると変更コストが極めて高い。以下を徹底する。

- 公開型はすべて `#[repr(C)]` で固定レイアウト。
- `SoundEngine` 等の Rust 構造体は **不透明ポインタ**（`*mut NeziaEngine`）として隠蔽し、
  内部表現の変更が ABI に波及しないようにする。
- 関数命名は `nezia_<domain>_<verb>` で統一（例: `nezia_buffer_load`, `nezia_bus_set_gain`）。
- 戻り値は「ハンドル」または `NeziaResult`（`i32` エラーコード）に統一。Rust の `Option` /
  `Result` を直接公開しない（csbindgen / 一般的な C 互換ジェネレータが扱えない型は使わない）。
- 列挙型は `u32` などの幅固定整数で表現し、値の追加は末尾のみとする。
- ポインタ引数は長さ付きの素朴な C 配列（`*const T` + `usize`）で表現し、各言語のラッパが
  自然に `Span<T>` / `TArray<T>` 等に変換できる形を保つ。

### パニック越境の遮断

Rust のパニックを C 側に伝播させると未定義動作になる。本クレートは以下のいずれかで遮断する。

- リリースプロファイルでは `panic = "abort"` を採用し、即座にプロセス停止。
- 各 `extern "C"` エントリ内では `std::panic::catch_unwind` で包み、パニック発生時は
  `NeziaResult::PANIC` を返す（または無効ハンドル）。

### スレッド安全性は呼出側責任

`SoundEngine` の操作スレッドモデルは [core/threading.md](../core/threading.md) で
定義されている。`ffi` 側はロックを追加せず、「`NeziaEngine *` への呼び出しは
単一スレッドからのみ」という契約を生成バインディング双方のコメントで明示する。
ゲームエンジン側ではメインスレッド（あるいは専用のオーディオ操作スレッド）から
呼び出すことを推奨し、Job System 等のワーカースレッドからはバッチ収集 →
メインスレッドで一括 flush するパターンを取る。

### AOT / 静的リンク互換

iOS や IL2CPP のような AOT・静的リンク前提のターゲットでも動作させるため、
以下を遵守する。

- ホスト言語からのコールバックは、ランタイムが固定ポインタとして渡せる形
  （C# なら `MonoPInvokeCallback` 付き static、C++ なら通常の関数ポインタ）のみを許容。
- マネージド型・所有権付きコンテナ（string, array 等）を直接やりとりせず、
  ポインタ + 長さで受け渡す。
- `cdylib` がリンクできない環境向けに `staticlib` も同時にビルドする。

## モジュール構成

| モジュール | 責務 |
|---|---|
| `lib.rs` | クレートルート、`pub use` での公開関数集約 |
| `types.rs` | `NeziaEngine`, `NeziaEntityId`, `NeziaBufferId`, `NeziaResult`, `NeziaVec3` 等の ABI 型 |
| `panic.rs` | `catch_unwind` ヘルパ、NULL チェックマクロ |
| `engine.rs` | エンジン生成・破棄・グローバル制御・イベントポーリング |
| `buffer.rs` | オーディオバッファのロード・アンロード |
| `source.rs` | Source の生成・再生・空間パラメータ・バッチ更新 |
| `bus.rs` | バスの生成・破棄・ゲイン/ミュート/ルーティング |
| `spatial.rs` | リスナー位置・姿勢の設定 |
| `event.rs` | コールバック関数ポインタの登録とイベント配信 |

## 公開 API の方針

### 引数渡しの規約

- **文字列**: `*const c_char` + `usize` 長さ。UTF-8 を要求し、FFI 側は借用のみ。
- **配列**: `*const T` + `usize` 長さ。所有権は呼出側に残す。
- **ベクトル**: `NeziaVec3 { x, y, z: f32 }` の値渡し。
- **ハンドル**: `NeziaEntityId { index, generation: u32 }` の値渡し。`NeziaBufferId` は `u32`。
- **無効値**: バッファ ID は `NEZIA_BUFFER_ID_INVALID = u32::MAX` を返す。エンティティ ID は
  `generation = 0` を無効として扱う。

### 戻り値の規約

| パターン | 戻り値型 |
|---|---|
| ハンドル発行系（`spawn`, `create`, `load`） | ハンドル型（無効値で失敗を表現） |
| 状態変更系（`set_*`, `destroy`, `unload`） | `NeziaResult`（i32） |
| 真偽値系 | `u8`（0/1） |
| ポインタ取得系 | 不透明ポインタ（NULL で失敗） |

### コールバック

`nezia_engine_poll_events` に対し、呼出側は以下のシグネチャの関数ポインタと
任意のユーザーデータを渡す。

```c
typedef void (*NeziaEventCallback)(const NeziaEvent* event, void* user_data);
```

`NeziaEvent` は `core::Event` を `#[repr(C)]` で写したタグ付きユニオン
（`tag: u32` + `union { ... }`）として表現する。各言語側ラッパは生成された関数
ポインタ型に対して、ランタイムが要求する形式の static コールバックを登録する。

## 二層 ID と FFI 公開面

[core/CONCEPT.md](../core/CONCEPT.md) で定義する論理 ID（Hash ID）→ 物理 ID（Entity ID）
のマッピングは、**現時点では FFI 公開面に出さない**。理由は以下の通り。

- 論理 ID は「オーサリングデータ（JSON 等）に紐づく不変識別子」として価値を持つため、
  プロジェクトファイル方式（NEZIA 側で `.nezia` 等のアセット定義をロードする運用）が
  確立されてから導入する方が、用途と整合する。
- 現状の FFI はゲームエンジン側がランタイムでオブジェクトを spawn する想定で、
  名前解決はゲームエンジン側 (`Dictionary<string, NeziaEntityId>` 等) で完結できる。
  この段階で FFI に Hash ID 解決 API を出しても二重管理を生むだけで実利が薄い。

プロジェクトファイル方式の導入時に、`nezia_resolve_hash_id(hash) -> NeziaEntityId` 等の
ルックアップ API を追加する。それまで FFI が扱う ID は物理 ID（`NeziaEntityId` /
`NeziaBufferId`）に限定する。

## ビルド成果物

- `libnezia.dylib` / `libnezia.so` / `nezia.dll` （`cdylib`、デスクトップ・Android 等動的リンク向け）
- `libnezia.a` / `nezia.lib` （`staticlib`、iOS など AOT・静的リンク向け）
- `bindings/nezia.h` （csbindgen 生成の C ヘッダ。Unreal / 自社製 C++ エンジン統合に使用）
- `bindings/NeziaNative.cs` （csbindgen 生成の C# `[DllImport]` バインディング。Unity 統合に使用）

これらは `build.rs` 内で `csbindgen::Builder` を駆動して同時生成し、CI で
プラットフォーム別ライブラリと合わせて artifact 化する。各エンジン向けの配布形態
（Unity プラグインパッケージ、UE プラグインモジュール等）はそれぞれ別リポジトリ /
別パッケージで提供する。

### csbindgen 設定

- 関数エントリポイントは Rust 側の `nezia_<domain>_<verb>` 名をそのまま使用し、
  C / C# 双方で同名となるようにする。
- C# 側名前空間・クラス名等の具体値は `crates/ffi/build.rs` で集中管理する。

## 非目標

- 各エンジン / 言語向けの高レベル API（C# の `IDisposable` ラッパ、UE の `UObject` 統合、
  Python の Pythonic ラッパ等）の同梱は対象外。本クレートのスコープは csbindgen の
  生成物（薄い `[DllImport]` 層 + C ヘッダ）までとし、使い勝手の良いラッパは
  各エンジン向けの別パッケージで提供する。
- 高レベルなアセット管理 API（JSON ロード等）は対象外。`ffi` は `core` の
  最小公開面のみを反映する。
- リモート接続・ネットワーク越しの操作は対象外。これらは `daemon` クレートの責務であり、
  `ffi` はインプロセス用途に特化する。
