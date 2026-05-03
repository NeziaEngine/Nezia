use std::sync::Arc;

use crate::audio::AudioBuffer;

/// 任意スレッドから読める PCM 読み取りハンドル。
///
/// `SoundEngine::open_buffer_reader` で生成し、ハンドル経由で `Arc<AudioBuffer>` を
/// 保持する。これにより:
/// - 任意スレッドから `read_frames()` を呼べる（lock-free）
/// - ハンドル生存中は `unload()` してもメモリが解放されない（reader 側で安全に読み続けられる）
pub struct BufferReader {
    pub(super) buffer: Arc<AudioBuffer>,
}

impl BufferReader {
    /// チャンネル数。
    pub fn channels(&self) -> u16 {
        self.buffer.channels
    }

    /// サンプルレート（Hz）。
    pub fn sample_rate(&self) -> u32 {
        self.buffer.sample_rate
    }

    /// 総フレーム数（チャンネルあたりのサンプル数）。
    pub fn total_frames(&self) -> usize {
        self.buffer.frame_count()
    }

    /// `frame_offset` 位置から `dst` を埋めるだけのインターリーブ PCM を書き込む。
    ///
    /// 戻り値は実際に書き込んだフレーム数（`dst.len() / channels` 以下）。EOF に達した
    /// 場合は要求より少ないフレーム数を返す。`dst.len()` は `channels` の倍数である必要が
    /// ある（そうでない場合は端数を切り捨てる）。
    pub fn read_frames(&self, frame_offset: usize, dst: &mut [f32]) -> usize {
        let channels = self.buffer.channels as usize;
        if channels == 0 {
            return 0;
        }
        let requested_frames = dst.len() / channels;
        let total_frames = self.buffer.frame_count();
        let available = total_frames.saturating_sub(frame_offset);
        let frames = requested_frames.min(available);
        let sample_offset = frame_offset * channels;
        let sample_count = frames * channels;
        dst[..sample_count]
            .copy_from_slice(&self.buffer.samples[sample_offset..sample_offset + sample_count]);
        frames
    }
}
