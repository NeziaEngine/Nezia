# 意図マクロ (Intent Macro)

NEZIA ENGINE の **アルゴリズム非依存エフェクト選択**の中核機構。
本ドキュメントは [ロードマップ](../../roadmap/roadmap.md) の **革新性 柱2「アルゴリズム非依存の
エフェクト選択」** に対応し、配置は **M3 (Unity より良いと言える) の差別化項目**である。

ユーザーが cutoff Hz / Q / threshold dB といった **DSP のパラメータを理解しなくても**、
「距離感」「こもり」「電話越し」のような**意図**を 1 つの意味軸 `amount: 0..1` で操作できる層を定義する。

このドキュメントの目的は **個別マクロの音作りレシピではなく、「意図という意味軸を既存エフェクト層の
上にどう載せ、どこで評価し、どこまでを escape hatch として開けるか」の判断と境界の確定**にある。

---

## 立ち位置 — 既存エフェクト層の「上」に乗る薄い層

意図マクロは [DSP パイプライン](dsp.md) の置き換えではなく、その**上位の authoring/control 層**である。

```
┌─────────────────────────────────────────────┐
│ 意図マクロ層 (Intent Macro)  ★本ドキュメント   │  ← メインスレッド / authoring・control
│   amount: 0..1  ──curve──▶ 複数 param を協調   │
└───────────────┬─────────────────────────────┘
                │ コンパイル: add_effect / set_effect_param
                ▼
┌─────────────────────────────────────────────┐
│ エフェクト層 (EffectWorld / 種別 World)        │  ← 既存 (dsp.md)
│   EffectId / EffectKind / set_effect_param     │
└─────────────────────────────────────────────┘
                ▼
            サウンドスレッド (DSP 本体)
```

**最重要の設計判断: サウンドスレッドは「意図」を一切知らない。** 意図マクロはメインスレッドで
curve を評価し、**既存の `add_effect` / `set_effect_param` コマンドに展開（コンパイル）するだけ**である。
サウンドスレッドが見るのは従来どおりのエフェクトチェーンとパラメータコマンドのみ。

→ これにより [スレッドモデル](threading.md) のリアルタイム制約と
[ロードマップ ガードレール 1](../../roadmap/roadmap.md#設計上のガードレール)
「新しい同期機構をデフォルトで増やさない」を**完全に守る**。意図マクロは新しい同期機構を 1 つも増やさない。

---

## 設計判断サマリ

| 判断点 | 採用 | 不採用 | 理由（要約） |
|---|---|---|---|
| 評価スレッド | **メインスレッドで curve 評価 → 既存コマンドに展開** | サウンドスレッドで意図を解釈 | サウンドスレッドに新概念・新経路を持ち込まない。amount 変化は低頻度（ノブ操作 / Snapshot 駆動）で、メイン評価で十分 |
| 意味軸の数 | **単一 `amount: 0..1`**（MVP） | 最初から多軸 | 「1 ノブで複数エフェクト協調」が差別化の核。多軸は後続拡張で接続 |
| マクロ定義の所有 | **データ（`IntentMacroDef`）**。同梱 curated セット + ユーザー定義 | コード埋め込み | Clip-centric / Mixer-as-asset と同じ「設計情報はデータ」の思想に一貫。LLM が生成・編集する中間表現にもなる（[副軸](#副軸-llm--cli-連動)） |
| curve 表現 | **既存のカスタム減衰カーブ（[spatial.md](spatial.md) / Phase 3-1）と同じサンプリング済みカーブ表現を流用** | 新規 curve 型 | 既存資産の再利用（ガードレール準拠）。定義域だけ「距離」→「amount 0..1」に読み替える |
| amount = 0 の扱い | **配下エフェクトを `enabled=false` に落として最速経路へ** | 常に係数計算を流す | [ガードレール 3](../../roadmap/roadmap.md#設計上のガードレール)「最速経路を保つ」。意図ノブを 0 にしたら素通しコストに戻る |
| escape hatch | **展開すると配下の `EffectId` 群が露出し、生 param を直接編集できる** | マクロは不可分のブラックボックス | 「プロが手で組みたい」逃げ道。展開後は既存 type-safe effect API がそのまま使える |
| 配下エフェクトの所有 | **マクロインスタンスが `EffectId` 群を保持・ライフサイクル管理** | エフェクトを別管理 | apply で spawn、remove で despawn を一括。ユーザーは個々の EffectId を意識しない |
| 適用対象 | **既存 `EffectTarget`（Bus / Source）をそのまま使う** | 専用ターゲット | エフェクト層と同じ語彙。Bus にも Source にも意図を載せられる |

---

## データ構造

### マクロ定義（テンプレート）

`IntentMacroDef` は「1 つの意図」を表す不変のデータ。同梱 curated セットも、ユーザー / LLM が
作った定義も同じ型。

```rust
/// 1 つの意図（"距離感" 等）の定義。amount 0..1 が複数 param を協調駆動する。
pub struct IntentMacroDef {
    pub id:   IntentMacroId,           // 論理 ID（Hash ID。二層 ID 設計に準拠）
    pub name: &'static str,            // "DistanceFeel" 等（デバッグ・オーサリング表示用）

    /// このマクロが生成するエフェクト群（チェーン順）。
    pub effects: Vec<IntentEffectSpec>,

    /// amount → 各 param への協調マッピング。
    pub bindings: Vec<IntentBinding>,
}

/// マクロが apply 時に spawn する 1 エフェクト。
pub struct IntentEffectSpec {
    pub kind:     EffectKind,          // 既存 enum（Lpf / Hpf / Reverb / ParamEq …）
    pub position: EffectPosition,      // Pre / Post（target 依存の意味は dsp.md 準拠）
    pub algo:     u8,                  // 物理アルゴリズム。既定 0
}

/// 「amount のこの区間で、effects[target] の param をこの curve で動かす」束縛。
pub struct IntentBinding {
    pub effect_index: u8,              // effects[] のどれを駆動するか
    pub param:        u8,              // 種別ごとの param ID（dsp.md の #[repr(u8)] enum）
    pub curve:        CurveId,         // amount(0..1) → param 値。既存カーブ表現を流用
}
```

- **正規化値での協調**: 配下 param は dsp.md の正規化値（`wet ∈ [0,1]` 等）か実値（`cutoff_hz`）。
  curve の値域は param に合わせる。amount は常に `0..1` の意味軸。
- **複数エフェクト束ね**: `effects` が複数・`bindings` が複数あることで、`amount` 1 つが
  「LowPass.cutoff を下げつつ Reverb.send を上げ、Volume を下げる」のような協調を表現する。
  これが「プリセットを並べるだけ」と決定的に違う点（柱2 の差別化条件）。

### 例: `DistanceFeel`（距離感）

```
DistanceFeel
  effects:
    [0] Lpf    (Pre)
    [1] Reverb (Post)        ※ Bus 対象時。Source 対象では Send 経由（後述・非目標）
  bindings:
    effect[0].Cutoff : curve(amount 0→1 を 20kHz→800Hz)
    effect[1].Wet    : curve(amount 0→1 を 0.0→0.6)
    （Volume の協調は Bus gain 連動として別途。後述「拡張余地」）

  amount: 0.0 ───●─── 1.0
    ユーザーが見るのは amount 1 つだけ。Lpf/Reverb/Cutoff/Wet は隠れる。
```

---

## ランタイム適用フロー

### apply（意図をターゲットに載せる）

```
engine.apply_intent_macro(target, macro_id) -> IntentHandle
  1. macro_id から IntentMacroDef を解決（同梱 or 登録済みユーザー定義）
  2. def.effects[] を順に add_effect(target, kind, position) で spawn
       → 得た EffectId 群を IntentInstance に保持
  3. def.bindings[] を (EffectId, param, curve) の実体に解決して保持
  4. amount = 0 で初期化 → 配下エフェクトを enabled=false（素通し）
  → IntentHandle を返す
```

### set_amount（意図の強さを動かす — ホットパスはここ）

```
engine.set_intent_amount(handle, amount)
  if amount == 0:
    配下全エフェクトを set_effect_enabled(false)   // 最速経路へ
  else:
    (前回 0 だったなら) set_effect_enabled(true)
    for b in instance.bindings:
      v = curve_eval(b.curve, amount)              // メインスレッドで評価（軽い）
      set_effect_param(b.effect_id, b.param, v)    // 既存コマンド経路
```

- `set_amount` は **メインスレッドで curve を評価し、既存の `SetEffectParam` / `SetEffectEnabled`
  コマンドを K 本発行するだけ**。K = binding 数（通常 2〜4）。新しいコマンド種別は不要。
- amount 変化は低頻度（UI ノブ / Snapshot 補間 / ゲームロジック）想定。毎フレーム高頻度駆動が
  必要になった場合の経路追加判断は dsp.md「パラメータ更新経路」の再評価フレームに従う。

### remove

```
engine.remove_intent_macro(handle)
  → instance が保持する EffectId 群を一括 remove_effect、handle を無効化
```

---

## escape hatch — 展開して生編集

意図マクロは**不可分のブラックボックスではない**。authoring 時にいつでも「展開」できる:

```
engine.expand_intent_macro(handle) -> Vec<EffectId>
  → 配下エフェクトの EffectId 群を返し、IntentInstance の amount 束縛を解除（detach）
  → 以後は通常のエフェクトとして set_effect_param / reorder_effects で直接編集可
```

- 展開後は既存の type-safe effect API（`AsLowPass().Cutoff` 等、Unity IP-2）がそのまま使える。
- 「アルゴリズムを知らない人は amount 1 ノブ、知っている人は展開して生 param」を**同一データ上で
  両立**させる。これが「知識ゼロで選べる」と「プロの自由度」を両取りする鍵。

---

## 同梱 curated マクロ（初期セット）

**柱2 の実体は API ではなく、良い音がする `amount → params` の curve 設計**である。DSP の専門性を
**engine 側に一度だけ焼き込み**、ユーザーは知識ゼロで使う。初期セットはチェーン内で完結するもの
（cross-bus な Send/Sidechain を要さないもの）に絞る:

| マクロ | 意図 | 協調する param（例） |
|---|---|---|
| `DistanceFeel` | 遠くで鳴っている感 | Lpf.cutoff ↓ / Reverb.wet ↑ |
| `Muffle` | こもり・布越し | Lpf.cutoff ↓（緩やか） |
| `Telephone` | 電話・無線越し | Hpf.cutoff ↑ + Lpf.cutoff ↓（帯域制限） |
| `Underwater` | 水中 | Lpf.cutoff ↓↓ + 緩い Reverb |
| `Tone` | 明るさ／暖かさ | ParamEq の高域シェルフを ± に振る |

> ダッキング（"会話の下で BGM を下げる"）は cross-bus の Send/Sidechain（[send.md](send.md)）を
> 要するため、チェーン内マクロでは表現しない。意図マクロの Send 対応は[拡張余地](#拡張余地)で扱う。

curated curve の質が凡庸なら API だけ作っても差別化にならない（**ここが最大の難所**）。初期セットの
curve は試聴・可視化（M1 daemon / M3 プロファイラ）の上で詰める前提とする。

---

## 公開 API / FFI 草案

```rust
impl SoundEngine {
    /// 同梱 or 登録済みの意図マクロを target に適用する。
    pub fn apply_intent_macro(
        &mut self,
        target: EffectTarget,
        macro_id: IntentMacroId,
    ) -> Option<IntentHandle>;

    /// 意図の強さ（0..1）を設定する。配下エフェクトの param を curve 経由で協調更新。
    pub fn set_intent_amount(&mut self, handle: IntentHandle, amount: f32) -> bool;

    /// 現在の amount を取得。
    pub fn intent_amount(&self, handle: IntentHandle) -> Option<f32>;

    /// 配下エフェクトごと除去する。
    pub fn remove_intent_macro(&mut self, handle: IntentHandle) -> bool;

    /// 展開して生エフェクトに落とす（amount 束縛を解除）。返り値は配下 EffectId 群。
    pub fn expand_intent_macro(&mut self, handle: IntentHandle) -> Vec<EffectId>;

    /// ユーザー / LLM 定義のマクロを登録する（JSON デコード結果など）。
    pub fn register_intent_macro(&mut self, def: IntentMacroDef) -> IntentMacroId;
}
```

- FFI は `nezia_intent_apply` / `nezia_intent_set_amount` / `nezia_intent_remove` /
  `nezia_intent_expand` として公開。Unity 側はこれを **意図ノブ 1 本**として Inspector に出す
  （実装は別途 Integration ロードマップ）。
- `IntentHandle = (index, generation)` の二層 ID（既存ハンドル規約に準拠）。
- API 語彙は「1 つの意味軸 + amount」という最小形に揃え、学習コストをゼロに近づける（ガードレール 5）。

---

## 副軸: LLM / CLI 連動

`IntentMacroDef` は**データ（JSON 化可能）**であることで、[柱3](../../roadmap/roadmap.md#柱3-daemon-による統合作業環境--エディタ内でライブに観る試す)
の daemon CLI / LLM 経路と自然に合成できる:

- 「もっとチープに、電話越しみたいに」→ LLM が `IntentMacroDef`（JSON）を新規生成 or 既存の
  curve を調整 → `register_intent_macro` → daemon 経由で即試聴。
- **意図マクロという中間表現を挟む**ことで、LLM が生 param を直接叩く案より再現性・編集性・
  差分レビュー性が高い。生成物が「人が読めるデータ」になる。
- すなわち **柱2 のデータモデルを柱3 の経路が増幅する**関係。柱2 を単独で実装しても価値があり、
  柱3 と合わさると「言葉で音を作る」体験になる。

---

## 容量・スレッド特性

| 項目 | 値 / 性質 |
|---|---|
| サウンドスレッドへの追加負荷 | **ゼロ**（意図概念はサウンドスレッドに存在しない。配下エフェクト自体のコストのみ） |
| 新規同期機構 | **なし**（既存コマンド経路に展開するだけ） |
| `set_amount` のコスト | curve 評価 × binding 数（通常 2〜4）+ 同数の `SetEffectParam` コマンド。メインスレッドで軽量 |
| メモリ | `IntentMacroDef`（curve 参照のみ）+ インスタンスごとの `EffectId` 配列。微小 |
| 配下エフェクトのプール | 既存 `MAX_EFFECTS` 等（dsp.md）を消費。マクロは複数 EffectId を確保する点に留意 |

---

## 拡張余地（後続への接続）

| 後続 | 確保した拡張点 |
|---|---|
| Snapshot 補間（[snapshot.md](snapshot.md)） | `amount` を Snapshot の補間対象パラメータにできる。マクロ展開後の生 param ではなく **amount 1 軸を補間**することで、宣言的なフェードが書ける |
| 多軸意図 | `bindings` を「軸 × binding」に拡張すれば "距離" + "素材" のような 2 軸協調に拡張可能。MVP は単軸 |
| Send / Sidechain 連動マクロ | ダッキング等 cross-bus な意図は、配下に Send 設定（[send.md](send.md)）を spawn する `IntentEffectSpec` 種別を追加して表現する |
| Spatial 連動 | `DistanceFeel` の amount を Spatial（距離）から自動駆動すれば、距離連動 LPF/Reverb が「意図」として宣言的に書ける（[spatial.md](spatial.md) 連動） |
| Volume 協調 | 配下に「Bus gain / Source volume scale」への binding 種別を追加し、エフェクト param 以外も amount で協調させる |

---

## 設計上の非目標

- **サウンドスレッドでの意図解釈**は行わない。意図はメインスレッドで既存コマンドにコンパイルされる。
- **任意 DAG / サンプル単位オートメーション**は持たない（dsp.md の非目標を踏襲）。amount の時間変化は
  Snapshot またはアプリ側ティックで表現する。
- **音作りレシピそのもの**（各 curated マクロの最終 curve 値）は本ドキュメントの対象外。初期セットの
  curve は試聴・可視化基盤の上でチューニングする運用課題として切り出す。
- **per-voice Reverb の直接挿入**は dsp.md 同様に行わない。Source 対象で Reverb を要する意図は
  Send 経由（拡張余地）で表現する。

---

## 関連ドキュメント

- [ロードマップ](../../roadmap/roadmap.md) — 革新性 柱2 / M3 差別化項目としての位置づけ
- [DSP パイプライン](dsp.md) — 意図マクロが展開先とするエフェクト層（`EffectId` / `EffectKind` / `set_effect_param`）
- [スレッドモデル](threading.md) — 「サウンドスレッドに新概念を持ち込まない」根拠
- [3D サウンド設計](spatial.md) — 流用するカスタムカーブ表現 / Spatial 連動の接続先
- [Mixer Snapshot](snapshot.md) — `amount` を補間対象にする拡張
- [Send / Sidechain Ducking](send.md) — ダッキング系意図の表現経路（拡張余地）
- [統合戦略](../integration/CONCEPT.md) — Unity への「意図ノブ」露出方針（A 経路）
