# Container (Random / Switch / Sequence)

NEZIA ENGINE の Container 機能設計。Wwise / CRI ADX の Cue 系機能の縮小版。
本ドキュメントは [ロードマップ](../../roadmap/better-than-unity-audio.md)
**Phase 4-2** の段階的実装のうち、**第一弾「Random Container 単体」** の設計を扱う。

Switch / Sequence Container は将来 PR で同じデータ構造の上に拡張する。

---

## スコープ

### この PR で扱う
- **Random Container** — 子の中から 1 つランダムに選んで再生する論理オブジェクト。
- 子: `BufferId` のみ (1 子 = 1 ワンショットサウンド)。
- 戦略: 一様確率 + **avoid-last** (子が 2 つ以上のとき、直前に引いたものは続けて引かない)。
- API: `create_random_container` / `destroy_container` / `play_container` / `play_container_with_handle`。

### この PR では扱わない (将来拡張)
- **Container ネスト** — 子に Container を含める。データ型 (`enum ContainerChild`) では予約だけしておき、API では受け付けない。
- **Switch Container** — 外部スイッチ値で分岐。
- **Sequence Container** — 順序再生 + クロスフェード (3-4 PlayScheduled が前提)。
- **重み付き選択** — 各子に確率を持たせる。
- **子ごとの volume / pitch ランダマイズ** — `±n dB` / `±n cent` の範囲指定。
- **3D 再生** — `play_to_bus` 系のみ対応。3D 位置を持つ Source として再生する API は将来追加。

---

## 設計判断サマリ

| 判断点 | 採用 | 不採用 | 理由 |
|---|---|---|---|
| 配置スレッド | **メインスレッド完結** | audio thread に Container World を持つ | Container は「再生指示の解決ロジック」であり DSP を持たない。audio thread に流す必要がない。リアルタイム制約 (lock / alloc 禁止) を考慮する場所が増えない |
| ハンドル型 | **独自 `ContainerId`** | `EntityId` を流用 | audio thread に流れないので EntityId の二層 ID 設計とは意味的に別物。混同を避けるため新規型 |
| 子の型 | **`enum ContainerChild { Source(BufferId) }`** | `Vec<BufferId>` 直 | 将来 `Container(ContainerId)` を足すときに破壊的変更を回避。今 PR では `Source` バリアントのみ受理 |
| スロット管理 | **generation 付き Vec<Slot>** (BufferPool 同型) | スパースセット (dense 配列) | Container は数が少なく random access 中心。dense packing の利点 (一括イテレーション) が活きない。BufferPool と同パターンで揃える |
| Random 選択 | **xorshift64 + avoid-last** | `rand` クレート / `Vec` シャッフルバッグ | 依存追加なし、状態 8 バイト、判定分岐ゼロで高速。avoid-last は再抽選方式 (子 2 個以上で常に終了) |
| PRNG seed | **時刻 (ns) ⊕ container index** | グローバル決定的 seed | 個体差 + 起動ごとの差をつけたいが、テスト容易性のため後で seed override API を足せる構造にする (今 PR ではしない) |
| `play_container` の意味論 | **メインスレッドで子を 1 つ選び `Command::PlayToBus` を発行** | 専用 Command を audio thread に送る | audio thread 側に追加コード不要。既存の Source 再生経路をそのまま再利用 |
| 0 子のときの挙動 | **`create_*` を `None` で拒否** | 作れるが `play` が no-op | 0 子 Container は意味がない。作成時に弾く |
| 子 1 個のときの avoid-last | **無効化** (常にその 1 個を返す) | 再抽選で無限ループ | 自明な防御 |

---

## アーキテクチャ全体図

```
[main thread]
SoundEngine
  ├─ create_random_container(&[BufferId]) ─→ ContainerWorld.spawn_random()
  │                                          ContainerId 返却
  │
  └─ play_container(id, vol, pitch, bus, looping)
       ↓
       ContainerWorld.pick(id) ─→ BufferId
       ↓
       既存 play_to_bus / play_with_handle 経路に委譲
       ↓
       Command::PlayToBus / SpawnSource ─→ [audio thread]
```

audio thread 側には新規コマンド・新規 World は **一切追加しない**。

---

## データ構造

### `ContainerId`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContainerId {
    pub index: u32,
    pub generation: u32,
}
```

`BufferId` と同じ generation 付きハンドル。FFI 層との互換のため `#[repr(C)]` を付与。

### `ContainerChild`

```rust
pub(crate) enum ContainerChild {
    Source(BufferId),
    // Container(ContainerId), // 将来用
}
```

Phase 4-2 第一弾では `Source` のみ。`Container` バリアントの追加で破壊的変更にならないよう enum 化しておく。

### `RandomContainer`

```rust
pub(crate) struct RandomContainer {
    children: Vec<ContainerChild>,
    last_picked: Option<usize>,
    rng_state: u64,
}
```

- `children`: 1 個以上 (作成時に保証)。
- `last_picked`: 直前に返したインデックス。avoid-last 用。
- `rng_state`: xorshift64 状態。0 にならないよう初期化時に保証。

### `ContainerWorld`

```rust
pub(crate) struct ContainerWorld {
    slots: Vec<Slot>,
    free_list: Vec<u32>,
    next_index: u32,
}

struct Slot {
    generation: u32,
    occupied: bool,
    container: Option<RandomContainer>, // Switch/Sequence を足すなら enum 化
}
```

`MAX_CONTAINERS` 上限あり (=128)。

---

## 主要 API

```rust
impl SoundEngine {
    /// Random Container を生成する。子は 1 個以上必須。
    /// `BufferId` の有効性は呼出側責任 (将来は resolve チェックも入れる)。
    #[must_use]
    pub fn create_random_container(&mut self, children: &[BufferId]) -> Option<ContainerId>;

    /// Container を破棄する。再生中の Source には影響しない (既に独立した Source として走っている)。
    pub fn destroy_container(&mut self, id: ContainerId) -> bool;

    /// Container から子を 1 つ選んで指定バスに再生する (fire-and-forget)。
    #[must_use]
    pub fn play_container(
        &mut self,
        container: ContainerId,
        vol: f32,
        pitch: f32,
        bus: EntityId,
        looping: bool,
    ) -> bool;

    /// Container から子を 1 つ選んでハンドル付きで再生する。
    /// 返るのは選ばれた **その 1 つの Source の EntityId**。
    /// Container 自体のハンドルは `ContainerId` のまま。
    #[must_use]
    pub fn play_container_with_handle(
        &mut self,
        container: ContainerId,
        vol: f32,
        pitch: f32,
        bus: EntityId,
        looping: bool,
    ) -> Option<EntityId>;
}
```

---

## Random 選択アルゴリズム

```
fn pick(&mut self) -> ContainerChild:
    if children.len() == 1:
        return children[0]                  # avoid-last 不要

    loop:
        let r = xorshift64(&mut rng_state) % children.len()
        if last_picked != Some(r):
            last_picked = Some(r)
            return children[r]
        # 同じインデックスを引いたら再抽選
```

- xorshift64 は 1 サイクルで `state ^= state << 13; state ^= state >> 7; state ^= state << 17;` の 3 演算。
- 子 2 個のとき、再抽選確率は 1/2 なので期待ループ回数は 2。子 3 個以上では 1.5 以下。最悪のケースでも O(1) 期待値で終了する。

---

## 設計上のガードレール

[ロードマップのガードレール](../../roadmap/better-than-unity-audio.md#設計上のガードレール) を遵守:

1. **既存 ECS / SoA / コマンドパターンを壊さない** — Container はメインスレッド側のみで完結し、audio thread には影響しない。
2. **サウンドスレッドのリアルタイム制約** — そもそも audio thread に Container コードが入らないので無関係。
3. **最速経路を保つ** — Container を使わない既存ユーザーには影響なし。
4. **API はメジャー実装に寄せる** — Wwise の Random Container / FMOD Multi Instrument の最低機能セットに揃える。

---

---

## Unity 統合での露出 (参考: `Nezia.Unity` リポジトリで実装)

core API のみを今 PR で出す。Unity 側の実装は別リポジトリだが、core 側の
`enum ContainerChild` の設計判断は Unity 側のクラス階層と同型である必要があるため、
方針だけここに記録する。

### A 経路 (ドロップイン互換)

**共通抽象基底 `NeziaSoundAsset` を介して Single / Random を統一的に扱う**:

```csharp
public abstract class NeziaSoundAsset : ScriptableObject {
    internal abstract void Resolve(NeziaSoundEngine engine);
    internal abstract bool PlayTo(NeziaAudioSource src, /* vol / pitch / ... */);
}

public class NeziaAudioClip : NeziaSoundAsset {
    [SerializeField] AudioClip clip;        // or byte[]
    BufferId resolvedBuffer;
}

public class NeziaRandomContainer : NeziaSoundAsset {
    [SerializeField] NeziaSoundAsset[] children;  // ← 基底型配列
    ContainerId resolvedContainer;
}

public class NeziaAudioSource : MonoBehaviour {
    [SerializeField] NeziaSoundAsset sound;       // Inspector D&D
    public void Play() => sound?.PlayTo(this, ...);
}
```

**設計判断**:
- `NeziaAudioSource.sound` は **基底型 `NeziaSoundAsset`** を受ける。Single でも Random でも
  Inspector 操作・コード側の `Play()` 呼び出しは同一。AudioSource 互換の体験を保つ。
- `NeziaRandomContainer.children` も **基底型 `NeziaSoundAsset[]`**。これにより:
  - 今は子に `NeziaAudioClip` のみ指定可 (`Resolve` で検証してエラー表示)。
  - 将来 core 側が `ContainerChild::Container(ContainerId)` を受け付けたとき、
    Inspector 構造を変えずにネストが組めるようになる (C# 側のクラス階層と
    core 側の `enum ContainerChild` の予約が同型になっている)。

### B 経路 (プロジェクトファイル)

オーサリングツールが論理 ID 文字列で Container を定義し、JSON にシリアライズ。
ランタイムロード時に `create_random_container` を呼んで物理 ID に解決し、
`HashMap<HashId, ContainerId>` に登録する。ゲームコードは
`engine.Play("sfx/footstep_random")` のように論理 ID 経由で発火する。

詳細は将来の `docs/design/authoring/` で扱う。

---

## 関連ドキュメント

- [ロードマップ](../../roadmap/better-than-unity-audio.md) — Phase 4-2 の位置付け
- [Source ワールド](source.md) — Container が解決した結果として spawn される Source の寿命モデル
- [統合戦略](../integration/CONCEPT.md) — A/B 二経路ワークフローの全体像
