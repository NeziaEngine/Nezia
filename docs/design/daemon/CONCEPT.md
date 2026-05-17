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

- 外部ツールからの RPC 受付 (gRPC server)
- `nezia-core` の `SoundEngine` を所有し、その寿命を管理する
- 外部ツールが指定したアセットをロード・再生・停止し、結果イベントを返す
- ミキサーアセット (NeziaMixerAsset 由来の構成) を反映する

`daemon` が引き受けないもの:

- **ゲーム本体への組み込み**: ランタイムは `nezia-ffi` を直接リンクする経路を使う。
  daemon を経由しない (レイテンシ・配布粒度の観点から)。
- **オーサリングツールの UI 実装**: daemon は IPC バックエンドのみ。UI は別プロセス
  (Unity Editor / 専用 authoring tool) が担当する。
- **アセットフォーマット策定**: 受け取るのは既存 core のフォーマット (WAV / Ogg / MP3 etc.)
  と将来の `.nez`。daemon 固有のフォーマットは持たない。

---

## アーキテクチャ概要

```
┌──────────────────────┐         ┌──────────────────────────────┐
│ Unity Editor         │         │ daemon (per-Editor session)  │
│                      │         │                              │
│  Grpc.Net.Client ────┼─── gRPC ┼────► tonic server            │
│    over loopback TCP │  (HTTP/2│         │                    │
│                      │  127.0.0.1:port)  │                    │
│  reads port from     │         │         ▼                    │
│  $TMPDIR/.port file  │         │   SoundEngine (nezia-core)   │
│                      │         │     ├─ メインスレッド         │
│                      │         │     └─ サウンドスレッド (cpal)│
└──────────────────────┘         └──────────────────────────────┘
        │ parent PID                       │ self-exit if parent dies
        └──────────────────────────────────┘
```

外部ツールが「制御」を gRPC で送り、daemon が `nezia-core` の `SoundEngine` を**長寿命で
所有・操作する**。サウンドスレッドは daemon プロセス内で動き、Editor からは見えない。

---

## 採用した設計判断

各論点と理由を以下に明示する。判断の背景については 0.2.x 設計議論を参照。

### 1. プロセスモデル — per-Editor-session spawn

Editor が起動時に daemon を `Process.Start` で spawn し、Editor 終了で kill する。

- **代替案 (グローバル共有 daemon)** は複数 Unity プロジェクト同時開発で衝突する。
  Preview 用途では「Editor 1 個に daemon 1 個」が最もライフサイクルが単純。
- daemon binary は Unity package に Editor-only として同梱 (後述「配布」節)。

### 2. IPC — gRPC over loopback TCP

- **トランスポート**: `127.0.0.1` (loopback) に bind した TCP ソケット。OS が割り当てた
  ephemeral port を **port discovery file** (`$TMPDIR/nezia-daemon-{parent_pid}.port`)
  に書き、Editor は同ファイルを読んで接続する。
- **プロトコル**: gRPC over HTTP/2 + Protocol Buffers。
  - Rust 側: `tonic` + `prost`、`tonic-build` で `.proto` から Rust コード生成 (OUT_DIR、非 commit)
  - C# 側: `Grpc.Net.Client` + `Google.Protobuf` を Unity package に Editor-only DLL 同梱

**選定理由**:

- スキーマ駆動 (`.proto`) で daemon / Editor / 将来の authoring tool 間の API が型レベルで
  一元管理される。breaking change が PR 段階で見える。
- gRPC のストリーミング RPC が **Event の push** に自然に乗る (独自フレーミング不要)。
- **loopback TCP** を選んだ理由:
  - 速度: UDS / Named Pipe との実測差は preview 用途の頻度では誤差。
  - セキュリティ: `127.0.0.1` 限定 bind により OS レベルで外部接続を拒否。macOS / Windows の
    OS Firewall は loopback 通信を監視しないため、Firewall ダイアログは発生しない。
  - 互換性: `Grpc.Net.Client` の UDS / Named Pipe 経由は `ConnectCallback` (.NET 5+ API) を
    必要とし、Unity Mono ランタイムでの互換性が不安定。loopback TCP は数十年枯れた API で
    Unity Editor の全プラットフォームで確実に動く。
- **port discovery file** は LSP / DAP (Debug Adapter Protocol) と同様の標準パターン。

### 3. RPC モデル — Unary + Server-streaming

| 種別 | 用途 | 例 |
|---|---|---|
| Unary RPC | 単発リクエスト / レスポンス | `LoadBuffer`, `Play`, `Stop`, `LoadMixer` |
| Server-streaming RPC | daemon → Editor の継続的 push | `SubscribeEvents` (`SourceFinished` / `Error` 等) |
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

Editor が Asset DB 経由で持つ絶対パスをそのまま `LoadBuffer { path: string }` で渡す。

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

- ワークスペースルートに `proto/` を置く。Rust / C# 双方が同じ source of truth を参照。
- バージョンを **パッケージ名に埋め込む** (`nezia.v1`) ことで、将来の breaking change 時に
  `v2` を並走させる余地を残す。
- Rust 側: `tonic-build` が `build.rs` で OUT_DIR にコード生成 (git に commit しない)。
- C# 側: 生成コードを **Unity package の `Editor/Generated/` に commit** する。
  Unity ビルドに protoc を要求しないことが目的。生成は core repo の CI で行い、
  Unity package の release pipeline が取り込む形を想定 (詳細は別 PR)。

### 8. 配布

Unity package 側の配置:

```
jp.nezia.unity/
  Editor/
    Bin/
      macOS/      nezia-daemon
      Windows/    nezia-daemon.exe
      Linux/      nezia-daemon
    Plugins/
      Grpc.Net.Client.dll          ← Editor-only
      Google.Protobuf.dll          ← Editor-only
    Generated/
      Nezia/V1/                    ← protoc 生成済み C# コード
```

- daemon binary は **Editor-only**。Player ビルドには絶対に含めない (Unity の
  `PluginImporter` 設定で `IncludeInBuildTarget` をすべて Off に)。
- ランタイム (`Runtime/Plugins/{platform}/`) には従来通り `nezia-ffi` のみ。
- core repo の CI が各プラットフォームの daemon binary を artifact として出力し、
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

- daemon が `StartProfiler` の Response で **共有メモリの名前** を返す。
- Editor が `MemoryMappedFile.OpenExisting(name)` で memory-map し直接読む。
- 既存 [`capture.rs`](../core/capture.rs) (master post-fader タップ) の lock-free SPSC リング
  パターンを共有メモリ上に展開するイメージ。
- `0.2.0 では実装しない` — gRPC 制御経路の余地を残しておくのみ。

これにより gRPC = **制御信号 / 通常頻度の状態問い合わせ**、共有メモリ = **高頻度
リアルタイム telemetry** という業界標準パターン (Tracy / Unity Profiler / Visual Studio
Profiler) に揃う。

---

## 非目標

- **マルチクライアント受付**: 1 daemon プロセスに対し同時接続するクライアントは Editor 1 個のみ。
  authoring tool との同時接続は別 daemon プロセスを起動する。
- **リモート接続**: 同一マシンの loopback 限定。LAN / リモートデバッグ用途は想定しない。
- **永続化**: daemon はプロセス寿命の状態しか持たない。プロジェクトファイルの保存・
  ロードは Editor / authoring tool 側の責務。
- **音声バイナリの加工 / 変換**: daemon は core が解釈できる既存フォーマットを受けて鳴らすだけ。
  `nezia-pack` 相当の変換ツールは別バイナリ。

---

## 0.2.0 までに実装する範囲

詳細は [`better-than-unity-audio.md`](../../roadmap/better-than-unity-audio.md) の
Phase 4-α / Phase 4-3 を参照。0.2.0 の到達目標は **Unity IP-6 Asset Preview を解除する
最小スコープ (Tier 2)**:

- [ ] `proto/nezia/v1/daemon.proto` 確定
- [ ] daemon binary 骨格 (gRPC server + port discovery file + parent PID 監視)
- [ ] `LoadBuffer` / `Play` / `Stop` (Tier 1 相当)
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

- [Better than Unity Audio ロードマップ](../../roadmap/better-than-unity-audio.md) — Phase 4-α
- [Integration Experience ロードマップ](../../../../Nezia_Integration/Packages/jp.nezia.unity/docs~/roadmap/integration-experience.md) — IP-6 (Asset Preview)
- [統合戦略](../integration/CONCEPT.md) — ドロップイン互換 (A 経路) / プロジェクトファイル方式 (B 経路) の 2 経路方針
- [スレッドモデル](../core/threading.md) — サウンドスレッドのリアルタイム制約 (将来の共有メモリ側経路でも遵守)
- [マスター出力キャプチャ](../core/capture.md) — 共有メモリ side-channel の参考実装
- [ECS アーキテクチャ](../core/ecs.md) — daemon が所有する SoundEngine の内部構造
