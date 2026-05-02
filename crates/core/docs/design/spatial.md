# 3D サウンドシステム設計

## 概要

3D サウンドシステムは、ゲーム世界空間内のサウンドソース位置とリスナー位置・向きに基づいて、距離減衰・パンニング・ドップラー効果をリアルタイムに適用する機能である。

既存の ECS アーキテクチャに **Spatial コンポーネント群**を追加することで実現する。空間演算は `SourceSystem::update()` 内で各ボイスのミックス前に行い、音量とピッチに反映させる。

---

## 機能要件

### 必須機能（MVP）

| ID  | 機能 | 説明 |
|-----|------|------|
| SP-01 | 距離減衰 | ソース・リスナー間距離に応じた音量変化 |
| SP-02 | 3D パンニング | リスナー基準のアジマス角に応じたステレオパン |
| SP-03 | リスナー管理 | 位置・向き・上方ベクトルの設定 |
| SP-04 | 減衰モデル選択 | ソースごとに減衰モデルを選択可能 |
| SP-05 | 有効/無効切替 | ソースごとに空間演算を ON/OFF できる |

### 拡張機能（将来）

| ID  | 機能 | 説明 |
|-----|------|------|
| SP-10 | ドップラー効果 | ソース・リスナーの速度差によるピッチシフト |
| SP-11 | サウンドコーン | 指向性（コーン内外で音量差） |
| SP-12 | オクルージョン | 遮蔽による音量・フィルタ変化 |
| SP-13 | HRTF パンニング | ヘッドフォン向け頭部伝達関数ベースのバイノーラル定位 |
| SP-14 | リバーブゾーン | 空間ベースの残響エフェクト適用 |

---

## 座標系

右手系 Y-up を採用する。

```
        Y (up)
        │
        │
        └──── X (right)
       /
      Z (forward → 手前方向が正)
```

- 単位はゲーム側から自由に定義可能（メートル推奨）
- 内部演算はすべて `f32` 精度で行う

---

## コンポーネント設計

### `SpatialSourceComponent`

ソース 1 体分の空間情報。`SourceWorld` に SoA として追加する。

```rust
// SoA フィールド（SourceWorld に追加）
positions:          Vec<[f32; 3]>,   // ワールド空間の位置
attenuation_models: Vec<AttenuationModel>,
min_distances:      Vec<f32>,        // この距離以内は最大音量（デフォルト: 1.0）
max_distances:      Vec<f32>,        // この距離以上は無音（デフォルト: 500.0）
rolloff_factors:    Vec<f32>,        // 減衰の急峻さ（デフォルト: 1.0）
spatial_enabled:    Vec<bool>,       // false なら空間演算をスキップ（2D ソース）
```

設計方針:
- `spatial_enabled` が `false` のソースは空間演算を完全にスキップし、従来の vol/pitch をそのまま使う。これにより BGM や UI サウンドを 2D のまま扱える。
- 位置はワールド空間のみ管理する。スクリーン空間への変換はゲーム側の責務。

### `ListenerState`

リスナーはゲームセッションにつき 1 体（シングルトン）。`SourceWorld` に直接フィールドとして持つ。

```rust
pub struct ListenerState {
    pub position: [f32; 3],   // ワールド空間の位置
    pub forward:  [f32; 3],   // 正規化済み前方ベクトル（デフォルト: [0,0,-1]）
    pub up:       [f32; 3],   // 正規化済み上方ベクトル（デフォルト: [0,1,0]）
    pub right:    [f32; 3],   // 派生値: normalize(cross(forward, up))。SetListener 受信時に更新。
}
```

マルチリスナー（分割画面など）は将来対応とし、MVP ではサポートしない。

---

## 距離減衰モデル

### `AttenuationModel` 列挙型

```rust
pub enum AttenuationModel {
    /// 減衰なし（音量変化しない）
    None,

    /// 線形減衰
    /// gain = 1 - rolloff * (dist - min) / (max - min)
    /// clamp(gain, 0.0, 1.0)
    Linear,

    /// 逆距離減衰（OpenAL AL_INVERSE_DISTANCE_CLAMPED 相当）
    /// gain = min / (min + rolloff * (dist - min))
    /// dist をまず [min, max] にクランプしてから適用
    InverseDistance,

    /// 指数減衰
    /// gain = (dist / min) ^ (-rolloff)
    /// dist をまず [min, max] にクランプしてから適用
    Exponential,
}
```

### 各モデルの比較

```
gain
 1.0 │\  Exponential(rolloff=2)
     │ \
     │  \ InverseDistance
     │   ──\
     │      ──\ Linear
     │          ──────
 0.0 └────────────────── dist
     min               max
```

ゲームジャンル別推奨:
- **FPS / アクション** → `InverseDistance`（現実に近い）
- **パズル / UI** → `Linear`（直感的）
- **環境音** → `Exponential`（遠方で急激に消える）

---

## ステレオパンニング

### アルゴリズム

リスナー座標系でのアジマス角（水平方向の角度）を求め、イコールパワーパンニングを適用する。

```
1. 差分ベクトル計算
   dir = source.position - listener.position

2. リスナーローカル空間へ変換
   right   = normalize(cross(listener.forward, listener.up))
   forward = listener.forward
   local_x = dot(dir, right)    // 右方向成分
   local_z = dot(dir, forward)  // 前方向成分

3. アジマス角（水平面内）
   azimuth = atan2(local_x, local_z)   // -π 〜 π

4. イコールパワーパン（-π/2 〜 π/2 にクランプ後）
   pan_angle = clamp(azimuth, -π/2, π/2)
   pan        = pan_angle / (π/2)     // -1.0(左) 〜 1.0(右)
   left_gain  = cos((pan + 1) * π/4)  // 右に行くほど減衰
   right_gain = sin((pan + 1) * π/4)  // 右に行くほど増大
```

### 俯角の扱い

MVP では俯角（エレベーション）は無視する。水平面のアジマスのみでパンニングを決定する。HRTF 対応時に 3D 定位へ昇格させる。

### モノソースの扱い

モノ（1ch）ソースの場合:
- ミックス前に `left_gain` / `right_gain` を乗算してステレオに展開する

ステレオ（2ch）ソースの場合:
- ch0（L）に `left_gain`、ch1（R）に `right_gain` を乗算する
- ソース本来のステレオ感は保持される

---

## 処理フロー

### サウンドコールバック内での処理順序

```
SourceSystem::update() 呼び出し時:

for each active source:
  1. if !spatial_enabled → vol をそのまま使用、距離減衰・パン演算スキップ
  2. dist = distance(source.position, listener.position)
  3. dist_gain = compute_attenuation(dist, model, min, max, rolloff)
  4. (azimuth, _elevation) = compute_angles(source.position, listener)
  5. (left_gain, right_gain) = equal_power_pan(azimuth)
  6. effective_left  = source.vol * dist_gain * left_gain
  7. effective_right = source.vol * dist_gain * right_gain
  8. mix into bus_mix_buffer[bus * stride]
     - ch0 += sample * effective_left
     - ch1 += sample * effective_right
```

### 既存 SourceSystem との統合点

現在の `SourceSystem::update()` はすべてのチャンネルに同一の `vol * sample` を書き込んでいる。3D 対応後は:

- `spatial_enabled = false` → 従来通り（変更なし）
- `spatial_enabled = true` → ch ごとに `effective_left` / `effective_right` を使う

---

## コマンド拡張

### 設計原則：更新頻度でコマンドを分離する

ゲーム側 ECS では Transform を持つエンティティを一括イテレートして位置を書き込む。
メインスレッドから `SourceWorld` へ直接書き込めないとしても、**コマンドの粒度がその一括性に合っていないと ECS の強みを失う**。

悪い例：位置と減衰パラメータを 1 コマンドに混ぜる

```rust
// 毎フレーム、動くソースの数だけコマンドを積む
// → model/min/max/rolloff は変化していないのに毎回送ってしまう
for source in moving_sources {
    engine.set_source_spatial(entity, new_pos, model, min, max, rolloff);
}
```

このパターンでは：
- 変化しないパラメータが帯域を無駄に消費する
- コマンドが 1 体 1 件なので N 体動けばリングバッファに N 件積まれる

### コマンド分割の方針

更新頻度でコマンドを分割する：

| コマンド | 頻度 | 内容 |
|---------|------|------|
| `SetSourceSpatialParams` | 低（初期化・変更時のみ） | 減衰モデル・距離範囲・ロールオフ |
| `BatchSetSourcePositions` | 高（毎フレーム） | 複数ソースの位置をまとめて更新 |
| `SetListener` | 高（毎フレーム） | リスナーの位置・向きを更新 |
| `SetSourceSpatialEnabled` | 低（状態変化時のみ） | 空間演算の ON/OFF |

### コマンド定義

```rust
pub enum Command {
    // 既存コマンド（省略）...

    /// 減衰パラメータを設定する（初期化・変更時のみ）。
    /// 毎フレーム送る必要はない。
    SetSourceSpatialParams {
        dense_index:  u32,
        model:        AttenuationModel,
        min_distance: f32,
        max_distance: f32,
        rolloff:      f32,
    },

    /// 複数ソースの位置を一括更新する（毎フレーム用）。
    /// ゲーム側 ECS が Transform を一括イテレートした結果をそのまま詰めて送る。
    /// 1 コマンドで最大 BATCH_SIZE 体分の位置を送ることで、
    /// N 体の移動を ceil(N / BATCH_SIZE) 件のコマンドに圧縮する。
    BatchSetSourcePositions {
        count:   u8,
        updates: [(u32, [f32; 3]); SPATIAL_BATCH_SIZE],  // (dense_index, position)
    },

    /// ソースの空間演算 ON/OFF（状態変化時のみ）。
    SetSourceSpatialEnabled {
        dense_index: u32,
        enabled:     bool,
    },

    /// リスナーの状態を更新する（毎フレーム）。
    /// forward, up は正規化済みであること。
    SetListener {
        position: [f32; 3],
        forward:  [f32; 3],
        up:       [f32; 3],
    },
}
```

`SPATIAL_BATCH_SIZE` はコマンドの固定サイズ制約（`Copy` 要件）と圧縮効率のバランスから決める。
`32` を基準とし、プロファイル結果に応じて調整する。

```rust
pub const SPATIAL_BATCH_SIZE: usize = 32;
```

### 典型的な毎フレーム処理

```rust
// ゲーム側 ECS システム
fn sync_audio_positions(world: &GameWorld, engine: &SoundEngine) {
    // Transform + AudioSource を持つエンティティを密配列で一括イテレート（ECS の強み）
    let mut batch = SpatialPositionBatch::new();
    for (transform, audio_source) in world.query::<(&Transform, &AudioSource)>() {
        batch.push(audio_source.dense_index, transform.position);
        if batch.is_full() {
            engine.flush_positions(&mut batch);
        }
    }
    engine.flush_positions(&mut batch);  // 端数を送信

    // リスナー（カメラ）は 1 件だけ
    engine.set_listener(camera.position, camera.forward, camera.up);
}
```

`SpatialPositionBatch` はゲーム側が `BatchSetSourcePositions` を組み立てるためのヘルパー。
NEZIA 側で提供する。

---

## SourceWorld の変更点

```rust
pub struct SourceWorld {
    // 既存フィールド
    pub sparse:          Vec<u32>,
    pub dense_to_sparse: Vec<u32>,
    pub generations:     Vec<u32>,
    pub vol:             Vec<f32>,
    pub pitch:           Vec<f32>,
    pub sample_offset:   Vec<usize>,
    pub audio_buffer_index: Vec<usize>,
    pub state:           Vec<SourceState>,
    pub output_bus:      Vec<u32>,

    // 追加フィールド（3D 空間）— 完全 SoA で SIMD 対応
    pub positions_x:        Vec<f32>,
    pub positions_y:        Vec<f32>,
    pub positions_z:        Vec<f32>,
    pub attenuation_models: Vec<AttenuationModel>,
    pub min_distances:      Vec<f32>,
    pub max_distances:      Vec<f32>,
    pub rolloff_factors:    Vec<f32>,
    pub spatial_enabled:    Vec<bool>,

    // リスナー（シングルトン）
    pub listener:           ListenerState,
}
```

スポーン時のデフォルト値:

| フィールド | デフォルト |
|-----------|-----------|
| `position` | `[0.0, 0.0, 0.0]` |
| `attenuation_model` | `AttenuationModel::InverseDistance` |
| `min_distance` | `1.0` |
| `max_distance` | `500.0` |
| `rolloff_factor` | `1.0` |
| `spatial_enabled` | `false`（2D デフォルト） |

`spatial_enabled` をデフォルト `false` にすることで、既存の 2D ソースへの後方互換性を維持する。

---

## SoundEngine API 拡張

```rust
impl SoundEngine {
    /// ソースに空間情報を設定する。
    /// play() / play_to_bus() 後に呼び出すことで 3D ソースとして動作させる。
    pub fn set_source_spatial(
        &self,
        entity: EntityId,
        position: [f32; 3],
        model: AttenuationModel,
        min_distance: f32,
        max_distance: f32,
        rolloff: f32,
    ) -> Result<(), SoundEngineError>;

    /// ソースの空間演算を有効化・無効化する。
    pub fn set_source_spatial_enabled(
        &self,
        entity: EntityId,
        enabled: bool,
    ) -> Result<(), SoundEngineError>;

    /// リスナーの状態を更新する。毎フレーム呼び出すことを想定。
    /// forward, up は内部で正規化される。
    pub fn set_listener(
        &self,
        position: [f32; 3],
        forward: [f32; 3],
        up: [f32; 3],
    );
}
```

典型的な使用パターン:

```rust
// 初期化時
engine.set_listener([0.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]);

// 爆発音（3D）を再生
let id = engine.play_to_bus(buffer_id, sfx_bus)?;
engine.set_source_spatial(id, [10.0, 0.0, -5.0],
    AttenuationModel::InverseDistance, 1.0, 100.0, 1.0)?;
engine.set_source_spatial_enabled(id, true)?;

// BGM（2D）はそのまま
let bgm = engine.play_to_bus(bgm_buffer, bgm_bus)?;
// spatial_enabled = false がデフォルトなので追加呼び出し不要

// ゲームループ内（毎フレーム）
engine.set_source_spatial(id, player.position, ...)?;
engine.set_listener(camera.position, camera.forward, camera.up);
```

---

## パフォーマンス特性

### 演算コスト

| 演算 | ソースあたりのコスト | 備考 |
|------|-------------------|------|
| 距離計算 | `sqrt` × 1 | SIMD 化可能 |
| ローカル変換 | `dot` × 2 | |
| atan2 | 高コスト | 近似式で代替可能 |
| 減衰モデル | 除算 or 乗算 × 数回 | |
| パン計算 | `cos/sin` × 2 | ルックアップテーブル化可能 |

### 最適化方針

1. `spatial_enabled = false` のソースはすべての空間演算をスキップ（ブランチプレディクタフレンドリーな早期 return）
2. `atan2` は近似式（最大誤差 0.005 ラジアン）で代替可能
3. `cos/sin` はプリコンピュートテーブル（512 エントリ）で代替可能
4. 将来的には SIMD（`std::simd` / `wide` クレート）で複数ソースを並列演算

### 既知のパフォーマンス上の懸念

#### (A) `right` ベクトルの再計算

パンニング計算では毎ソースに `right = normalize(cross(forward, up))` を使うが、これはリスナーが変わらない限り定数値である。現在の設計のままではホットループ内で N ソース分 N 回計算してしまう。

**対策**: `ListenerState` に `right: [f32; 3]` を派生フィールドとして持たせ、`SetListener` コマンド受信時に一度だけ計算・キャッシュする。

```rust
pub struct ListenerState {
    pub position: [f32; 3],
    pub forward:  [f32; 3],
    pub up:       [f32; 3],
    pub right:    [f32; 3],  // forward と up から派生。SetListener 受信時に更新。
}
```

#### (B) `Vec<[f32; 3]>` は SIMD 非友好 → MVP から完全 SoA で設計する

`positions: Vec<[f32; 3]>` は要素が 12 バイトの AoS 的レイアウトになっている。SIMD レジスタ（128/256 bit）に x/y/z をそろえて読み込もうとすると stride 付きギャザーが必要になり、逆に遅くなる。

**MVP から完全 SoA で設計する。** 後から分割するとインターフェース全体に影響が及ぶため、最初から正しいレイアウトを採用する:

```rust
// NG: AoS 的で SIMD 非友好
pub positions: Vec<[f32; 3]>,

// OK: 完全 SoA
pub positions_x: Vec<f32>,
pub positions_y: Vec<f32>,
pub positions_z: Vec<f32>,
```

同様にドップラー対応時の速度フィールドも最初から SoA にする:

```rust
pub velocities_x: Vec<f32>,
pub velocities_y: Vec<f32>,
pub velocities_z: Vec<f32>,
```

SIMD 処理の例（`wide` クレートを使用した場合）:

```rust
// 8 ソース分の距離を一括計算
let dx = f32x8::from_slice(&positions_x[i..]) - f32x8::splat(listener.position[0]);
let dy = f32x8::from_slice(&positions_y[i..]) - f32x8::splat(listener.position[1]);
let dz = f32x8::from_slice(&positions_z[i..]) - f32x8::splat(listener.position[2]);
let dist_sq = dx * dx + dy * dy + dz * dz;
let dist    = dist_sq.sqrt();
```

#### (C) `AttenuationModel` enum によるブランチ予測の乱れ

ホットループ内で `match model { Linear => ..., InverseDistance => ..., ... }` を実行すると、ソースごとにモデルが混在している場合にブランチ予測ミスが頻発する。

**対策案**: モデル別にソースをグループ化して処理する。`spatial_enabled` と同様のタグ分離を `AttenuationModel` に適用し、同一モデルのソースを連続したスライスで処理すれば、各スライスのループ内でブランチが消える。

```
dense 配列内の並び（理想）:
[ spatial_enabled=false ... ][ Linear ... ][ InverseDistance ... ][ Exponential ... ]
 ← ループ1: 無演算スキップ →← ループ2 →←     ループ3          →← ループ4        →
```

ただしこのグループ維持にはスポーン・デスパウン時のソートコストが発生するため、ソース数が多い場合にのみ効果が出る。プロファイル結果を見て判断する。

#### (D) リングバッファ容量の圧迫

`BatchSetSourcePositions`（`SPATIAL_BATCH_SIZE=32`）は約 400 バイトのコマンドになる。現行の容量 128 のリングバッファを使うと、数十体が動くだけでフレーム内に複数コマンドが積まれ、容量が逼迫する。

**対策**: 空間位置更新専用の第 2 リングバッファを設けるか、容量を見直す。専用バッファにする場合、サウンドコールバック冒頭で既存コマンドバッファとは別にドレインする。

### メモリ使用量

`MAX_SOURCES = 512` の場合、追加メモリ:

```
positions_x/y/z:     512 × 4B × 3 = 6 KB（レイアウト変更なし、SoA 分割のみ）
attenuation_models:  512 ×  1B     = 0.5 KB
min/max_distances:   512 ×  8B     = 4 KB
rolloff_factors:     512 ×  4B     = 2 KB
spatial_enabled:     512 ×  1B     = 0.5 KB
ListenerState:                      48B（right フィールド追加後）
合計: 約 13 KB
```

---

## スレッド安全性

既存のスレッドモデルを踏襲する。

- **メインスレッド**: `set_listener()` / `set_source_spatial()` を呼び出してコマンドをリングバッファに積む
- **サウンドスレッド**: コールバック冒頭でコマンドをドレインして `SourceWorld` を更新し、直後の `SourceSystem::update()` で参照する

`ListenerState` と SoA フィールドはすべてサウンドスレッド専有であり、ロック不要。

---

## ドップラー効果（SP-10）

> **ステータス: 将来対応**

### 基本式

```
f_observed = f_source × (v_sound + v_listener) / (v_sound + v_source)
```

- `v_sound` = 340.0 m/s（空気中の音速、設定可能）
- `v_listener` = リスナーの接近速度（正 = 近づく）
- `v_source` = ソースの接近速度（正 = 近づく）

### 追加コンポーネント

```rust
// SourceWorld に追加
velocities: Vec<[f32; 3]>,

// ListenerState に追加
velocity: [f32; 3],
```

### ピッチへの反映

```rust
let doppler_pitch = compute_doppler(
    source_velocity, listener_velocity,
    source_position, listener_position,
    sound_speed
);
effective_pitch = source.pitch * doppler_pitch;
```

---

## サウンドコーン（SP-11）

> **ステータス: 将来対応**

ソースに向き（`direction: [f32; 3]`）と内角・外角（`inner_angle`, `outer_angle`）を持たせ、リスナーがコーン外にいる場合は音量を減衰させる。

```
                  inner_angle
                ╱─────╲
  outer_angle ╱─────────╲
             ╱  Source →  ╲
             ╲             ╱
              ╲───────────╱
               ╲─────────╱
```

- コーン内（`< inner_angle / 2`）: 減衰なし
- コーン外（`> outer_angle / 2`）: `outer_gain` まで減衰
- 中間: 線形補間

---

## 実装順序

```
Phase 1 (MVP)
  └─ SP-01 距離減衰
  └─ SP-02 3D パンニング（ステレオ・イコールパワー）
  └─ SP-03 リスナー管理
  └─ SP-04 減衰モデル選択
  └─ SP-05 有効/無効切替

Phase 2
  └─ SP-10 ドップラー効果
  └─ SP-11 サウンドコーン

Phase 3
  └─ SP-12 オクルージョン
  └─ SP-13 HRTF（binaural）
  └─ SP-14 リバーブゾーン
```
