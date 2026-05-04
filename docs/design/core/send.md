# Send / Receive + Sidechain Ducking

NEZIA ENGINE における Send (副ルート) と Sidechain Ducking の設計。
Unity AudioMixer の Send / Receive / Duck Volume 互換 + 業界標準 (Wwise / FMOD) の Aux Bus 運用。
本ドキュメントは [ロードマップ](../../roadmap/better-than-unity-audio.md) の **Phase 3-3「Send / Receive + Sidechain Ducking」** に対応する設計を扱う。

このドキュメントの目的は **「バスを木構造から DAG に拡張するための境界の確定」と「Sidechain 駆動コンプレッサーをどう DoD に載せるか」の判断** にある。Phase 3-2 で確定した Snapshot / Phase 2-3 で確定した DSP パイプラインの上に乗る形で、副ルート経路と外部入力駆動エフェクトを規定する。

---

## スコープ

### このフェーズで扱う

- **バス → バス Send** (副ルート)。本線の `output_bus` とは別に、任意のバスへの追加経路を 0〜N 本生やせる。
- **Pre-Fader / Post-Fader Send** の 2 種類のタップ位置。
- **Send ごとの独立 gain**。
- **Sidechain Ducking 専用 Compressor エフェクト** (`EffectKind::Compressor`)。サイドチェーン入力は Send で駆動する。
- **Send/Compressor の Snapshot 補間**。Snapshot エントリに `send_gain` / Compressor パラメータを追加。
- **DAG 化に伴うサイクル検出 + トポロジカルソート** の更新。
- **Aux Bus パターン** (Reverb / Delay 等を 1 個の Aux Bus に載せ、複数バスから Send する) の実用例を API 設計の検証用に揃える。

### このフェーズでは扱わない

- **Source → Bus Send**。Source は引き続き単一の `output_bus` のみ持つ (`play_to_bus` パターン)。Aux 効果が必要な場合は Source → Bus → (Send) → Aux Bus と中継する。理由は [なぜ Source Send を入れないか](#なぜ-source-send-を入れないか) に詳述。
- **Send → Effect の sidechain 入力以外のルート**。Send の宛先は「バス」もしくは「Compressor の sidechain 入力」の 2 択であり、任意のエフェクト内部入力に Send することは扱わない。
- **マルチバンド / ルックアヘッド付き Compressor**。Phase 3-3 はピーク検出 + RMS 平均 + attack/release のシンプルな 1 段。マルチバンドは Phase 3-5 (DSP 拡充) で再検討。
- **Lookahead サイドチェーン**。Send 経路に遅延を入れるとレイテンシ管理が複雑化するため、当面はゼロレイテンシ Send + ルックアヘッドなし Compressor で固定。
- **Send パラメータの Atomic per-slot 駆動**。コマンド経路で十分。Phase 5 の Spatial 連動が必要になった時点で再評価。
- **任意 DAG の DSP グラフ** (バス内チェーンを跨ぐ任意接続)。DSP ドキュメントの非目標を継承する。

---

## 設計判断サマリ

| 判断点 | 採用 | 不採用 | 理由 |
|---|---|---|---|
| グラフ構造 | **DAG (`output_bus` + `sends`)** | 木 + 仮想 send / 任意グラフ | Wwise / FMOD / Unity 全て DAG。木のままだと parity gap、任意グラフは過剰 |
| Send ID | **`SendId = (index, generation)`** (二層 ID) | バス内 slot index 直指定 | バス削除や send 再配置に追随。Snapshot / Set 系コマンドが永続ハンドルを持てる |
| Send 格納 | **`BusWorld` 内に owner 側 SoA** (`send_dest_dense[bus][slot]` + `send_gain[]` + `send_position[]` + `send_count[]`) | 独立 `SendWorld` に集中管理 | バスの hot loop で送信処理を行うため owner 局所が最速。`pre_chain` / `post_chain` と同パターン |
| Send タップ位置 | **Pre-Fader / Post-Fader 2 段** (Pre-Fader chain 後 / Post-Fader chain 後の 2 ポイント) | Post のみ | Wwise / Unity 共に両対応。Sidechain trigger は Pre-Fader が定石 (本線 mute でもダッキングが効く) |
| Send 信号タップ | **Fader 段で計算した「次に親へ流す信号」を tap & 別バスにも加算** | 別 chain で signal を再計算 | tap は単純な加算 (1 ループ × send 数)。再計算はコスト二倍 |
| 容量 | `MAX_SENDS_PER_BUS = 4` / `MAX_SENDS = 128` | 動的拡張 | 事前確保。Wwise/FMOD の実用例は 1〜3 send。`MAX_BUSES × MAX_SENDS_PER_BUS = 256` の半数 (実運用で全バス満杯はない) を見込んで 128 |
| 処理順序 | **DAG トポロジカルソート (入力先行順)** | リーフ→ルート / 本線のみソート | Send 先のエフェクト (Compressor sidechain など) が tap に依存するためソース側を先に処理する必要あり。木構造の「リーフ→ルート」では DAG では不十分 |
| Send 信号のチャネル | **interleaved stereo のまま tap & 加算** (`mix_buffer` と同形式) | mono ダウンミックス | bus の `mix_buffer` がそもそも interleaved stereo。同形式で運ぶ方が tap が単純コピー + 乗算で済む。Compressor 検波器側で `max(|L|, |R|)` を取る |
| サイクル検出 | **メインスレッドで DFS、検出時はコマンド送信を拒否** | サウンドスレッド側で実行時検出 | サウンドスレッドはロックフリー実行。検出と再ソートは cold path |
| Sidechain 入力経路 | **Compressor が dense な sidechain 入力バッファを持ち、Send 宛先にできる** | Compressor が「監視するバス」を持つ別経路 | Send 経路で統一すれば実行時計算は 1 本化。Pre/Post 切替も Send 側で完結 |
| Compressor アルゴリズム | **ピーク+RMS 検波 / log domain attack-release / soft knee / オプショナル sidechain** | LA-2A / opto モデル等 | parity 用途では基本コンプで十分。アナログモデルは Phase 6+ で別 algo として追加 |
| Sidechain なし時の挙動 | **自分のバスの post-fader 入力で内部検波** | sidechain なしで動作不可 | 通常コンプとしても使えるようにし、sidechain は「外部 trigger を Send で差し込む」差分機能とする |
| Snapshot 統合 | **`send_gain` エントリ + `effect_param` で Compressor パラメータ** | Send 専用の独立補間機構 | snapshot.md の `effect_param` 経路を流用、`bus_send_gain` を新エントリ種別として追加 |
| send_gain 補間空間 | **dB 空間で線形補間** (バスゲインと同じ) | 線形 | 聴感の対数特性。Snapshot 既存の `lerp_db_gain` を流用 |
| Compressor パラメータ補間空間 | **線形** (threshold dB, ratio, attack ms, release ms, makeup dB) | dB 線形 | UI スライダーで弄る単位そのまま、Reverb と同じ判断 |
| Send の SIMD | **当面スカラ実装** (タップ + send_gain 乗算 + 加算) | 即時 SIMD 化 | Send 数は per-bus 数本。バスループ自体が支配的でないため最適化は計測後 |
| 公開 API 形 | `add_send(src, dst, position) -> Option<SendId>` + `set_send_gain(SendId, gain)` | バス内 slot index API | Unity の `audioMixer.GetFloat("SendVolume")` 系 + Wwise `SetGameObjectAuxSendValues` の語彙折衷 |

---

## 用語

| 用語 | 意味 |
|---|---|
| **本線 (primary route)** | バスの `output_bus_dense` で指定された 1 本の親バス。Phase 1 の木構造そのもの |
| **Send (副ルート)** | 本線とは別に、バスの信号を追加で別バスへ送る経路。Send ごとに gain と Pre/Post タップ位置を持つ |
| **Aux Bus** | Reverb / Delay 等の効果専用バス。複数バスから Send で集約され、効果をかけて本線に戻す運用 |
| **Sidechain** | コンプレッサーが「自分の入力ではない別の信号」を検波器に入れる仕組み。NEZIA では Send で入力する |
| **Trigger Bus** | Sidechain ducking で「ダッキングのきっかけになる信号を持つバス」(例: ナレーション) |
| **Target Bus** | ダッキングされる側のバス (例: BGM)。Compressor が挿さる側 |

---

## アーキテクチャ全体図

```
[main thread]                                 [sound thread]
add_send(BGM, Reverb_Aux, Pre, gain=0.4)
   ↓
   1. cycle detection (DFS over output_bus + sends)
   2. topological sort
   ↓
Command::AddSend { id, src_dense, dst_dense, position, gain }
Command::UpdateProcessOrder { order, len }
   ↓ ringbuf push
                                BusWorld の SoA に追加
                                  send_id[src][slot] = id
                                  send_dest_dense[src][slot] = dst_dense
                                  send_gain[src][slot] = gain
                                  send_position[src][slot] = Pre|Post
                                  send_count[src] += 1

                                BusSystem::update():
                                  for d in process_order:
                                    Pre-Fader chain
                                    [ tap pre-fader sends → 各 dest mix_buffer に加算 ]
                                    Fader (mute / gain)
                                    Post-Fader chain
                                    [ tap post-fader sends → 各 dest mix_buffer に加算 ]
                                    if d != master: parent mix_buffer に加算

                                CompressorWorld::process():
                                  外部 sidechain buffer (Send 宛) を envelope detector に入れ
                                  本線信号にゲインリダクションを適用
```

---

## グラフ構造の DAG 化

### 現状 (Phase 1〜3-2)

各バスは `output_bus_dense: u32` を 1 つだけ持ち、ルートはマスターバス。木構造。`process_order` はリーフ→ルートのトポロジカル順 (木の場合は両者が一致する)。

### Phase 3-3 後

各バスは以下を持つ:

- `output_bus_dense: u32` — 本線の出力先 (従来どおり、必須 1 本)
- `send_dest_dense: [u32; MAX_SENDS_PER_BUS]` — Send 宛先 (0〜MAX_SENDS_PER_BUS 本)
- `send_count: u8` — 有効な Send 数

エッジは「本線 1 本 + Send N 本」となり、グラフは **DAG (有向非巡回グラフ)** になる。

#### 処理順序は「入力先行順」

DAG 化に伴い、`process_order` の意味は「リーフ→ルート」から「**入力先行のトポロジカル順**」に変わる。具体的には:

- バス `d` を処理する時点で、`d` への入力 (本線で流れ込む子バス + Send で流れ込むバス) はすべて処理済みである必要がある。
- Compressor sidechain が動作する時点で、sidechain buffer の writer (Send 元バス) の処理が完了している必要がある。

木構造ではこれが「リーフ→ルート」と一致するが、Send が加わると一致しないケースがある。例えば「子バス A → Send → Aux Bus B → Send → Aux Bus C」のような連鎖では A → B → C の順で処理する必要があり、これは「リーフ→ルート」とは別軸の依存。本ドキュメント以下では `process_order` を **DAG トポロジカル順 (入力先行順)** と呼ぶ。

#### サイクル検出

任意の Send 追加時に「`src` から `dst` に既存の経路 (本線 or 他の Send) で到達可能なら、新たな `src → dst` Send は循環を作る」ため拒否する。

```rust
// メインスレッドのバス管理層
fn would_create_cycle(src: u32, dst: u32) -> bool {
    // dst から始めて output_bus + sends を辿り、src に到達できれば循環
    let mut stack = vec![dst];
    let mut visited = bitset![MAX_BUSES];
    while let Some(cur) = stack.pop() {
        if cur == src { return true; }
        if visited.test(cur) { continue; }
        visited.set(cur);
        // 本線
        let parent = bus_view.output_bus_dense(cur);
        if parent != cur { stack.push(parent); }
        // sends
        for &dest in bus_view.send_dests(cur) { stack.push(dest); }
    }
    false
}
```

DFS の最悪計算量は `O(MAX_BUSES + MAX_BUSES * MAX_SENDS_PER_BUS) = O(64 + 256) = O(320)`。Send 操作は cold path なのでコストは無視できる。

#### トポロジカルソート

Send 追加・削除・本線変更のたびにメインスレッドで再ソートし、`UpdateProcessOrder` を送る。Kahn のアルゴリズムを採用 (本線 + Send をすべて入次数として数える)。

```rust
fn topo_sort(bus_view: &BusView) -> Option<Vec<u32>> {
    // in-degree: 各バスへの「入ってくるエッジ数」(本線が来る数 + Send で送られてくる数)
    // 初期キュー: in-degree == 0 のバス (リーフ)
    // pop して order に追加、出て行くエッジを切って in-degree を減らす
    // 全バス処理できなければサイクル (None)
}
```

#### マスターバスの扱い

マスターバスは引き続き `output_bus_dense = 0` (自己参照) で本線を持たない。マスターバスから Send を出すことは禁止 (`add_send(master, _, _)` は拒否)。マスターバスへの Send は許可。

#### 削除時の整合性

- バス despawn 時: 当該バスを `dst` とするすべての Send を一斉削除する。`src` とする Send はバス自体が消えるため自動的に消える。
- Send 個別削除時: `SendId` で resolve し、当該 slot を swap-remove。

---

## データ構造

### BusWorld への追加 SoA

```rust
pub struct BusWorld {
    // ... 既存 ...

    // ── Send (Phase 3-3) ──
    /// 各バスの Send 宛先 (dense index)。固定長配列 + count。
    pub(super) send_dest_dense: Vec<[u32; MAX_SENDS_PER_BUS]>,
    /// 各 Send の gain。`bus * MAX_SENDS_PER_BUS + slot` で索引。
    /// flat にすることで Snapshot 補間 / SIMD 余地を残す。
    pub(super) send_gain: Vec<[f32; MAX_SENDS_PER_BUS]>,
    /// 各 Send のタップ位置 (Pre/Post)。
    pub(super) send_position: Vec<[SendPosition; MAX_SENDS_PER_BUS]>,
    /// 各 Send の SendId (despawn / Set 系のため)。
    pub(super) send_id: Vec<[SendId; MAX_SENDS_PER_BUS]>,
    /// 各バスの有効 Send 数 (0..=MAX_SENDS_PER_BUS)。
    pub(super) send_count: Vec<u8>,
}
```

### `SendId` (型定義)

```rust
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct SendId {
    pub index: u32,
    pub generation: u32,
}
```

`EffectId` と同様、独立した `SparseSet` (`SendAllocator`) で発行する。`SendAllocator` は **メインスレッドが事前発行** し、コマンド経路で `AddSend { id, .. }` の形で運ぶ。サウンドスレッドはこの `id` を `BusWorld.send_id[bus][slot]` に書き込むだけ。

### `SendPosition`

```rust
#[repr(u8)]
pub enum SendPosition { Pre = 0, Post = 1 }
```

- `Pre`: Pre-Fader chain 適用後、Fader (gain/mute) 適用前の信号を tap。**本線 mute / gain 0.0 でも Send は流れる** (sidechain trigger 用途で重要)。
- `Post`: Post-Fader chain 適用後、親バスへの加算直前の信号を tap。**本線 mute なら Send もゼロ** (一般的な Aux Reverb 用途)。

### Compressor (新規エフェクト種別)

```rust
struct CompressorWorld {
    effect_id_at_dense: Vec<EffectId>,
    // パラメータ SoA
    threshold_db: Vec<f32>,
    ratio:        Vec<f32>,
    attack_ms:    Vec<f32>,
    release_ms:   Vec<f32>,
    knee_db:      Vec<f32>,
    makeup_db:    Vec<f32>,
    // 状態 SoA (per channel)
    envelope_l:   Vec<f32>,
    envelope_r:   Vec<f32>,
    // Sidechain 入力バッファ (各 compressor が固有に保持、interleaved stereo)
    /// flat: `compressor_dense * MAX_MIX_BUFFER_SIZE .. + sample_count`
    /// レイアウトは bus mix_buffer と同形式 (interleaved stereo `[L0, R0, L1, R1, ...]`)。
    /// Send が writer (複数 Send が同コンプを指す場合は加算ミックス)、Compressor::process が
    /// reader。callback 内で「writer 全完了 → reader 実行」の順序が DAG トポソートで保証される。
    sidechain_buffer: Vec<f32>,
    /// このコンプが sidechain 入力を使う設定か。false なら自バス内部検波。
    use_sidechain: Vec<bool>,
    /// dirty フラグ (係数キャッシュは持たないので将来用)。
    dirty: Vec<bool>,
}
```

`MAX_COMPRESSORS = 16` (Reverb と同等。1.6 MB の Reverb プールに比べて Compressor は数十 byte/instance + sidechain buffer 32 KB/instance なので合計 ~512 KB)。

#### Compressor パラメータ ID

```rust
#[repr(u8)]
pub enum CompressorParam {
    ThresholdDb = 0,
    Ratio       = 1,
    AttackMs    = 2,
    ReleaseMs   = 3,
    KneeDb      = 4,
    MakeupDb    = 5,
}
```

Sidechain 接続/解除はパラメータではなく独立コマンド (後述 `BindCompressorSidechain`)。

---

## 信号フロー (Phase 3-3 後)

### Source 側 (変化なし)

```
AudioBuffer
  ↓ Pitch
  ↓ Pre-Spatial Effect Chain
  ↓ Spatial
  ↓ Post-Spatial Effect Chain (Phase 2 では空)
  ↓ mix_buffer[output_bus] へ加算
```

### Bus 側 (Send 追加)

```
mix_buffer[d] (全 Source からの加算済み)
  ↓ Pre-Fader Effect Chain
  │
  ├─[tap]→ Pre-Fader Sends:
  │         for s in 0..send_count[d]:
  │           if send_position[d][s] == Pre:
  │             dest = send_dest_dense[d][s]
  │             gain = send_gain[d][s]
  │             mix_buffer[dest * stride ..] += mix_buffer[d * stride ..] * gain
  │             OR sidechain_buffer[comp * stride ..] += ... (Compressor sidechain 宛の場合)
  │
  ↓ Fader: if muted[d] zero-fill else *= gain[d]
  ↓ Post-Fader Effect Chain
  │  └─ Compressor がここに挿さっている場合、sidechain_buffer を読んで GR を適用
  │
  ├─[tap]→ Post-Fader Sends: (Pre と同様だが post-fader 後の信号)
  │
  ↓ if d != master: parent mix_buffer に加算
```

### コールバック内呼び出し順序

```
1. BusWorld::clear_mix_buffers()
2. CompressorWorld::clear_sidechain_buffers()    ← 新規
3. SourceSystem::update(...)                        ← 各 Source を mix_buffer に加算
4. BusSystem::update(...)                            ← 各バスで Pre-chain, Send, Fader, Post-chain, Send, parent 加算
5. master の mix_buffer を output_buffer にコピー
```

`process_order` が DAG トポロジカル順 (入力先行順) なので、`d` を処理する時点で `d` への入力 (本線+Send) はすべて確定済み。Send 出力先のバスはまだ未処理 (後で処理される)。Compressor の sidechain buffer は「先に処理された source bus からの Send で書かれる」ため、Compressor が動く時点で読み出し可能になっている。

### Pre-Fader Send が `mute` を貫通する理由

Pre-Fader Send は **Fader 段の `muted[d]` 判定の前** で tap する。これにより本線が mute されていても sidechain trigger は流れ続ける。

```
mute された Voice バス:
   Pre-chain → [Pre-Send tap, sidechain は流れる] → Fader (zero-fill) → Post-chain → 親加算 (= 0)
```

Unity AudioMixer も同じ挙動 (Pre Fader Send は mute 後でも有効)。

---

## サイクル検出と process_order 更新

### メインスレッドでの実行

`add_send` / `remove_send` / `set_bus_output` (本線変更) のすべてで:

1. **シャドー状態を更新**: メインスレッド側で持つ `BusGraphView` を変更。
2. **サイクル検出**: 全バスから DFS して self-reach があれば即拒否 (コマンドを送らない)。
3. **トポロジカルソート**: Kahn 法で `process_order` を再計算。
4. **コマンド送信**:
   - `AddSend { id, src_dense, dst_dense, position, gain }` または `RemoveSend { id }` または `SetBusOutput { id, output_bus_dense }`
   - 続けて `UpdateProcessOrder { order, len }`

### マスター削除不可ルール

マスターバスを送信元または唯一の入力先とするケースは特別扱いしない。マスターバスは `dst` にはなれるが `src` にはなれない。`output_bus_dense = 0` (自己参照) は本線処理ではスキップされるため、トポロジカルソートでは「入次数を減らさない自己ループ」として扱う必要がある (実装上は `src == dst == 0` のエッジを除外)。

### `MAX_BUSES` 縮退時のフォールバック

トポロジカルソートが失敗 (= サイクル検出漏れ) した場合は **コマンドを送らずメインスレッド状態をロールバック**。サウンドスレッド側は安全。

---

## Sidechain Ducking のセットアップ手順

ユースケース: ナレーションが鳴っている間 BGM をダッキング。

```rust
// 0. 既存のバスとコンプレッサー
let bgm_bus = engine.create_bus(1.0)?;
let voice_bus = engine.create_bus(1.0)?;
let compressor = engine.add_effect(EffectTarget::Bus(bgm_bus), EffectKind::Compressor, EffectPosition::Post)?;

// 1. ナレーションバスから compressor の sidechain 入力に Send (Pre-Fader 推奨)
let send_id = engine.add_send_to_compressor(
    voice_bus,             // src
    compressor,             // dst (sidechain 入力)
    SendPosition::Pre,      // mute されていても trigger は効く
    1.0,                    // send gain
)?;

// 2. compressor を sidechain モードに切替
engine.bind_compressor_sidechain(compressor, send_id)?;

// 3. パラメータ調整
engine.set_effect_param(compressor, CompressorParam::ThresholdDb, -20.0);
engine.set_effect_param(compressor, CompressorParam::Ratio, 4.0);
engine.set_effect_param(compressor, CompressorParam::AttackMs, 5.0);
engine.set_effect_param(compressor, CompressorParam::ReleaseMs, 200.0);
```

### `add_send_to_compressor` と `add_send` の使い分け

| API | 宛先 | 用途 |
|---|---|---|
| `add_send(src, dst_bus, ...)` | バス | Aux Reverb / Delay の集約、ダッキングしない並列ルーティング |
| `add_send_to_compressor(src, comp, ...)` | Compressor の sidechain 入力 | Sidechain ducking |

内部実装では「Send の宛先種別」を `SendDestKind` で分岐:

```rust
#[repr(u8)]
enum SendDestKind { Bus = 0, CompressorSidechain = 1 }
```

`BusWorld.send_dest_dense[d][s]` の意味は `kind` によって変わる (バスの dense index か、CompressorWorld の dense index か)。`send_dest_kind: Vec<[SendDestKind; MAX_SENDS_PER_BUS]>` を SoA に追加して保持する。

---

## Compressor アルゴリズム

サイドチェーン入力 (`use_sidechain == true` なら `sidechain_buffer[comp]`、false なら自バス信号) を検波器に入れ、ゲインリダクションを本線信号に適用する。

```
detector_signal = if use_sidechain { sidechain_buffer[comp] } else { bus_signal };

for n in 0..sample_count {
    // Peak-RMS hybrid: peak detector with one-pole smoothing
    let abs = detector_signal[n].abs();
    let env_target = abs;  // peak
    let coeff = if env_target > envelope { attack_coeff } else { release_coeff };
    envelope = envelope + coeff * (env_target - envelope);

    // Gain reduction (log domain)
    let env_db = 20.0 * envelope.max(1e-7).log10();
    let over = env_db - threshold_db;
    let gr_db = if over <= -knee_db / 2.0 {
        0.0
    } else if over < knee_db / 2.0 {
        // soft knee
        let x = over + knee_db / 2.0;
        -(1.0 - 1.0 / ratio) * x * x / (2.0 * knee_db)
    } else {
        -(1.0 - 1.0 / ratio) * over
    };
    let gain_lin = 10.0_f32.powf((gr_db + makeup_db) / 20.0);

    bus_signal_l[n] *= gain_lin;
    bus_signal_r[n] *= gain_lin;
}
```

`attack_coeff = 1.0 - exp(-1.0 / (attack_ms * 0.001 * sample_rate))` を係数キャッシュ。`dirty` フラグで `attack_ms / release_ms` 変更時のみ再計算。

### ステレオリンク

ステレオ Compressor は `max(|L|, |R|)` を検波器入力に使い、L/R に同じ gain を適用する。これにより L/R 間の像が崩れない (業界標準)。Phase 3-3 はステレオリンクのみ実装。

### 状態の扱い

`envelope_l/r` はサンプル間で持続する。bypass / disable 中は更新せず、再有効化時に `release_coeff` のみで自然減衰する形 (急激な復帰を避ける)。

---

## コマンド追加分

```rust
enum Command {
    // 既存 ...

    // ── Send (Phase 3-3) ──
    AddSend {
        id: SendId,
        src_dense: u32,
        dst_dense: u32,
        dest_kind: SendDestKind,
        position: SendPosition,
        gain: f32,
    },
    RemoveSend { id: SendId },
    SetSendGain { id: SendId, gain: f32 },
    SetSendPosition { id: SendId, position: SendPosition },

    // ── Compressor sidechain ──
    BindCompressorSidechain { effect: EffectId, send: Option<SendId> },
    // None で sidechain 解除 = 自バス内部検波に戻る
}
```

`UpdateProcessOrder` は既存。Send 操作のたびに送る。

### `AddSend` のサイズ

`SendId(8B) + u32 × 2 + u8 × 2 + f32 = 8 + 8 + 2 + 4 = 22 byte`。`UpdateProcessOrder { [u32; 64] + u8 } = 257 byte` より小さく、ringbuf スロット幅に影響しない。

### `BindCompressorSidechain` の意味

`send: Some(id)` で、ある Send (`SendDestKind::CompressorSidechain` 必須) を当該 Compressor の sidechain ソースとして紐付ける。`None` で解除。サウンドスレッドは `CompressorWorld.use_sidechain[comp_dense] = send.is_some()` を更新。

---

## 公開 API (草案)

```rust
impl SoundEngine {
    /// バス → バスの Send を作成。サイクル検出に失敗したら None。
    /// マスターバス src は不可 (None)。
    pub fn add_send(
        &mut self,
        src: EntityId,
        dst: EntityId,
        position: SendPosition,
        gain: f32,
    ) -> Option<SendId>;

    /// バス → Compressor sidechain 入力の Send を作成。
    /// 効果対象が Compressor でない場合は None。
    pub fn add_send_to_compressor(
        &mut self,
        src: EntityId,
        compressor: EffectId,
        position: SendPosition,
        gain: f32,
    ) -> Option<SendId>;

    pub fn remove_send(&mut self, id: SendId) -> bool;
    pub fn set_send_gain(&mut self, id: SendId, gain: f32) -> bool;
    pub fn set_send_position(&mut self, id: SendId, position: SendPosition) -> bool;

    /// Compressor を sidechain モードに切替。`send` の dest が当該 Compressor でなければ false。
    /// `None` で sidechain 解除 (自バス内部検波)。
    pub fn bind_compressor_sidechain(
        &mut self,
        compressor: EffectId,
        send: Option<SendId>,
    ) -> bool;
}
```

- `set_send_gain` は **Snapshot 補間と非競合**: snapshot から書かれる経路 (`write_send_gain_by_dense`) と、コマンドから書かれる経路 (`SetSendGain`) は両方とも同じ `BusWorld.send_gain[bus][slot]` を更新する。最後に書かれた値が勝つ (Phase 3-2 の `set_bus_gain` と同じ整合性ルール)。

---

## Snapshot との連携

snapshot.md `## 後続フェーズへの拡張余地` に「Send ゲインの snapshot 化」が予告されている。Phase 3-3 で実装する。

### Snapshot エントリの追加

```rust
pub struct Snapshot {
    pub bus_gains: Vec<BusGainEntry>,
    pub bus_muted: Vec<BusMutedEntry>,
    pub send_gains: Vec<SendGainEntry>,         // 新規 { send: SendId, gain: f32 }
    pub effect_params: Vec<EffectParamEntry>,    // Compressor も同じエントリで運ぶ
}
```

### `ActiveSnapshot` への追加

```rust
pub struct ActiveSnapshot {
    // ... 既存 ...

    // Send gain (dB 線形補間、bus_gain と同パターン)
    pub send_gain_bus_dense: Vec<u32>,    // resolve 済みの src バス
    pub send_gain_slot: Vec<u8>,           // 当該バス内の send slot
    pub send_gain_from: Vec<f32>,
    pub send_gain_to: Vec<f32>,
}
```

`apply_snapshot` 時に `SendId → (bus_dense, slot)` を resolve してキャッシュ。fade 中に当該 Send が destroy されると別 Send に書き込んでしまうリスクは snapshot.md の bus destroy と同じ未定義動作扱い (現状 Phase 3-3 では非サポート)。

### Snapshot Builder API 追加

```rust
let s = engine.snapshot_builder()
    .set_bus_gain(bgm, 0.3)
    .set_send_gain(reverb_send, 0.6)
    .set_effect_param(compressor, CompressorParam::ThresholdDb, -25.0)
    .commit()?;
```

Compressor パラメータは既存の `set_effect_param` 経路で扱える (`effect_param!` マクロは `EffectKind::Compressor` を追加するだけ)。

---

## DoD 観点

- **Send は owner 側 (BusWorld) SoA**: `send_dest_dense[bus][slot]` は固定長配列で `pre_chain` と同じレイアウト。callback 内で送信処理は `for s in 0..send_count[d]` の単純ループ。
- **Send 0 本のバスはコストゼロ**: `send_count[d] == 0` で全スキップ。Send 未使用バスは Phase 1 と完全に等価な性能。
- **Compressor の sidechain buffer はフラット配列**: `MAX_COMPRESSORS × MAX_MIX_BUFFER_SIZE` を事前確保。Send writer / Compressor reader が同 callback 内で完結し、ロックフリー。
- **DAG ソートは cold path**: Send 操作のたびに走るが、サウンドスレッドでは事前ソート済み `process_order` を順に走査するだけ。
- **サイクル検出はメインスレッド**: サウンドスレッドは検出しない (前提が DAG)。
- **Compressor 係数キャッシュ**: `attack_coeff` / `release_coeff` は `dirty` フラグで 1 度だけ再計算。LPF/HPF と同パターン。

---

## 容量とメモリ見積もり

| 項目 | 初期値 | 備考 |
|---|---|---|
| `MAX_SENDS_PER_BUS` | 4 | Wwise/FMOD 実用例は 1〜3 |
| `MAX_SENDS` | 128 | 全バス合計の send 上限。理論最大 `MAX_BUSES × MAX_SENDS_PER_BUS = 256` の半数。実運用では全バス満杯にはならない |
| `MAX_COMPRESSORS` | 16 | Bus 限定 (Source 対象は API 段階で拒否) |
| BusWorld の Send SoA 追加 | ~1.5 KB | 64 バス × (`[u32;4] + [f32;4] + [u8;4] + [SendId;4]` + u8) |
| CompressorWorld 状態 | ~64 byte/instance | パラメータ + envelope L/R + 係数キャッシュ |
| Compressor sidechain buffer | 32 KB/instance × 16 = 512 KB | フラット f32 配列で事前確保 |
| `AddSend` コマンドサイズ | 22 byte | `UpdateProcessOrder` (257 B) より小 |
| サイクル検出の最悪計算量 | O(MAX_BUSES + MAX_BUSES × MAX_SENDS_PER_BUS) ≈ 320 ops | cold path |

すべて初期化時に一括確保。サウンドスレッドでは alloc・解放・realloc いずれも発生しない。

---

## なぜ Source Send を入れないか

Wwise / FMOD では Source レベルにも Aux Send 設定がある (3D 距離に応じた Reverb Send 量など) が、NEZIA Phase 3-3 では入れない。

**理由**:

1. **parity 用途では不要**: Unity AudioSource には Send 機能が無い。Unity Audio Mixer の Send/Receive はバス間のみ。
2. **DoD への載せ方が複雑**: Source は `MAX_SOURCES = 256` で多数。各 Source に固定長 Send 配列を持たせると `256 × MAX_SENDS_PER_SOURCE × ...` で SoA が膨らむ。Send が 0 のときの最速経路を保つ判定も増える。
3. **代替手段がある**: ゲーム側でカテゴリごとにバスを切れば「環境音バス → Reverb Aux Send」で同等の音響効果が得られる。Source 単位のきめ細かい Send 量制御 (e.g. 距離に応じた wet 量) は Phase 5 の Spatial 連動 LPF / Spatial Aux Send として別途検討。
4. **業界標準 Aux 運用に揃う**: dsp.md と snapshot.md で繰り返し述べられている「Aux Bus + Send で 1 個の Reverb を共有」は Bus → Bus Send で完結する。

将来 Source Send が必要になった時点では本ドキュメントを再評価する。設計上の余地として、`SendId` が `EntityId` ではなく独立 ID 体系であることが活きる。

---

## 制限事項 (Phase 3-3 時点)

### Send 中継先のバスが destroy されるケース

`BusWorld.send_dest_dense[bus][slot]` は dense index でキャッシュ。バス despawn の swap_remove で dense が詰まると Send 先が別バスに切り替わる可能性がある。

**対処**: バス despawn 処理で「当該バスを `dst` または `src` とするすべての Send を削除する + dense 詰めの再マッピング」を BusWorld 側で一括対応。`output_bus_dense` の再マッピングと同パターンを Send にも拡張する。

### Compressor が destroy されたが Send が残るケース

`SendDestKind::CompressorSidechain` の Send が指す Compressor が remove された場合、Send は宛先不在となる。

**対処**: `EffectWorld::despawn` 時に「当該 EffectId を sidechain 宛とするすべての Send を削除」する逆引きを通す。EffectWorld 側に `comp_to_send: HashMap<EffectId, Vec<SendId>>` を持たせる案もあるが、Send 数が少ないので **線形走査で十分** (cold path)。

### マルチコンプ並列駆動

複数の Compressor が同じ trigger バスから Send を受ける構成は許可。各 Compressor が独立した sidechain buffer を持つため衝突しない。

### 単一 Compressor への複数 Send

複数のバスから同じ Compressor の sidechain 入力に Send することも許可。`sidechain_buffer[comp]` に各 Send が **加算ミックス** され、合算信号が検波器に入る (例: Voice + UI 両方が鳴ったらダッキングしたい場合)。

---

## 後続フェーズへの拡張余地

| 拡張 | この設計のどこに乗るか |
|---|---|
| Source Send (Spatial 連動 Aux) | `SendDestKind::Bus` を Source target にも拡張。SourceWorld に send SoA 追加 |
| Lookahead Compressor | Send 経路にディレイラインを挿入する `SendDelay` を追加 (もしくは Compressor 内部で循環バッファ) |
| Multiband Compressor | `EffectKind::MbCompressor` を新規 algo 追加。sidechain 経路は同じ |
| Send の Atomic 駆動 (高頻度オートメーション) | `send_gain` を AtomicU32 化、コマンド経路と並存 |
| Send レイヤー (Bus グループ全体への一括 Send) | Send の `src` を「グループ ID」拡張、ソート時に展開 |
| Reverb Zone (位置依存の Send 量変化) | 上記 Source Send + Spatial 駆動の `set_send_gain` 自動更新で実装 |

---

## 実装フェーズ分割

設計どおり 1 PR で実装すると差分が大きく review しづらいため、Phase 2-3 と同様に分割する。

### PR 1: Send/Receive 基盤

- `BusWorld` への Send SoA 追加 (`send_dest_dense`, `send_gain`, `send_position`, `send_id`, `send_count`)
- `SendId` / `SendAllocator` / `SendPosition` / `SendDestKind = Bus` のみ
- `add_send` / `remove_send` / `set_send_gain` / `set_send_position` API
- メインスレッドの DAG 化シャドー状態 + サイクル検出 + Kahn 法トポロジカルソート
- `Command::AddSend` / `RemoveSend` / `SetSendGain` / `SetSendPosition`
- `BusSystem::update` の Pre-Fader / Post-Fader Send tap
- バス despawn 時の Send 一括削除 + dense 再マッピング
- `tests/send.rs` 結合テスト (Aux Reverb パターン、サイクル検出、削除整合性)

### PR 2: Sidechain Ducking

- `EffectKind::Compressor` 追加 + `CompressorWorld` 実装
- `CompressorParam` enum + `set_effect_param` 経路の拡張
- `SendDestKind::CompressorSidechain` の追加
- `add_send_to_compressor` / `bind_compressor_sidechain` API
- `Command::BindCompressorSidechain`
- Compressor の peak/RMS 検波 + soft knee + attack/release アルゴリズム
- Snapshot への `send_gain` エントリ + `CompressorParam` 補間追加
- `tests/sidechain_ducking.rs` 結合テスト (ナレーション → BGM ダッキングシナリオ)

---

## 関連ドキュメント

- [ロードマップ](../../roadmap/better-than-unity-audio.md) — Phase 3-3 の位置づけ
- [バスルーティング](bus.md) — 木構造の前提を DAG に拡張する形で乗せる
- [DSP パイプライン](dsp.md) — Compressor は本ドキュメントの効果種別追加経路に従う。`gain → effects → (ここに Send 挿入) → 親加算` の隙間を実装で埋める
- [Mixer Snapshot](snapshot.md) — `send_gain` / Compressor パラメータが補間対象に加わる
- [スレッドモデル](threading.md) — サイクル検出/トポソートはメインスレッド、補間/送信処理はサウンドスレッドという責務分担を継承
