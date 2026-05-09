# Roadmap — Better than Unity Audio

NEZIA ENGINE の直近の到達目標は **「Unity 標準 Audio より良いサウンド体験」を NEZIA 単体で提供できる状態** である。
本ドキュメントは、その目標までに何を・どの順で・なぜ実装するかをエンジン全体で定義する。
個別機能の詳細設計は `docs/design/core/*.md` を正とし、ここでは**領域横断の優先順序と判断基準**のみを扱う。

---

## 立ち位置と定義

- ターゲットは Unity プロジェクトの**ドロップイン代替**として機能するサウンドエンジン (`docs/design/integration/CONCEPT.md` 参照)。
- 競合は Wwise / FMOD / CRI ADX ではなく、まずは **Unity 標準 `AudioSource` + `AudioListener` + `AudioMixer` 一式**である。
- 「Unity より良い」とは具体的に次の 2 条件を同時に満たすこと:
  1. **Parity (同等機能)** — Unity 標準にある機能はすべて NEZIA にも存在する
  2. **Differentiation (差別化)** — Unity 標準に**無い**機能を NEZIA は持っており、移行する価値がある

差別化機能だけを派手に並べても parity に穴があると採用判定の最初で落ちるため、**順序は parity の致命傷を埋める → 差別化** を厳守する。

---

## 領域別ギャップ分析

NEZIA がカバーすべき領域を 7 つに分け、Unity との差を整理する。

### A. Spatial Audio (3D サウンド)

| 機能 | Unity 標準 | NEZIA | 区分 |
|------|-----------|-------|------|
| 距離減衰 (3 モデル) | ○ | **○** | 実装済 (SP-01, SP-04) |
| ステレオパン | ○ | **○** | 実装済 (SP-02、後方連続化済 #8) |
| リスナー管理 | ○ | **○** | 実装済 (SP-03) |
| 2D/3D 切替 | ○ | **○** | 実装済 (SP-05) |
| Listener Focus (仮想リスナー) | ✕ | **○** | **差別化 (実装済 SP-06)** |
| Doppler 効果 | ○ | **○** | 実装済 (SP-10) |
| Custom Attenuation Curve | ○ | **○** | 実装済 (Phase 3-1) |
| Spread (ステレオ広がり) | ○ | ✕ | Parity gap (低優先) |
| Sound Cone (指向性音源) | ✕ | ✕ | **差別化候補** (SP-11) |
| Occlusion | ✕ | ✕ | 差別化候補 (SP-12) |
| HRTF | ✕ (要プラグイン) | ✕ | 差別化候補 (SP-13) |

### B. DSP / エフェクト

| 機能 | Unity 標準 | NEZIA | 区分 |
|------|-----------|-------|------|
| Bus 単位エフェクト挿入 | ○ (Mixer) | ✕ | **Parity gap (最大)** |
| Reverb / SFX Reverb | ○ | **○** | 実装済 (Bus 専用、Phase 3-3 Send 後に共有) |
| LPF / HPF / ParamEQ | ○ | **○** | 実装済 (PeakingEq は Phase 3-5) |
| Compressor / Limiter | ○ | **○** | 実装済 (Compressor / 単体 Limiter / master soft limiter すべて実装済、Phase 3-5) |
| Chorus / Flanger / Distortion | ○ | ✕ | Parity gap (中優先) |
| Source 単位 LPF (距離・遮蔽連動) | ✕ (要 Mixer 経由) | ✕ | 差別化候補 |
| プラグイン SDK (ユーザー定義 DSP) | ○ (Native Audio Plugin) | ✕ | 中期目標 |

エフェクトパイプラインの不在は **NEZIA の最大の構造的 parity gap**。3D サウンドが揃っていてもエフェクトが無いと「Unity の代わりに使う」ことが原理的に成立しない。

### C. アセット / ストリーミング

| 機能 | Unity 標準 | NEZIA | 区分 |
|------|-----------|-------|------|
| WAV / PCM 読込 | ○ | **○** | 基本 |
| Vorbis / Ogg | ○ | **○** | 実装済 (Phase 2-4) |
| MP3 | ○ | **○** | 実装済 (Phase 2-4、symphonia 経由) |
| Opus | △ | ✕ | 差別化候補 (BGM 圧縮率) |
| ストリーミング再生 (BGM 用) | ○ | **○** | 実装済 (Phase 2-4) |
| 非同期ロード | ○ | ✕ | Parity gap |
| メモリ常駐圧縮 (ADPCM 等) | ○ | ✕ | 中期 |

長時間 BGM をメモリに全展開すると数十 MB 単位で食うため、ストリーミングが無いと事実上 BGM が使えない。

### D. Mixer / ルーティング

| 機能 | Unity 標準 | NEZIA | 区分 |
|------|-----------|-------|------|
| バス階層 | ○ | **○** | 実装済 |
| Source → バスルーティング | ○ | **○** | 実装済 |
| バス音量・ミュート | ○ | **○** | 実装済 |
| **Send / Receive** (副ルート) | ○ | **○** | 実装済 (Phase 3-3) |
| **Snapshot** (バス状態のスムーズ補間) | ○ | **○** | 実装済 (Phase 3-2) |
| Exposed Parameters | ○ | ✕ | Parity gap |
| Sidechain Ducking | ○ (Send 経由) | **○** | 実装済 (Phase 3-3) |
| バスの動的追加・削除 | ✕ (Editor 編集のみ) | △ (要確認) | 差別化候補 |

### E. 再生制御

| 機能 | Unity 標準 | NEZIA | 区分 |
|------|-----------|-------|------|
| Play / Stop / Pause | ○ | **○** | 実装済 |
| Loop | ○ | **○** | 実装済 |
| 音量 / ピッチ | ○ | **○** | 実装済 |
| **PlayScheduled (サンプル精度)** | △ (dspTime 制約あり) | ✕ | **差別化機会** |
| **Voice Virtualization** (発音数超過時) | ○ | **○** | 実装済 |
| Priority (発音優先度) | ○ | **○** | 実装済 |
| **Random / Switch / Sequence Container** | ✕ (要スクリプト) | ✕ | **差別化候補 (大)** |
| ループ点 (loop start/end) | △ | ✕ | 差別化候補 ([nezia 独自フォーマット](#nezia-独自フォーマット-asset-container) と対で実装) |

`MAX_SOURCES` を超える同時発音要求が来たときに priority で間引く仕組みが無いと、大規模 SFX で破綻する。Random Container 系は Unity で全員が自作している領域で、ミドルウェアが標準提供すると一気に差が出る。

### F. 出力 / プラットフォーム

| 機能 | Unity 標準 | NEZIA | 区分 |
|------|-----------|-------|------|
| ステレオ出力 | ○ | **○** | 実装済 |
| 5.1 / 7.1 サラウンド | ○ | ✕ | 中期 (コンソール向け) |
| サンプルレート可変 | ○ | △ (要確認) | 基本 |
| バックエンド抽象 (CoreAudio / WASAPI / ALSA) | ○ | △ (`audio.rs` 要確認) | 基本 |
| モバイル (iOS / Android) | ○ | ✕ | 中期 |

### G. オーサリング / ツーリング

| 機能 | Unity 標準 | NEZIA | 区分 |
|------|-----------|-------|------|
| ランタイムプロファイラ | △ (素朴) | ✕ | **差別化機会** |
| デバッグビジュアライザ (バス・Source 一覧) | ✕ | ✕ | **差別化候補** |
| プロジェクトファイル方式 (本格オーサリング) | ✕ | ✕ | **差別化候補 (CONCEPT.md B 経路)** |
| ホットリロード | △ | ✕ | 差別化候補 |
| Unity Inspector 上の表示 | ○ | ✕ | Parity (Nezia.Unity 側) |

### H. 統合 (Unity / Unreal)

| 機能 | Unity 標準 | NEZIA | 区分 |
|------|-----------|-------|------|
| `AudioSource` ドロップイン互換 | — | △ (進行中) | **Parity (CONCEPT.md A 経路)** |
| `AudioListener` 互換 | — | △ | Parity |
| `AudioMixerGroup` 相当 | — | △ | Parity |
| `AudioReverbZone` 相当 | — | ✕ | Parity (Reverb 実装後) |
| Unreal `UAudioComponent` 互換 | — | ✕ | 中期 |

---

## 実装フェーズ

各フェーズ完了時に主張できる立ち位置を明確にし、リリースを切れる粒度で区切る。

```
Phase 1 (完了)
  ECS / バス / Source / Spatial 基本 / Listener Focus

Phase 2  ── "Unity の最低限" を埋める ──
  Doppler  +  Voice Virtualization  +  最低限の DSP (LPF/HPF/Reverb)  +  Ogg/Vorbis ストリーミング

Phase 3  ── "Unity 並み" ──
  Custom Attenuation Curve  +  Mixer Snapshot / Send  +  PlayScheduled  +  ParamEQ/Compressor

Phase 4  ── "Unity 超え" ──
  Sound Cone  +  Random/Switch/Sequence Container  +  プロファイラ・ビジュアライザ

Phase 5  ── 大型差別化 (ターゲット層で分岐) ──
  Occlusion  または  HRTF  +  Reverb Zone

Phase 6  ── 中長期 ──
  サラウンド  /  プラグイン SDK  /  プロジェクトファイル方式 (オーサリングツール)  /  Unreal 対応
```

### Phase 2 — 「Unity の最低限」を埋める

**目的**: Unity から移ってきた人が「これでは何もできない」と言わない状態にする。

| ステップ | 領域 | 内容 | 概算コスト |
|---------|------|------|-----------|
| 2-1 | A | ~~**Doppler 効果 (SP-10)**~~ **実装済** | 1〜2 週間 |
| 2-2 | E | ~~**Voice Virtualization + Priority**~~ **実装済** | 1〜2 週間 |
| 2-3 | B | ~~**DSP パイプラインの土台 + 最低 3 種 (LPF / HPF / Reverb)**~~ **実装済** | 3〜5 週間 |
| 2-4 | C | ~~**Ogg/Vorbis デコード + ストリーミング再生**~~ **実装済** | 2〜3 週間 |

**判断ポイント**:
- DSP パイプラインの土台 (2-3) は単独の実装より「**バスごとにエフェクトチェーンを差し込めるアーキテクチャ**」を入れることが本質。最初のエフェクト 3 種はその検証用。詳細設計は [DSP パイプライン](../design/core/dsp.md) に分離済み（Phase 2 着手前に確定）。
- Voice Virtualization は parity の中で最も見落とされやすいが、`AudioSource` を数百個まいた Unity プロジェクトは**暗黙にこれに依存している**。これが無いと NEZIA に置き換えた途端に音が出なくなる事故が起きる。
- Vorbis ストリーミングは BGM が無いと「NEZIA でゲームを動かす」が成立しないので必須。

**完了時の主張**: 「Unity AudioSource + 簡素な AudioMixer の置き換えとして実用最低限」

### Phase 3 — 「Unity 並み」

**目的**: Unity プロジェクトの実例の 9 割が NEZIA でそのまま動く。

| ステップ | 領域 | 内容 | 概算コスト |
|---------|------|------|-----------|
| 3-1 | A | ~~**Custom Attenuation Curve**~~ **実装済** | 1〜2 週間 |
| 3-2 | D | ~~**Mixer Snapshot + 補間**~~ **実装済** | 2〜3 週間 |
| 3-3 | D | ~~**Send / Receive + Sidechain Ducking**~~ **実装済** (#20, #21) | 2〜3 週間 |

> **Reverb の運用パターン補足**: Phase 2-3 では Reverb は Bus 専用で実装する (Source 対象は API 段階で拒否)。Wwise/FMOD/Unity いずれも技術的には per-voice Reverb が可能だが、業界標準の運用は **Aux Bus + Send/Return で 1 個の Reverb を全ソースで共有**する形であり、per-voice はほぼ使われない。NEZIA も Phase 3-3 で Send/Return が入った時点で同じ標準パターンが組めるようになり、Source 単位 Reverb の直接挿入機能を別途追加する必要はない。Phase 2-3 の Bus 専用制約はこの段階までの暫定対応である。
| 3-4 | E | ~~**PlayScheduled (サンプル精度の予約再生)**~~ **実装済** ([設計](../design/core/scheduling.md)) | 1〜2 週間 |
| 3-5 | B | ~~**DSP 拡充: ParamEQ / Compressor / Limiter**~~ **実装済** (Compressor #21 / PeakingEq / 単体 Limiter / master soft limiter すべて完了) | 2 週間 |

**判断ポイント**:
- Snapshot と Send が揃うと「ゲーム内で BGM がフェードしながら戦闘 BGM に切替、SFX は自動でダッキング」が標準で書ける。これが Phase 3 の事実上のゴール。
- PlayScheduled はリズムゲーム / 音楽演出系で必須。Unity の `AudioSettings.dspTime` には既知の不安定性があり、**NEZIA がここで Unity より精度を出せれば差別化材料**になる (差別化を意識しつつ Phase 3 に置く)。

**完了時の主張**: 「Unity 標準 Audio + AudioMixer の機能を網羅」

### Phase 4 — 「Unity 超え」

**目的**: NEZIA に移行する積極的な動機を作る。Unity 単体ではできないことが標準で書ける。

| ステップ | 領域 | 内容 | 概算コスト |
|---------|------|------|-----------|
| 4-1 | A | **Sound Cone (SP-11)** | 1 週間 |
| 4-2 | E | **Random / Switch / Sequence Container** ([設計](../design/core/container.md)、現状 Random のみ実装) | 3〜4 週間 |
| 4-3 | G | **ランタイムプロファイラ + デバッグビジュアライザ** | 2〜3 週間 |

**判断ポイント**:
- Sound Cone は OpenAL/FMOD 互換の API で出すだけで「Unity に無いものが NEZIA にある」と即座に主張できる。コスト/インパクト比が最良。
- Random/Switch/Sequence Container は Unity プロジェクトで毎回自作されている領域。標準提供すれば「自作コードを捨てて NEZIA に乗る」動機になる。Wwise/CRI 流の Cue 概念の縮小版から始める。
- プロファイラ/ビジュアライザはエンジン採用の意思決定で**最後のひと押し**になる。データ指向設計の利点 (大量同時発音時の性能) を**可視化することで初めて伝わる**。

**この時点で「Unity より良い」を最短で主張可能になる。** Phase 1 完了からの累計でおおよそ 4〜6 ヶ月の見積もり。

### Phase 5 — 大型差別化 (分岐)

ターゲット層によって優先順を分岐する。両方やるとフェーズが長くなりすぎる。

#### 5-A. モバイル / カジュアル / TPS / FPS 向け
- **Occlusion (SP-12)** — 壁・遮蔽の表現で聴感差が大きい
- Reverb Zone (SP-14)

#### 5-B. VR / コンソール / ヘッドフォン主体向け
- **HRTF (SP-13)** — 没入感の決定打。Unity は別プラグイン依存
- 5.1 / 7.1 サラウンド

判断は Phase 4 完了時のターゲット案件次第。

### Phase 6 — 中長期

- プラグイン SDK (ユーザー定義 DSP)
- プロジェクトファイル方式の本格オーサリングツール (`docs/design/integration/CONCEPT.md` の B 経路)
- [nezia 独自フォーマット (asset container)](#nezia-独自フォーマット-asset-container)
- Unreal Engine 統合
- ホットリロード / 編集時プレビュー

---

## nezia 独自フォーマット (asset container)

### 動機

汎用音声フォーマット (MP3 / OGG / WAV …) を **そのまま** ランタイムでロードする現方針は便利だが、以下の根本的な不整合を抱える:

- **MP3 priming/padding**: LAME タグ / iTunSMPB タグ / 何も無し、3 通りの方言。タグなしファイルは原理的にギャップレス再生不能
- **ループ点情報が無い**: どのフォーマットも標準では loop_start/loop_end を持たない。ループ用素材は「ファイル全体 = ループ範囲」の慣習に頼るしかなく、頭/尻に無音が混入すれば即破綻
- **デコーダ分岐コスト**: ロード時にコーデック種別ディスパッチが必要。`.nez` 1 種にすればホットパスが単一化
- **オーサリング時の意図がランタイムに伝わらない**: アタック点・リリース点・推奨音量・カテゴリ等のメタを別ファイルで管理する必要がある

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

別バイナリ `nezia-pack` で `.wav / .mp3 / .ogg → .nez` 変換。CLI で loop_start/end・default volume・category を指定し、ビルド時に走らせる。Wwise SoundBanks の生成と同等の責務。

### 配置: なぜ Phase 6 なのか

- ランタイム (Phase 1〜4) は **汎用フォーマット直読み** で完結させ、ユーザーがすぐ使える状態を優先
- フォーマット策定はオーサリングツール ([CONCEPT.md B 経路](../design/integration/CONCEPT.md)) と一体で進める方が破綻しない
- `SourceComponent.loop_start / loop_end` の追加 ([streaming.md §部分ループ](../design/core/streaming.md#部分ループ再生への-forward-compatibility)) は独自フォーマット導入と並行で実装するのが筋

### 当面の運用

独自フォーマット導入までの期間は:
- **MP3**: LAME タグ + iTunSMPB タグ ([audio.rs](../../crates/core/src/audio.rs) 実装済) + n_frames truncation で「タグ付き MP3 はギャップレス、タグなし MP3 は best effort」
- **Vorbis / FLAC / WAV**: 仕様レベルでギャップレス。ループ用素材はこちらを推奨
- **ループ点指定**: 全体ループ (`looping = true`) のみ。部分ループは独自フォーマット導入時に解禁

---

## フェーズ完了時に主張できる立ち位置

| 完了時点 | 主張できる立ち位置 |
|---------|------------------|
| Phase 1 (現在) | 「3D サウンドの基礎が動く」 |
| Phase 2 完了 | 「**Unity 標準の最低限を網羅**」 — DSP, BGM, 大量発音が揃う |
| Phase 3 完了 | 「**Unity 標準と同等**」 — 既存 Unity プロジェクトの大部分が動く |
| **Phase 4 完了** | 「**Unity 標準より良い**」 — Cue 系・指向性・可視化で明確に上 |
| Phase 5 完了 | 「Unity + 主要プラグイン相当」 — VR/環境表現が決定的に強い |
| Phase 6 以降 | Wwise / FMOD クラスの本格ミドルウェア基盤 |

「Unity より良い」を最短で名乗れるのは **Phase 4 完了時点**。

Phase 4 完了後の極限パフォーマンス追求 (タスクベース並列化・SIMD 徹底化) は [post-unity-performance.md](post-unity-performance.md) を参照。

---

## 設計上のガードレール

すべてのフェーズで守る原則。

1. **既存 ECS / SoA / SIMD / リングバッファコマンドのパターンを壊さない。** 新機能は原則、既存 ECS 上に SoA 列追加 + コマンド追加で実装する。
   - 新しい同期機構の導入は **デフォルトでは避ける** が、完全には禁じない。リングバッファコマンドより明確に有利な性能・遅延特性が**実測で**示せる場合 (Triple Buffer によるリスナー状態のロックフリー共有など) は導入を検討してよい。
   - 導入する場合は (a) サウンドスレッドのリアルタイム制約 (ロック・確保・syscall なし) を破らないこと、(b) ベンチマークで既存方式との比較を残すこと、(c) 設計ドキュメントに採用理由を明記すること、を満たす。
   - 「なんとなく速そう」での追加は不可。同期機構の数が増えるとデバッグ性とテスト面積が二乗で増えるため、**新しい同期機構は 1 つ増やすたびに正味の利得を説明できる状態**を保つ。
2. **サウンドスレッドはロック・確保・syscall を行わない** (`docs/design/core/threading.md`)。新機能でも例外を作らない。
3. **`spatial_enabled = false` / `effect_enabled = false` などの最速経路を保つ。** 機能追加で 2D ソースや素通しバスが遅くならないこと。
4. **Unity 標準にある機能は Unity 互換のデフォルト値で動く。** 新規パラメータは「設定しなければ Unity と同じ挙動」を維持する。
5. **API 形はメジャー実装に寄せる。** Cone は OpenAL/FMOD/Web Audio、HRTF は Steam Audio/Resonance Audio、Effects は AudioMixer の語彙に揃え、学習コストをゼロに近づける。
6. **二経路ワークフロー (CONCEPT.md A/B) を常に意識する。** 機能追加時に「ドロップイン互換側でどう見えるか」「プロジェクトファイル側でどう設定するか」を両方検討する。

---

## 関連ドキュメント

- [統合戦略](../design/integration/CONCEPT.md) — Unity / Unreal とのドロップイン互換 + 本格オーサリングの 2 経路方針
- [3D サウンド設計](../design/core/spatial.md) — Spatial 領域 (A) の詳細設計とフェーズ分け
- [バスルーティング](../design/core/bus.md) — Mixer 領域 (D) の詳細
- [Source ワールド](../design/core/source.md) — 再生制御領域 (E) の詳細
- [スレッドモデル](../design/core/threading.md) — すべての新機能が守るべき制約
- [ECS アーキテクチャ](../design/core/ecs.md) — 新機能を載せる土台
- [コールバック](../design/core/callbacks.md) — イベント通知の設計
