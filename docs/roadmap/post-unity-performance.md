# Roadmap — Post-Unity Performance (極限パフォーマンス追求フェーズ)

`better-than-unity-audio.md` の Phase 4 完了 = 「Unity 標準より良い」を達成した**後**に着手するロードマップ。
ここからは **機能 parity ではなくランタイム性能そのもの** を競合軸に据え、Wwise / FMOD / CRI ADX クラスの本格ミドルウェアと同じ土俵に立つことを目標とする。

> **前提**: このフェーズは Phase 4 完了が条件。parity に穴がある状態でアーキテクチャを大改造すると、機能追加と再設計が並走して破綻する。Unity 超えを名乗れるまでは、現行のシリアル + SoA + SIMD 路線で押し切る。

---

## 立ち位置

- 直近競合は **Unity 標準** → これは Phase 4 で打倒済の想定。
- 次の競合は **Wwise / FMOD / CRI ADX**。これらはいずれも近年 **タスクベース (job graph) 並列処理** に移行済、もしくは移行中。
- NEZIA の現状は **単一 cpal callback スレッドで全処理を完結する完全シリアルモデル** (`docs/design/core/threading.md`)。
  - 数十〜数百 Source、数十 Bus 規模の通常ゲームではこれで十分。
  - だが**数千 Source / 数百 Bus / 重量 DSP チェーン** が常時走る AAA タイトル・大規模オープンワールド・VR/AR では、コア 1 本では確実に詰まる。
- このフェーズのミッションは **「コア数に比例してスケールするサウンドエンジン」** を作ること。

---

## 目標と非目標

### 目標
1. **マルチコアスケール**: 物理コア数を増やすほど処理 Source 数が線形に伸びる。
2. **レイテンシ維持**: コールバック周期 (5-10ms) を絶対に破らない。並列化はスループット向上のためであり、最悪レイテンシは現行と同等以上。
3. **DoD 維持**: 並列化のために SoA / dense 配列 / キャッシュ親和性を犠牲にしない。
4. **ゼロアロケーション維持**: ワーカースレッドも含め、全オーディオ処理経路で alloc / lock / syscall ゼロを維持。

### 非目標
- 機能追加。このフェーズでは新しい DSP・新しい spatial アルゴリズムは増やさない。**既存機能を高速化するだけ**。
- 「並列化のための並列化」。プロファイルで詰まりが見えていない箇所はシリアルのまま残す。

---

## 現行アーキテクチャの再確認

性能改造の出発点として、現行の固定シリアル順を明示しておく (`audio_thread/mod.rs:121-270`)。

```
[cpal callback 1 本]
  1. Command queue 処理         (SPSC ring からの取り込み)
  2. Snapshot 補間 flush
  3. Triple buffer 取り込み      (listener / source 位置・速度)
  4. Live params 反映           (Atomic per-slot → dense)
  5. Bus mix_buffer クリア
  6. SourceSystem               (pitch → pre-spatial → spatial → post-spatial → bus 加算)
  7. SourceLifecycleSystem
  8. BusSystem                  (pre-fader → tap → fader → post-fader → tap → 親加算)  ← DAG 順
  9. Master → output_buffer
```

ここで**並列化余地が大きい順**:
- **(6) SourceSystem**: 各 Source は独立。最大 256 体を SIMD バッチ + ワーカー分散できる余地が最大。
- **(8) BusSystem の同レベル**: DAG トポロジカル順で**同じ depth の Bus は依存しない**。レベル別並列実行が可能。
- **(4) Live params 反映**: Atomic load の dense 走査。SIMD gather で詰められるが効果は限定的。

---

## 段階的タスクベース移行プラン

一気にタスクグラフ化すると壊れるので、**4 段階** に分ける。各段階の完了条件は **「ベンチで明確な改善が出ること」+ 「既存テスト全 pass」**。

### Stage P-1: SIMD バッチ化の徹底 (シングルスレッドのまま)

**動機**: タスクグラフ化の前に**まずシングルスレッドで使える計算資源を使い切る**。並列化はその後。

- Source の volume / pitch / spatial 計算を **f32x4 / f32x8 (std::simd or wide crate)** で 4-8 体同時処理。
- Bus mix の加算ループを SIMD 化 (既に一部実装済なら計測で確認)。
- 距離減衰・パンの計算を分岐レス + SIMD 化。

**完了条件**: 256 Source ベンチで**現行比 30-50% 高速化**。これが出ない時点で並列化しても無駄なので止める。

### Stage P-2: ワーカープール + Source 並列化

**動機**: SourceSystem が一番並列化のコスパがいい。先に取り組む。

- **ワーカースレッドプール** (物理コア数 - 2、メイン・サウンドコールバック分を除く) を engine 起動時に確保。
  - **CPU affinity 固定**、**RT priority 設定** (プラットフォーム別)。
  - ワーカーは `Arc<AtomicBool>` の wake フラグでスピン待機 (短時間) → futex 待機 (長時間) のハイブリッド。
- cpal callback スレッドは **コーディネータ役** に徹する。
  - Source を N 体ずつのチャンクに分割し、ワーカーキューに投入。
  - `std::sync::Barrier` 相当の lock-free counter で全完了を待つ。
- Bus mix_buffer への加算は **per-bus per-worker のローカルバッファ** に貯め、全 Source 完了後にコーディネータが集約 (false sharing 回避)。

**設計判断**:
- ワーカー間で work-stealing を入れるか? → **入れない**。Source 数が固定上限 (256) で偏りが出にくく、deque 同期コストが利得を食う。**静的チャンク分割で十分**。
- ワーカーへのジョブ投入は何で渡すか? → **専用 SPSC ring (コーディネータ → 各ワーカー)**。MPSC は同期コストが高い。

**完了条件**: 4 コア環境で 1024 Source 処理が**現行比 2.5-3 倍高速化**。最悪レイテンシが現行を下回らないこと。

### Stage P-3: Bus DAG の並列実行

**動機**: Source 並列化後、次のボトルネックは Bus chain (DSP)。Compressor / Reverb は重い。

- メインスレッドが **DAG をレベル分解** (`process_order` を depth ごとに区切る)。
- 同レベルの Bus は依存しないのでワーカープールに分散。
- Send tap も同レベル内では独立 (送出先 Bus は次レベル以降にいる)。

**注意点**:
- DSP インスタンス (Reverb 等) は**内部状態を持つ**ので、同じバスを 2 ワーカーが触ることは絶対にない設計を保証する。
- **キャッシュライン分離**: Bus dense 配列をワーカー別に prefetch しやすい形に SoA 再配置するかどうかは計測してから決める。

**完了条件**: 重 DSP チェーン (Compressor + Reverb + EQ) を 32 Bus に積んだベンチで**現行比 1.8-2.2 倍高速化**。

### Stage P-4: タスクグラフ化 (本番)

ここで初めて「タスクベース」を名乗る。

- 上記 P-2 / P-3 の手書き並列化を **統一タスクグラフ抽象** に置き換える。
- ノード = 処理単位 (Source chunk / Bus chain / Send tap / Snapshot flush)。
- エッジ = データ依存。
- スケジューラはトポロジカル順 + ワーカープールで実行。
- **静的グラフ**: 毎フレーム再構築せず、トポロジ変化時 (バス追加・Send 追加) のみメインスレッドが再構築してサウンドスレッドに差し替える (snapshot pattern)。

**やらないこと**:
- 動的 work-stealing scheduler。
- 汎用ジョブシステム (rayon 流用も含む)。**オーディオ専用**で切る。汎用化するとリアルタイム制約が守れない。

**完了条件**: 4096 Source + 128 Bus + DAG 4 段の極限ベンチで**コア数線形スケール (8 コアで 6 倍以上)**。

---

## ガードレール (このフェーズ専用)

`better-than-unity-audio.md` のガードレールに加え、性能フェーズでは以下を追加で守る。

1. **「測ってから書く」を絶対化する**。
   - 各 Stage 着手前に**現行のプロファイル (perf / Instruments / cargo-flamegraph) を取り**、ボトルネック仮説を文書化してから実装に入る。
   - 完了時には**改善前後のベンチ結果を PR に貼る**。出てなければ revert。

2. **ワーカースレッドもサウンドスレッドと同じリアルタイム制約**。
   - alloc / lock / syscall ゼロ。`Mutex`, `RwLock`, `Box::new`, `Vec::push` (capacity 内除く) はワーカー内で禁止。
   - 例外を作るくらいなら並列化を諦める。

3. **シリアル fallback を残す**。
   - `engine.toml` (もしくは初期化フラグ) で `parallel = false` を選ぶと P-1 までの SIMD シリアル経路で動く。
   - デバッグ時・小規模ゲーム時・組込環境で常用される。

4. **最悪レイテンシを必ず測る**。
   - 平均スループットが上がっても **p99 / p99.9 レイテンシが悪化したら不採用**。並列化のコンテキストスイッチでスパイクが出ることがある。

5. **DoD を壊さない**。
   - 並列化のために構造体を細かく切り刻んで AoS 化することは禁止。SoA + チャンク分割で並列性を出す。

6. **新しい同期プリミティブは 1 つずつ追加し、それぞれに justification を残す**。
   - SPSC ring / Triple buffer / Atomic per-slot に加えて Barrier / Futex / Worker SPSC が増える。**増えるたびに既存と置き換えできないか検討する**。

---

## 想定スケジュール

Phase 4 完了後にスタートとして:

| Stage | 期間 (FTE 1 名) | 期間 (FTE 2 名) |
|-------|----------------|----------------|
| P-1 SIMD 徹底 | 3-4 週 | 2 週 |
| P-2 Source 並列 | 6-8 週 | 4 週 |
| P-3 Bus DAG 並列 | 4-6 週 | 3 週 |
| P-4 タスクグラフ統一 | 6-8 週 | 4 週 |
| **合計** | **5-7 ヶ月** | **3-4 ヶ月** |

ただし**各 Stage の完了条件 (ベンチ改善) を満たさない場合はそこで一時停止し、設計を見直す**。スケジュール優先で押し切らない。

---

## 関連ドキュメント

- [Better than Unity Audio](better-than-unity-audio.md) — このフェーズの**前提となる Phase 1-4**
- [スレッドモデル](../design/core/threading.md) — 現行のシリアルモデルと同期機構の選定理由
- [ECS アーキテクチャ](../design/core/ecs.md) — DoD / SoA の前提
- [DSP パイプライン](../design/core/dsp.md) — 並列化対象の DSP チェーン構造
- [Send / Sidechain Ducking](../design/core/send.md) — Bus DAG の構造
