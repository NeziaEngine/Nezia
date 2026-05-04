# ストリーミング再生

NEZIA ENGINE における長尺オーディオ（BGM など）のストリーミング再生設計。
本ドキュメントは [ロードマップ](../../roadmap/better-than-unity-audio.md) の **Phase 2-4「Ogg/Vorbis デコード + ストリーミング再生」** に対応する設計を扱う。

このドキュメントの目的は **デコーダ実装の解説ではなく、「DoD（データ指向設計）の強みを保ったまま streaming をどう SoA に載せるか」の判断と境界の確定** にある。Vorbis フルデコード自体は既に `audio.rs` で symphonia 経由で動いており、Phase 2-4 で追加するのは **長尺ファイルを部分デコードしながら再生する経路** とそのライフサイクル管理。

---

## スコープ

### このフェーズで扱う
- **長尺ファイルのストリーミング再生**: ファイル全体を一度に PCM 展開せず、再生位置に応じてバックグラウンドで部分デコードしながら供給する。
- **既存 `BufferId` 空間との統合**: streaming 用の新しいハンドル型は導入せず、`BufferId` 1 種類で静的バッファとストリーミングバッファを参照可能にする。
- **既存 `play_with_handle` 経路との互換**: `play_with_handle(buffer_id, ...)` の API はそのまま利用でき、呼び出し側は静的かストリーミングかを意識しなくてよい。
- **Loop / Pause / Stop / Seek**: 静的バッファと同等の制御。
- **アンダーラン時の安全動作**: 0 埋め + イベント通知。

### このフェーズでは扱わない（後続で接続する）
- **複数同時ストリーミングの最適化** — Phase 2-4 では 8 本程度の同時ストリーミングを想定。worker 集約や decode pool 化は実利用で逼迫が見えてから（Phase 5+）。
- **部分ループ再生（loop start/end マーカー）** — ロードマップ §E の差別化候補。Phase 2-4 では「全体ループ（先頭〜末尾）」のみ実装するが、**本ドキュメントの全箇所で部分ループへの forward compatibility を確保する**（[後述](#部分ループ再生への-forward-compatibility)）。
- **クロスフェード / 自動接続** — Phase 4 の Random/Switch/Sequence Container と統合して扱う。
- **ネットワークストリーミング**（HTTP/Range）— 将来課題。本ドキュメントはローカルファイルに限定する。
- **可変ビットレート専用のシーク高速化**（インデックス事前構築）— symphonia の標準シーク機構で十分な範囲に留める。

---

## 設計判断サマリ

| 判断点 | 採用 | 不採用 | 理由（要約） |
|---|---|---|---|
| ハンドル空間 | **`BufferId` を統一**（静的/ストリーミング共通スロット）| streaming 用に別 ID 型（`StreamId` 等） | `SourceWorld::audio_buffer_index` と spawn API の分岐を作らない。ID 空間が分かれると毎 spawn / seek / set 系で「どっちの ID？」分岐が増殖する |
| `AudioBuffer` の表現 | **内部 enum 化**（`AudioBuffer::Static \| AudioBuffer::Streaming`）| trait object / 専用 `StreamingPool` 並設 | サウンドスレッドが `Arc<AudioBuffer>` だけ見ればよい。trait は alloc/dyn dispatch、別 pool は分岐ロジックが call site に分散 |
| ミキシング hot loop の分岐 | **ソース 1 個につき callback 開始時に 1 度 window slice を取得、以降の inner loop は `&[f32]` のみ参照** | inner loop で毎サンプル種別判定 | DoD の核。Phase 2-3 で確立した「per-source 1 度の cold dispatch + dense 配列 inner loop」パターンを streaming でも崩さない |
| リング読み出しの wrap 処理 | **ミラーバッファ方式**（物理 `2N` 確保、書き込みは `[i]` と `[i+N]` の両方）| 二分割スライス返却 / 都度コピー | 読み出し側は常に contiguous slice を得られる。inner loop の wrap 分岐ゼロ。書き込み側は 2 倍書込だが SIMD/メモリ帯域には影響軽微 |
| デコードワーカ | **streaming 1 本 = thread 1 本**（Phase 2-4）| グローバル decode pool / async runtime | 同時数本程度の BGM では worker pool の調整コストが利得を上回る。Phase 5+ で本数が増えたら再検討 |
| ワーカ → サウンド の供給 | **lock-free SPSC mirror ring**（事前確保、サウンドスレッド側 alloc/lock 一切なし）| Mutex 保護リング / channel | `threading.md` のサウンドスレッド制約に準拠 |
| ワーカ制御 | **メイン → ワーカ MPSC コマンド**（Seek / Stop）| atomic フラグのみ | Seek は flush と再 fill を伴うので順序保証が要る |
| アンダーラン | **0 埋め + `Event::StreamingUnderrun(BufferId)` 発火**（サウンド → メイン）| ブロック / 再生停止 | サウンドスレッドのリアルタイム制約。アプリ側がログ／品質低下表示する |
| Seek セマンティクス | **コマンド経路で「ワーカに seek 指示」、ワーカが flush + 先頭から再 fill 開始、サウンド側 `sample_offset` は再生開始位置にリセット**（過渡的にアンダーラン許容）| 同期 seek（メインがワーカ完了を待つ） | ゲームスレッドをブロックしない。アンダーランは 1〜2 callback で解消 |
| Loop | **ワーカが EOF で内部 cursor を 0 に戻す**（既存の `SourceComponent.looping` と直交）| サウンドスレッドが loop 判定 | ストリーミング側で全 PCM を保持していない以上、loop 巻き戻しはワーカ責務 |
| 静的経路の最速性 | **既存 `Static` バリアントの hot path に分岐を増やさない** | 統一インタフェース化で全て enum match | 設計ガードレール「最速経路を保つ」 |
| 自動 unload | **しない**（メインスレッドが明示的に `unload`） | 参照カウントで自動 drop | 静的バッファと挙動を揃える。worker thread の join タイミングを明確化 |

---

## DoD 観点での核

streaming は本来「per-source の独立した状態（decoder cursor / file handle / ring）」を伴うため、素直に実装すると DoD が崩れやすい領域である。NEZIA は以下 4 点で DoD の強みを保つ。

### 1. ミキシング inner loop が静的/ストリーミングで完全同形

per-source の cold path（`update()` 内のソースループ冒頭）で **1 回だけ** window slice `&[f32]` を取得する。この slice は

- 静的バッファ → `&samples[start..start + len]`
- ストリーミング → `&mirror_ring[read_pos..read_pos + len]`（ミラーバッファによりラップしない）

の **どちらも contiguous な `&[f32]`**。後続の inner loop（resampler + spatial gain + bus mix）は完全に同一コードを通る:

```rust
for n in 0..total_frames {
    let s_l = window[n * src_channels + 0];
    let s_r = window[n * src_channels + 1.min(src_channels - 1)];
    bus_buf[n * dev_ch + 0] += s_l * left_gain;
    bus_buf[n * dev_ch + 1] += s_r * right_gain;
}
```

streaming かどうかの分岐は inner loop に存在しない。ソースごとに 1 度の enum match（cold）→ 連続メモリ走査（hot）の Phase 2-3 で確立済みパターンと整合。

### 2. ミラーバッファでリング wrap を hot loop から消す

通常の SPSC リングは読み出しが書き込み境界を跨ぐとき `[read..N] ++ [0..rem]` の 2 分割になり、call site で wrap 分岐が必要になる。NEZIA の streaming ring は **物理長 `2N`** で、書き込み時に `buf[i] = sample` と同時に `buf[i + N] = sample` の二重書きを行う（`i < N`）。

これにより読み出しは `read % N` を起点として常に `&buf[start..start + len]`（`len ≤ N`）の **contiguous slice** で取得できる。inner loop は wrap を一切意識しない。

> 二重書きの代償は書き込み帯域 2 倍だが、典型ケース（48kHz × 2ch × 4byte ≈ 384 KB/s）では SIMD memcpy 1 命令分の帯域差で問題にならない。むしろ inner loop で wrap 判定が消えることによる分岐予測ミス削減 / SIMD 化容易性の利得が支配的。

### 3. デコードワーカ側も将来 SoA で集約可能な余地を残す

Phase 2-4 では「streaming 1 本 = thread 1 本」で素直に実装するが、struct のレイアウトは:

```
StreamingTable
├── ring[0..N]            (Vec<Arc<MirrorRing>>)
├── decoder[0..N]         (Vec<DecoderState>)   // worker thread 内で保持
├── read_pos[0..N]        (Vec<AtomicU64>)
├── write_pos[0..N]       (Vec<AtomicU64>)
└── status[0..N]          (Vec<AtomicU8>)        // Active / Seeking / Eof / Underrun
```

の SoA 形を取る。Phase 5+ で「decode worker pool」化する際、worker は status 配列を dense に走査して fill 候補を選び、cache-friendly に処理できる。Phase 2-4 ではこの dense table の slot を 1 worker が独占する形で十分。

### 4. 静的経路の hot path に何も足さない

`AudioBuffer::Static` は既存実装と同一データ構造（`samples: Vec<f32>`）を保持し、`samples` への直接インデクシングが従来通り可能。`Streaming` バリアントが追加されてもコンパイラは:

- ソースが `Static` のみで構成されるシーンでは（典型的な SFX 主体ゲーム）、enum tag のチェックが branch predictor で完全予測され実質 0 cost
- `Streaming` が混ざっても per-source 1 回の match でしかなく、inner loop には届かない

を保証する。設計ガードレール「`spatial_enabled = false` 等の最速経路を保つ」と整合。

---

## アーキテクチャ全体図

```
[main thread]                                    [decode worker thread (1 per stream)]
nezia_load_streaming(path)                       symphonia FormatReader + Decoder
   │                                                │
   ├─ spawn worker ─────────────────────────────────┘
   ├─ allocate streaming slot in AudioBufferPool
   ├─ create Arc<StreamingBuffer> {                    
   │     ring: Arc<MirrorRing>,                     ┌─ loop:
   │     channels, sample_rate,                     │    if ring.write_available() >= chunk_size {
   │     status: AtomicU8,                          │        decode next packet (PCM frames)
   │     cmd_tx: SyncSender<StreamCmd>,             │        ring.write_with_mirror(samples)
   │  }                                             │    } else {
   ├─ pool.insert(AudioBuffer::Streaming(...))      │        wait_notify (or short sleep)
   └─ return BufferId                               │    }
                                                    │    if recv StreamCmd::Seek(pos) {
                                                    │        decoder.seek(pos)
                                                    │        ring.flush()
                                                    │    }
                                                    │    if recv StreamCmd::Stop { break }
                                                    └─

[sound thread]
audio_callback(out):
   for each playing source:
     buf = buffers[source.audio_buffer_index]
     match buf {
       Static(s)     => window = &s.samples[off..off+len]   // 既存と同一
       Streaming(st) => window = st.consume_window(len)      // ミラーリング contiguous slice
                        // 不足時は 0 埋め slice + status=Underrun を立てる
     }
     // inner loop は window: &[f32] のみ参照（静的/ストリーミングで完全同形）
     mix(window, ...)
   // SourceFinished 等のイベント発火（既存通り）
   // 新規: status==Underrun を見て Event::StreamingUnderrun(buffer_id) 発火
```

---

## データ構造

### `AudioBuffer`（既存型を内部 enum 化）

```rust
pub struct AudioBuffer {
    pub channels: u16,
    pub sample_rate: u32,
    inner: AudioBufferInner,
}

enum AudioBufferInner {
    Static {
        samples: Vec<f32>,           // インターリーブ PCM
    },
    Streaming {
        ring: Arc<MirrorRing>,       // ワーカと共有
        status: Arc<AtomicU8>,
        cmd_tx: SyncSender<StreamCmd>,
        // worker join handle はメインスレッド側 (AudioBufferPool) で保持
    },
}
```

公開 API は次の 2 関数のみ追加:

```rust
impl AudioBuffer {
    /// callback 内で呼ぶ。指定フレーム数ぶんの contiguous window を返す。
    /// streaming で読み出し量不足のときは Cow::Owned の 0 埋め slice ではなく、
    /// 「読み出せた長さ」を別途返して呼び出し側に判定させる。
    pub fn read_window(&self, start_frame: u64, frames: usize) -> ReadWindow<'_>;
}

pub struct ReadWindow<'a> {
    pub samples: &'a [f32],     // 常に contiguous（mirror 効果）
    pub frames_filled: usize,   // streaming: 実際に得られたフレーム数（< frames でアンダーラン）
    pub underrun: bool,
}
```

`Static` 経路は `frames_filled == frames`、`underrun == false` を常に返す（既存挙動と完全互換）。

### `MirrorRing`（新規）

```rust
pub struct MirrorRing {
    /// 物理長 2 * capacity_frames * channels の f32 バッファ。
    /// 書き込み: data[w] = s; data[w + N] = s;  （w < N の範囲、N = capacity_frames * channels）
    /// 読み出し: &data[r..r + len]              （len ≤ N、wrap しない）
    data: UnsafeCell<Vec<f32>>,

    capacity_frames: usize,
    channels: u16,

    /// 書き込みは worker thread のみ。読み出しは sound thread のみ。SPSC。
    write_pos: AtomicU64,    // フレーム単位の累積。% capacity_frames で実 index
    read_pos:  AtomicU64,
}

impl MirrorRing {
    pub fn read_window(&self, frames: usize) -> &[f32];   // sound thread
    pub fn write_with_mirror(&self, samples: &[f32]);     // worker thread
    pub fn advance_read(&self, frames: usize);            // sound thread
    pub fn flush(&self);                                  // worker thread (Seek 完了後)
}
```

`UnsafeCell<Vec<f32>>` は SPSC のため worker と sound thread の write/read が時間的に重ならないことを atomic positions で保証する（SAFETY コメントで明記）。

### `StreamCmd`（main → worker）

```rust
enum StreamCmd {
    Seek(u64),                              // 目標フレーム
    SetLoopRegion(Option<LoopRegion>),      // 部分ループ用（Phase 2-4 では全体ループのみ使用）
    Stop,
}

pub struct LoopRegion {
    pub start: u64,   // フレーム単位
    pub end:   u64,   // フレーム単位（exclusive）
}
```

worker は `try_recv` で毎ループ非ブロック確認。
- `Seek` 受信時: デコーダを再シーク → `ring.flush()` → `status = Active` に戻す。
- `SetLoopRegion(Some(r))` 受信時: 内部 `loop_region` を更新。次に decoder cursor が `r.end` に到達したとき `r.start` へ seek する。`None` または `Some((0, total_frames))` は「全体ループ」を表現（Phase 2-4 のデフォルト動作）。
- `Stop` 受信時: ループ脱出して thread 終了。

部分ループは Phase 2-4 では API 経路のみ確保し、`SourceComponent` 側に `loop_start/loop_end` フィールドが追加されるフェーズで利用開始する（[後述](#部分ループ再生への-forward-compatibility)）。

### `StreamStatus`（worker → sound thread / main thread）

```rust
const STATUS_ACTIVE:    u8 = 0;
const STATUS_SEEKING:   u8 = 1;
const STATUS_EOF:       u8 = 2;
const STATUS_UNDERRUN:  u8 = 3;  // sound thread が立てる、main が読む（イベント経由）
```

worker は `Active` / `Seeking` / `EOF` を、sound thread は読み出し時に `Underrun` を立てる（compare_exchange）。

---

## スレッドモデル

| スレッド | 担当 | 制約 |
|---|---|---|
| メイン | `load_streaming` で worker spawn、`unload` で `Stop` 送信 + join、Seek コマンド送出 | 通常通り（lock/alloc OK） |
| デコードワーカ（ストリーミング 1 本につき 1 本） | symphonia decoder ループ、ring 書き込み、`StreamCmd` 受信 | 通常通り（ファイル I/O OK）|
| サウンド | `read_window` でリングから contiguous slice を取得、ミキシング、underrun フラグ立て | 既存制約（lock/alloc/syscall 不可） |

**サウンドスレッドの新たな許可操作**:
- `MirrorRing::read_window`（`AtomicU64` load + `&Vec<f32>[..]` slice 計算）
- `AtomicU8::compare_exchange`（underrun フラグ）

すべて `threading.md` の許可リスト範疇（atomic op のみ）。

**デコードワーカの責務外**:
- リアルタイム制約はないが、**サウンドスレッドが必要とするフレームより速く供給** することは責務。実装目安: ring 容量 = 1 秒分（48kHz × 2ch ≈ 384 KB）、ワーカは ring が半分以下になったら fill。OS スケジューリングで多少の遅れは許容、それを吸収するための ring 容量。

---

## ミキシングシステムの改修

`SourceMixingSystem::update` の per-source ループ冒頭に以下の差分を入れる。

**現行 (Phase 2-3 時点)**:
```rust
let buf_idx = world.audio_buffer_index[source_i] as usize;
let Some(audio_buf) = buffers.get(buf_idx).and_then(|b| b.as_ref()) else { continue };
let src_frame_count = audio_buf.frame_count();
let advance = pitch * doppler * (audio_buf.sample_rate / device_sample_rate);
// 以降 audio_buf.samples[idx] でランダムアクセス
```

**Phase 2-4 後**:
```rust
let buf_idx = world.audio_buffer_index[source_i] as usize;
let Some(audio_buf) = buffers.get(buf_idx).and_then(|b| b.as_ref()) else { continue };
let advance = pitch * doppler * (audio_buf.sample_rate as f32 / device_sample_rate);

// per-source 1 度だけ window slice を取得（cold path）
let needed_frames = ((total_frames as f32 * advance) as usize) + 1;  // resampler lookahead
let window = audio_buf.read_window(world.sample_offset[source_i] as u64, needed_frames);

if window.underrun {
    // 0 埋め slice が返る (mirror buffer 上の dead zone を指す既定領域)
    // または samples=&[] で frames_filled=0 のときは inner loop 全スキップして OK
    emit_underrun_event(buf_idx);
}

// inner loop は window.samples: &[f32] のみ参照（静的 / streaming 同形）
for n in 0..total_frames.min(window.frames_filled) {
    // 既存の resampler + spatial gain + bus mix と完全同一
}

// streaming のときだけ ring read_pos を進める（cold）
audio_buf.consume(window.frames_filled);
```

`window.samples` が contiguous であるおかげで Pre-Spatial chain（mono scratch）への書き出しコードも streaming 用に書き直す必要がない。

**`sample_offset` のセマンティクス**:
- Static: 既存通りファイル先頭からのフレームオフセット（looping/seek もそのまま動く）
- Streaming: `sample_offset` は **ストリーム開始（または最後の Seek）以降の累積再生フレーム数**。worker がリングに供給するので、ファイル内位置はワーカ側で管理する。loop は `SourceComponent.looping` と独立にワーカ内 EOF 巻き戻しで実現。

ループ判定の二重化を避けるため、**streaming バッファに対しては `SourceComponent.looping` の効果を「ワーカに loop region を伝達する」だけにする**（ソース側 mixing 内で `rem_euclid` で巻き戻すロジックは streaming パスでは無効化）。Phase 2-4 では `looping=true` ⇔ `LoopRegion { start: 0, end: total_frames }` で固定的に翻訳されるが、将来 `SourceComponent` に `loop_start/loop_end` が追加されればそのまま `LoopRegion` に橋渡しされる。

---

## ライフサイクル / API

### メインスレッド API（既存 `AudioBufferPool` への追加）

```rust
impl AudioBufferPool {
    /// ストリーミング再生用にロード。worker thread を spawn し BufferId を返す。
    /// 失敗（ファイルなし、デコーダ未対応など）時はエラー。
    pub fn load_streaming<P: AsRef<Path>>(
        &mut self,
        path: P,
        opts: StreamingOpts,
    ) -> Result<BufferId, Box<dyn std::error::Error>>;

    /// 既存の load / load_from_memory / load_from_pcm はそのまま（Static 経路）。
    /// unload は Static / Streaming 共通エントリ。Streaming のときは worker に Stop 送信 + join。
    pub fn unload(&mut self, id: BufferId) -> bool;  // 挙動拡張
}

#[derive(Clone, Copy)]
pub struct StreamingOpts {
    pub buffer_seconds: f32,    // ring 容量の目安。default 1.0
    pub loop_in_decoder: bool,  // SourceComponent.looping と冗長だが API ヒント。default false
}
```

### Source 側 API（既存に変更なし）

```rust
let bgm = pool.load_streaming("path/bgm.ogg", StreamingOpts::default())?;
let id = engine.play_with_handle(bgm, ...);  // 既存 API がそのまま使える
engine.set_source_loop(id, true);
engine.seek_source(id, 1234567);  // streaming にも正しく届く
```

`seek_source` は Source の `sample_offset` リセット + 該当 buffer が Streaming なら `StreamCmd::Seek` を ring 経由で worker に送る（メインスレッドの SoundEngine ファサード側で dispatch）。

### unload のセマンティクス

- 該当バッファを参照中のソースは「次の callback で停止扱い」（既存仕様）。
- worker thread に `Stop` を送信し join。worker は最大数十 ms で抜ける（次の packet decode 完了境界）。長時間ブロックしないよう atomic flag 併用。
- ring メモリは `Arc` 参照が 0 になった時点で drop（サウンドスレッドの `Arc<Vec<Option<Arc<AudioBuffer>>>>` スナップショットがまだ参照していれば、そのスナップショット差し替えのタイミングで実 drop される）。

---

## イベント / コールバック

新規イベント:

```rust
pub enum Event {
    // 既存:
    SourceFinished { token: u32 },
    PlayFailed,
    SourceDespawned { id: EntityId },

    // 新規:
    StreamingUnderrun { buffer_id: BufferId },  // 1 度発火したら次 100 ms はサプレス
    StreamingEof { buffer_id: BufferId },        // looping=false で EOF 到達
}
```

`StreamingUnderrun` は callback 内で立てたフラグをメインの `update_events()` でドレインして発火。連続発火すると挙動把握しにくいので 100 ms（実装上は callback カウンタ）でサプレスする。

---

## 性能 / メモリ見積もり

- ring 容量（典型）: 48 kHz × 2 ch × 4 byte × 1 sec = **384 KB / stream**。ミラー方式で物理 768 KB。
- 同時 streaming 8 本想定: **~6 MB**（メモリ指向ゲームの BGM プールとして妥当）。
- worker thread スタック: pthread default 8 MB（OS により異なる）。8 本で 64 MB のアドレス空間（実コミットは数 MB）。Phase 5+ で本数増えたら pool 化。
- mixing 側オーバーヘッド: per-source 1 度の `AtomicU64::load` + slice 計算（数 ns）。inner loop は静的と完全同形なので追加コスト無し。

---

## 後続フェーズへの拡張余地

| 拡張 | この設計のどこに乗るか |
|---|---|
| **部分ループ再生（loop start/end マーカー）** | 静的: `SourceComponent` に `loop_start/loop_end` 追加 + 既存 `rem_euclid` を区間内ラップへ変更。streaming: `StreamCmd::SetLoopRegion` 既設のため SourceComponent 拡張のみ。詳細は[次節](#部分ループ再生への-forward-compatibility) |
| 複数同時 streaming の worker pool 化 | `StreamingTable` の SoA 化前提が既に入っており、worker 集約は dispatcher を 1 個追加するだけ |
| ネットワークストリーミング | `MediaSource` を symphonia の `MediaSource` trait 実装に差し替えるだけで decode loop は再利用可能 |
| クロスフェード | Phase 4 の Container 機能で 2 本同時再生 + gain 補間。streaming 自体には変更不要 |
| 圧縮済みメモリ常駐（ADPCM 等） | `AudioBufferInner` に第 3 バリアント追加。`read_window` の実装差替で吸収 |
| Source 単位 LPF（距離連動）との合成 | `read_window` が contiguous slice を返すため、Pre-Spatial chain は streaming でもそのまま機能 |

---

## 部分ループ再生への forward compatibility

ロードマップ §E の「ループ点 (loop start/end)」は差別化候補機能。Phase 2-4 のストリーミング設計でこれを **後付けでも破綻しない** ことを保証するため、以下の境界を本フェーズ時点で確定させる。

### 1. ループ責務の所在

| バッファ種別 | ループ判定 | 理由 |
|---|---|---|
| 静的 (`Static`) | **ミキシングシステム**（`sample_offset` の wrap 計算）| 全 PCM がメモリにあるので任意位置への巻き戻しは即時可能 |
| ストリーミング (`Streaming`) | **デコードワーカ**（cursor が `loop_end` に到達時 `loop_start` へ seek）| ワーカしか source 全体に random access できない |

この分離は **全体ループでも部分ループでも変わらない**。Phase 2-4 では「区間 = 全体」を退化ケースとして扱う。

### 2. 静的バッファ側の inner loop 影響

現行案（Phase 2-4 着手時点）:
```rust
if looping && offset >= frame_count { offset = offset.rem_euclid(frame_count); }
```

部分ループ導入後:
```rust
if looping && offset >= loop_end {
    offset = loop_start + (offset - loop_start).rem_euclid(loop_end - loop_start);
}
```

- `loop_start = 0`, `loop_end = frame_count` の退化ケースで現行と等価。
- 演算は加算 1 + 減算 1 増えるだけ。inner loop の SIMD 化容易性 / 分岐構造に影響なし。
- DoD: `SourceWorld` に `loop_start: Vec<f32>`, `loop_end: Vec<f32>` の dense 列 2 本追加。256 source × 4byte × 2 = **2 KB 増加**（無視可）。

### 3. ストリーミング側の追加負荷

- `StreamCmd::SetLoopRegion` 既設のため、API 拡張点は `SourceComponent` 側のみ。
- worker 内 decoder ループに `if cursor >= loop_end { decoder.seek(loop_start); }` を 1 行追加。decode 1 packet あたり 1 比較なので無視可。
- ring 内のサンプル列は loop boundary をまたいでも contiguous（境界はワーカ側で解消済み）。サウンドスレッドは何も気にしない。

### 4. Phase 2-4 で確定させること（後フェーズで変更しない）

- `StreamCmd::SetLoopRegion(Option<LoopRegion>)` の存在と意味（Phase 2-4 では `looping` の bool 変換にしか使われないが、構造は固定）。
- worker 内の loop 責務（seek 巻き戻し）。Phase 2-4 では「`loop_end == total_frames`」固定だが、可変化への余地を確保。
- streaming パスでは mixing system が loop 判定をしない原則。これにより部分ループ導入時に streaming mixing コードに変更が及ばない。

### 5. Phase 2-4 で**確定させない**こと（部分ループ導入時に決める）

- `SourceComponent.loop_start / loop_end` のフィールド追加と並び順（dense 配列レイアウト）。
- `loop_start > loop_end` のような不正値の扱い（拒否か silent clamp か）。
- 部分ループ × seek の挙動（loop region 外へ seek したときの再進入規則）。
- リバースループや ping-pong ループ（Phase 4+ の別議論）。

これらは部分ループ実装フェーズで `SourceWorld` を拡張する際に確定する。本フェーズでは「拡張余地が残っていること」のみを保証する。

---

## 設計ガードレール（このフェーズでも厳守）

1. **サウンドスレッドの lock/alloc/syscall 禁止** — 守る（atomic op のみ追加）。
2. **静的バッファ経路の最速性維持** — 守る（`Static` バリアント hot path 不変）。
3. **既存 `BufferId` / `play_with_handle` 経路の互換** — 守る（`AudioBuffer` 内部 enum 化のみ）。
4. **DoD inner loop の同形維持** — 守る（per-source 1 度の cold dispatch、inner loop 分岐ゼロ）。
5. **新同期機構を増やすときは正味の利得を説明** — `MirrorRing` は SPSC リング 1 種類追加だが、既存 `ringbuf` クレートで mirror が標準提供されていない（要確認）ため独自実装する。利得は inner loop の wrap 分岐削減と DoD 整合性。

---

## 関連ドキュメント

- [Source ワールド](source.md) — `SourceComponent` / `audio_buffer_index` の責務
- [スレッドモデル](threading.md) — サウンドスレッド制約と 3 経路通信
- [DSP パイプライン](dsp.md) — Pre-Spatial chain との合成（streaming でも変更不要）
- [統合戦略](../integration/CONCEPT.md) — Unity `AudioClip` の `loadType=Streaming` ドロップイン互換
- [ロードマップ](../../roadmap/better-than-unity-audio.md) — Phase 2-4 の位置づけ
