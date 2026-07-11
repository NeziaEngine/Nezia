# nezia-cli — コンセプト

`nezia-cli` は NEZIA ENGINE の **front door** となる薄い CLI ツールである。
daemon の gRPC を叩く唯一のクライアントとして、Unity Editor・LLM / AIエージェント・
将来の authoring tool という **3 種の外部フロントに単一のコマンドインターフェース** を提供する。

```
 外部フロント                      front door              backend
┌─────────────────────┐  Process  ┌──────────────┐  gRPC   ┌────────────────────┐
│ Unity Editor        │─起動+stdout│ nezia-cli    │loopback │ nezia-daemon       │
│ LLM / AIエージェント │─コマンド ──►│ (Go)         │─ TCP ──►│ SoundEngine (Rust) │
│ authoring tool (B)  │           │              │         │                    │
└─────────────────────┘           └──────────────┘         └────────────────────┘
                                   gRPC はこの区間に閉じる
```

---

## コンセプト — 2 つの設計軸

### 軸1. AIエージェント特化 — 「言葉でサウンドを試聴・調整する」経路の実体

[ロードマップ 柱3](../../roadmap/roadmap.md) が主張する「エージェント駆動のワークフロー」は、
GUI を前提とする既存ミドルウェア (Wwise / FMOD のオーサリングアプリ) には無い経路である。
`nezia-cli` はその経路の**実体**であり、したがって **LLM / AIエージェントを第一級の
ユーザーとして設計する**。人間向け CLI に AI 対応を後付けするのではなく、逆である。

AIエージェントにとって良い CLI の条件を、設計要件として明文化する:

| 要件 | 理由 | 対応する設計 |
|---|---|---|
| **低トークンな入出力** | エージェントはすべての出力をコンテキストとして消費する。冗長な出力は毎ターン課金される | 1 行コンパクト JSON (デフォルト) / さらに削る `-o text` |
| **1 回の呼び出しで全仕様を取得できる** | help の試行錯誤はターン数とトークンの浪費 | `schema` コマンド (全コマンド・引数・出力形・エラーコードを機械可読で一括出力) |
| **出力を読まずに成否分岐できる** | exit code だけで制御フローを組める | exit code 規約 (0 / 1 / 2) + 安定したエラーコード enum |
| **推測の余地がない出力形** | 揺れる出力はパースの再試行を誘発する | 全コマンドで統一された `{"ok":...}` 形、1 行 1 メッセージ |
| **反復操作が安い** | 「load → play → 聴く → 調整して play」を高速に回したい | `batch` モード (stdin 常駐、プロセス起動 1 回) |

### 軸2. エディタ拡張フレンドリー — 依存ゼロで組み込める

外部フロント (Unity Editor / authoring tool / エディタ拡張一般) は
**`Process` 起動 + stdout 読み**だけで NEZIA を操作できる。

- **gRPC / HTTP2 / protobuf の依存を持ち込まない**。gRPC は cli ↔ daemon の内部
  プロトコルに閉じ、proto は cli の実装詳細である。Unity での
  `Grpc.Net.Client` の HTTP/2 互換問題・protobuf DLL のバージョン衝突・
  Domain Reload でのチャネル破壊は**そもそも発生しない** (追加 DLL ゼロ)。
- **API の契約は「サブコマンド + stdout の 1 行 JSON」に一本化**する。
  どの言語・どのランタイムのエディタ拡張でも、プロセスを起動して行をパースできれば統合できる。
- **長寿命が欲しければ `batch` を 1 プロセス持つ**だけでよい。Domain Reload 対策も
  「プロセスを殺して再起動」で済み、フロント側の状態管理が単純になる。

2 つの軸は同じ結論に収束する: **入出力は行指向・低ノイズ・機械可読**であること。
AIエージェントに優しいインターフェースは、そのままエディタ拡張にも優しい。

---

## 責務の境界

`nezia-cli` が引き受けるもの:

- daemon の gRPC を叩く**唯一の front door** (外部フロントはこれを `Process` 起動する)
- 引数 → gRPC リクエスト変換、レスポンス / ストリーム → stdout 整形
- port discovery file を読んで起動済み daemon に接続する
- ヘッドレスセッション (エージェント等) 向けの daemon ライフサイクル管理
  (`daemon start|stop|status`)

`nezia-cli` が引き受けないもの:

- **per-command 呼び出しでの daemon spawn**: セッション所有者 (Unity なら Editor) が
  spawn する。一瞬で終わる cli を親にすると parent PID self-exit が誤発火するため
  ([daemon CONCEPT.md](../daemon/CONCEPT.md) 参照)
- **音声処理・アセット変換**: すべて daemon / core の責務
- **UI**: フロントの責務

---

## 採用した設計判断

### 1. 実装言語 — Go

daemon CONCEPT.md 起草時は Rust (tonic) を想定していたが、**Go を採用する**。

- **クロスコンパイルが一級**: `GOOS/GOARCH` 指定だけで macOS / Windows / Linux の
  static binary を CI 一発で生成できる。配布物 (Unity package の `Editor/Bin/`) の
  ビルドパイプラインが単純になる。
- **gRPC client の実装コストが低い**: grpc-go は canonical 実装であり、
  スキーマ駆動 (`.proto`) なので Rust daemon との相性問題は生じない。
  「両端とも Rust」の利点はスキーマが契約である以上、本質ではなかった。
- **CLI としての起動が速い**: per-command 起動のユースケースで数十 ms に収まる。
- トレードオフ: リポジトリに Go toolchain が増える。cli は core とコードを共有しない
  薄いレイヤなので、言語が分かれる不利益は小さいと判断した。

### 2. リポジトリ配置 — nezia-core に同居

```
nezia-core/
  proto/nezia/v1/          # single source of truth (既存)
  nezia-cli/
    go.mod                 # module jp.nezia/nezia-cli
    cmd/nezia-cli/main.go
    internal/
      command/             # サブコマンド定義・引数パース
      client/              # gRPC 接続、port discovery、version handshake
      daemonctl/           # daemon start/stop/status
      output/              # フォーマッタ (json / text)
      gen/neziav1/         # protoc-gen-go 生成物 (commit する)
    Makefile               # build / gen (protoc + ローカル bin/ の plugin) / test
```

- `.proto` は daemon と同一リポジトリの同一ファイルを参照し、**契約のドリフトを
  構造的に防ぐ** (別リポジトリ案は proto 同期機構が必要になるため不採用)。
- 生成コードは Go 慣習に従い **commit する** (Rust の OUT_DIR 方式と異なる)。
  ビルドに protoc が不要になり、CI・コントリビュータの敷居が下がる。
- CLI フレームワークは**標準 `flag` + 手書きディスパッチ**を採用 (コマンド数が少なく、
  help のサイズ規律を手書きで完全制御できるため。依存は grpc + protobuf のみ)。
  標準 `flag` は位置引数以降のフラグを解釈しないため、`play 3-1 --volume 0.4` の
  順序を許容する `parseAnywhere` ヘルパーを併用する。

### 3. コマンド体系 — proto と 1:1 を基本とする薄い写像

```
nezia-cli ping                                  # 疎通 + バージョン照合
nezia-cli load <path>                           # → LoadBuffer
nezia-cli play <buffer> [--volume 1.0] [--pitch 1.0] [--loop]
nezia-cli stop <source> | stop --all
nezia-cli daemon start|stop|status              # ヘッドレスセッション管理
nezia-cli batch                                 # stdin 常駐モード
nezia-cli schema                                # 全コマンド仕様の機械可読出力
nezia-cli subscribe                             # (Tier 2) イベント JSONL ストリーム
nezia-cli mixer load <path> / bus list          # (Tier 2)
```

- RPC への写像は薄く保ち、cli 独自のロジック (リトライ・キャッシュ等) は持たない。
  例外は `daemon` / `batch` / `schema` の 3 つで、これらは front door としての付加価値。
- ハンドル (`BufferId` / `SourceHandle` の `{index, generation}`) は
  **`<index>-<generation>`** 形式の短い文字列に畳む (例: `3-1`)。
  トークンが短く、コピーで往復可能。

### 4. 出力フォーマット — 1 行コンパクト JSON + `-o text`

**デフォルト: 1 行コンパクト JSON** (pretty print しない)。
外部フロントとの契約はこの形である。

```
$ nezia-cli load /path/to/explosion.wav
{"ok":true,"buffer":"3-1"}

$ nezia-cli play 3-1 --volume 0.8
{"ok":true,"source":"12-4"}

$ nezia-cli play 99-0
{"ok":false,"error":{"code":"INVALID_HANDLE","msg":"buffer 99-0 not found"}}
```

**`-o text`: さらにトークンを削る行指向形式** (エージェントの対話ループ向け)。

```
$ nezia-cli -o text load /path/to/explosion.wav
ok buffer=3-1
$ nezia-cli -o text play 99-0
err INVALID_HANDLE: buffer 99-0 not found
```

共通ルール:

- **1 メッセージ = 1 行**。ストリーム (`subscribe`) は 1 イベント 1 行の JSONL。
- 成功形は `{"ok":true, ...}`、失敗形は `{"ok":false,"error":{"code","msg"}}` に統一。
- 診断・ログの類は stderr へ。stdout は契約された出力のみ (パイプ・パースを汚さない)。

### 5. エラーモデル — exit code + 安定エラーコード

| exit code | 意味 |
|---|---|
| 0 | 成功 |
| 1 | アプリケーションエラー (daemon がエラーを返した / 引数不正) |
| 2 | 接続不可 (daemon 未起動 / port discovery 失敗 / version 不一致) |

- gRPC status は安定した enum 文字列に正規化する:
  `DAEMON_NOT_RUNNING` / `INVALID_HANDLE` / `LOAD_FAILED` / `INVALID_ARGUMENT` /
  `VERSION_MISMATCH` / `QUEUE_FULL` 等。
- エラーコードは `schema` の出力に含め、**後方互換を保つ** (削除・意味変更は breaking)。

### 6. AIエージェント向け機能

#### `schema` — ツール定義の一括取得

全コマンド・引数・出力スキーマ・エラーコード一覧をコンパクト JSON で 1 回で出力する。
エージェントはこれを読めば help の試行錯誤なしに全操作を把握できる
(MCP のツールスキーマ相当。将来 MCP サーバー化する場合もこの定義を流用する)。

#### `batch` — stdin 常駐モード

stdin から 1 行 1 コマンドを読み、stdout に 1 行 1 結果を返す。

```
$ nezia-cli batch
load /path/to/explosion.wav          ← stdin
{"ok":true,"buffer":"3-1"}           ← stdout
play 3-1 --pitch 1.2
{"ok":true,"source":"12-4"}
```

- プロセス起動と gRPC 接続を 1 回に抑え、「調整して聴き直す」反復を高速化する。
- エージェントセッションだけでなく、**Editor が常駐プロセスとして 1 個持つ**用途にも使える
  (Domain Reload 時はプロセスごと再起動すればよい)。

#### help のサイズ規律

トップレベル help は「全コマンド 1 行ずつ + 使用例」で **40 行以内**を維持する。
LLM のコンテキストに丸ごと入るサイズであることを CI の行数上限テストで強制する。

### 7. 接続 — port discovery と version handshake

- daemon が書く port discovery file (`$TMPDIR/nezia-daemon-{parent_pid}.port`) を読んで
  loopback TCP で接続する。対象 daemon の特定は `--parent-pid <pid>` /
  環境変数 `NEZIA_DAEMON_PID`、直接指定の `--port` も用意する。
- 接続時に `Ping` でバージョンを照合し、cli / daemon の不一致は warning
  (`--strict` でエラー) とする。

### 8. daemon ライフサイクル — ヘッドレスセッション対応

GUI を持たないエージェントセッションでは、セッション所有者となるプロセスが存在しない。
このため `nezia-cli daemon start|stop|status` で明示管理する。

- **daemon 側の変更は不要**: daemon は `--parent-pid` なしのスタンドアロン起動を
  既にサポートしており、その場合 port file 名は daemon 自身の PID がキーになり
  self-exit もしない (`lifecycle.rs::port_file_path`)。`daemon start` はこのモードで
  spawn し、子 PID を出力する。cli 側は port file を glob して単一候補なら自動接続する。
- Unity Editor 等のセッション所有者がいる場合は従来どおりフロントが spawn し、
  cli は接続するだけ (per-command の cli は daemon を spawn しない)。

### 9. 配布

- CI が `GOOS/GOARCH` クロスコンパイルで macOS / Windows / Linux binary を生成し、
  `-ldflags -X` でバージョンを埋め込む。
- Unity package の `Editor/Bin/{platform}/` に daemon と並べて同梱する
  (daemon CONCEPT.md §8 と同一の配置。Editor-only、Player ビルドには含めない)。

---

## 非目標

- **リッチな対話 UI**: TUI・カラー出力・プログレスバー等は持たない。出力は機械可読が第一。
  人間の対話用途は将来のフロント (authoring tool) の責務。
- **daemon の代替**: cli にエンジン状態を持たない。すべての状態は daemon 側にある。
- **リモート接続**: daemon と同じく同一マシン loopback 限定。
- **アセット変換・パッキング**: `nezia-pack` 相当は別バイナリ。
- **gRPC の外部公開**: proto / gRPC を外部フロントの契約にすることは今後もない。
  契約は「サブコマンド + stdout の 1 行 JSON」である。

---

## マイルストーン

| | 到達点 | 内容 |
|---|---|---|
| **M0** | 骨格 | scaffold、buf codegen、`ping` が実 daemon と疎通 [済] |
| **M1** | Tier 1 完了 | `load` / `play` / `stop` / `stop --all`、JSON / text 両フォーマット、エラーコード体系 (Unity IP-6 解除の前提が揃う) [済] |
| **M2** | セッション運用 | `daemon start/stop/status` (daemon 既存のスタンドアロンモードを利用)、`batch`、`schema` [済] |
| **M3** | Tier 2 追随 | `subscribe` (JSONL)、`mixer load`、Clip-centric 反映系コマンド |

---

## 関連ドキュメント

- [daemon コンセプト](../daemon/CONCEPT.md) — backend 側の設計。プロセスモデル・IPC・
  ライフサイクルの正はこちら (本ドキュメントの §1 Rust 前提の記述は Go 採用により更新対象)
- [ロードマップ](../../roadmap/roadmap.md) — 柱3 (daemon による統合作業環境) の front door
- [統合戦略](../integration/CONCEPT.md) — A 経路 (Unity) / B 経路 (authoring tool) の 2 経路方針
