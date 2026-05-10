//! ミラーバッファ方式 SPSC リング (streaming 専用)。
//!
//! 物理長 `2 * capacity_samples` の領域に **二重書き込み** することで、
//! 読み出し時に常に contiguous slice を返せる構造。
//! wrap-around 分岐をサウンドスレッドの inner loop から完全に消すための仕掛け。
//!
//! ## レイアウト
//!
//! ```text
//! capacity_samples = capacity_frames * channels
//!
//!  storage:  [ A B C D E F G H | A B C D E F G H ]   (物理 2N)
//!             ^ primary half     ^ mirror half
//!
//! 書き込み:    storage[w] = s;  storage[w + N] = s;   (w < N)
//! 読み出し:    &storage[r..r + len]                   (len <= N、wrap しない)
//! ```
//!
//! ## 同期
//!
//! - `write_pos_frames` / `read_pos_frames` は単調増加の絶対カウンタ (frame 単位)。
//! - 物理 index は `pos % capacity_frames` で求める。
//! - 書き込み = worker thread のみ。読み出し = sound thread のみ。SPSC。
//! - Acquire/Release 順序で `storage` のメモリへの書き込み/読み出しを同期する。

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU64, Ordering};

pub struct MirrorRing {
    /// 物理長 `2 * capacity_samples` のストレージ。
    /// SPSC で worker と sound thread が共有するが、書き込み領域と読み出し領域は
    /// `read_pos_frames` / `write_pos_frames` の atomic 同期で重ならない。
    storage: UnsafeCell<Box<[f32]>>,
    capacity_frames: usize,
    channels: usize,
    /// worker thread が monotonic に増やす書き込み済みフレーム累積数。
    write_pos_frames: AtomicU64,
    /// sound thread が monotonic に増やす消費済みフレーム累積数。
    read_pos_frames: AtomicU64,
}

// SAFETY: SPSC で書き込み (worker) と読み出し (sound thread) は時間的に
// 重ならない。`write_pos_frames` / `read_pos_frames` の Acquire/Release で
// メモリ可視性は保証される。
unsafe impl Send for MirrorRing {}
unsafe impl Sync for MirrorRing {}

impl MirrorRing {
    /// `capacity_frames` フレーム × `channels` チャンネルのリングを確保する。
    /// 物理メモリは `2 * capacity_frames * channels` の f32。
    #[must_use]
    pub fn new(capacity_frames: usize, channels: usize) -> Self {
        assert!(capacity_frames > 0, "capacity_frames must be > 0");
        assert!(channels > 0, "channels must be > 0");
        let physical_samples = 2 * capacity_frames * channels;
        let storage = vec![0.0_f32; physical_samples].into_boxed_slice();
        Self {
            storage: UnsafeCell::new(storage),
            capacity_frames,
            channels,
            write_pos_frames: AtomicU64::new(0),
            read_pos_frames: AtomicU64::new(0),
        }
    }

    /// チャンネル数。
    #[inline]
    #[must_use]
    #[allow(dead_code)]
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// リング `Box<[f32]>` の実バイト数 (`memory_stats` walker 用)。
    /// 物理長 = `2 * capacity_frames * channels` の f32 (= 8 * capacity_frames * channels バイト)。
    #[inline]
    #[must_use]
    pub(crate) fn byte_size(&self) -> usize {
        2 * self.capacity_frames * self.channels * std::mem::size_of::<f32>()
    }

    /// リング容量 (フレーム単位)。
    #[inline]
    #[must_use]
    #[allow(dead_code)]
    pub fn capacity_frames(&self) -> usize {
        self.capacity_frames
    }

    /// 現在書き込み可能なフレーム数 (worker 側から呼ぶ想定)。
    #[must_use]
    pub fn write_available_frames(&self) -> usize {
        let w = self.write_pos_frames.load(Ordering::Relaxed);
        let r = self.read_pos_frames.load(Ordering::Acquire);
        let in_flight = (w - r) as usize;
        self.capacity_frames.saturating_sub(in_flight)
    }

    /// 現在読み出し可能なフレーム数 (sound thread 側から呼ぶ想定)。
    #[must_use]
    pub fn read_available_frames(&self) -> usize {
        let w = self.write_pos_frames.load(Ordering::Acquire);
        let r = self.read_pos_frames.load(Ordering::Relaxed);
        (w - r) as usize
    }

    /// インターリーブ PCM (`samples.len() == frames * channels`) を二重書き込みする。
    /// 容量超過分は drop される (worker は事前に `write_available_frames` で制限すること)。
    ///
    /// # Panics
    /// `samples.len()` が `channels` の倍数でない場合。
    pub fn write_with_mirror(&self, samples: &[f32]) -> usize {
        assert_eq!(
            samples.len() % self.channels,
            0,
            "samples.len() must be a multiple of channels"
        );
        let frames_in = samples.len() / self.channels;
        let avail = self.write_available_frames();
        let frames = frames_in.min(avail);
        if frames == 0 {
            return 0;
        }

        let w = self.write_pos_frames.load(Ordering::Relaxed);
        let phys_w_start = (w as usize) % self.capacity_frames;

        // SAFETY: SPSC で書き込み領域 [phys_w_start..phys_w_start+frames] と
        // mirror 領域 [phys_w_start+capacity..+frames] は読み出し領域 (read_pos 以降の
        // frames 分) と重ならない (write_available_frames でガード済み)。
        let storage = unsafe { &mut *self.storage.get() };
        let n_samples = self.capacity_frames * self.channels;
        let start = phys_w_start * self.channels;
        let count = frames * self.channels;

        // 物理レイアウト: storage[0..N] = primary, storage[N..2N] = mirror。
        // 不変条件: 任意 i in [0,N) に対し storage[i] == storage[i + N]。
        // 書き込みは primary と mirror の 2 パス。各パスで自領域内の wrap を処理する。

        // ── primary 書き込み (storage[0..N]) ──
        if start + count <= n_samples {
            storage[start..start + count].copy_from_slice(&samples[..count]);
        } else {
            let head = n_samples - start;
            storage[start..n_samples].copy_from_slice(&samples[..head]);
            storage[..count - head].copy_from_slice(&samples[head..count]);
        }

        // ── mirror 書き込み (storage[N..2N]、領域内で primary と同じパターン) ──
        let m_start = start + n_samples;
        if m_start + count <= 2 * n_samples {
            storage[m_start..m_start + count].copy_from_slice(&samples[..count]);
        } else {
            let head = 2 * n_samples - m_start;
            storage[m_start..2 * n_samples].copy_from_slice(&samples[..head]);
            // wrap 後は mirror 領域の先頭 (= n_samples) に戻る。
            storage[n_samples..n_samples + count - head].copy_from_slice(&samples[head..count]);
        }

        self.write_pos_frames
            .store(w + frames as u64, Ordering::Release);
        frames
    }

    /// 現在の読み出し位置から最大 `frames` フレーム分の **contiguous slice** を返す。
    /// ミラーバッファのおかげで wrap せず常に `len * channels` 連続スライスが取れる。
    /// 戻り値の `len` は `min(frames, read_available_frames)`。
    ///
    /// この呼出は `read_pos` を進めない。消費したら `advance_read` を呼ぶこと。
    #[must_use]
    pub fn peek(&self, frames: usize) -> &[f32] {
        let avail = self.read_available_frames();
        let n = frames.min(avail);
        if n == 0 {
            return &[];
        }
        let r = self.read_pos_frames.load(Ordering::Relaxed);
        let phys_r_start = (r as usize) % self.capacity_frames;
        let start = phys_r_start * self.channels;
        let count = n * self.channels;

        // SAFETY: write_available_frames のガードと Acquire load により、
        // [start..start+count] (mirror 領域含む) には worker の write が完了している。
        // 読み出し側が借用している間、worker は capacity_frames - in_flight より多く
        // 書き込めない (= この領域を上書きしない)。SPSC 不変条件。
        let storage = unsafe { &*self.storage.get() };
        // 物理 2N 領域なので start..start+count は常に範囲内 (count <= N samples)。
        &storage[start..start + count]
    }

    /// 消費したフレーム数を read_pos に加算する (sound thread 側)。
    ///
    /// # Panics
    /// `frames` が `read_available_frames` を超える場合 (debug build のみ)。
    pub fn advance_read(&self, frames: usize) {
        if frames == 0 {
            return;
        }
        debug_assert!(
            frames <= self.read_available_frames(),
            "advance_read({}) exceeds available {}",
            frames,
            self.read_available_frames()
        );
        let r = self.read_pos_frames.load(Ordering::Relaxed);
        self.read_pos_frames
            .store(r + frames as u64, Ordering::Release);
    }

    /// リングを空にする (worker thread が seek 完了時に呼ぶ)。
    /// sound thread が同時 read していると競合するため、
    /// **worker は seek 完了まで sound thread を読み待たせる責任は持たない**。
    /// 実装上は read_pos を write_pos に追いつかせる単純な操作で OK
    /// (sound thread は次フレームから新しい内容を読む)。
    pub fn flush(&self) {
        let w = self.write_pos_frames.load(Ordering::Relaxed);
        self.read_pos_frames.store(w, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_with_zero_panics() {
        let result = std::panic::catch_unwind(|| MirrorRing::new(0, 1));
        assert!(result.is_err());
    }

    #[test]
    fn empty_ring_reads_nothing() {
        let ring = MirrorRing::new(8, 2);
        assert_eq!(ring.read_available_frames(), 0);
        assert_eq!(ring.peek(4).len(), 0);
        assert_eq!(ring.write_available_frames(), 8);
    }

    #[test]
    fn write_then_peek_returns_same_samples() {
        let ring = MirrorRing::new(8, 2);
        let input = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]; // 3 frames
        let written = ring.write_with_mirror(&input);
        assert_eq!(written, 3);
        assert_eq!(ring.read_available_frames(), 3);
        let view = ring.peek(3);
        assert_eq!(view, &input);
    }

    #[test]
    fn peek_returns_contiguous_slice_across_wrap() {
        let ring = MirrorRing::new(4, 1); // 4 frames, mono
        // 最初に 3 フレーム書いて 3 フレーム消費し read_pos = 3 まで進める
        ring.write_with_mirror(&[1.0, 2.0, 3.0]);
        let _ = ring.peek(3);
        ring.advance_read(3);

        // この時点で write_pos=3, read_pos=3, phys_w = 3, phys_r = 3
        // 4 フレーム書き込む -> primary 側で wrap が発生する (3,4,5,6 を書く)
        let written = ring.write_with_mirror(&[10.0, 20.0, 30.0, 40.0]);
        assert_eq!(written, 4);

        // peek(4) は phys_r=3 から contiguous slice を返す。
        // mirror 領域のおかげで [10, 20, 30, 40] が連続で取れる。
        let view = ring.peek(4);
        assert_eq!(view, &[10.0, 20.0, 30.0, 40.0]);
    }

    #[test]
    fn write_drops_excess_when_full() {
        let ring = MirrorRing::new(4, 1);
        let written = ring.write_with_mirror(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert_eq!(written, 4);
        assert_eq!(ring.write_available_frames(), 0);
    }

    #[test]
    fn advance_read_progresses_position() {
        let ring = MirrorRing::new(4, 2);
        ring.write_with_mirror(&[1.0, 2.0, 3.0, 4.0]); // 2 frames
        assert_eq!(ring.read_available_frames(), 2);
        ring.advance_read(1);
        assert_eq!(ring.read_available_frames(), 1);
        let view = ring.peek(1);
        assert_eq!(view, &[3.0, 4.0]);
    }

    #[test]
    fn flush_empties_ring() {
        let ring = MirrorRing::new(4, 1);
        ring.write_with_mirror(&[1.0, 2.0, 3.0]);
        ring.flush();
        assert_eq!(ring.read_available_frames(), 0);
        assert_eq!(ring.write_available_frames(), 4);
    }

    #[test]
    fn mirror_works_for_stereo_with_wrap() {
        let ring = MirrorRing::new(4, 2); // 4 frames stereo
        // 3 フレーム書いて 3 フレーム消費
        ring.write_with_mirror(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        ring.advance_read(3);
        // write_pos=3, read_pos=3 (frame), phys=3
        // 3 フレーム追加書き (wrap): 期待される読み出し = [10,11,12,13,14,15]
        let written = ring.write_with_mirror(&[10.0, 11.0, 12.0, 13.0, 14.0, 15.0]);
        assert_eq!(written, 3);
        let view = ring.peek(3);
        assert_eq!(view, &[10.0, 11.0, 12.0, 13.0, 14.0, 15.0]);
    }

    #[test]
    fn spsc_threaded_smoke() {
        use std::sync::Arc;
        use std::thread;

        let ring = Arc::new(MirrorRing::new(64, 2));
        let producer_ring = Arc::clone(&ring);

        let producer = thread::spawn(move || {
            let mut total_written = 0;
            let target = 1000;
            while total_written < target {
                let avail = producer_ring.write_available_frames();
                if avail == 0 {
                    thread::yield_now();
                    continue;
                }
                let n = avail.min(target - total_written).min(8);
                let mut buf = Vec::with_capacity(n * 2);
                for f in 0..n {
                    let v = (total_written + f) as f32;
                    buf.push(v);
                    buf.push(v + 0.5);
                }
                let w = producer_ring.write_with_mirror(&buf);
                total_written += w;
            }
        });

        let mut total_read = 0;
        let target = 1000;
        while total_read < target {
            let avail = ring.read_available_frames();
            if avail == 0 {
                std::thread::yield_now();
                continue;
            }
            let n = avail.min(target - total_read).min(8);
            let view = ring.peek(n);
            for f in 0..n {
                assert_eq!(view[f * 2], (total_read + f) as f32);
                assert_eq!(view[f * 2 + 1], (total_read + f) as f32 + 0.5);
            }
            ring.advance_read(n);
            total_read += n;
        }

        producer.join().unwrap();
    }
}
