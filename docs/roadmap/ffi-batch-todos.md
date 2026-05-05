# Roadmap — FFI batch / DoD 最適化 TODO

PR #24 (Send / Snapshot / Container / Curve / Streaming の FFI 公開) のレビュー時に
洗い出した、まだ DoD の特徴を完全には活かしきれていない箇所と、追加すべき batch API。

各項目は **core 側の batch API 追加 + FFI 側の zero-copy slice cast 化** がセットで
必要。`spatial.rs` の `nezia_source_batch_set_velocities` (`NeziaSourceVelocityUpdate` ↔
`core::SourceVelocityUpdate` のレイアウト一致を `const _` でアサート → slice の
`cast` で alloc 0 / コピー 0) が参照実装。

優先度はゲームでの発生頻度 × DoD で得られるゲインで判定する。

---

## 高優先

### 1. Container batch spawn (`batch_play_containers_with_handles`)

**ユースケース**: 爆発・破片系シーンで Random Container を 1 フレームに数十〜100 体
spawn する。現状は `nezia_container_play_with_handle` を N 回呼ぶため、SPSC コマンド
キューに `Command::SpawnSource` が N 個積まれる (`COMMAND_RING_CAPACITY = 128` を
1 フレームで使い切るリスクあり)。

**改善案**:
- core: `play_containers_batch_with_handles(&[(ContainerId, vol, pitch, bus, looping)],
  &mut [EntityId])` を追加。1 個の `Command::BatchSpawnSources { entries: ArrayVec<...> }`
  に集約してサウンドスレッドが連続展開する。
- FFI: `nezia_container_batch_play_with_handles(engine, params_ptr, params_len, out_ids_ptr)`。
  入力配列は `#[repr(C)]` の `NeziaContainerPlayParams` で zero-copy slice cast 化。

**設計検討点**:
- enum バリアント幅: `Command::BatchSpawnSources` のスロット幅が大きくなり既存
  ringbuf の常駐メモリが増える (`threading.md` の「enum 最大バリアントの呪縛」参照)。
  → batch 専用に別 SPSC を切るか、`ArrayVec<_, N>` で N を慎重に決める。
- 1 フレーム上限 N (= 32 程度想定)。それ以上は呼出側で複数バッチに分けてもらう。

---

### 2. `core::EffectId` / `core::SendId` / `core::SnapshotId` 等の `#[repr(C)]` 化

**現状**: `core::BufferId` には PR #24 で `#[repr(C)]` を入れたが、`SendId` /
`SnapshotId` / `AttenuationCurveId` / `ContainerId` (済) は混在している。
FFI の handle 配列を batch API に渡すときに毎回 `Vec<_>` 経由で詰め直しが発生する。

**改善案**:
- `core::SendId` / `core::SnapshotId` / `core::AttenuationCurveId` に `#[repr(C)]` を付与。
- FFI 側でレイアウト一致 const アサート + slice cast 化のための準備。

**コスト**: ほぼゼロ (アトリビュート追加のみ、API 互換性も維持)。

---

## 中優先

### 3. `batch_set_send_gains` / `batch_set_send_positions`

**ユースケース**: ミキサーシーン切替で複数 Send の gain を同時調整。Snapshot で
代替可能だが、フェードを伴わない即時切替なら直接 batch のほうが軽い。

**改善案**:
- core: `batch_set_send_gains(&[(SendId, f32)])` を追加。1 個の Command にまとめる
  か、Snapshot と同じ「fade_samples = 0 の即時 Snapshot」として処理してもよい。
- FFI: `nezia_send_batch_set_gains(engine, entries_ptr, len)`。

**判断**: Snapshot で代替できるなら追加しない方針も有り。明確な需要が見えるまでは
保留。

---

### 4. `batch_set_source_attenuation_curves`

**ユースケース**: シーン遷移で複数ソースの curve を一斉切替 (室内 → 屋外で減衰
モデルを変えるなど)。発生頻度はシーン境界のみで低い。

**改善案**:
- core: `batch_set_source_attenuation_curves(&[(EntityId, Option<AttenuationCurveId>)])`。
- FFI: `nezia_source_batch_set_attenuation_curves`。

**判断**: 現状の per-source API は SPSC コマンド経路。256 ソース同時切替で 256 個の
コマンドが詰まる懸念はあるが頻度が低い。優先度低。

---

## 低優先 / 検討中

### 5. オフライン (非リアルタイム) render pump

`docs/design/core/capture.md` の「将来拡張」で言及済。Unity Recorder の
"Capture Frame Rate = Constant" モード (slow-motion 録画) 対応に必要。
現在の cpal コールバック起点アーキテクチャに大改造が要るため別フェーズ扱い。

### 6. バス単位 stem capture

`docs/design/core/capture.md` の「将来拡張」で言及済。Music / SFX / Voice 別
トラック録音用。Master 限定の `CaptureReader` を一般化して `enable_bus_capture(bus_id)`
を追加する。V1 では master のみで十分。

### 7. snapshot.rs の中間 Vec 削除

現在 FFI 側 `NeziaSnapshotBuilder` は 4 つの `Vec<>` でエントリを貯めて commit 時に
core builder へ流す。理屈上は core 側に直接 push できる経路があれば中間 Vec を
省ける。`SnapshotBuilder<'a>` の借用問題が解ければ単純化できるが、ABI 越しに mut
借用を持ち回るのは現実的でないので、現状の二段構えで OK。alloc は commit 時の
1 度だけ (cold path) なので影響軽微。

---

## 参考

- 参照実装: `crates/ffi/src/spatial.rs::nezia_source_batch_set_velocities`
- DoD 設計原則: `CLAUDE.md` 「データ指向設計」セクション、`docs/design/core/threading.md`
- 関連 PR: #23 (capture)、#24 (Send/Snapshot/Container/Curve/Streaming FFI)
