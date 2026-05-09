# DSP パイプライン

NEZIA ENGINE におけるエフェクト処理（フィルタ・リバーブ等の DSP）の土台設計。
本ドキュメントは [ロードマップ](../../roadmap/better-than-unity-audio.md) の **Phase 2-3「DSP パイプラインの土台 + 最低 3 種 (LPF / HPF / Reverb)」** に対応する設計を扱う。

このドキュメントの目的は **個別エフェクトのアルゴリズム解説ではなく、「バスごとに任意エフェクトを差し込めるアーキテクチャをどう SoA に載せるか」の判断と境界の確定** にある。Send / Snapshot / Source 単位エフェクト / プラグイン SDK といった後続フェーズの機能はすべてここで決めた境界の上に乗るため、Phase 2-3 着手前に確定させる。

---

## スコープ

### このフェーズで扱う
- **Bus / Source の統一エフェクト API**（`add_effect(target, kind, params)` の単一経路）。挿入対象は将来含めて多態化する。
- **バス単位** で全エフェクト（LPF / HPF / Reverb）を挿入可能にする。
- **Source 単位** で LPF / HPF を挿入可能にする。Source 単位 Reverb は Phase 2-3 の時点では拒否する（理由は後述。Wwise/FMOD/Unity の業界標準である Send/Return パターンが Phase 3-3 で導入された後、自然に「Aux Bus + Send」経由で表現可能になる）。
- パラメータ更新（メイン → サウンドスレッド）の経路規定。
- エフェクトチェーン内の順序制御。
- **論理エフェクト種別と物理アルゴリズムの分離**を最初から行う。Phase 2 では各論理種別に対し物理アルゴリズム 1 つだけを実装するが、API は論理種別のみを受ける。

### このフェーズでは扱わない（後続で接続する）
- **アルゴリズム自動切替**（優先度・CPU/メモリ予算に応じた品質可変）。差別化機能だが「真のミドルウェア」段階（Phase 6+）の課題と位置づける。設計上の余地のみ残す。
- **Source 単位 Reverb** の直接挿入。Wwise/FMOD/Unity いずれも技術的には可能だが、業界標準の運用は **Aux Bus + Send/Return** で 1 個の Reverb を全ソースで共有する形であり、per-voice Reverb はほぼ使われない。NEZIA も Send/Return（Phase 3-3）を介して同じ標準パターンを提供するため、Source 単位 Reverb の直接挿入は実装しない。Phase 2-3 時点では Send 未実装のため**暫定的に Bus 専用**となる。
- **Send / Receive / Sidechain Ducking**（Phase 3-3）。エフェクトチェーン処理位置を `post-fader, pre-send` と固定し、Send が後段に挟まれる前提を残す。
- **Snapshot 補間**（Phase 3-2）。エフェクトパラメータが補間対象になるが、補間ロジック自体は別ドキュメント。
- **Spatial 連動の自動 LPF 制御**（Phase 5、距離・遮蔽による cutoff 自動更新）。本パイプラインの Source LPF を Spatial 側から駆動する形で接続する。
- **プラグイン SDK**（Phase 6）。エフェクト種別は core 側で有限の enum とし、外部プラグインは扱わない。

---

## 設計判断サマリ

| 判断点 | 採用 | 不採用 | 理由（要約） |
|---|---|---|---|
| エフェクト挿入点 | **Bus / Source 統一**（`Owner = Bus(d) \| Source(d)` 多態化）| Bus のみ / Source のみ | Wwise / FMOD / Unreal いずれも統一 API。Unity だけが分離設計で parity が破綻している。認知負荷を下げる効果も大きい |
| 論理種別とアルゴリズムの分離 | `EffectKind`（公開）と `*Algo`（内部）を分離 | 単一 enum | Phase 6+ のアルゴリズム自動切替への伏線。Phase 2 では各種別 1 アルゴリズムのみ実装。**自動切替実装後もユーザーが物理アルゴリズムを直接指定できる API を必ず併存させる**（自動決定は手段、自己決定は権利） |
| ホットループのディスパッチ | **二段階 match (`kind` → `algo`)** | 関数ポインタテーブル / 種別バッチング | Phase 2 は algo 段が退化（compiler が単一 arm 消去）、Phase 6+ も algo 数は少なく分岐予測ミス無視可。網羅性チェックが効くのは大きい。Source 単位 LPF の大量並列化（Phase 5）時のみ「種別バッチング fast-path」を別経路として追加する余地を残す |
| ディスパッチ方式 | enum + match | `Box<dyn Effect>` | サウンドスレッド alloc/vtable 不可。種別は core 内で有限 |
| 状態（state）格納 | **メタ層 (`EffectWorld`) + 種別ごと専用 World**。種別 World 内の SoA 粒度は種別ごとに最適化（LPF/HPF は細粒度 SoA、Reverb は AoS）| 全エフェクト共通の enum Vec | Reverb の状態が巨大（~100KB）で enum 最大バリアントに引きずられる。LPF/HPF は Source 単位の大量並列化（Phase 5）で SoA が刺さるが、Reverb は遅延リングバッファ支配で SoA 恩恵薄。種別ごとに適切な粒度を選ぶ |
| LPF 係数再計算 | サウンドスレッドが `dirty` フラグを見て次回コールバック冒頭で再計算 | メインで計算して係数を送る / Atomic per-slot で cutoff を流して毎フレーム再計算 | コマンドサイズ削減（cutoff/q の 2 つだけ送る）。係数計算は数 op で軽い。高頻度オートメーションが必要になったら Phase 3 で Atomic 化を検討 |
| 鎖（チェーン）表現 | **owner 側（BusWorld / SourceWorld）に固定長 slot 配列**（`[EffectId; N]` + count）| EffectWorld 内に owner→chain 逆引き構築 / 連結リスト | `effect_count == 0` の素通し判定が O(1)、走査も owner 局所で最速 |
| 全体エフェクト容量 | 種別横断のメタ層プール `MAX_EFFECTS` + 種別別プール（`MAX_LPF`, `MAX_REVERB` …）| バス × slot 数の二次元固定確保 | Reverb の重い状態を必要数だけ確保。Source 単位 LPF の最悪ケース（256 体）にも対応 |
| エフェクトハンドル | `EffectId = (index, generation)` | バス内 slot index 直指定 | エフェクト本体の再配置（並べ替え・削除）に追随できるよう二層 ID を使う |
| Bus 内挿入位置 | **Pre-Fader / Post-Fader 2 段チェーン**（`EffectPosition::Pre \| Post`）| Post-Fader 1 段のみ | Wwise / ADX2 / FMOD いずれも Pre/Post 両対応。本格ミドルウェア準拠 |
| Source 内挿入位置 | **Pre-Spatial / Post-Spatial 2 段チェーン**（同 enum を流用）。Phase 2 は Pre のみ実装、Post は Phase 3+ | 1 段のみ | Wwise Voice 構造に準拠。Pre はモノラル（距離 LPF/遮蔽 HPF 用）、Post はステレオ（L/R 独立処理用）|
| パラメータ更新経路 | 既定はコマンド経路。**high-frequency なスカラーのみ Atomic per-slot** | 全部 Atomic | エフェクトの大半のパラメータは「シーン切替時に 1 度だけ動く」レベル。Atomic は per-frame 駆動の少数に限る |
| 効果なしバスの扱い | `effect_count[d] == 0` で全スキップ | 常にチェーン関数を呼ぶ | 設計ガードレール 3「最速経路を保つ」に従い、エフェクト未使用バスを劣化させない |

---

## アーキテクチャ全体図

```
SoundEngine（ファサード）
  │
  ├─▶ SourceWorld / SourceSystem  …… 既存
  │
  ├─▶ BusWorld / BusSystem  …… 既存
  │     ├── per-bus: pre_chain[d]  = [EffectId; N], pre_count[d]
  │     └── per-bus: post_chain[d] = [EffectId; N], post_count[d]
  │           （pre_count + post_count <= MAX_EFFECTS_PER_BUS）
  │
  ├─▶ SourceWorld / SourceSystem  …… 既存
  │     ├── per-source: pre_chain[s]  = [EffectId; N], pre_count[s]   ← Phase 2 で実装
  │     └── per-source: post_chain[s] = [EffectId; N], post_count[s]  ← Phase 3+ で実装
  │
  └─▶ EffectWorld / EffectSystem  …… 新規
        ├── sparse / dense 管理（EffectId 発行）
        ├── 共通 SoA（全エフェクト共通の薄いメタ層）
        │     ├── kind[]: EffectKind          ── 論理種別
        │     ├── algo[]: u8                  ── 物理アルゴリズム index（Phase 2 は常に 0）
        │     ├── owner[]: Owner              ── Bus(dense) | Source(dense) の多態
        │     ├── slot_index[]: u8            ── チェーン内位置
        │     ├── enabled[]: bool             ── タグコンポーネント相当
        │     └── state_index[]: u32          ── 種別別 World の dense index
        │
        ├── LpfWorld   ── 専用 SoA（cutoff, q, prev_in_l/r, prev_out_l/r …）
        ├── HpfWorld   ── LPF と対称
        ├── ReverbWorld
        │     ├── パラメータ SoA（room_size, damping, wet, dry, …）
        │     └── 遅延ラインプール（フラット f32 配列・事前確保 MAX_REVERBS × N）
        └── （将来）EqWorld / CompressorWorld / …
```

各エフェクト種別ごとに専用 World を持つ。`EffectWorld` は「種別を跨いだメタ管理層」で、ハンドル発行・所属関係・enabled タグだけを持ち、信号処理本体は種別 World が担う。

### なぜ「種別ごとに別 World」か

- LPF と Reverb で必要な状態が桁違い（LPF: 数 byte、Reverb: 数 KB〜十数 KB）。共通配列に詰めるとメモリが膨らむか、最大値合わせで無駄が出る。
- SoA で一括処理する際、同種エフェクトはほぼ同じ命令列を踏むため、種別を分けることで `update()` ループが分岐なし・SIMD 化容易になる。
- プラグイン SDK（Phase 6）で動的種別が必要になっても、その時点でメタ World 側に「`EffectKind::Plugin(plugin_id)`」を追加する拡張点を残せる。

---

## バス処理との結合

### Source 信号フロー（Phase 2）

Wwise Voice 構造に準拠した 2 段チェーン。

```
AudioBuffer
  ↓ Pitch (resampler)
  ↓ Pre-Spatial Effect Chain   ── モノラル前提。距離 LPF / 遮蔽 HPF はここ
  ↓ Spatial (pan / attenuation / doppler)
  ↓ Post-Spatial Effect Chain  ── ステレオ前提。Phase 3+ で実装、Phase 2 では空チェーン
  ↓ mix_buffer[output_bus] へ加算
```

- Pre-Spatial と Post-Spatial で扱うチャネル数が異なる（モノラル / ステレオ）ため、エフェクト種別ごとに **どちら側で動作可能か** を実装側で属性として持つ。例: Reverb は Bus 専用 (Phase 3-3 の Send/Return 経由で Source からも利用可)、距離 LPF は Source Pre 専用、ParamEQ は両方可。
- Phase 2 では **Source の Post-Spatial chain は API 上拒否**（`add_effect(SourceTarget, _, Post)` は `None` を返す）。Phase 3 で許可する際にステレオ対応エフェクトを追加する。

### Bus 信号フロー（Phase 2）

Wwise / ADX2 / FMOD と同じ「Pre-Fader → Fader → Post-Fader」3 ステージ。

```
全 Source からの加算済み mix_buffer[d]
  ↓ Pre-Fader Effect Chain
  ↓ if muted[d]: zero-fill, else: gain[d] 乗算       ← Fader
  ↓ Post-Fader Effect Chain
  ↓ [Phase 3+: Send 挿入余地]
  ↓ if d != master: 親バスへ加算
master の mix_buffer → 出力デバイス
```

### オーディオコールバック内の呼び出し順序

```
1. BusWorld::clear_mix_buffers()
2. SourceSystem::update(...)
     for each active source:
        resample (pitch)
        Pre-Spatial chain を適用（chain 空ならスキップ）
        Spatial 適用
        Post-Spatial chain を適用（Phase 2 では常に空、後続フェーズで有効化）
        mix_buffer[output_bus * stride ..] に加算
3. BusSystem::update(...)
     for d in process_order:
        Pre-Fader chain を適用（chain 空ならスキップ）
        if muted[d]: zero-fill
        else:        apply gain[d]
        Post-Fader chain を適用（chain 空ならスキップ）
        if d != master: accumulate into mix_buffer[parent * stride ..]
4. master の mix_buffer を output_buffer にコピー
```

### 効果なし target の最速経路

各チェーン（Pre/Post いずれも）について `count == 0` なら `EffectSystem::apply_chain` を呼ばない。チェーン関数の呼び出し・分岐コストともゼロ。Phase 1 までの動作と完全に等価な性能を保つ。

### in-place 処理と一時バッファ

- LPF / HPF: バスの mix_buffer を直接 in-place で書き換える。一時バッファ不要。
- Reverb: wet 信号を一時バッファに書き、最後に dry とミックスしてバスに書き戻す。一時バッファは `EffectSystem` が事前確保した固定長スクラッチ（`scratch: Vec<f32>`、長さ `MAX_MIX_BUFFER_SIZE`）を借用する。
  - スクラッチは 1 本だけ確保し、エフェクトチェーン内で逐次再利用する。同時に複数の wet バッファが必要な構成（並列センド等）は Phase 3 で追加判断。

---

## エフェクト種別の状態設計

### LPF / HPF（Biquad 1 段、細粒度 SoA）

```rust
struct LpfWorld {
    // 逆引き（メタ層との往復参照、despawn 再マッピング用）
    effect_id_at_dense: Vec<EffectId>,
    // パラメータ SoA
    cutoff_hz: Vec<f32>,
    q:         Vec<f32>,
    dirty:     Vec<bool>,        // 係数再計算フラグ
    // 計算済み係数 SoA
    coeffs:    Vec<BiquadCoeffs>, // a1, a2, b0, b1, b2
    // フィルタ状態 SoA（チャネルごと）
    z_l:       Vec<[f32; 2]>,    // z^-1, z^-2 サンプル（左）
    z_r:       Vec<[f32; 2]>,    // 〃 （右）
}
```

- 係数再計算は **サウンドスレッドで `dirty` フラグを見て次回コールバック冒頭にフラッシュ**する。メイン側で係数 5 個を運ぶ案より `SetEffectParam` のサイズが小さく済む。
- 高頻度オートメーション（毎フレーム cutoff を流すユースケース）が必要になったら Phase 3 で Atomic per-slot 化を再検討。Phase 2 では不要。

### Reverb（Freeverb 系、AoS）

遅延リングバッファへの read/write が支配的なため細粒度 SoA の恩恵が薄い。状態とパラメータを `ReverbState` に同居させた AoS で持つ。

```rust
struct ReverbWorld {
    effect_id_at_dense: Vec<EffectId>,
    states: Vec<ReverbState>,
    // 遅延ラインプール（フラット、初期化時に一括確保）
    delay_pool: Vec<f32>,        // MAX_REVERBS × (N_COMBS × COMB_LEN + N_ALLPASS × AP_LEN)
}

struct ReverbState {
    // パラメータ
    room_size: f32, damping: f32, wet: f32, dry: f32, width: f32,
    // 遅延プール内オフセットとリングポインタ
    pool_offset: u32,
    write_pos:   u32,
}
```

- `delay_pool` は `EffectWorld` ではなく `ReverbWorld` が保有。Phase 2 は `MAX_REVERBS = 16`（Freeverb 既定で 1 体 ~100 KB、合計 1.6 MB を事前確保）。サウンドスレッドでは alloc しない。
- Source 対象に Reverb を指定された場合、`add_effect` は `None` を返す（プール枯渇を待たず API 段階で拒否）。これは Phase 2-3 時点での暫定制約であり、業界標準 (Wwise / FMOD / Unity) では per-voice Reverb は技術的に可能だが運用上ほぼ使われず、**Aux Bus + Send/Return で 1 個の Reverb を共有**する形が標準パターンとなる。NEZIA も Phase 3-3 で Send/Return が入った時点で同じパターンが組めるようになり、Source 単位 Reverb の直接挿入機能を別途追加する必要はない。

### despawn と state_index の再マッピング

メタ層 (`EffectWorld`) と種別 World は独立に swap-remove するため、`despawn` 時のメンテが 2 箇所に走る:

1. メタ層で `EffectId` を解決 → 対応する `state_index[i]` と `kind[i]` を取得
2. 種別 World 側で当該 `state_index` の dense slot を swap-remove。最後尾だった state がここに移動する
3. 種別 World の `effect_id_at_dense[]` で「移動した state を保持していた `EffectId`」を逆引き
4. メタ層の `state_index[逆引き先 EffectId]` を新 dense index に書き換え
5. メタ層自身も swap-remove（owner / slot_index / chain 側の整合は別ループで処理）

逆引きテーブル `effect_id_at_dense: Vec<EffectId>` を種別 World 側に持たせることで O(1) で再マップできる。

### 共通: enabled タグ

`enabled[i] == false` のエフェクトは状態を保持したまま処理だけスキップする。バス mute と同様、ホットループで `if !enabled[i] { continue; }` の 1 分岐で素通しできる。

---

## パラメータ更新経路

エフェクトパラメータの性質を分類すると:

| 性質 | 例 | 頻度 |
|---|---|---|
| 静的（シーン構築時に 1 度設定） | Reverb の room_size, damping | 低 |
| 状態遷移時に変化（Snapshot 補間） | フェード中の wet 量 | 中（補間中のみ） |
| 連続駆動（オートメーション） | Spatial 連動 LPF cutoff | 高（毎フレーム） |
| トグル系 | enabled, bypass | 低 |

### Phase 2 での経路割り当て

| 経路 | 用途 |
|---|---|
| **Command (SPSC ringbuf)** | `SetEffectParam` / `SetEffectEnabled` / `SpawnEffect` / `DespawnEffect` / `UpdateEffectOrder` のすべて |
| Triple Buffer | 使わない |
| Atomic per-slot | 使わない |

**Phase 2 はコマンド経路 1 本で構成する**。エフェクトパラメータは「静的 or 低頻度」想定であり、新しい同期機構を追加する正味の利得がない。設計ガードレール 1 の「同期機構の数が増えるとデバッグ性とテスト面積が二乗で増える」を厳守する。

### 後続フェーズでの再評価（必須）

新しい経路を追加する判断は以下のタイミングで**必ず再評価する**。各フェーズの設計着手時に本セクションに戻り、追加要否を明文化したうえで進む:

| 再評価タイミング | 検討すべき経路追加 | 判断材料 |
|---|---|---|
| **Phase 3-2 Snapshot 補間** | Triple Buffer もしくは「サウンドスレッド内部補間 + Command で開始指示」 | 多パラメータの一貫遷移が必要か、補間中の中間値の tearing が音響的に許容できるか |
| **Phase 5 Spatial 連動 LPF** | Atomic per-slot（256 ソース毎フレーム駆動）/ あるいは Spatial が直接 SoA に書き込む内部経路 | 公開パラメータとして外に出すか、Spatial → DSP の内部経路で完結するか |

「なんとなく速そう」での追加は不可。ベンチマークで現行コマンド経路との比較を残し、設計ドキュメントに採用理由を明記すること。

### `SetEffectParam` のパラメータ ID

種別ごとに `#[repr(u8)]` enum を定義し、コマンド層では数値で運ぶ:

```rust
#[repr(u8)]
pub enum LpfParam   { Cutoff = 0, Q = 1 }
#[repr(u8)]
pub enum HpfParam   { Cutoff = 0, Q = 1 }
#[repr(u8)]
pub enum ReverbParam { RoomSize = 0, Damping = 1, Wet = 2, Dry = 3, Width = 4 }

SetEffectParam { id: EffectId, param: u8, value: f32 }
```

- 文字列キーは使わない（コマンドサイズ・サウンドスレッドでの hash 計算を避ける）。
- 公開 API はトレイトでラップして型安全にする（次節）。

---

## コマンド追加分

```rust
enum Command {
    // 既存（省略）

    // ── DSP ──
    SpawnEffect {
        id: EffectId,
        target: EffectTarget,
        kind: EffectKind,
        algo: u8,                 // Phase 2 では常に 0
        position: EffectPosition,
        slot_index: u8,
    },
    DespawnEffect { id: EffectId },
    SetEffectEnabled { id: EffectId, enabled: bool },
    SetEffectParam { id: EffectId, param: u8, value: f32 },
    /// 対象の指定チェーン順序を更新する。
    /// 可変長を固定サイズで運ぶため上限サイズの配列で渡す（Bus/Source 共通の最大値で固定）。
    UpdateEffectOrder {
        target: EffectTarget,
        position: EffectPosition,
        order: [EffectId; MAX_EFFECTS_PER_BUS],
        len: u8,
    },
}
```

- `UpdateEffectOrder` のサイズは `EffectTarget(16B) + EffectPosition(1B) + 8 × EffectId(8B) + len(1B) ≈ 82 byte`。最大バリアント `UpdateProcessOrder { [u32; 64] + u8 }` (= 257 byte) より小さく ringbuf スロット幅に影響しない。
- `SpawnEffect` の `id` は **メインスレッド側 `EffectIdAllocator` が事前発行**（Source の `play_with_handle` と同パターン）。これにより返り値を待たずに直後に `SetEffectParam` を発行できる。Atomic per-slot を将来追加する際の前提となる「`EffectId.index` の bounded 性」もここで保証される。

---

## 公開 API（草案）

```rust
/// パラメータ ID の型安全ラッパ。種別ごとの enum がこれを実装する。
pub trait EffectParamId: Copy {
    const KIND: EffectKind;
    fn as_u8(self) -> u8;
}

impl SoundEngine {
    /// 対象（バスまたはソース）の指定チェーン末尾にエフェクトを追加する。
    /// 拒否されるケース:
    /// - Source 対象 + Reverb (Phase 2-3 時点の暫定制約。Phase 3-3 の Send/Return 実装後は
    ///   業界標準どおり Aux Bus に Reverb を載せて Source からは Send で参照する形になる)
    /// - Phase 2 では Source 対象 + `EffectPosition::Post`（Post-Spatial 未実装）
    /// - 各チェーンの上限超過、または `MAX_EFFECTS` プール枯渇
    pub fn add_effect(
        &mut self,
        target: EffectTarget,
        kind: EffectKind,
        position: EffectPosition,
    ) -> Option<EffectId>;

    /// 物理アルゴリズムを直接指定して追加する（手動オーバーライド経路）。
    /// Phase 2 では各種別 1 アルゴリズムなので `add_effect` と等価。
    /// Phase 6+ のアルゴリズム自動切替実装後も、この API は残してプロが手動で組めるようにする。
    pub fn add_effect_with_algo(
        &mut self,
        target: EffectTarget,
        kind: EffectKind,
        algo: u8,
        position: EffectPosition,
    ) -> Option<EffectId>;

    pub fn remove_effect(&mut self, id: EffectId) -> bool;
    pub fn set_effect_enabled(&mut self, id: EffectId, enabled: bool) -> bool;

    /// 型安全なパラメータ設定。
    /// 例: `engine.set_effect_param(eff, LpfParam::Cutoff, 1000.0);`
    /// `P::KIND` と当該エフェクトの実 kind が一致しない場合は `false` を返す。
    pub fn set_effect_param<P: EffectParamId>(
        &mut self,
        id: EffectId,
        param: P,
        value: f32,
    ) -> bool;

    pub fn effect_param<P: EffectParamId>(&self, id: EffectId, param: P) -> Option<f32>;

    /// 対象の指定チェーンの順序を入れ替える。
    pub fn reorder_effects(
        &mut self,
        target: EffectTarget,
        position: EffectPosition,
        order: &[EffectId],
    ) -> bool;
}
```

`EffectKind` は **論理種別** を表す公開 enum。物理アルゴリズムは内部で隠す:

```rust
// 公開 API
pub enum EffectKind {
    Lpf,
    Hpf,
    Reverb,
    // 後続フェーズで Compressor / ParamEq / … を追加
}

pub enum EffectTarget {
    Bus(EntityId),
    Source(EntityId),
}

/// エフェクトの挿入位置。意味は target によって変わる:
/// - Bus: Pre = Pre-Fader（gain 適用前）, Post = Post-Fader（gain 適用後）
/// - Source: Pre = Pre-Spatial（モノラル）, Post = Post-Spatial（ステレオ）
pub enum EffectPosition {
    Pre,
    Post,
}

// 内部のみ
enum ReverbAlgo { Freeverb /* Phase 6+ で Schroeder, Convolution を追加 */ }
enum LpfAlgo    { Biquad   /* 〃 */ }
```

論理パラメータは正規化された値を取る（`wet ∈ [0, 1]`, `room_size ∈ [0, 1]` 等）。Phase 6+ でアルゴリズムが増えた際、各物理実装が同じ正規化パラメータを内部マッピングして一貫挙動を保つ。

API 形は **Unity AudioMixer の AddEffect/RemoveEffect/SetEffectParameter** と
**FMOD `addDSP` / Web Audio `connect`** の語彙に揃える（ガードレール 5）。
具体パラメータ型は将来種別が増えた際に `effect_param!` マクロや builder で吸収するが、Phase 2 では数値 ID 直渡しで十分。

---

## 容量とメモリ見積もり

いずれも後から定数調整可能。Phase 2 着手時の初期値:

| 項目 | 初期値 | 備考 |
|---|---|---|
| `MAX_EFFECTS` | 256 | 全種別合算メタ層プール（Bus 64×少数 + Source 256×1 LPF を想定） |
| `MAX_EFFECTS_PER_BUS` | 8 | Bus チェーン上限。Wwise/FMOD 実用例は 3〜5 |
| `MAX_EFFECTS_PER_SOURCE` | 4 | Source チェーン上限。距離 LPF + 遮蔽 HPF + EQ + 余り |
| `MAX_LPF` / `MAX_HPF` | 256 | Source 全数 LPF を許容するため Source 数と同等 |
| `MAX_REVERB` | 16 | Bus 限定。Source 対象は API 段階で拒否 |
| LPF/HPF state（per instance） | ~64 byte | 係数 + z 状態 × 2ch |
| Reverb state（per instance） | ~100 KB | Freeverb 既定 8 comb + 4 allpass |
| EffectWorld メタ層 | ~6 KB | 256 エフェクト × ~24 byte |
| Reverb プール合計 | ~1.6 MB | 100 KB × 16 |
| EffectSystem スクラッチ | 32 KB | `MAX_MIX_BUFFER_SIZE × 4 byte`、wet 用一時 |
| `UpdateEffectOrder` コマンドサイズ | 65 byte | 8 × `EffectId(8B)` + `len(1B)`、`UpdateProcessOrder` (257B) より小さく ringbuf 幅に影響なし |

すべて初期化時に一括確保。サウンドスレッドでは alloc・解放・realloc いずれも発生しない。

---

## 拡張余地（後続フェーズへの接続）

| 後続フェーズ | 本設計で確保した拡張点 |
|---|---|
| Phase 3-2 Snapshot 補間 | `SetEffectParam` のパラメータ ID 体系をそのまま補間トラックの addressing に流用できる |
| Phase 3-3 Send / Sidechain | バス処理の `gain → effects → (ここに Send 挿入) → 親加算` の隙間を確保済み。**この時点で「Aux Bus に Reverb を 1 個載せて全ソースから Send で参照」という業界標準パターンが組めるようになり、Source 単位 Reverb の直接挿入機能は不要のまま** |
| Phase 3-5 ParamEQ / Compressor / Limiter | `EffectKind` への variant 追加 + 専用 World 追加で完了 (Compressor は Phase 3-3 Sidechain Ducking で先行実装済、PeakingEq・単体 Limiter も実装済) |
| Phase 5 Spatial 連動 LPF（距離・遮蔽による cutoff 自動更新）| Source 単位 LPF は Phase 2 で既に動作するため、Spatial 側から `set_effect_param` を毎フレーム駆動するだけで実現。新しい経路は不要 |
| Phase 6+ アルゴリズム自動切替（品質予算）| `EffectKind` と内部 `*Algo` が分離済み。priority/quality budget を見て切替するスケジューラを別レイヤーで追加する。state は破棄して再初期化する経路をコマンド処理段で確保。**自動決定はあくまで手段として提供し、ユーザーが物理アルゴリズムを直接指定する API（`add_effect_with_algo(target, kind, algo)`）も併存させる**。プロが手動で組みたいユースケース（特定アルゴリズムの音響特性に依存した演出）への逃げ道を残す |
| Phase 6+ プラグイン SDK | `EffectKind::Plugin(PluginId)` を追加し、メタ World が動的状態を保持する経路を追加する |

---

## 設計上の非目標

- **任意 DAG の DSP グラフ**は組まない。エフェクトは「バス内で一直線のチェーン」に限定する。任意グラフは Wwise/FMOD 流の Cue/Patch 概念とともに Phase 6 以降で再検討する。
- **サンプル単位の自動オートメーション**（DAW 的なエンベロープ）は持たない。パラメータの時間変化は Snapshot（Phase 3）またはアプリ側のティック更新で表現する。
- **オーバーサンプリング**（歪み系の前段で 2x/4x かける処理）は当面入れない。Compressor/Limiter（Phase 3-5）で必要が出た際に個別判断。

---

## 関連ドキュメント

- [ロードマップ — Better than Unity Audio](../../roadmap/better-than-unity-audio.md) Phase 2-3 / 3-2 / 3-3 / 3-5
- [バスルーティング](bus.md) — エフェクト挿入位置の前提となるバス処理フロー
- [スレッドモデル](threading.md) — パラメータ経路選択のガードレール
- [ECS アーキテクチャ](ecs.md) — World/System 命名規則と SoA レイアウト方針
- [統合戦略](../integration/CONCEPT.md) — `AudioMixerGroup` 互換 API（A 経路）への露出方針
