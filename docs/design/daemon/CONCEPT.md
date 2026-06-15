# daemon クレート — コンセプト

`daemon` は NEZIA ENGINE の **out-of-process ホスト** で、Unity Editor やオーサリングツール、
将来のプロファイラ等の **外部ツールから core エンジンを操作するための独立プロセス**である。

ランタイム (ゲーム本体に組み込まれる `nezia-ffi`) とは別経路で、**Editor / ツール側に
core ロジックを直接持ち込まずに音を鳴らす / 状態を観測する** ことを目的とする。

---

## なぜ daemon が必要か

`jp.nezia.unity` の Integration ロードマップ
([`integration-experience.md`](../../../../Nezia_Integration/Packages/jp.nezia.unity/docs~/roadmap/integration-experience.md))
で **IP-6 Asset Preview** が「デーモン依存により保留」状態にある。これは以下の制約から導かれる:

1. **Unity Editor で音声バイナリを直接扱わない方針**
   - Editor アセンブリに `nezia-ffi` を読ませて Editor プロセス内で鳴らす案は採らない。
   - 理由: Editor プロセスのオーディオ状態 (Unity 自身の `AudioSource` プレビュー等) と
     混在させると挙動が予測しにくく、Domain Reload で core 状態が壊れる。
2. **実機ランタイムと同一の挙動を Editor 上でプレビューしたい**
   - Unity 標準の `AudioClip` で代替プレビューを実装すると、実機とサウンドの聴感が乖離する。
   - **同じ `nezia-core` を別プロセスで動かす**のが最も確実。
3. **将来のオーサリングツール (CONCEPT.md B 経路) と Editor で実装を共有したい**
   - プロジェクトファイル方式の本格オーサリングツール側でも「設計中のサウンドを試聴」
     「ミキサー状態を可視化」する経路が必要。daemon があれば両者が同じバックエンドを共有できる。

daemon は **Editor / オーサリングツール / プロファイラ** という別ベクトルの 3 用途を
**1 つの独立プロセスに集約**するための土台である。

---

## 責務の境界

`daemon` が引き受けるもの:

- `nezia-cli` からの RPC 受付 (gRPC server)
- `nezia-core` の `SoundEngine` を所有し、その寿命を管理する
- 指定されたアセットをロード・再生・停止し、結果イベントを返す
- ミキサーアセット (NeziaMixerAsset 由来の構成) を反映する

`nezia-cli` が引き受けるもの:

- daemon の gRPC を叩く**唯一の front door** (外部フロントはこれを `Process` 起動する)
- 引数を gRPC リクエストに変換し、レスポンス / ストリームを **stdout (JSON)** に整形して返す
- port discovery file を読んで起動済み daemon に接続する (daemon の spawn は行わない)

`daemon` / `nezia-cli` が引き受けないもの:

- **ゲーム本体への組み込み**: ランタイムは `nezia-ffi` を直接リンクする経路を使う。
  daemon を経由しない (レイテンシ・配布粒度の観点から)。
- **オーサリングツールの UI 実装**: daemon は IPC バックエンドのみ。UI は別プロセス
  (Unity Editor / 専用 authoring tool) が担当する。
- **アセットフォーマット策定**: 受け取るのは既存 core のフォーマット (WAV / Ogg / MP3 etc.)
  と将来の `.nez`。daemon 固有のフォーマットは持たない。

---

## アーキテクチャ概要

```
 外部フロント                  front door            backend (per-session)
┌─────────────────────┐  Process ┌──────────────┐ gRPC  ┌──────────────────────────────┐
│ Unity Editor        │─起動+stdout│ nezia-cli    │loopback│ daemon                       │
│ LLM / エージェント   │─コマンド ──►│ (gRPC client)│─ TCP ─►│   tonic server               │
│ authoring tool (B)  │           │ port 解決:    │       │     ▼                        │
└─────────────────────┘           │ $TMPDIR/.port │       │   SoundEngine (nezia-core)   │
        │                         └──────────────┘       │     ├─ メインスレッド         │
        │ Editor が daemon を spawn (--parent-pid)        │     └─ サウンドスレッド (cpal)│
        └────────────────────────────────────────────────┴──────────────────────────────┘
                                          parent PID 消失で daemon self-exit
```

外部フロント (Editor / エージェント / authoring tool) は**共通の薄い CLI (`nezia-cli`) を
front door** として制御を送る。`nezia-cli` が daemon に gRPC を投げ、daemon が `nezia-core`
の `SoundEngine` を**長寿命で所有・操作する**。サウンドスレッドは daemon プロセス内で動き、
フロントからは見えない。

**gRPC は daemon ↔ cli の内部プロトコルに閉じる**。外部フロントは `nezia-cli` を `Process`
起動して stdout を読むだけで、gRPC / HTTP2 / protobuf の依存を持たない (Unity Editor 統合は
追加 DLL ゼロ)。これにより 1 つの backend を複数フロントが実装言語・ランタイム制約なく共有できる。

> **daemon を spawn するのは「セッションの所有者」** に限る。Unity なら Editor が起動時に 1 回
> spawn し `--parent-pid=EditorのPID` を渡す。authoring tool も同様に自分を親に spawn する。
> GUI を持たないエージェント / ヘッドフルでないセッションは `nezia-cli daemon start|stop` で
> 明示管理する。**per-command の `nezia-cli` 呼び出しは daemon を spawn せず、起動済み daemon に
> 接続して 1 コマンド送るだけ**にする (一瞬で終わる cli を親にすると parent PID self-exit が
> 誤発火するため)。

---

## 採用した設計判断

各論点と理由を以下に明示する。判断の背景については 0.2.x 設計議論を参照。

### 1. プロセスモデル — per-session spawn + cli front door

セッションの所有者 (Unity なら Editor) が起動時に daemon を `Process.Start` で 1 回 spawn し、
終了で kill する。各操作は **`nezia-cli` を `Process` 起動**して daemon に送る。

- **代替案 (グローバル共有 daemon)** は複数プロジェクト同時開発で衝突する。
  Preview 用途では「セッション 1 個に daemon 1 個」が最もライフサイクルが単純。
- **daemon と cli の役割分担**: daemon は長寿命で `SoundEngine` を所有。`nezia-cli` は
  起動済み daemon に gRPC で 1 コマンド送って stdout に結果を出し即終了する薄いクライアント。
  per-command の cli は daemon を spawn しない (アーキテクチャ概要の注記参照)。
- daemon / cli の binary はどちらも Editor-only として Unity package に同梱 (後述「配布」節)。

### 2. IPC — gRPC over loopback TCP (daemon ↔ cli の内部)

gRPC は **`nezia-cli` ↔ daemon の内部プロトコル**。外部フロント (Editor / authoring tool /
エージェント) は gRPC を直接話さず、`nezia-cli` を `Process` 起動して stdout を読む。

- **トランスポート**: `127.0.0.1` (loopback) に bind した TCP ソケット。OS が割り当てた
  ephemeral port を **port discovery file** (`$TMPDIR/nezia-daemon-{parent_pid}.port`)
  に書き、`nezia-cli` は同ファイルを読んで接続する。
- **プロトコル**: gRPC over HTTP/2 + Protocol Buffers。**両端とも Rust** なので相性問題が出ない。
  - daemon 側: `tonic` + `prost`、`tonic-build` で `.proto` から server コード生成 (OUT_DIR、非 commit)
  - cli 側: 同じ `.proto` から `tonic-build` で client コード生成 (OUT_DIR、非 commit)

**選定理由**:

- スキーマ駆動 (`.proto`) で daemon / cli 間の API が型レベルで一元管理される。
  breaking change が PR 段階で見える。
- gRPC のストリーミング RPC が **Event の push** に自然に乗る (独自フレーミング不要)。
  cli はこれを受けて `nezia-cli subscribe` の stdout ストリームとして外部フロントへ中継する。
- **gRPC を外部フロントに露出しない**ことで、`Grpc.Net.Client` の Unity Mono 互換性や
  HTTP/2 サポートといった懸念が**そもそも発生しない** (gRPC を話すのは Rust の cli だけ)。
  C# 側は標準 `Process` + stdout だけで済み、追加 DLL ゼロ。
- **loopback TCP** を選んだ理由:
  - 速度: UDS / Named Pipe との実測差は preview 用途の頻度では誤差。
  - セキュリティ: `127.0.0.1` 限定 bind により OS レベルで外部接続を拒否。macOS / Windows の
    OS Firewall は loopback 通信を監視しないため、Firewall ダイアログは発生しない。
  - 互換性: 全プラットフォームで数十年枯れた API。Rust ↔ Rust なので追加の互換懸念もない。
- **port discovery file** は LSP / DAP (Debug Adapter Protocol) と同様の標準パターン。

### 3. RPC モデル — Unary + Server-streaming

| 種別 | 用途 | 例 |
|---|---|---|
| Unary RPC | 単発リクエスト / レスポンス | `LoadBuffer`, `Play`, `Stop`, `LoadMixer` |
| Server-streaming RPC | daemon → cli の継続的 push (cli が stdout へ中継) | `SubscribeEvents` (`SourceFinished` / `Error` 等) |
| (将来) Bidi-streaming | 双方向制御が必要になった場合に検討 | — |

Client-streaming は当面用途なし。

### 4. ステートモデル — ステートフル長寿命 SoundEngine

daemon は起動時に `SoundEngine` を 1 個だけ生成し、プロセス寿命の終わりまで保持する。
各 RPC はその状態を変えていく (load → play → stop)。

ステートレス (毎 Request で init) 案は Mixer asset 反映のたびに再初期化が走り、
複数同時発音や Snapshot 補間の連続性が破綻するため不採用。

### 5. ライフサイクル — parent PID 監視で self-exit

daemon は起動時にコマンドライン引数で `--parent-pid <pid>` を受け取る。

- macOS / Linux: `kill(parent, 0)` を 1Hz でポーリング、ESRCH なら self-exit
- Windows: `OpenProcess(SYNCHRONIZE) + WaitForSingleObject` で待機

これにより Editor がクラッシュ・強制終了しても孤児プロセスが残らない。
IPC レイヤのハートビートは不要 (parent PID 監視で十分カバー)。

### 6. アセット受け渡し — ファイルパス第一級

Editor が Asset DB 経由で持つ絶対パスを `nezia-cli` に渡し、cli が
`LoadBuffer { path: string }` で daemon に送る。

バイト列転送 (`LoadBufferFromBytes { data: bytes }`) は **0.2.0 では実装しない**。
Addressables / WebRequest / インメモリ生成のプレビューは 0.3.x 以降で追加検討する。

### 7. .proto の所有と配布

```
proto/
  nezia/
    v1/
      daemon.proto      ← gRPC service 定義
      common.proto      ← 共有型 (BufferId, BusName 等)
```

- ワークスペースルートに `proto/` を置く。**`.proto` を参照するのは Rust の daemon / cli のみ**
  (C# は gRPC を話さないので protoc 不要)。
- バージョンを **パッケージ名に埋め込む** (`nezia.v1`) ことで、将来の breaking change 時に
  `v2` を並走させる余地を残す。
- daemon / cli 双方とも `tonic-build` が `build.rs` で OUT_DIR にコード生成 (git に commit
  しない)。daemon は server、cli は client を生成する。
- **C# 側のコード生成・protobuf DLL は不要**になった。外部フロントは `nezia-cli` を
  `Process` 起動して stdout を読むだけで、API の契約は **cli のサブコマンド + stdout の
  JSON 形** に移る (proto は cli の内部実装詳細)。

### 8. 配布

Unity package 側の配置:

```
jp.nezia.unity/
  Editor/
    Bin/
      macOS/      nezia-daemon   nezia-cli
      Windows/    nezia-daemon.exe   nezia-cli.exe
      Linux/      nezia-daemon   nezia-cli
```

- daemon / cli binary は **Editor-only**。Player ビルドには絶対に含めない (Unity の
  `PluginImporter` 設定で `IncludeInBuildTarget` をすべて Off に)。
- **gRPC / protobuf の C# DLL (`Grpc.Net.Client` / `Google.Protobuf`) は同梱しない。**
  Editor は標準 `System.Diagnostics.Process` で cli を起動するだけ (追加依存ゼロ)。
- ランタイム (`Runtime/Plugins/{platform}/`) には従来通り `nezia-ffi` のみ。
- core repo の CI が各プラットフォームの daemon / cli binary を artifact として出力し、
  Unity package の release pipeline が取り込む (詳細は別 PR で設計)。

---

## 将来拡張 — プロファイラの side-channel

Phase 4-3 (ランタイムプロファイラ + デバッグビジュアライザ) では、以下の理由により
**gRPC 単独では不十分**になることが見えている:

- dB メーター・波形 telemetry は 60〜100Hz の高頻度更新が必要
- データ量は 600KB/s オーダー、シリアライズコストが無視できない
- 何より **サウンドスレッドが書き手**であり、gRPC 経路でのシリアライズ + ソケット書き込みは
  syscall を含むため [`threading.md`](../core/threading.md) のリアルタイム制約に違反する

このため Phase 4-3 で **共有メモリ side-channel** を追加する方針を予約する:

```
[Control plane]  gRPC over loopback TCP    StartProfiler / StopProfiler / configure
[Data plane]     共有メモリ (mmap)          サウンドスレッドが lock-free SPSC に書く
```

- daemon が `StartProfiler` の Response で **共有メモリの名前** を返し、`nezia-cli` が
  それを stdout に出す。
- フロント (Editor 等) が `MemoryMappedFile.OpenExisting(name)` で memory-map し直接読む
  (高頻度データなので cli の stdout は経由せず、共有メモリを直読みする)。
- 既存 [`capture.rs`](../core/capture.rs) (master post-fader タップ) の lock-free SPSC リング
  パターンを共有メモリ上に展開するイメージ。
- `0.2.0 では実装しない` — gRPC 制御経路の余地を残しておくのみ。

これにより gRPC = **制御信号 / 通常頻度の状態問い合わせ**、共有メモリ = **高頻度
リアルタイム telemetry** という業界標準パターン (Tracy / Unity Profiler / Visual Studio
Profiler) に揃う。

---

## 非目標

- **マルチセッション受付**: 1 daemon は 1 セッション (Editor 1 個 / authoring tool 1 個) の
  `nezia-cli` 呼び出しだけを相手にする。per-command の cli は短命で実質直列。別セッションは
  別 daemon プロセスを起動する (同時並行の本格マルチクライアントは想定しない)。
- **リモート接続**: 同一マシンの loopback 限定。LAN / リモートデバッグ用途は想定しない。
- **永続化**: daemon はプロセス寿命の状態しか持たない。プロジェクトファイルの保存・
  ロードは Editor / authoring tool 側の責務。
- **音声バイナリの加工 / 変換**: daemon は core が解釈できる既存フォーマットを受けて鳴らすだけ。
  `nezia-pack` 相当の変換ツールは別バイナリ。

---

## 0.2.0 までに実装する範囲

詳細は [`roadmap.md`](../../roadmap/roadmap.md) の
Phase 4-α / Phase 4-3 を参照。0.2.0 の到達目標は **Unity IP-6 Asset Preview を解除する
最小スコープ (Tier 2)**:

- [x] `proto/nezia/v1/daemon.proto` 確定 (+ `common.proto`)
- [x] daemon binary 骨格 (gRPC server + port discovery file + parent PID 監視)
- [x] `LoadBuffer` / `Play` / `Stop` (Tier 1 相当、`StopAll` / `Ping` を追加)
- [ ] **`nezia-cli` — daemon gRPC を叩く front door** (`load` / `play` / `stop`、stdout JSON)
- [ ] Bus tree / Mixer asset ロード
- [ ] Clip-centric パラメータ反映 (volume / pitch / loop / spatial / effect chain / send)
- [ ] Random Container プレビュー
- [ ] `SubscribeEvents` (SourceFinished / Error)

0.2.0 で実装しないもの:
- 共有メモリ telemetry (Phase 4-3 で追加)
- バイト列アセット受け渡し
- バス hot reload (Snapshot 経由で代替)
- マルチクライアント

---

## 関連ドキュメント

- [Better than Unity Audio ロードマップ](../../roadmap/roadmap.md) — Phase 4-α
- [Integration Experience ロードマップ](../../../../Nezia_Integration/Packages/jp.nezia.unity/docs~/roadmap/integration-experience.md) — IP-6 (Asset Preview)
- [統合戦略](../integration/CONCEPT.md) — ドロップイン互換 (A 経路) / プロジェクトファイル方式 (B 経路) の 2 経路方針
- [スレッドモデル](../core/threading.md) — サウンドスレッドのリアルタイム制約 (将来の共有メモリ側経路でも遵守)
- [マスター出力キャプチャ](../core/capture.md) — 共有メモリ side-channel の参考実装
- [ECS アーキテクチャ](../core/ecs.md) — daemon が所有する SoundEngine の内部構造
