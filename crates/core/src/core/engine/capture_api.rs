//! マスター出力キャプチャと出力フォーマットの公開 API。

use std::sync::atomic::Ordering;

use crate::capture::CaptureReader;

use super::SoundEngine;

impl SoundEngine {
    /// マスター出力 PCM のキャプチャを有効化し、リーダーハンドルを返す。
    ///
    /// 戻り値は **初回呼び出し時のみ Some**。リーダーは 1 個しか発行されないため、
    /// 2 回目以降は `None` を返す (既に取得済みのハンドルが流量を受け続けている)。
    /// `disable_master_capture()` 後に再度 enable しても新しいリーダーは発行されない。
    ///
    /// 取得後は任意スレッドから `CaptureReader::read_interleaved()` を呼んでよい。
    /// Unity Recorder からは Unity 側のオーディオスレッドや専用録音スレッドで drain 可能。
    pub fn enable_master_capture(&mut self) -> Option<CaptureReader> {
        let reader = self.capture_reader.take()?;
        self.capture_shared.enabled.store(true, Ordering::Release);
        Some(reader)
    }

    /// マスター出力キャプチャを無効化する。
    ///
    /// audio thread はこれ以降リングへ push しない。既存リーダーはリング内に残る
    /// サンプルを最後まで drain してよい。再 enable も可能 (フラグを再 store するだけ)。
    pub fn disable_master_capture(&self) {
        self.capture_shared.enabled.store(false, Ordering::Release);
    }

    /// デバイス出力フォーマットを `(sample_rate_hz, channels)` で返す。
    ///
    /// Unity Recorder の wav/mp4 mux 設定や、自前の録音 muxer 構築に使う。
    #[must_use]
    pub fn output_format(&self) -> (u32, u16) {
        (self.device_sample_rate as u32, self.device_channels)
    }
}
