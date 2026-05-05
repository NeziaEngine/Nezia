//! マスター出力 PCM キャプチャ (Unity Recorder 等の外部録音インテグレーション向け)。
//!
//! NEZIA は cpal に直接出力するため、Unity の `AudioListener` 経由で録音する
//! Unity Recorder のような仕組みからはオーディオが見えない。このモジュールは
//! サウンドスレッドが master post-fader 後に書き出す `data` を任意スレッドから
//! drain できるよう、lock-free SPSC リングへ tap する経路を提供する。
//!
//! 設計詳細は `docs/design/core/capture.md` 参照。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ringbuf::HeapCons;
use ringbuf::traits::Consumer;

/// audio thread と main thread が共有する制御フラグ群。
///
/// `enabled` が false のとき audio thread は capture ring を一切触らないため、
/// hot path のオーバーヘッドは AtomicBool 1 回の relaxed load のみ。
pub(crate) struct CaptureShared {
    /// キャプチャ on/off フラグ。`enable_master_capture` / `disable_master_capture` で切り替える。
    pub(crate) enabled: AtomicBool,
    /// リングオーバーフローで audio thread が捨てたサンプル累積数。
    /// 単位はインターリーブサンプル数 (frames * channels)。
    pub(crate) dropped_samples: AtomicU64,
}

impl CaptureShared {
    pub(crate) fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            dropped_samples: AtomicU64::new(0),
        }
    }
}

/// マスター出力 PCM のキャプチャリーダー。
///
/// `SoundEngine::enable_master_capture()` で初回取得後は任意スレッドから
/// `read_interleaved()` を呼んでよい (lock-free SPSC consumer)。
///
/// `disable_master_capture()` 後もリーダーは生存し続け、リング内に残っている
/// サンプルを最後まで drain できる。再 enable しても新しいリーダーは発行されず、
/// 既存ハンドルがそのまま流量を受け取り続ける。
pub struct CaptureReader {
    consumer: HeapCons<f32>,
    shared: Arc<CaptureShared>,
    sample_rate: u32,
    channels: u16,
}

impl CaptureReader {
    pub(crate) fn new(
        consumer: HeapCons<f32>,
        shared: Arc<CaptureShared>,
        sample_rate: u32,
        channels: u16,
    ) -> Self {
        Self {
            consumer,
            shared,
            sample_rate,
            channels,
        }
    }

    /// デバイスサンプルレート (Hz)。録音 wav/mp4 mux 設定に使う。
    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// デバイスチャンネル数 (通常は 2)。インターリーブ PCM の stride。
    #[must_use]
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// 起動以降にリング満杯で捨てたサンプル累積数 (インターリーブサンプル単位)。
    ///
    /// 連続的に増える場合は drain 周期が短すぎる / リング容量が足りない兆候。
    #[must_use]
    pub fn dropped_samples(&self) -> u64 {
        self.shared.dropped_samples.load(Ordering::Relaxed)
    }

    /// `dst` にインターリーブ PCM を最大 `dst.len()` サンプル書き出す。
    ///
    /// 戻り値は実際に書き込んだサンプル数 (チャンネル数の倍数とは限らない点に注意:
    /// リングは要素単位で動作する)。フレーム揃えしたい場合は呼出側で
    /// `dst.len()` を `channels` の倍数にしておくこと。
    pub fn read_interleaved(&mut self, dst: &mut [f32]) -> usize {
        self.consumer.pop_slice(dst)
    }
}
