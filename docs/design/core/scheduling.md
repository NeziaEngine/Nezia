# PlayScheduled (サンプル精度の予約再生)

NEZIA ENGINE の予約再生機能設計。Unity `AudioSource.PlayScheduled` 互換 +
**サブコールバック (sample 単位) 精度** での開始タイミング制御。
本ドキュメントは [ロードマップ](../../roadmap/better-than-unity-audio.md) の
**Phase 3-4「PlayScheduled (サンプル精度の予約再生)」** に対応する設計を扱う。

---

## スコープ

### このフェーズで扱う
- `play_scheduled_in(buffer, delay_seconds, ...)` を **主 API** とした予約再生。
  内部的には絶対 DSP frame (u64) で表現し、低レベル `_at_frame(...)` バリアントも併設する。
- **callback 内サブフレーム精度**: `start_dsp_frame` が callback 区間の途中にあるとき、
  その frame からミキシングを開始する (前半は無加算)。
- 予約済みソースに対する `stop_source` (キャンセル) と Voice Virtualization での扱い。
- 予約時刻が**既に過去**の場合は **silent fallback で即時再生** (offset = 0)。Unity 同等。
- 既存の Pause / Resume / Seek / ループ / 3D / DSP チェーンとの直交性確認。

### このフェーズでは扱わない
- **ホストクロックや DAW テンポへの同期** (BPM 同期、PPQ)。Phase 6 の
  プロジェクトファイル方式 + 音楽演出機能で扱う。
- **再スケジューリング API** (`SetScheduledStartTime`)。Unity の `SetScheduledStartTime`
  相当。実装コストの割に使用頻度が低いと判断、後続フェーズで必要になれば追加。
- **コールバック付き予約再生のフル網羅**。`play_with_handle_scheduled_*_and_callback`
  のみ提供し、fire-and-forget + callback の組合せは Phase 3-4 では省く
  (callback 必須ケースは handle 経由で書ける)。
- **Random / Switch / Sequence Container との連動**。Container (Phase 4-2) が本機能を
  内部で使う想定だが、それは Container 側の責務として後で組む。本ドキュメントでは
  「サンプル精度のスポーン経路を提供する」までを保証する。
- **CPU 高負荷時の Scheduled drift 補正**。callback 落ち (underrun) で DSP clock が
  実時間より遅延した場合、予約は DSP clock 基準で動くため自動的にずれる。
  これは正しい挙動 (ユーザの「DSP 時刻 t に鳴らす」要求は守られる) として確定する。

---

## 設計判断サマリ

| 判断点 | 採用 | 不採用 | 理由 |
|---|---|---|---|
| 主 API 形 | **`play_scheduled_in(buffer, delay_seconds, ...)`** + 低レベル `_at_frame(u64)` バリアント | 既存 `play()` への optional 引数追加 / builder API | 「N ms 後に鳴らす」が支配的ユースケース。秒指定が一行で書け、絶対 frame は内部表現と一致して精度劣化なし。`play()` 改変は呼出側全置換が必要 |
| 時刻の内部表現 | **絶対 DSP frame (u64) — `dsp_time_frames` と同一クロック** | 相対 (callback 投入時に sound thread で `now + delta`) / f64 秒 | コマンド経路の遅延・ジッタを吸収しても精度が保たれる。 `dsp_time_samples()` と直接演算可。f64 秒は `2^53 / 48kHz ≈ 5900 年` で精度十分だが内部 u64 と相互変換コストを毎度払う |
| 過去時刻指定 | **silent fallback で即時再生** (offset = 0、Playing) | 失敗 (`PlayFailed`) / panic | コマンドキュー往復遅延 (~ms) でユーザの想定通り組んだ予約も「過去」になり得る。失敗扱いはリズムゲームで致命傷 |
| 状態表現 | **新 `SourceState::Scheduled` を追加** | `Playing` のまま `start_offset > 0` で表現 | mixing system / virtualizer / lifecycle の各段で「無音区間にいるか」の判定を SoA 列 1 本で済ませられる。is_virtual 同様の bool 列で済むが、ライフサイクル意味が違うので state enum に乗せる |
| `start_dsp_frame` 格納 | **`SourceWorld` に dense 列 `start_dsp_frame: Vec<u64>`** | コマンド毎に LUT で対応付け / SourceComponent ローカル | 既存 SoA 拡張が最小手数。Voice Virtualization スコアリングで参照することがある |
| サブコールバック開始の表現 | **`bus_buf[start_offset_frames * channels..]` から書き込み (mix 範囲のシフト)** | 別 scratch にレンダして加算でコピー | bus mix buffer は呼出前にゼロクリア済 (cf. `AudioThread::process`) なので、後ろから書くだけで前半は自然に 0 のまま。追加コピー不要 |
| 状態遷移タイミング | **callback 冒頭 (mixing 直前)** で `Scheduled → Playing` | コマンド受信時 / spatial 計算後 | `dsp_time_frames` の前進は callback 末尾で行うので、callback 冒頭の clock 値 = 「この callback の最初の frame の DSP 時刻」。これを基準に判定するのが論理的に正しい |
| 予約中の Voice Virtualization | **Scheduled は audible でないため virtualizer のスコアリング対象外** (= 物理ボイス枠を消費しない) | Playing 同様にスコアリングし常に物理化 / 常に仮想化 | 予約中は音を出さない以上 mix コストはゼロ、物理ボイス予算を圧迫しないのが正しい。発音タイミングで Playing になるとそこから virtualizer の対象に乗る |
| 予約中の Pause/Resume | **no-op (state は Scheduled のまま、schedule は予定通り fire)** | 一時停止扱いで予定をキャンセル / 経過時間ぶん遅延 | Unity も `PlayScheduled` を Pause した場合の挙動は不定で、ゲーム実装的にも「Pause で予約破棄」を期待する設計は稀。Pause したいなら `stop_source` でキャンセルし、再 schedule する想定 |
| 予約中の `stop_source` | **`Stopped` 経由で lifecycle が despawn (= 予約キャンセル)** | 専用 `cancel_schedule` API | 既存 stop 経路と統一でき API 増えない。token 付きならコールバックは「予約はあったが鳴らずに終わった」として `SourceFinished` ではなく **発火しない** (現行 `stop_*` と同じ) |
| `start_dsp_frame = 0` の意味 | **「未指定 = 即時再生」のセンチネル** | 「DSP frame 0 ぴったりに予約」 | 起動直後の frame 0 は実用上「即時」と区別不能。sentinel 0 で Command enum / SourceComponent のサイズを増やさず Optional 表現を回避 |
| API のコールバック対応 | **`play_with_handle_scheduled_*_and_callback` のみ提供** | 全 6 関数に callback 版を全網羅 | 予約再生で「終了通知だけほしい」ユースは少数。handle 経由なら token 登録 + tail 制御が標準パスで通る |
| FFI native callback 版 | **Phase 3-4 では未提供 (Rust 側のみ)** | 同時提供 | 既存 native 版は alloc-free 通知の高頻度ヒット音想定。予約再生はそこまで高頻度に発音されない (1 callback あたり数本程度の想定) ため、必要が見えてから足す |

---

## アーキテクチャ全体図

```
[main thread]                                   [sound thread]
play_scheduled_in(buf, 0.05, vol, pitch, looping)
  ↓
  start_frame = dsp_time_samples() + (0.05 * sample_rate) as u64
  ↓                                              │
  Command::Play {                                │
    audio_buffer_index, vol, pitch,              │
    token, looping,                              │
    start_dsp_frame: start_frame                 │
  }                                              │
  ringbuf push                                   │
                                                 │
                                  AudioThread::process()
                                  ├─ command 反映
                                  │   try_spawn_source(...)
                                  │     state = if start_dsp_frame > clock_at_callback_start
                                  │             { Scheduled } else { Playing };
                                  │     start_dsp_frame 列に書き込み
                                  │
                                  └─ SourceMixingSystem::update(clock_at_callback_start, ...)
                                      Phase 0.5 (NEW): activate_scheduled
                                          for each Scheduled source:
                                              if start_dsp_frame >= clock_at_callback_start + frames_in_callback:
                                                  skip (state = Scheduled)
                                              elif start_dsp_frame <= clock_at_callback_start:
                                                  state = Playing, sub_offset_frames = 0
                                              else:
                                                  state = Playing
                                                  sub_offset_frames = start_dsp_frame - clock_at_callback_start

                                      Phase 1 spatial: 通常通り
                                      Phase 1.5 virtualizer: Scheduled は除外
                                      Phase 2 mix: sub_offset_frames > 0 のソースは
                                          bus_buf[sub_offset_frames * device_channels..]
                                          だけに書く (前半はゼロのまま)
```

---

## データ構造

### `Command::Play` / `Command::PlayToBus` / `Command::SpawnSource` 拡張

3 バリアントすべてに `start_dsp_frame: u64` を追加。`0` は「未指定 = 即時再生」のセンチネル。
それ以外の値は「絶対 DSP frame でその時刻に発音開始」を意味する。

```rust
Command::Play {
    audio_buffer_index: u32,
    vol: f32,
    pitch: f32,
    token: u32,
    looping: bool,
    start_dsp_frame: u64, // NEW
}
// PlayToBus / SpawnSource も同様
```

### `SourceComponent` 拡張

```rust
pub struct SourceComponent {
    // ... 既存 ...
    /// 絶対 DSP frame 時刻での発音開始指示。`0` で即時再生。
    pub start_dsp_frame: u64,
}
```

### `SourceState` 拡張

```rust
pub enum SourceState {
    Playing,
    Pausing,
    Stopped,
    /// Phase 3-4: 発音待機中。`start_dsp_frame > clock_at_callback_start` の間は
    /// mixing をスキップし、virtualizer のスコアリング対象外。
    /// callback 冒頭で `start_dsp_frame <= clock_at_callback_start + frames_in_callback`
    /// になった時点で `Playing` へ遷移し、必要なら sub-callback offset を伴って発音する。
    Scheduled,
}
```

### `SourceWorld` 拡張

```rust
pub struct SourceWorld {
    // ... 既存 ...
    /// Phase 3-4: 予約再生開始時刻 (絶対 DSP frame)。即時再生は 0。
    pub(super) start_dsp_frame: Vec<u64>,
}
```

`spawn` / `spawn_with_id` / `despawn` / `despawn_by_dense_index` / `StopAll` の各経路に
push / swap_remove / clear を追加。

---

## サブコールバック精度ミキシング

### 基本式

callback 冒頭での DSP clock を `T0`、この callback で処理する frame 数を `N`
(= `sample_count / device_channels`) とする。Source の `start_dsp_frame` を `Ts` とする。

```
Ts >  T0 + N      → state は Scheduled のまま、ミキシング全スキップ
T0 <= Ts <= T0+N  → state ← Playing、sub_offset = (Ts - T0) として
                       mix 範囲を [sub_offset, N) に絞る
Ts <  T0           → state ← Playing、sub_offset = 0 (過去指定 → 即時再生扱い)
```

`Ts == T0` は `sub_offset = 0` で当該 callback の先頭 frame から発音される。

### 実装上の伝搬

`SourceMixingSystem::update` が新しい Phase 0.5 として `activate_scheduled()` を呼ぶ。
これは:

1. `clock_at_callback_start`, `frames_in_callback` を引数で受ける。
2. `state == Scheduled` のソースを走査し、上記分岐で `Playing` / `Scheduled` を確定。
3. `Playing` 化したソースの **per-source sub-callback start offset** を
   ローカルの一時配列 `start_offset_frames: [u32; MAX_SOURCES]` に記録。
4. `Scheduled` のままのソースは Phase 2 mix で `world.state[i] != Playing`
   の既存ガードで自然にスキップ。

Phase 2 mix の各ソースループでは `start_offset_frames[i]` を読み:

```rust
let off = start_offset_frames[source_i] as usize;
let process_len_frames = total_frames - off;
let bus_buf_shifted = &mut bus_buf[off * device_channels .. (off + process_len_frames) * device_channels];
```

として `mix_static` / `mix_streaming` に渡す。bus_buf は callback 冒頭で
`clear_mix_buffers` 済みのため、書き込まなかった `[0, off)` 区間は無音のまま。

`mix_static` / `mix_streaming` 関数自体は **無改変** で済む。呼出側で範囲を
シフトするだけで sub-callback 精度が達成できる (これが採用判断の決め手)。

### `start_offset_frames` の領域確保

`AudioThread` 構造体に `mono_scratch` と並んで `start_offset_scratch: Vec<u32>` を
事前確保 (`MAX_SOURCES` 長)。callback 内で alloc しない。

### Voice Virtualization との順序

```
Phase 0.5 activate_scheduled  ← state を Scheduled or Playing に確定
Phase 1   spatial gain compute  (Playing のみ計算してもよいが現状は全 source 走査でも安価)
Phase 1.5 virtualizer rebalance  ← Playing のみスコアリング、Scheduled は対象外
Phase 2   mix                   ← state == Playing && !is_virtual のみミキシング
```

`is_virtual` は Playing にのみ意味があり、Scheduled は false 固定で push される。

---

## API 詳細

### Engine 公開 API

主 API (秒指定):

```rust
impl SoundEngine {
    /// 現在から `delay_seconds` 後にマスターバスで再生する (sample 精度)。
    /// `delay_seconds <= 0.0` は即時再生として扱う。
    #[must_use]
    pub fn play_scheduled_in(&mut self, buffer: BufferId, delay_seconds: f64,
                             vol: f32, pitch: f32, looping: bool) -> bool;

    /// 同上、指定バス版。
    #[must_use]
    pub fn play_to_bus_scheduled_in(&mut self, buffer: BufferId, delay_seconds: f64,
                                     vol: f32, pitch: f32, bus: EntityId, looping: bool) -> bool;

    /// 同上、ハンドル付き版。
    #[must_use]
    pub fn play_with_handle_scheduled_in(&mut self, buffer: BufferId, delay_seconds: f64,
                                          vol: f32, pitch: f32, bus: EntityId, looping: bool)
                                          -> Option<EntityId>;

    /// ハンドル + 終了コールバック付き、秒指定版。
    #[must_use]
    pub fn play_with_handle_scheduled_in_and_callback(...) -> Option<EntityId>;
}
```

低レベル (絶対 frame 指定):

```rust
impl SoundEngine {
    pub fn play_scheduled_at_frame(&mut self, buffer: BufferId, start_dsp_frame: u64,
                                    vol: f32, pitch: f32, looping: bool) -> bool;
    pub fn play_to_bus_scheduled_at_frame(...) -> bool;
    pub fn play_with_handle_scheduled_at_frame(...) -> Option<EntityId>;
    pub fn play_with_handle_scheduled_at_frame_and_callback(...) -> Option<EntityId>;
}
```

`_in` の内部実装は:

```rust
let start_frame = if delay_seconds <= 0.0 {
    0 // sentinel: 即時
} else {
    let delta = (delay_seconds * self.device_sample_rate as f64) as u64;
    self.dsp_time_samples().saturating_add(delta).max(1)
    // 0 は sentinel と衝突するため、計算結果が 0 になっても 1 へ繰り上げる
};
```

> **Note**: `start_dsp_frame = 0` を sentinel に使う以上、絶対 frame 指定で 0 を
> 渡されても「即時」として動く。これは「起動直後の frame 0 ぴったりにスケジュール」
> という非実用ケースのみで意味が衝突するが、ゲーム用途では問題にならない。

### ヘルパ

`engine.dsp_time_samples()` は既存。秒 → frame 換算の内部用ヘルパを `SoundEngine` に
private で持つ:

```rust
fn delay_seconds_to_dsp_frame(&self, delay_seconds: f64) -> u64 {
    if delay_seconds <= 0.0 {
        0
    } else {
        let delta = (delay_seconds * self.device_sample_rate as f64) as u64;
        self.dsp_time_samples().saturating_add(delta).max(1)
    }
}
```

---

## エッジケース

| ケース | 期待挙動 |
|---|---|
| `start_dsp_frame` が「次の callback よりさらに先」 | 当該 callback では `Scheduled` のまま、何 callback もまたいで待機 |
| `start_dsp_frame` が「ちょうど callback 境界 = `T0`」 | sub_offset = 0、当該 callback の先頭から発音 |
| `start_dsp_frame` が「callback 区間内 = `T0 < Ts < T0 + N`」 | sub_offset = `Ts - T0`、前半 `[0, sub_offset)` は無音 |
| `start_dsp_frame` が過去 (`Ts < T0`) | sub_offset = 0 で即時再生 (silent fallback、warning なし) |
| `delay_seconds = 0.0` または負 | `_in` 内部で sentinel 0 に変換 → 通常の `Play` と同じ |
| 予約中に `stop_source` | `Stopped` 経由で despawn、コールバックは発火しない |
| 予約中に `pause_source` | no-op (Scheduled は Pause を受けない、schedule は予定通り fire) |
| 予約中に `seek_source` | `sample_offset` が書き換わるが発音はまだ。`Playing` 化した時点で seek 後位置から再生開始 |
| 予約中に `set_source_volume` / `set_source_pitch` | live_params 経由で反映、`Playing` 化した callback で発音時に既に新 vol/pitch |
| 予約中にループフラグ変更 | `set_source_loop` 通常通り反映 |
| `MAX_SOURCES` 上限 | spawn 失敗、`PlayFailed` (token あれば) 発火、`dropped_play_calls` インクリメント |
| 予約再生 + 3D ソース (`SpawnSource`) | spatial パラメータは `SetSourceSpatialParams` で設定。Scheduled 中も spatial 計算は走るが mixing が走らないので影響なし |
| Voice Virtualization 圧迫下 | Scheduled は枠を消費しない。Playing 化した瞬間から virtualizer の対象 |

---

## 実装計画

### コミット順 (1 PR 内)

1. **`SourceState::Scheduled` 追加 + `start_dsp_frame` SoA 列追加** (state 列の意味更新、spawn / despawn パス)
2. **`Command::Play` / `PlayToBus` / `SpawnSource` に `start_dsp_frame: u64` 追加** + 既存呼出側の 0 埋め
3. **`SourceMixingSystem::update` に `clock_at_callback_start`, `frames_in_callback` 引数追加** + Phase 0.5 `activate_scheduled` 実装 + `start_offset_scratch` 経由のサブフレーム mix
4. **`AudioThread::process` から `dsp_time_frames` の現在値を取得して mixing に渡す**
5. **`SoundEngine` に `play_scheduled_*_in` / `play_scheduled_*_at_frame` 系 API 追加**
6. **Voice Virtualizer から Scheduled を除外** (rebalance スコアリング、is_virtual 対象から外す)
7. **結合テスト `tests/scheduled_play.rs`**:
   - 0.05s 後に予約 → callback 駆動して該当時点まで無音 → 以降に音
   - サブコールバック精度 (frame 単位の境界テスト)
   - 過去時刻指定 → 即時再生
   - 予約中に stop → 無音のまま終了
   - Voice Virtualization 上限超過下で予約は枠を食わない
8. **ロードマップ更新**: `docs/roadmap/better-than-unity-audio.md` の Phase 3-4 行を「実装済」に
9. **CLAUDE.md** にこのドキュメントへのリンクを追加

### 非機能要件

- **ホットパス追加コスト**: `activate_scheduled` は `state == Scheduled` の数だけ走る軽量ループ。
  Scheduled 不在時 (典型的な状態) は `state[i] == Scheduled` の判定で全件スキップでき、
  `MAX_SOURCES = 256` に対し 1 ループのみで O(N)。
- **Mix インナーループ**: `mix_static` / `mix_streaming` は無改変、bus_buf スライス境界が
  シフトするだけなので命令数は増えない。
- **alloc**: ホットパスでの新規 alloc は 0。`start_offset_scratch` は `AudioThread::new()`
  時に `vec![0; MAX_SOURCES]` で確保。

---

## テスト戦略

`tests/scheduled_play.rs`:

```rust
#[test]
fn schedules_audio_to_start_at_target_dsp_frame() {
    // 1. 静音バッファ (1 秒 1.0 amplitude) を登録
    // 2. play_scheduled_at_frame(buf, 1024, ...) を投入
    // 3. 0..1024 frame の出力 = 0、1024.. frame の出力 = 1.0 を確認
}

#[test]
fn sub_callback_precision_starts_mid_callback() {
    // callback サイズ 512 frames とする
    // start = 200 で予約 → 1 callback で [0, 200) 静音、[200, 512) は音
}

#[test]
fn past_schedule_falls_back_to_immediate() {
    // dsp_time_samples 進めた後、過去 frame で予約 → 即時発音
}

#[test]
fn cancel_via_stop_source_yields_silence() {
    // play_with_handle_scheduled_in → stop_source → 無音継続
}

#[test]
fn scheduled_does_not_consume_voice_budget() {
    // MAX_PHYSICAL_VOICES + 5 個の Playing 発音中に追加で Scheduled を入れる
    // → Playing 群の物理化状況が変わらないことを確認
}

#[test]
fn play_scheduled_in_seconds_lands_within_one_callback_of_target() {
    // _in(0.1) → 0.1s 後 ±1 callback の精度で発音されることを確認
}
```

ベンチマーク (任意):

- `bench/scheduled_play.rs`: 100 同時 scheduled (32 callback 跨ぎ) のオーバヘッド計測。
  目標: Phase 2 ベンチライン比 +5% 以下。

---

## 関連ドキュメント

- [ロードマップ](../../roadmap/better-than-unity-audio.md) — Phase 3-4 が本機能の所属
- [Source ワールド](source.md) — SourceComponent / SourceState / SourceWorld の本拠地
- [スレッドモデル](threading.md) — `dsp_time_frames` の publish ルール
- [Container (Phase 4-2)](container.md) — Random/Sequence Container が将来本機能を内部利用する
- [Mixer Snapshot](snapshot.md) — `fade_total_samples` 等の DSP frame ベース時間表現が本機能と整合
