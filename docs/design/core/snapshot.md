# Mixer Snapshot

NEZIA ENGINE の Mixer Snapshot 機能設計。Unity AudioMixer の Snapshot 互換 + 補間。
本ドキュメントは [ロードマップ](../../roadmap/better-than-unity-audio.md) の
**Phase 3-2「Mixer Snapshot + 補間」** に対応する設計を扱う。

---

## スコープ

### このフェーズで扱う
- **宣言的ビルダー** で Snapshot を構築する API (`engine.snapshot_builder().set_bus_gain(...).commit()`)。
- 含めるパラメータ: **バスゲイン / バスミュート / エフェクトパラメータ** (LPF cutoff/Q、HPF cutoff/Q、Reverb 5 パラメータ)。
- `apply_snapshot(id, fade_seconds)` で線形補間によるクロスフェード。`fade_seconds = 0` で即時適用。
- 進行中の補間を中断して新規 apply をトリガできる (現在値が新しい from になる)。

### このフェーズでは扱わない
- **マルチ snapshot のレイヤー合成** (Unity の TransitionToSnapshots 複数同時 weight)。Phase 3-2 では「現在値 → 単一 to」の単純モデル。
- **Source 個別パラメータ・3D 位置・リスナー姿勢** の snapshot 化。これらは「ミキサー設定」ではなく毎フレーム動的状態のため、思想的に分離する。
- **AttenuationCurve / AudioBuffer の差し替え**。アセット参照の保存と切替は別機能 (将来の Asset Bundle 機能で扱う)。
- **イージングカーブ** (linear 以外の補間カーブ)。Phase 3-2 は線形補間のみ。

---

## 設計判断サマリ

| 判断点 | 採用 | 不採用 | 理由 |
|---|---|---|---|
| API 形 | **宣言的ビルダー** (`set_bus_gain` などをチェーン) | キャプチャ型 (`engine.capture_current_state()`) | キャプチャは audio thread 状態の読み出しが必要で複雑化。宣言的なら main thread だけで完結 |
| Snapshot のオーナーシップ | **共有レジストリ** (`SnapshotRegistry`) | 各 apply で値を Command にコピー | Command enum は固定サイズ Copy 制約。Snapshot は可変長エントリを持つので Arc 経由で共有 |
| サウンドスレッドへの配信 | **`Arc<ArcSwap<Vec<Option<Arc<Snapshot>>>>>`** | triple buffer | apply 時に 1 度だけ load する稀パスのため triple buffer のメリットなし。`AudioBufferPool` / `CurveRegistry` と同パターンで揃える |
| 補間タイミング | **サウンドスレッド per-callback で lerp** | メインスレッドで `set_bus_gain` を毎フレーム呼ぶ | リアルタイム精度 (sample 単位)、メインがフレームレートを変動させても影響なし、コマンドキュー圧迫を回避 |
| 補間状態の表現 | **`ActiveSnapshot` (SoA `Vec<...>`)** をサウンドスレッドが所有 | snapshot data 直接補間 | apply 時に ID resolve + from キャプチャ を 1 回行い、以降は dense 配列の lerp ループ。DoD 整合 |
| バスゲインの補間空間 | **dB 空間で線形補間** (`lerp_db_gain`) | linear 空間で線形補間 | 人間の聴覚は対数。Unity AudioMixer も同じ。線形補間は「終盤で急減」と感じる |
| エフェクトパラメータの補間空間 | **線形** (cutoff Hz / Q / wet / dry など) | dB 空間 | UI スライダーで弄る値であり、線形が直感的。LPF cutoff の対数感は将来 ParamEQ で別途検討 |
| from 値の決定 | **apply 時点の現在値をキャプチャ** | snapshot に from を埋め込む | from を埋めると「snapshot A から snapshot B へ」のような遷移しかできず、任意状態からの apply ができない |
| 中断時の挙動 | **interrupt-and-restart**: 進行中 fade を破棄、現在値を新 from にして再開 | キューイング (順次実行) | クロスフェード中の再切替が自然 (BGM が「Normal → Battle 中の途中で Boss」のようなケース) |
| bool パラメータ (muted) | **fade 完了時 (`fade_remaining == 0`) に snap** | 中点 (`t >= 0.5`) snap / 開始時 snap | gain など f32 パラメータの最終状態と timing が一致。Unity AudioMixer も同じ仕様。「フェードアウトしてからミュート」は同 snapshot に `set_bus_gain(0.0)` を併記することで実現できる |
| エフェクトパラメータ参照 | **dense index を resolve してキャッシュ** | 毎 callback で `EffectId` を resolve | apply 時 1 度きり。fade 中に effect が destroy されたら値は書けないが panic はしない (write_*_by_dense は範囲外無視) |
| destroy 済み snapshot の apply | **`false` を返す** (no-op) | エラー | `play_with_handle` などの既存 API と一貫 |
| 進行中 fade と destroy | **`destroy_snapshot` は registry slot 解放のみ、ActiveSnapshot 影響なし** | apply 中の Snapshot の destroy 禁止 | apply 時に値を `ActiveSnapshot` に複製済みなので registry が消えても安全 |

---

## アーキテクチャ全体図

```
[main thread]                                    [sound thread]
SnapshotBuilder
  .set_bus_gain(bgm, 0.3)
  .set_effect_param(lpf, Cutoff, 500.0)
  .commit() ─→ SnapshotRegistry.create()
                ↓
                Arc<Snapshot> stored in shared snapshot
                ↓
                Arc<ArcSwap<Vec<Option<Arc<Snapshot>>>>>
                                                  ↑
apply_snapshot(id, 2.0)                           │
   ↓                                              │
   Command::ApplySnapshot { index, fade_samples } │
   ↓                                              │
   ringbuf push                                   │
                                                  │
                                    AudioThread::process()
                                    ├─ Command::ApplySnapshot intercept
                                    │   ↓
                                    │   apply_snapshot()
                                    │   ├─ load shared snapshot (Arc<ArcSwap>)
                                    │   ├─ resolve all bus/effect IDs to dense
                                    │   ├─ capture current values as `from`
                                    │   └─ write to ActiveSnapshot (SoA)
                                    │
                                    └─ tick_snapshot_interpolation(samples)
                                        ├─ t = (consumed + samples) / total
                                        ├─ for each entry: lerp(from, to, t)
                                        ├─ write to BusWorld / Lpf/Hpf/ReverbWorld
                                        ├─ bool: snap only at fade completion
                                        └─ on completion: clear ActiveSnapshot
```

---

## データ構造

### `Snapshot` (不変、registry 内)

```rust
pub struct Snapshot {
    pub bus_gains: Vec<BusGainEntry>,        // { bus: EntityId, gain: f32 }
    pub bus_muted: Vec<BusMutedEntry>,       // { bus: EntityId, muted: bool }
    pub effect_params: Vec<EffectParamEntry>, // { effect, kind, param: u8, value: f32 }
}
```

ビルダー API はチェーン形:

```rust
let s = engine.snapshot_builder()
    .set_bus_gain(bgm_bus, 0.3)
    .set_bus_gain(sfx_bus, 1.0)
    .set_bus_muted(voice_bus, false)
    .set_effect_param(lpf_id, LpfParam::Cutoff, 500.0)
    .set_effect_param(reverb_id, ReverbParam::Wet, 0.5)
    .commit()?;
```

同一 (bus, kind, param) の重複指定は **後勝ち**。

### `ActiveSnapshot` (ミュータブル、サウンドスレッド所有)

apply 時に `Snapshot` を resolve + キャプチャして展開。SoA レイアウト:

```rust
pub struct ActiveSnapshot {
    // バスゲイン
    pub bus_gain_dense: Vec<u32>,     // resolve 済み dense
    pub bus_gain_from: Vec<f32>,      // 適用時の現在値
    pub bus_gain_to: Vec<f32>,        // ターゲット値

    // バスミュート (bool は補間ではなく fade 完了時に snap)
    pub bus_muted_dense: Vec<u32>,
    pub bus_muted_to: Vec<bool>,
    pub bus_muted_applied: Vec<bool>, // 二重書き防止

    // エフェクトパラメータ
    pub effect_kind: Vec<SnapshotEffectKind>,
    pub effect_state_dense: Vec<u32>,
    pub effect_param: Vec<u8>,
    pub effect_from: Vec<f32>,
    pub effect_to: Vec<f32>,

    // fade 進行
    pub fade_total_samples: u64,
    pub fade_remaining_samples: u64,
}
```

`SnapshotRegistry` から該当 `Snapshot` を 1 度だけ load し、ID を dense へ resolve した状態で持つので、以降の callback では registry を触らない。registry の destroy は ActiveSnapshot に影響しない。

### `SnapshotRegistry`

`AudioBufferPool` / `CurveRegistry` と同パターン:

- `slots: Vec<SnapshotSlot>` (generation + occupied)
- `snapshots: Vec<Option<Arc<Snapshot>>>`
- `shared: Arc<ArcSwap<Vec<Option<Arc<Snapshot>>>>>`
- `MAX_SNAPSHOTS = 64`

---

## 補間アルゴリズム

### 進行率の計算

```rust
let consumed = fade_total_samples - fade_remaining_samples;
let next_consumed = (consumed + samples_this_callback).min(fade_total_samples);
let t = next_consumed as f32 / fade_total_samples as f32;  // [0.0, 1.0]
fade_remaining_samples -= samples_this_callback;
```

`fade_total_samples = 0` の場合は `t = 1.0` で即時適用。

### バスゲインは dB 空間で補間 (Phase 3-2)

線形ゲイン空間で `lerp(from, to, t)` を行うと、人間の聴覚が対数特性のため
**「序盤ほぼ変化なし → 終盤で急減」** と感じてしまう。Unity AudioMixer や
業界標準のミキサーは Volume パラメータを **dB 空間で線形補間** する。NEZIA も同じ。

```rust
fn lerp_db_gain(from: f32, to: f32, t: f32) -> f32 {
    if t >= 1.0 { return to; }   // 端点は exact (snap)
    if t <= 0.0 { return from; }
    if from == to { return from; }
    const MIN_LIN: f32 = 1e-5;   // -100 dB floor (聴感上 silence と区別不能)
    let from_db = 20.0 * from.max(MIN_LIN).log10();
    let to_db   = 20.0 * to.max(MIN_LIN).log10();
    let v_db = from_db + (to_db - from_db) * t;
    10.0_f32.powf(v_db / 20.0)
}
```

- 端点 (`t = 0` / `t = 1`) では **正確に from / to を返す**。`from = 1.0, to = 0.0` の
  fade が完了したとき gain は厳密に 0 になる (フロアの `MIN_LIN` には張り付かない)。
- 0 = -∞ dB は数値的に扱えないので `MIN_LIN = 1e-5` (= -100 dB) でフロアする。
  -100 dB は人間の聴覚閾値より低く、聴感上 silence と区別不能。
- コスト: `log10` + `powf` 各 1 回 / エントリ / callback。dense ループ内の数 op で軽量。

### エフェクトパラメータは線形補間

LPF cutoff (Hz) / Q / Reverb の各パラメータ (room_size / damping / wet / dry / width) は
**線形補間**。これらは UI スライダーで直接弄る値であり、線形が直感的。LPF / HPF の
`set_cutoff` は dirty フラグを立てるため、次の `flush_dirty` で biquad 係数が再計算
される (callback ごとに recalc されるが計算は数 op で軽量)。

### bool 補間 (= 完了時 snap)

`muted` のような bool パラメータは補間できないため、**fade 完了時にのみ書き込む**。
`tick_snapshot_interpolation` 内では何もせず、`fade_remaining_samples == 0` のブロックで
未適用エントリを一括適用してから `clear()` する。

```rust
if active.fade_remaining_samples == 0 {
    for i in 0..bus_muted_dense.len() {
        if !bus_muted_applied[i] {
            bus_world.write_muted_by_dense(bus_muted_dense[i], bus_muted_to[i]);
            bus_muted_applied[i] = true;
        }
    }
    active.clear();
}
```

**「フェードアウトしてからミュート」を実現するには**、同じ snapshot に `set_bus_gain(0.0)` を
併記する。gain は線形補間で 1.0 → 0.0 に下がり、fade 完了時に muted フラグが立つ。
gain の時間変化と muted の bool 変化が **完了 timing で揃う** ため、プチノイズが乗らない。

---

## DoD 観点

- **`ActiveSnapshot` は SoA**: 各エントリ種別ごとに `Vec<...>` を並列保持し、callback ごとに dense ループで lerp。L1 親和的。
- **エントリが空のときコストゼロ**: `is_active()` が false なら `tick_snapshot_interpolation` を呼ばない。snapshot 未使用時の overhead は `is_active()` 1 命令のみ。
- **apply は cold path**: ID resolve + キャプチャは apply 時に 1 度だけ。callback ごとの hot path は dense ループのみ。
- **ロックフリー**: `Arc<ArcSwap<...>>` の load は lock-free atomic (1 命令)。registry destroy は ActiveSnapshot に影響しない。

---

## 制限事項 (Phase 3-2 時点)

### Bus / Effect が fade 中に destroy されるケース

`ActiveSnapshot` は apply 時に **dense index をキャプチャ** して以降 re-resolve しない。
fade 中に bus / effect が destroy されると swap_remove で dense が詰まり、当該エントリ
の書き込み先が **別の bus / effect になる** 可能性がある。

```text
apply 時:    bus A (dense=5) → ActiveSnapshot.bus_gain_dense=5
fade 中:     bus A destroy → bus C (last) が dense=5 に移動
次の tick:   write_gain_by_dense(5, ..) → bus C を誤上書き
```

**現状の対処**: なし (fade 中の bus / effect destroy は非サポート、未定義動作)。
業界標準 (Unity / FMOD など) も transition 中の bus 削除を保証しない。実害が見えてから
EntityId / EffectId キャッシュ + 毎 callback re-resolve を導入する。

回避策:
- 本番運用では bus / effect の destroy は scene 切替などの非リアルタイム境界で行う
- fade の終了を `engine.poll_events()` 経由で確認してから destroy する

---

## 後続フェーズへの拡張余地

| 拡張 | この設計のどこに乗るか |
|---|---|
| イージングカーブ (ease-in/out など) | `ActiveSnapshot` に `easing: u8` フィールドを追加し、`t` を curve 通したものに置換 |
| マルチ snapshot レイヤー合成 | `ActiveSnapshot` を `Vec<ActiveSnapshot>` 化し、各エントリに weight を持たせる |
| Source 個別パラメータの snapshot | 別エントリ種別を追加 (現状の bus_gain と同パターン) |
| Send / Receive ゲインの snapshot 化 | Phase 3-3 で Send が入った後、`bus_send_gain` エントリを追加 |
| パラメータ exposure (動的バインディング) | Builder で直接値を渡す代わりに `ParamRef` を取り、apply 時に解決する間接層を追加 |

---

## 関連ドキュメント

- [バスルーティング](bus.md) — `BusWorld` の構造、`write_gain_by_dense` / `write_muted_by_dense` の使用箇所
- [DSP パイプライン](dsp.md) — Effect の dirty フラグ / 係数再計算
- [スレッドモデル](threading.md) — Command 経路の使い分け
- [ロードマップ](../../roadmap/better-than-unity-audio.md) — Phase 3-2 の位置づけ
