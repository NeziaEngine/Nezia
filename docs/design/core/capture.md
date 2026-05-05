# マスター出力キャプチャ

NEZIA の最終ミックス (master post-fader) を任意スレッドから drain できるよう、
サウンドスレッドが書き出す `data` を lock-free SPSC リングへ tap する経路。
主用途は **Unity Recorder などの外部録音インテグレーション** で、Unity の
`AudioListener` を経由しない NEZIA 出力をビデオ録画と同期させて記録する。

## なぜ必要か

NEZIA は cpal (もしくは将来的に他のオーディオドライバ) に直接出力するため、
Unity の `AudioMixer` / `AudioListener` グラフを経由しない。Unity Recorder の
標準オーディオキャプチャは `AudioListener` 出力を録音するので、何もしなければ
**録画ファイルのオーディオトラックは無音**になる。Integration 層 (C# ラッパ) だけ
では Recorder にサンプルを供給できず、core 側にタップ機構が必要になる。

## 経路の位置付け

スレッド間通信 (`threading.md`) の 4 経路目に相当する **「サウンドスレッド →
任意スレッドへの PCM ストリーミング」**。`Event` リング (1 サンプル数 byte の
構造化メッセージ) や triple buffer (newest-wins スナップショット) とは目的が
異なり、**全サンプルを順序通り取りこぼさず通したい** のでリングバッファを使う。

| 経路 | 方向 | データ | 順序保証 | 詰まったら |
|---|---|---|---|---|
| Command ring | Main → Audio | 構造化コマンド | あり | (今のところ) silent drop |
| Event ring | Audio → Main | 構造化イベント | あり | silent drop |
| Triple buffer | Main → Audio | 最新スナップショット | なし | 古い値破棄 |
| Atomic per-slot | Main → Audio | スカラー | なし | 古い値破棄 |
| **Capture ring** | Audio → Any | f32 PCM 連続 | あり | dropped\_samples 加算 + Event 発火 |

## 配置: BusSystem::update 直後

最終ミックスは `audio_thread::AudioThread::process` 内で
`BusSystem::update(... data ...)` が device buffer に書いた直後の状態がそのまま
「master post-fader 出力」になる。ここを `push_slice(data)` で SPSC リングへ
コピーする。エフェクトバスの DSP やマスター fader 後の最終結果が含まれるため、
Recorder 視点で「ユーザに聞こえている音そのもの」を録音できる。

```rust
// 簡略化された hot path
BusSystem::update(..., data, ...);
if capture_shared.enabled.load(Relaxed) {
    let pushed = capture_producer.push_slice(data);
    if pushed < data.len() {
        capture_shared.dropped_samples.fetch_add(...);
        event_producer.try_push(Event::CaptureOverflow { ... });
    }
}
dsp_time_frames.fetch_add(sample_count / device_channels, Relaxed);
```

`enabled` が false のとき hot path コストは **AtomicBool::load 1 回 (Relaxed)**。
キャプチャ未使用のシーンで実質ゼロ。

## ライフサイクル

1. `SoundEngine::new()` で **常にリングを 1 個確保** する (容量: device\_sample\_rate
   × device\_channels × `CAPTURE_RING_SECONDS = 1.0`)。`CaptureReader` ハンドルは
   `Option<CaptureReader>` として engine 内に格納し、初回 `enable_master_capture()`
   で `take()` する。
2. `enable_master_capture()` は `AtomicBool::store(true)` でフラグを立て、
   `CaptureReader` を返す。**戻り値は初回だけ Some**。再 enable しても新しい
   ハンドルは発行されず、既存ハンドルが流量を受け続ける。
3. `disable_master_capture()` は `AtomicBool::store(false)` するのみ。リーダーは
   そのまま残量 drain に使ってよい。再 enable も可能。
4. `CaptureReader` 自体には `disable_*` 操作はない。drop すると consumer 端が
   消えるだけ (audio thread の producer 側は engine が drop されるまで生存)。

ハンドルの一意性によって「複数スレッドが同じリングを drain して読み逃しが起きる」
事故を型レベルで防ぐ。複数経路で record したい場合は呼出側で `Arc<Mutex<>>` 等で
合議するか、将来的に **バス単位 stem capture** を追加する (本ドキュメント末尾)。

## 容量設計

リング容量 = `device_sample_rate * device_channels * CAPTURE_RING_SECONDS` (samples)。

- 48000 Hz / 2ch / 1.0s → 96000 samples = 384 KB
- 44100 Hz / 2ch / 1.0s → 88200 samples = ~344 KB

1 秒分あれば、Unity メインスレッドの GC スパイクや IO 遅延 (典型 100ms 級) を
吸収できる。Recorder ファイル書き込みが 500ms 以上ブロックすると drop が始まるが、
そのケースは録画品質の根本問題なので、core 側で容量を増やすより呼出側で
書き込みスレッドを分けるなどで対処する方針。

## オーバーフロー処理

`push_slice` が要求未満しか push できなかった場合:

1. 差分を `CaptureShared::dropped_samples` (AtomicU64) に `fetch_add(Relaxed)` で加算。
   `CaptureReader::dropped_samples()` で累積を取得できる。
2. 同コールバック内で `Event::CaptureOverflow { dropped_samples: u32 }` を 1 個 push。
   `try_push` 失敗 (Event ring も満杯) は無視。
3. 連続発火抑制は **Event ring 容量 (64) 自体が暗黙のレート制限** として働く。
   それでも頻発するなら累積カウンタ側で総量を確認できる。

Recorder UI からは「累積 dropped > 0」で警告バッジを出し、Event 単発発火で即座に
通知する、の二段構えで品質劣化を検知する想定。

## DSP クロック

audio thread が毎コールバック末尾で `dsp_time_frames.fetch_add(frames_advanced)`
する単調増加カウンタ。任意スレッドから `SoundEngine::dsp_time_samples()` /
`dsp_time_seconds()` で読める。

Unity Recorder は **動画フレーム時刻と PCM サンプル位置の対応点** がないと
オーディオトラックを正しく aligning できない。Recorder の OnRecord フック内で
`(Time.time, dsp_time_seconds())` のペアを 1 度サンプリングしておけば、以後は
`dsp_time` を基準にビデオフレームと相関を取れる。

## オフライン (非リアルタイム) 描画について

Unity Recorder の "Capture Frame Rate = Constant" モード (ゲームを slow-motion で
進めて高品質録画する) を完全サポートするには、cpal のコールバック起点ではなく
外部から `render_offline(frames, dst)` を pull する経路が必要。これは現在の
アーキテクチャに大きな変更を要するため **現状はサポートしない**。
"Capture Frame Rate = Play" (リアルタイム録画) はそのまま動作する。

将来オフライン対応する場合は、`AudioThread::process` を cpal コールバックから
切り離して、cpal 駆動 / 手動 pump 駆動の 2 モードを切り替えられる構造にする。
コマンド処理・triple buffer 反映・ミキシングはどちらのモードでも同じ。

## 将来拡張: バス単位 stem capture

Music / SFX / Voice などを別トラックで録りたい需要に対応するなら、Master 限定の
`CaptureReader` を一般化して `enable_bus_capture(bus_id)` を追加する。`BusSystem`
内で対象バスの post-fader 時点でミックスバッファをタップして別 SPSC へ push する。
V1 では master のみで十分 (Recorder 標準ユースケースは AudioListener 1 系統)。

## 公開 API

### Rust

- `SoundEngine::enable_master_capture() -> Option<CaptureReader>`
- `SoundEngine::disable_master_capture()`
- `SoundEngine::output_format() -> (u32, u16)`
- `SoundEngine::dsp_time_samples() -> u64`
- `SoundEngine::dsp_time_seconds() -> f64`
- `CaptureReader::sample_rate() -> u32`
- `CaptureReader::channels() -> u16`
- `CaptureReader::dropped_samples() -> u64`
- `CaptureReader::read_interleaved(&mut [f32]) -> usize`

### C ABI

- `nezia_engine_enable_master_capture(*mut NeziaEngine) -> *mut NeziaCaptureReader`
- `nezia_engine_disable_master_capture(*mut NeziaEngine)`
- `nezia_engine_output_format(*const NeziaEngine, *mut u32, *mut u16)`
- `nezia_engine_dsp_time_samples(*const NeziaEngine) -> u64`
- `nezia_engine_dsp_time_seconds(*const NeziaEngine) -> f64`
- `nezia_capture_reader_close(*mut NeziaCaptureReader)`
- `nezia_capture_reader_sample_rate(*const NeziaCaptureReader) -> u32`
- `nezia_capture_reader_channels(*const NeziaCaptureReader) -> u16`
- `nezia_capture_reader_dropped_samples(*const NeziaCaptureReader) -> u64`
- `nezia_capture_reader_read(*mut NeziaCaptureReader, *mut f32, usize) -> u64`
