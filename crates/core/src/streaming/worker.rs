//! ストリーミング再生のデコードワーカ。
//!
//! - 1 ストリーミングバッファにつき 1 ワーカスレッドを spawn する (Phase 2-4)。
//! - symphonia でファイルを部分デコードし、`MirrorRing` に書き込む。
//! - `StreamCmd` (Seek / SetLoopRegion / Stop) をメインから受信して状態を更新。

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

use super::mirror_ring::MirrorRing;

/// ストリーミングバッファのステータス (sound thread / main thread が観測)。
pub mod status {
    pub const ACTIVE: u8 = 0;
    pub const SEEKING: u8 = 1;
    pub const EOF: u8 = 2;
    pub const STOPPED: u8 = 3;
}

/// 部分ループ再生用の区間。Phase 2-4 では全体ループ (start=0, end=total_frames) のみ使用。
#[derive(Debug, Clone, Copy)]
pub struct LoopRegion {
    pub start: u64,
    pub end: u64,
}

/// メイン → ワーカに送るコマンド。
#[derive(Debug, Clone, Copy)]
pub enum StreamCmd {
    /// 指定フレームへシーク (worker 側で flush + 再 fill 開始)。
    Seek(u64),
    /// ループ region を更新。`None` でループ無効、`Some(全範囲)` で全体ループ。
    SetLoopRegion(Option<LoopRegion>),
    /// ワーカを停止 (thread を終了させる)。
    Stop,
}

/// ストリーミングバッファの共有状態。
///
/// `AudioBuffer::Streaming` が `Arc<StreamingState>` を保持。
/// sound thread は `ring` と `status` だけ触る。
pub struct StreamingState {
    pub ring: MirrorRing,
    pub status: AtomicU8,
    /// sound thread が underrun を検出したときに立てるフラグ。main 側がドレインしてイベント発火。
    pub underrun_flag: AtomicBool,
    pub cmd_tx: SyncSender<StreamCmd>,
    /// 自身を指す `BufferId` を `(generation as u64) << 32 | index as u64` でパック。
    /// `AudioBufferPool::insert_with_streaming` が slot allocation 直後に書き込み、
    /// sound thread が `Event::StreamingUnderrun` 発火時に読み出して正しい BufferId を載せる。
    /// 0 = 未初期化 (sound thread から見えるよりも前に書き込まれているはず)。
    pub buffer_id_packed: AtomicU64,
}

impl StreamingState {
    /// パックされた BufferId を読み出す。
    #[inline]
    #[must_use]
    pub fn buffer_id(&self) -> Option<crate::buffer_pool::BufferId> {
        let packed = self.buffer_id_packed.load(Ordering::Relaxed);
        if packed == 0 {
            return None;
        }
        Some(crate::buffer_pool::BufferId {
            index: packed as u32,
            generation: (packed >> 32) as u32,
        })
    }

    /// パックされた BufferId を書き込む (`load_streaming` から 1 度だけ)。
    #[inline]
    pub fn set_buffer_id(&self, id: crate::buffer_pool::BufferId) {
        let packed = ((id.generation as u64) << 32) | (id.index as u64);
        self.buffer_id_packed.store(packed, Ordering::Relaxed);
    }

    /// underrun フラグを立てる (sound thread から呼ぶ)。
    #[inline]
    pub fn mark_underrun(&self) {
        self.underrun_flag.store(true, Ordering::Relaxed);
    }
}

/// ストリーミングワーカへのハンドル (メインスレッドが保持)。
pub struct StreamingHandle {
    pub state: Arc<StreamingState>,
    pub channels: u16,
    pub sample_rate: u32,
    join_handle: Option<JoinHandle<()>>,
}

impl StreamingHandle {
    /// コマンドを worker に送る。送信失敗 (worker が既に終了済) は無視。
    pub fn send(&self, cmd: StreamCmd) {
        let _ = self.state.cmd_tx.try_send(cmd);
    }

    /// worker を停止して join する。
    pub fn shutdown(mut self) {
        let _ = self.state.cmd_tx.send(StreamCmd::Stop);
        if let Some(h) = self.join_handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for StreamingHandle {
    fn drop(&mut self) {
        // shutdown が呼ばれずに drop された場合のフォールバック。
        if self.join_handle.is_some() {
            let _ = self.state.cmd_tx.send(StreamCmd::Stop);
            if let Some(h) = self.join_handle.take() {
                let _ = h.join();
            }
        }
    }
}

/// ストリーミングオプション。
#[derive(Debug, Clone, Copy)]
pub struct StreamingOpts {
    /// リング容量の目安 (秒)。default 1.0。
    pub buffer_seconds: f32,
}

impl Default for StreamingOpts {
    fn default() -> Self {
        Self {
            buffer_seconds: 1.0,
        }
    }
}

/// ファイルを開き、メタデータを取得してワーカを起動する。
pub fn spawn_streaming_worker<P: AsRef<Path>>(
    path: P,
    opts: StreamingOpts,
) -> Result<StreamingHandle, Box<dyn std::error::Error>> {
    let path_buf = path.as_ref().to_path_buf();
    let extension = path_buf
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_string());

    // メタデータと最初の decoder を準備する (メインスレッドで一度開いてエラーを早期検出)。
    let file = std::fs::File::open(&path_buf)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = &extension {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;
    let format = probed.format;

    let track = format.default_track().ok_or("no audio track found")?;
    let channels = track
        .codec_params
        .channels
        .map(|ch| ch.count() as u16)
        .unwrap_or(2);
    let sample_rate = track.codec_params.sample_rate.unwrap_or(44100);
    let total_frames = track.codec_params.n_frames.unwrap_or(0);
    let track_id = track.id;
    let codec_params = track.codec_params.clone();

    let capacity_frames = ((sample_rate as f32 * opts.buffer_seconds.max(0.1)) as usize).max(1024);
    let ring = MirrorRing::new(capacity_frames, channels as usize);

    let (cmd_tx, cmd_rx) = sync_channel::<StreamCmd>(16);
    let state = Arc::new(StreamingState {
        ring,
        status: AtomicU8::new(status::ACTIVE),
        underrun_flag: AtomicBool::new(false),
        cmd_tx,
        buffer_id_packed: AtomicU64::new(0),
    });

    let worker_state = Arc::clone(&state);
    let join_handle = thread::Builder::new()
        .name("nezia-streaming-decode".into())
        .spawn(move || {
            let decoder = match symphonia::default::get_codecs()
                .make(&codec_params, &DecoderOptions::default())
            {
                Ok(d) => d,
                Err(_) => {
                    worker_state
                        .status
                        .store(status::STOPPED, Ordering::Release);
                    return;
                }
            };
            run_worker(
                worker_state,
                format,
                decoder,
                cmd_rx,
                track_id,
                channels as usize,
                sample_rate,
                total_frames,
            );
        })?;

    Ok(StreamingHandle {
        state,
        channels,
        sample_rate,
        join_handle: Some(join_handle),
    })
}

/// ワーカ本体。FormatReader + Decoder を所有しデコードループを回す。
#[allow(clippy::too_many_arguments)]
fn run_worker(
    state: Arc<StreamingState>,
    mut format: Box<dyn FormatReader>,
    mut decoder: Box<dyn symphonia::core::codecs::Decoder>,
    cmd_rx: Receiver<StreamCmd>,
    track_id: u32,
    channels: usize,
    sample_rate: u32,
    total_frames: u64,
) {
    let mut loop_region: Option<LoopRegion> = None;
    let mut current_frame: u64 = 0;
    let mut at_eof = false;

    // パケットからインターリーブ PCM を取り出す再利用バッファ。
    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    // ring 書き込み時の overflow を保持するキャリーオーバ。
    let mut carry: Vec<f32> = Vec::new();

    loop {
        // ── コマンド処理 (非ブロック) ──
        loop {
            match cmd_rx.try_recv() {
                Ok(StreamCmd::Stop) => {
                    state.status.store(status::STOPPED, Ordering::Release);
                    return;
                }
                Ok(StreamCmd::Seek(target)) => {
                    state.status.store(status::SEEKING, Ordering::Release);
                    let _ = format.seek(
                        SeekMode::Accurate,
                        SeekTo::Time {
                            time: Time::new(
                                target / sample_rate.max(1) as u64,
                                (target % sample_rate.max(1) as u64) as f64
                                    / sample_rate.max(1) as f64,
                            ),
                            track_id: Some(track_id),
                        },
                    );
                    decoder.reset();
                    state.ring.flush();
                    carry.clear();
                    current_frame = target;
                    at_eof = false;
                    state.status.store(status::ACTIVE, Ordering::Release);
                }
                Ok(StreamCmd::SetLoopRegion(r)) => {
                    loop_region = r;
                    // ループが有効化されたら EOF 状態を解除して巻き戻し読み出し可能に。
                    if r.is_some() {
                        at_eof = false;
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    state.status.store(status::STOPPED, Ordering::Release);
                    return;
                }
            }
        }

        // ── EOF 後でループ無効ならアイドル ──
        if at_eof && loop_region.is_none() {
            state.status.store(status::EOF, Ordering::Release);
            // sound thread が ring を読み切るまで待ち、cmd を受け続ける。
            if let Ok(cmd) = cmd_rx.recv_timeout(Duration::from_millis(50)) {
                match cmd {
                    StreamCmd::Stop => {
                        state.status.store(status::STOPPED, Ordering::Release);
                        return;
                    }
                    StreamCmd::Seek(target) => {
                        let _ = format.seek(
                            SeekMode::Accurate,
                            SeekTo::Time {
                                time: Time::new(
                                    target / sample_rate.max(1) as u64,
                                    (target % sample_rate.max(1) as u64) as f64
                                        / sample_rate.max(1) as f64,
                                ),
                                track_id: Some(track_id),
                            },
                        );
                        decoder.reset();
                        state.ring.flush();
                        carry.clear();
                        current_frame = target;
                        at_eof = false;
                        state.status.store(status::ACTIVE, Ordering::Release);
                    }
                    StreamCmd::SetLoopRegion(r) => {
                        loop_region = r;
                        if r.is_some() {
                            at_eof = false;
                        }
                    }
                }
            }
            continue;
        }

        // ── ring が満杯付近ならスリープ ──
        let avail_write = state.ring.write_available_frames();
        if avail_write < 256 && carry.is_empty() {
            // 読み出しが進むのを少し待つ。
            thread::sleep(Duration::from_millis(2));
            continue;
        }

        // ── キャリーオーバを先に書く ──
        if !carry.is_empty() {
            let frames_in_carry = carry.len() / channels;
            let writable = avail_write.min(frames_in_carry);
            if writable > 0 {
                let n_samples = writable * channels;
                let written = state.ring.write_with_mirror(&carry[..n_samples]);
                debug_assert_eq!(written, writable);
                carry.drain(..n_samples);
            }
            continue;
        }

        // ── ループ end に到達していれば seek back ──
        if let Some(r) = loop_region
            && r.end > 0
            && current_frame >= r.end
        {
            let _ = format.seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time: Time::new(
                        r.start / sample_rate.max(1) as u64,
                        (r.start % sample_rate.max(1) as u64) as f64 / sample_rate.max(1) as f64,
                    ),
                    track_id: Some(track_id),
                },
            );
            decoder.reset();
            current_frame = r.start;
        }

        // ── 1 パケットデコード ──
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                // EOF: ループ region があれば巻き戻し、なければ EOF 状態に入る。
                if let Some(r) = loop_region {
                    let target = r.start;
                    let _ = format.seek(
                        SeekMode::Accurate,
                        SeekTo::Time {
                            time: Time::new(
                                target / sample_rate.max(1) as u64,
                                (target % sample_rate.max(1) as u64) as f64
                                    / sample_rate.max(1) as f64,
                            ),
                            track_id: Some(track_id),
                        },
                    );
                    decoder.reset();
                    current_frame = target;
                    continue;
                } else {
                    at_eof = true;
                    continue;
                }
            }
            Err(_) => {
                at_eof = true;
                continue;
            }
        };
        if packet.track_id() != track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let spec = *decoded.spec();
        let duration = decoded.capacity();
        let buf = sample_buf.get_or_insert_with(|| SampleBuffer::<f32>::new(duration as u64, spec));
        // capacity が変わった場合は作り直す。
        if buf.capacity() < duration {
            *buf = SampleBuffer::<f32>::new(duration as u64, spec);
        }
        buf.copy_interleaved_ref(decoded);
        let pcm = buf.samples();
        let frames = pcm.len() / channels.max(1);

        // ループ end でクランプ (region 内に収める)。
        let frames_to_write = if let Some(r) = loop_region {
            let limit = r.end.saturating_sub(current_frame) as usize;
            frames.min(limit)
        } else if total_frames > 0 {
            let limit = total_frames.saturating_sub(current_frame) as usize;
            frames.min(limit)
        } else {
            frames
        };
        if frames_to_write == 0 {
            continue;
        }

        let samples_to_write = frames_to_write * channels;
        let avail = state.ring.write_available_frames();
        let writable_now = avail.min(frames_to_write);

        if writable_now > 0 {
            let n = writable_now * channels;
            let written = state.ring.write_with_mirror(&pcm[..n]);
            debug_assert_eq!(written, writable_now);
        }
        // 残りはキャリーオーバ。
        let written_samples = writable_now * channels;
        if written_samples < samples_to_write {
            carry.extend_from_slice(&pcm[written_samples..samples_to_write]);
        }
        current_frame += frames_to_write as u64;
    }
}
