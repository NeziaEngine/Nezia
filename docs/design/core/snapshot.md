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
| from 値の決定 | **apply 時点の現在値をキャプチャ** | snapshot に from を埋め込む | from を埋めると「snapshot A から snapshot B へ」のような遷移しかできず、任意状態からの apply ができない |
| 中断時の挙動 | **interrupt-and-restart**: 進行中 fade を破棄、現在値を新 from にして再開 | キューイング (順次実行) | クロスフェード中の再切替が自然 (BGM が「Normal → Battle 中の途中で Boss」のようなケース) |
| bool パラメータ (muted) | **`t >= 0.5` で snap** | 補間しない / 0% で即適用 | 補間できないので最も自然な「中点でスイッチ」を採用 |
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
                                        ├─ bool: snap at t >= 0.5
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

    // バスミュート (bool は補間ではなく t>=0.5 で snap)
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

### 線形補間

```rust
for i in 0..bus_gain_dense.len() {
    let v = bus_gain_from[i] + (bus_gain_to[i] - bus_gain_from[i]) * t;
    bus_world.write_gain_by_dense(bus_gain_dense[i] as usize, v);
}
```

エフェクトパラメータも同様。LPF / HPF の `set_cutoff` は dirty フラグを立てるため、次の `flush_dirty` で biquad 係数が再計算される (5ms ごとの fade 更新で毎回 recalc されるが、計算は数 op で軽量)。

### bool 補間

```rust
if !bus_muted_applied[i] && t >= 0.5 {
    bus_world.write_muted_by_dense(dense, bus_muted_to[i]);
    bus_muted_applied[i] = true;
}
```

fade 完了時 (`fade_remaining_samples == 0`) は未適用エントリも全て確実に書き込んで `clear()`。

---

## DoD 観点

- **`ActiveSnapshot` は SoA**: 各エントリ種別ごとに `Vec<...>` を並列保持し、callback ごとに dense ループで lerp。L1 親和的。
- **エントリが空のときコストゼロ**: `is_active()` が false なら `tick_snapshot_interpolation` を呼ばない。snapshot 未使用時の overhead は `is_active()` 1 命令のみ。
- **apply は cold path**: ID resolve + キャプチャは apply 時に 1 度だけ。callback ごとの hot path は dense ループのみ。
- **ロックフリー**: `Arc<ArcSwap<...>>` の load は lock-free atomic (1 命令)。registry destroy は ActiveSnapshot に影響しない。

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
