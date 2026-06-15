# Roadmap — NEZIA ENGINE

NEZIA ENGINE が「どの順で・なぜ・何を実装するか」をエンジン全体で定義する最上位ロードマップ。
個別機能の詳細設計は `docs/design/{core,daemon,ffi,integration}/*.md` を正とし、ここでは
**領域横断の優先順序と判断基準**のみを扱う。

---

## なぜ NEZIA か — 革新性の3本柱

ロードマップが「何を・どの順で」を定義するのに対し、本節は「**なぜ作るに値するか**」を定義する。
NEZIA が他のサウンドミドルウェアに対して主張する革新は、以下の 3 本に絞られる。
各柱には**現状の解像度**を併記し、「既にある強み」と「これから作る差別化」を取り違えないようにする。

### 柱1. データ指向アーキテクチャ全体 — 速い・大規模

> サウンドオブジェクト（バス / Source / エフェクト）**全体**をスパースセット + SoA + ECS で
> 構成し、コンポーネント種別ごとの密配列を一括処理する。

- DSP の**バッファ単位処理**（連続サンプル列の SIMD 一括処理）にとどまらず、
  **オブジェクトモデルそのもの（バス / Source / エフェクトの管理）まで DoD 化**している。
  これにより密配列の一括処理がエンジン全体に行き渡る。
- 帰結: 大規模同時発音・キャッシュ効率・低レイテンシという**圧倒的なパフォーマンス**。
  詳細は [ECS アーキテクチャ](../design/core/ecs.md) / [スレッドモデル](../design/core/threading.md)。
- **解像度**: アーキテクチャは**実在・稼働中**。主張は他社比較ではなく
  「**DoD によって圧倒的なパフォーマンスを実現している**」という自己完結の形を取る
  (競合の内部実装を持ち出さない)。

### 柱2. アルゴリズム非依存のエフェクト選択 — 知識ゼロで選べる 〔設計方針確定・未実装〕

> cutoff Hz / Q / threshold dB / ratio といった **DSP のパラメータを理解しなくても**、
> 「こもらせる」「遠くで鳴っている感」「会話の下でダッキング」のような**意図**でエフェクトを選べる。

Wwise / FMOD / Unity はいずれも**アルゴリズムのパラメータを露出**しており、ユーザーに DSP 知識を
要求する。ここを意図ベースに変えるのが柱2の狙い。

#### 主軸: 意図マクロ層 (Intent Macro)

「プリセットを並べるだけ」では Wwise/FMOD も持つため差別化にならない。柱2 が差別化になる条件は
**意図が 1 つの意味軸になっていて、複数エフェクトを協調させる**こと。これを満たす中核機構を
**意図マクロ (Intent Macro)** と呼ぶ。

- **意図 = データアセット**。`DistanceFeel` のような意図を 1 つの意味軸 `amount: 0..1` として
  アセット化する。Clip-centric / Mixer-as-asset と同じ「設計情報はデータ」の思想に一貫させる。
- **1 ノブが複数エフェクトを協調**。`amount` がカーブ経由で複数パラメータを同時に動かす:

  ```
  DistanceFeel (intent asset)
    amount: 0.0 ───●─── 1.0
     ├─ LowPass.cutoff : 20kHz → 800Hz   (curve)
     ├─ Reverb.send    : 0.0 → 0.6        (curve)
     └─ Volume         : 0dB → -6dB        (curve)
  ```

  ユーザーが見るのは `amount` 1 つだけ。アルゴリズム名・パラメータは隠れる。
- **escape hatch を残す**。意図マクロは生エフェクト層の**上に乗る薄い層**であり、置き換えではない。
  パワーユーザーは展開して cutoff 等を直接生編集できる。既存の type-safe effect API (IP-2) は
  そのまま「展開後の生パラメータ」として機能する。
- **DSP 知識は engine 側に一度だけ焼く**。良い音がする `amount → params` のマッピングを作るには
  オーディオ設計の専門性が要る。これを**curated な意図マクロ集として engine に同梱**し、
  ユーザーは知識ゼロで使う。**この曲線設計の質こそが柱2 の実体**であり、API だけ作っても
  マッピングが凡庸なら差別化にならない (ここが最大の難所)。

#### 副軸: LLM オーサリング (柱3 と合成)

意図マクロを「人が作る／調整する」工程に、[柱3](#柱3-daemon-による統合作業環境--エディタ内でライブに観る試す)の
**daemon CLI / LLM 経路**を重ねられる:

- 「もっとチープに、電話越しみたいに」のような自然言語 → LLM が意図マクロの新規生成 or
  既存マクロの `amount` / カーブ調整を提案 → daemon 経由で即試聴。
- すなわち **柱2 のデータモデル（意図マクロ）を、柱3 の経路（LLM/CLI）が増幅する**関係。
  LLM 単独で生パラメータを叩く案より、意図マクロという中間表現を挟む方が再現性・編集性が高い。

#### 解像度と配置

- **現状は未実装**。type-safe effect API (IP-2) はパラメータ露出のままで、まだ他社と同じ。
  **「今ある強み」ではなく「これから作る差別化」**である点は変わらない。
- 本ロードマップでは **M3 (Unity より良いと言える) の差別化項目**として扱う
  (可視化基盤の上で、意図マクロの効きを試聴・可視化しながら curate する)。
- 詳細設計 (意図マクロのデータ構造 / カーブ表現 / 同梱マクロの初期セット / FFI 露出) は
  [意図マクロ (Intent Macro) 設計](../design/core/intent-macro.md) に確定済み。

### 柱3. daemon による統合作業環境 — エディタ内でライブに観る・試す

> 実機と**同一の core** を別プロセス (daemon) で駆動し、ゲームエンジン (Unity) の**エディタ内**に
> ライブの試聴・可視化・プロファイリングを**一手に**持ち込む。

- 注意: 「試聴・可視化できること」自体は Wwise / FMOD も自前のオーサリングアプリで実現している。
  したがって NEZIA の主張は「試聴ができる」ではなく、**統合の仕方**にある:
  - 別アプリに切り替えず、**ホストエディタ内で実機と同じ音**を確認できる (コンテキストスイッチゼロ)
  - **同じ daemon** が試聴・アクティブソース可視化・dB メーター・プロファイラを兼ねる
    (バラバラのツールにならない)
  - 外部ツールはすべて**共通の薄い CLI (`nezia-cli`) を front door として daemon を叩く**。
    Unity Editor (`Process` 起動 + stdout)・LLM / エージェント (コマンド)・将来の
    authoring tool (B経路) が**同一の front door** を共有する。GUI を介さずアセットの
    ロード・再生・ミキサー構成の問い合わせをコマンドで叩けるため、「サウンドを言葉で指示して
    試聴・調整する」エージェント駆動のワークフローに自然に乗る。GUI を前提とする既存
    ミドルウェアのオーサリングアプリには無い経路。

  ```
                         ┌── Unity Editor      (Process 起動 + stdout)
  nezia-daemon ◄─ gRPC ─ nezia-cli ◄───────────── LLM / エージェント (コマンド)
  (長寿命 core)   内部     (front door)            └── Nezia authoring tool (B経路)
  ```

  - **gRPC は daemon ↔ cli の内部プロトコルに閉じる**。外部フロント (Editor / authoring tool /
    エージェント) に gRPC・HTTP/2・protobuf の依存を持ち込まない (Unity 側は標準 `Process`
    起動のみ、追加 DLL ゼロ)。これにより「1 つの core backend を 3 種のフロントが共有する」
    統合が、各フロントの実装言語・ランタイム制約に縛られずに成立する。
- **解像度**: daemon は[仕様確定済み](../design/daemon/CONCEPT.md)。**骨格 (gRPC server +
  port discovery + parent PID 監視 + `LoadBuffer`/`Play`/`Stop`) は実装済み**で、`nezia-cli`
  と Tier 2 (Bus/Mixer ロード・Clip-centric 反映・Random・イベント) がこれから (M1)。
  「統合の仕方」を曖昧にすると「他社もやっている」に溶けるため、上記の差分を主張の核に据える。

### 柱に含めないもの — ドロップイン互換

`AudioSource` ドロップイン互換 (学習コストゼロ採用 / Day 1 モック / 段階移行) は強力だが、
これは **`jp.nezia.unity` 統合層の性質**であって**エンジン本体の革新ではない**。
かつ「技術的革新」ではなく「**採用の摩擦を下げる Go-to-Market 戦略**」とカテゴリが異なる。
価値はあるので [CONCEPT.md A 経路](../design/integration/CONCEPT.md) で扱うが、
**革新性の柱には数えない**。

> **3 本柱の現状サマリ**: 柱1 は実在・稼働中、柱2 は設計方針確定 (意図マクロ)・未実装、
> 柱3 は仕様確定・実装これから。**「既にある」のは柱1 のみ**であり、柱2・柱3 を実体化することが
> NEZIA を「語れるエンジン」にする本丸である。これは後続のマイルストーン
> (M1 = 柱3 の試聴、M3 = 柱3 の可視化 + 柱2 の意図マクロ) と直接対応する。

---

## この文書の軸 — なぜ描き直したか

旧ロードマップ (`better-than-unity-audio.md`) は **「Unity 標準との parity → 差別化」という
ランタイム単一軸**で構成されていた。これは「鳴らす力」を測る軸としては正しかったが、
構造的な歪みを生んだ:

- **「作る・試す・観る」体験がランタイム Phase に埋もれた。** プレビュー試聴 (preview daemon)・
  可視化・本格オーサリングのような authoring 体験が、ランタイム機能の「Phase 4-α」等として
  差し込まれ、優先度も依存関係も見えなくなっていた。
- **進捗の非対称が表現できなかった。** 実際にはランタイムは parity をほぼ達成している一方で、
  「編集中の音をその場で試聴する」という最も基本的な authoring 体験がまだ存在しない。
  単一軸ではこのズレが「Phase 4 の途中」としか書けなかった。

そこで本ロードマップは軸を **ユーザー価値マイルストーン**に置き換える。
「ユーザーが何をできるようになるか」を最上位に据え、その下に **2 つのトラック**
(Runtime / Authoring・Experience) を束ねる。

```
              M1            M2            M3            M4
        鳴らせる       Unity から     Unity より     本格制作
        試聴できる     移行できる     良いと言える   できる
        ───────────────────────────────────────────────────────►
Runtime   再生基盤    parity 完成   Cone/Container  大型差別化
(鳴らす)   [済]         [済]         可視化の対象      (分岐)

Authoring  preview      Clip-centric  プロファイラ   B経路
・Experience daemon       authoring     可視化        オーサリング
(作る/試す/観る) [未]       [済]         [未]          .nez / hot reload
```

> **マイルストーンは順序ゲートではなく「主張できる立ち位置」である。**
> 現在地は後述のとおり非対称で、M2 (parity) のランタイムは達成済みだが、
> M1 の「試聴できる」がまだ空白という状態が併存する。順番に M1→M4 を埋めるのではなく、
> **各マイルストーンが『揃った』と言えるために何が欠けているか**で優先度を決める。

---

## ユーザー価値マイルストーン

| マイルストーン | 主張できる立ち位置 | 「揃った」と言える条件 |
|---|---|---|
| **M1 鳴らせる・試聴できる** | 音を鳴らせ、編集中に**その場で試聴**できる | ランタイム再生基盤 + preview daemon で Editor から試聴可 |
| **M2 Unity から移行できる** | Unity 標準 Audio + AudioMixer の置き換えとして実用十分 | parity gap が埋まり、既存プロジェクトを移行できる |
| **M3 Unity より良いと言える** | Unity 単体にない機能を持ち、**移行する積極的動機**がある | Cone/Container 等の差別化 + データ指向を裏付ける可視化 |
| **M4 本格制作できる** | サウンド専門スタッフが本格的なサウンド設計を回せる | プロジェクトファイル方式オーサリング (CONCEPT.md B 経路) |

定義の根拠は [統合戦略 CONCEPT.md](../design/integration/CONCEPT.md) の 2 経路方針
(A: ドロップイン互換 / B: プロジェクトファイル方式) に対応する。M1〜M3 は主に A 経路と
ランタイム能力、M4 は B 経路が主役になる。

---

## 現在地 — 正直なスナップショット

**本ロードマップで描き直した最大の理由がここにある。** 進捗はマイルストーン順ではなく、
トラックごとに大きく非対称である。

| トラック | 到達度 | 補足 |
|---|---|---|
| **Runtime (鳴らす力)** | **M2 相当まで到達**。M3 の一部も先取り | DSP/Send/Snapshot/PlayScheduled/Voice Virtualization まで実装済。Listener Focus 等の差別化機能も既にある |
| **Authoring — Config 設計** | **成立** | Mixer / Snapshot / Clip-centric authoring が Inspector で組める (Unity IP-1〜4 完了) |
| **Authoring — 試聴 (試す)** | **空白** ← 最大の非対称 | 編集中の音をその場で鳴らす preview daemon が未実装。**M1 の最も基本的なピースがまだ無い** |
| **Authoring — 可視化 (観る)** | **空白** | バスツリー/アクティブソース/dB メーターを覗くプロファイラが未実装。M3 の片輪が欠けている |
| **Authoring — 本格オーサリング (B経路)** | **未着手** | M4。プロジェクトファイル方式・`.nez` 形式は構想段階 |

### この非対称から導かれる直近の最優先

> ランタイムは「Unity 並み」に達しているのに、**作ったサウンドを編集中に聴く手段が無い。**
> これは「鳴らせる・試聴できる」という最も基本的な M1 の未達であり、機能の派手さに関係なく
> **authoring 体験の最大のボトルネック**である。
>
> したがって直近の最優先は **preview daemon (M1 の試聴ピース)** である。
> Unity 統合ロードマップでも IP-6 Asset Preview が「他のどんな自動化より先に効く」と
> 明言されつつ daemon 依存で止まっている。ここを開けることが最も即効性が高い。

---

## 各マイルストーンの中身

各マイルストーンを Runtime トラックと Authoring・Experience トラックの 2 列で束ねる。
Authoring 側の `IP-n` は Unity 統合ロードマップ
([`integration-experience.md`](../../../Nezia_Integration/Packages/jp.nezia.unity/docs~/roadmap/integration-experience.md))
のフェーズ番号。

### M1 — 鳴らせる・試聴できる

**主張**: 音を鳴らせ、編集中にその場で試聴できる。

| トラック | 項目 | 状態 |
|---|---|---|
| Runtime | ECS / バス / Source / Spatial 基本 / DSP (LPF/HPF/Reverb) / Ogg・MP3 ストリーミング | **済** |
| Authoring・Experience | **preview daemon** — Editor / オーサリングツールから core を別プロセスで駆動し試聴 | **未 ← 最優先** |

- daemon の責務・IPC 設計は [daemon CONCEPT.md](../design/daemon/CONCEPT.md) で確定済み
  (gRPC over loopback TCP / per-Editor-session spawn / parent PID 監視)。
- core 側スコープ: `proto/nezia/v1/daemon.proto` 確定 → daemon binary 骨格 →
  `LoadBuffer`/`Play`/`Stop` → **`nezia-cli` (daemon gRPC を叩く front door)** →
  Bus/Mixer ロード → Clip-centric パラメータ反映 → Random Container プレビュー →
  `SubscribeEvents`。詳細は daemon CONCEPT.md の「0.2.0 までに実装する範囲」。
- Unity 側ペア: **IP-6 Asset Preview** — `nezia-cli` を `Process` 起動して試聴する
  (Editor 側に gRPC / HTTP クライアントを持たない。追加 DLL ゼロ)。

**M1 が揃う条件**: アーティストが Project ビューで ▶ を押すと daemon 経由で
**Clip の音響パラメータ込みの音が出る**。

### M2 — Unity から移行できる

**主張**: Unity 標準 Audio + AudioMixer の置き換えとして実用十分。既存プロジェクトを移行できる。

| トラック | 項目 | 状態 |
|---|---|---|
| Runtime | Doppler / Voice Virtualization / Custom Attenuation Curve / Mixer Snapshot / Send・Sidechain Ducking / PlayScheduled / ParamEQ・Compressor・Limiter | **済** |
| Authoring・Experience | Mixer アセット化 (IP-1) / Effect type-safe (IP-2) / Snapshot アセット化 (IP-3) / **Clip-centric 責務再設計 (IP-4)** | **済** |
| 残 parity gap | 非同期ロード / Exposed Parameters / Chorus・Flanger・Distortion / Spread | **未 (低〜中優先)** |

- ランタイムの parity はほぼ達成済み。Clip-centric authoring (IP-4) により
  「鳴り方は Clip が持ち、Source は『いつ・どこで』だけ」という業界標準の責務分離が成立した。
- 残る parity gap は採用判定の致命傷ではないものが中心。M1 (試聴) と M3 (可視化) を
  優先し、ここは個別に随時埋める。

**M2 が揃う条件**: Unity プロジェクトの実例の大部分が、`ReplaceAudioSources` 相当の
移行操作で NEZIA バックエンドに載る。

### M3 — Unity より良いと言える

**主張**: Unity 単体ではできないことが標準で書け、移行する積極的動機がある。

| トラック | 項目 | 状態 | 連動 |
|---|---|---|---|
| Authoring・Experience | **ランタイムプロファイラ FFI + デバッグビジュアライザ基盤** (バスツリー/アクティブソース/dB メーター) | **未** | Unity IP-10 |
| Runtime (露出) | **PlayScheduled の Unity 露出** (core 完了済) | **未** | Unity IP-7 |
| Runtime (露出) | **論理ID / Sound Dictionary 経路** (Hash ID ↔ EntityId の二層を実プロダクトで活かす) | **未** | Unity IP-8 |
| Runtime (差別化) | **Switch / Sequence Container** ([設計](../design/core/container.md)、Random は実装済) | **未** | Unity Container Inspector |
| Runtime (差別化) | **Sound Cone (SP-11)** 指向性音源 | **未** | Unity 露出 (薄い) |
| Runtime (差別化) | **意図マクロ (Intent Macro)** — アルゴリズム非依存エフェクト ([設計](../design/core/intent-macro.md)) | **未 (設計確定)** | Unity 露出 (意図ノブ) |

順序の判断:

- **可視化 (プロファイラ) を差別化機能より前**に置く。データ指向設計の利点は
  可視化で初めて伝わり、[`post-unity-performance.md`](post-unity-performance.md) の
  「測ってから書く」前提でもある。Sound Cone 等の性能影響もこの上で測りたい。
- **PlayScheduled 露出**はコスト小・core 完了済で Unity IP-7 を即解除できる cheap win。
- **Sound Dictionary** は CLAUDE.md が掲げる二層 ID 設計の真価が現れる箇所。
  IP-4 (Clip-centric) 完了で前提が揃った。
- **Switch/Sequence** と **Sound Cone** は純粋差別化として可視化基盤の上に積む。

**M3 が揃う条件**: 「Unity 単体ではスクリプトを書かないとできないこと
(指向性・Cue 系コンテナ・ミキサー可視化)」が NEZIA では標準機能として書ける。
**この時点で「Unity より良い」を最短で主張可能になる。**

### M4 — 本格制作できる

**主張**: サウンド専門スタッフが、専用オーサリングツール上で本格的なサウンド設計を回せる
(CONCEPT.md B 経路)。

| 項目 | 内容 |
|---|---|
| **プロジェクトファイル方式オーサリング** | 論理 ID 体系・バス階層・イベント・ランダム化を専用ツールで設計し、プロジェクトファイルとして出力。ランタイムがロードしてプレイ |
| **`.nez` 独自フォーマット** | loop point・メタを埋め込んだ独自コンテナ ([付録参照](#付録-nez-独自フォーマット-asset-container)) |
| **ホットリロード / 編集時プレビュー** | preview daemon (M1) の上に構築 |
| **大型差別化 (ターゲット層で分岐)** | Occlusion / HRTF / 5.1・7.1 サラウンド。両方やるとフェーズが伸びすぎるため案件で分岐 |

- M4 は preview daemon (M1) と可視化基盤 (M3) が揃ってから本格化する。
  daemon は「Editor とオーサリングツールが同じバックエンドを共有する」土台であり、
  authoring tool も Editor と同じく **`nezia-cli` 経由で daemon にアクセスする**
  (GUI フロントを問わず front door を `nezia-cli` に統一)。M1 で作る daemon + cli が
  そのまま M4 の前提になる。
- 大型差別化の分岐方針:
  - **モバイル / カジュアル / TPS・FPS 向け**: Occlusion (SP-12) + Reverb Zone
  - **VR / コンソール / ヘッドフォン主体向け**: HRTF (SP-13) + サラウンド
  - 判断は M3 完了時のターゲット案件次第。

---

## 直近の作業キュー

現在地の非対称から導かれる優先順。上から着手する。

1. ~~**preview daemon — `daemon.proto` 確定 + daemon binary 骨格**~~ **(完了)**
   gRPC server + port discovery file + parent PID 監視 + `LoadBuffer`/`Play`/`Stop`。
2. **`nezia-cli` — daemon gRPC を叩く薄いクライアント** (M1)
   `load` / `play` / `stop` サブコマンド。Unity Editor・LLM/エージェント・authoring tool
   共通の front door。結果は stdout に JSON で出す。これで Editor 側は `Process` 起動だけで
   試聴でき、gRPC/HTTP クライアントを Editor に持たずに済む。
3. **preview daemon — Bus/Mixer ロード + Clip-centric 反映 + Random + `SubscribeEvents`** (M1)
   Unity IP-6 を解除する最小スコープ (daemon CONCEPT.md の Tier 2)。`SubscribeEvents` は
   `nezia-cli subscribe` の stdout ストリームとして露出する。
4. **ランタイムプロファイラ FFI + デバッグビジュアライザ基盤** (M3、Unity IP-10)
   差別化機能より前。可視化の共有メモリ side-channel は daemon CONCEPT.md「将来拡張」参照。
5. **PlayScheduled Unity 露出** (M3、Unity IP-7、cheap win)
6. **Sound Dictionary 経路の整備** (M3、Unity IP-8)
7. **Switch / Sequence Container** (M3)
8. **Sound Cone (SP-11)** (M3)

---

## 設計上のガードレール

すべてのマイルストーンで守る原則。

1. **既存 ECS / SoA / SIMD / リングバッファコマンドのパターンを壊さない。** 新機能は原則、
   既存 ECS 上に SoA 列追加 + コマンド追加で実装する。
   - 新しい同期機構の導入は **デフォルトでは避ける**が、完全には禁じない。リングバッファ
     コマンドより明確に有利な性能・遅延特性が**実測で**示せる場合 (Triple Buffer による
     リスナー状態のロックフリー共有など) は導入を検討してよい。
   - 導入する場合は (a) サウンドスレッドのリアルタイム制約 (ロック・確保・syscall なし) を
     破らないこと、(b) ベンチマークで既存方式との比較を残すこと、(c) 設計ドキュメントに
     採用理由を明記すること、を満たす。
   - 「なんとなく速そう」での追加は不可。**新しい同期機構は 1 つ増やすたびに正味の利得を
     説明できる状態**を保つ。
2. **サウンドスレッドはロック・確保・syscall を行わない** ([threading.md](../design/core/threading.md))。
   新機能でも例外を作らない。preview daemon の高頻度 telemetry も共有メモリ side-channel で
   この制約を守る (daemon CONCEPT.md「将来拡張」)。
3. **`spatial_enabled = false` / `effect_enabled = false` などの最速経路を保つ。**
   機能追加で 2D ソースや素通しバスが遅くならないこと。
4. **Unity 標準にある機能は Unity 互換のデフォルト値で動く。** 新規パラメータは
   「設定しなければ Unity と同じ挙動」を維持する。
5. **API 形はメジャー実装に寄せる。** Cone は OpenAL/FMOD/Web Audio、HRTF は
   Steam Audio/Resonance Audio、Effects は AudioMixer の語彙に揃え、学習コストを
   ゼロに近づける。
6. **二経路ワークフロー (CONCEPT.md A/B) を常に意識する。** 機能追加時に「ドロップイン
   互換側でどう見えるか」「プロジェクトファイル側でどう設定するか」を両方検討する。
7. **core の機能漏れを Integration で補わない。** parity gap は core 側で埋める。
   Integration は「鳴らしやすさ」に専念する (順序逆転による保守不能を防ぐ)。
8. **外部フロントは `nezia-cli` を front door とし、gRPC を直接話さない。** Unity Editor /
   authoring tool / LLM・エージェントはすべて `nezia-cli` 経由で daemon を叩く。gRPC は
   daemon ↔ cli の内部プロトコルに閉じ、GUI フロント側に gRPC / HTTP2 / protobuf の依存を
   持ち込まない (Editor 統合は標準 `Process` 起動 + stdout のみ、追加 DLL ゼロ)。
   これにより 1 つの core backend を複数フロントが言語・ランタイム制約なく共有できる。

---

## 付録 A — 旧 Phase ↔ 新マイルストーン対応表

旧 `better-than-unity-audio.md` の Phase 番号を参照している設計ドキュメントのために、
対応関係を残す。

| 旧 Phase | 内容 | 新マイルストーン |
|---|---|---|
| Phase 1 | ECS / バス / Source / Spatial 基本 / Listener Focus | M1 (Runtime) |
| Phase 2 | Doppler / Voice Virtualization / DSP 土台 / Ogg ストリーミング | M1〜M2 (Runtime) |
| Phase 3 | Custom Attenuation / Snapshot / Send / PlayScheduled / ParamEQ・Compressor・Limiter | M2 (Runtime) |
| Phase 4-α | daemon CONCEPT + preview IPC 仕様 | **M1 (Authoring — 試聴)** |
| Phase 4-3 | プロファイラ + ビジュアライザ基盤 | M3 (Authoring — 可視化) |
| Phase 4-7 | PlayScheduled Unity 露出 | M3 (Runtime 露出) |
| Phase 4-8 | Sound Dictionary | M3 (Runtime 露出) |
| Phase 4-2 | Switch / Sequence Container | M3 (Runtime 差別化) |
| Phase 4-1 | Sound Cone | M3 (Runtime 差別化) |
| Phase 5 | Occlusion / HRTF / Reverb Zone / サラウンド | M4 (大型差別化・分岐) |
| Phase 6 | プラグイン SDK / B経路オーサリング / `.nez` / Unreal | M4 |

> 設計ドキュメント内の「Phase 3-3」「Phase 4-α」等の表記は当面そのまま有効。
> 段階的に新マイルストーン表記へ寄せる。

---

## 付録 B — 領域別ギャップ分析 (Unity 標準との差分)

M2 (parity) の判断根拠となる、Unity 標準との領域別ギャップ。NEZIA がカバーすべき領域を
7 つに分ける。区分の「実装済」はランタイム実装を指す。

### A. Spatial Audio (3D サウンド)

| 機能 | Unity 標準 | NEZIA | 区分 |
|------|-----------|-------|------|
| 距離減衰 (3 モデル) | ○ | **○** | 実装済 (SP-01, SP-04) |
| ステレオパン | ○ | **○** | 実装済 (SP-02、後方連続化済 #8) |
| リスナー管理 | ○ | **○** | 実装済 (SP-03) |
| 2D/3D 切替 | ○ | **○** | 実装済 (SP-05) |
| Listener Focus (仮想リスナー) | ✕ | **○** | **差別化 (実装済 SP-06)** |
| Doppler 効果 | ○ | **○** | 実装済 (SP-10) |
| Custom Attenuation Curve | ○ | **○** | 実装済 |
| Spread (ステレオ広がり) | ○ | ✕ | Parity gap (低優先) |
| Sound Cone (指向性音源) | ✕ | ✕ | **M3 差別化 (SP-11)** |
| Occlusion | ✕ | ✕ | M4 差別化候補 (SP-12) |
| HRTF | ✕ (要プラグイン) | ✕ | M4 差別化候補 (SP-13) |

### B. DSP / エフェクト

| 機能 | Unity 標準 | NEZIA | 区分 |
|------|-----------|-------|------|
| Bus 単位エフェクト挿入 | ○ (Mixer) | **○** | 実装済 |
| Reverb / SFX Reverb | ○ | **○** | 実装済 (Bus 専用、Send で共有) |
| LPF / HPF / ParamEQ | ○ | **○** | 実装済 (PeakingEq 含む) |
| Compressor / Limiter | ○ | **○** | 実装済 (Compressor / 単体 Limiter / master soft limiter) |
| Chorus / Flanger / Distortion | ○ | ✕ | Parity gap (中優先) |
| Source 単位 LPF (距離・遮蔽連動) | ✕ (要 Mixer 経由) | ✕ | 差別化候補 |
| プラグイン SDK (ユーザー定義 DSP) | ○ (Native Audio Plugin) | ✕ | M4 |

### C. アセット / ストリーミング

| 機能 | Unity 標準 | NEZIA | 区分 |
|------|-----------|-------|------|
| WAV / PCM 読込 | ○ | **○** | 基本 |
| Vorbis / Ogg | ○ | **○** | 実装済 |
| MP3 | ○ | **○** | 実装済 (symphonia 経由) |
| Opus | △ | ✕ | 差別化候補 (BGM 圧縮率) |
| ストリーミング再生 (BGM 用) | ○ | **○** | 実装済 |
| 非同期ロード | ○ | ✕ | Parity gap |
| メモリ常駐圧縮 (ADPCM 等) | ○ | ✕ | M4 |

### D. Mixer / ルーティング

| 機能 | Unity 標準 | NEZIA | 区分 |
|------|-----------|-------|------|
| バス階層 | ○ | **○** | 実装済 |
| Source → バスルーティング | ○ | **○** | 実装済 |
| バス音量・ミュート | ○ | **○** | 実装済 |
| **Send / Receive** (副ルート) | ○ | **○** | 実装済 |
| **Snapshot** (バス状態のスムーズ補間) | ○ | **○** | 実装済 |
| Exposed Parameters | ○ | ✕ | Parity gap |
| Sidechain Ducking | ○ (Send 経由) | **○** | 実装済 |
| バスの動的追加・削除 | ✕ (Editor 編集のみ) | △ (要確認) | 差別化候補 |

### E. 再生制御

| 機能 | Unity 標準 | NEZIA | 区分 |
|------|-----------|-------|------|
| Play / Stop / Pause | ○ | **○** | 実装済 |
| Loop | ○ | **○** | 実装済 |
| 音量 / ピッチ | ○ | **○** | 実装済 |
| **PlayScheduled (サンプル精度)** | △ (dspTime 制約あり) | **○** | 実装済 (Unity 露出は M3) |
| **Voice Virtualization** (発音数超過時) | ○ | **○** | 実装済 |
| Priority (発音優先度) | ○ | **○** | 実装済 |
| **Random / Switch / Sequence Container** | ✕ (要スクリプト) | △ (Random のみ) | **M3 差別化 (大)** |
| ループ点 (loop start/end) | △ | ✕ | M4 (`.nez` と対で実装) |

### F. 出力 / プラットフォーム

| 機能 | Unity 標準 | NEZIA | 区分 |
|------|-----------|-------|------|
| ステレオ出力 | ○ | **○** | 実装済 |
| 5.1 / 7.1 サラウンド | ○ | ✕ | M4 (コンソール向け) |
| サンプルレート可変 | ○ | △ (要確認) | 基本 |
| バックエンド抽象 (CoreAudio / WASAPI / ALSA) | ○ | △ (`audio.rs` 要確認) | 基本 |
| モバイル (iOS / Android) | ○ | ✕ | M4 |

### G. オーサリング / ツーリング

| 機能 | Unity 標準 | NEZIA | 区分 |
|------|-----------|-------|------|
| 編集中の試聴 (preview) | △ (Editor 内蔵) | ✕ | **M1 最優先 (preview daemon)** |
| ランタイムプロファイラ | △ (素朴) | ✕ | **M3 差別化機会** |
| デバッグビジュアライザ (バス・Source 一覧) | ✕ | ✕ | **M3 差別化** |
| プロジェクトファイル方式 (本格オーサリング) | ✕ | ✕ | **M4 (CONCEPT.md B 経路)** |
| ホットリロード | △ | ✕ | M4 |
| Unity Inspector 上の表示 | ○ | **○** | 実装済 (Nezia.Unity 側 IP-1〜4) |

### H. 統合 (Unity / Unreal)

| 機能 | Unity 標準 | NEZIA | 区分 |
|------|-----------|-------|------|
| `AudioSource` ドロップイン互換 | — | **○** | IP-4 完了 (Clip-centric) |
| `AudioListener` 互換 | — | △ | Parity |
| `AudioMixerGroup` 相当 | — | **○** | IP-1 完了 |
| `AudioReverbZone` 相当 | — | ✕ | M4 |
| Unreal `UAudioComponent` 互換 | — | ✕ | M4 |

---

## 付録 C — `.nez` 独自フォーマット (asset container)

M4 で導入予定の独自アセットフォーマット。動機・先例・設計方向の検討メモ。

### 動機

汎用音声フォーマット (MP3 / OGG / WAV …) を **そのまま** ランタイムでロードする現方針は
便利だが、以下の根本的な不整合を抱える:

- **MP3 priming/padding**: LAME タグ / iTunSMPB タグ / 何も無し、3 通りの方言。
  タグなしファイルは原理的にギャップレス再生不能
- **ループ点情報が無い**: どのフォーマットも標準では loop_start/loop_end を持たない。
  ループ用素材は「ファイル全体 = ループ範囲」の慣習に頼るしかなく、頭/尻に無音が
  混入すれば即破綻
- **デコーダ分岐コスト**: ロード時にコーデック種別ディスパッチが必要。`.nez` 1 種に
  すればホットパスが単一化
- **オーサリング時の意図がランタイムに伝わらない**: アタック点・リリース点・推奨音量・
  カテゴリ等のメタを別ファイルで管理する必要がある

### 業界先例

| エンジン | 独自形式 | 中身 |
|---|---|---|
| Wwise | `.wem` | Vorbis ベース + Wwise ヘッダ (loop point, marker, attenuation curve など) |
| FMOD | `.fsb` | コンテナ形式、複数 sound 同梱、stream/decompress フラグ、loop point |
| Unity | `.asset` (Vorbis 内部) | インポート時に再エンコード |

共通点:
1. **ランタイムでは独自形式 1 つだけ**を扱う (デコーダディスパッチ不要)
2. **loop_start / loop_end が明示メタ** として埋め込まれる
3. **オーサリングツールでビルド時に整形**する (汎用形式 → 独自形式)

### 最小設計の方向性 (確定ではなく検討メモ)

```
.nez (Nezia Engine zone Z)  仮称
─────────────────────────────────────
[Header] 固定 64 byte
  magic: "NEZIA\0"        (6 byte)
  version: u16
  sample_rate: u32
  channels: u16
  frame_count: u32
  loop_start: u32          ← キモ
  loop_end:   u32
  flags: u32 (looping_default, streaming_eligible, …)
  reserved: u32 × N

[Optional metadata block]
  attenuation_curve_id, default_volume, category, attack/release marker, …

[PCM data]
  interleaved f32 (or i16)、もしくは Vorbis 等で内部圧縮
```

### 戦略の選択肢

| 案 | 中身 | サイズ | 実装コスト | 備考 |
|---|---|---|---|---|
| **A. PCM コンテナ** | f32 / i16 生 PCM | 大 (≈ WAV) | 小 | MVP 向き、デコードコスト 0 |
| **B. Vorbis ラップ** | Vorbis + nezia ヘッダ | 小 | 中 | Wwise/FMOD 同等方針 |
| **C. ハイブリッド** | flag で A/B 切替 | 可変 | 中〜大 | SFX = A, BGM = B のような使い分け |

### オーサリングツール

別バイナリ `nezia-pack` で `.wav / .mp3 / .ogg → .nez` 変換。CLI で loop_start/end・
default volume・category を指定し、ビルド時に走らせる。Wwise SoundBanks の生成と同等の責務。

### なぜ M4 なのか

- ランタイム (M1〜M3) は **汎用フォーマット直読み**で完結させ、ユーザーがすぐ使える状態を優先
- フォーマット策定はオーサリングツール ([CONCEPT.md B 経路](../design/integration/CONCEPT.md)) と
  一体で進める方が破綻しない
- `SourceComponent.loop_start / loop_end` の追加
  ([streaming.md §部分ループ](../design/core/streaming.md#部分ループ再生への-forward-compatibility)) は
  独自フォーマット導入と並行で実装するのが筋

### 当面の運用

独自フォーマット導入までの期間は:
- **MP3**: LAME タグ + iTunSMPB タグ ([audio.rs](../../crates/core/src/audio.rs) 実装済) +
  n_frames truncation で「タグ付き MP3 はギャップレス、タグなし MP3 は best effort」
- **Vorbis / FLAC / WAV**: 仕様レベルでギャップレス。ループ用素材はこちらを推奨
- **ループ点指定**: 全体ループ (`looping = true`) のみ。部分ループは独自フォーマット導入時に解禁

---

## 関連ドキュメント

- [Integration Experience ロードマップ](../../../Nezia_Integration/Packages/jp.nezia.unity/docs~/roadmap/integration-experience.md) — Unity 統合層 (`jp.nezia.unity`) のフェーズ分け。本ロードマップと Cross-layer ペアで進行する
- [統合戦略 CONCEPT.md](../design/integration/CONCEPT.md) — Unity / Unreal とのドロップイン互換 (A 経路) + 本格オーサリング (B 経路) の 2 経路方針
- [daemon CONCEPT.md](../design/daemon/CONCEPT.md) — preview daemon (M1 最優先) の責務・IPC 設計
- [post-unity-performance.md](post-unity-performance.md) — M3 完了後の極限パフォーマンス追求 (タスクベース並列化・SIMD 徹底化)
- [3D サウンド設計](../design/core/spatial.md) — Spatial 領域 (A) の詳細
- [バスルーティング](../design/core/bus.md) — Mixer 領域 (D) の詳細
- [Source ワールド](../design/core/source.md) — 再生制御領域 (E) の詳細
- [スレッドモデル](../design/core/threading.md) — すべての新機能が守るべき制約
- [ECS アーキテクチャ](../design/core/ecs.md) — 新機能を載せる土台
- [コールバック](../design/core/callbacks.md) — イベント通知の設計
