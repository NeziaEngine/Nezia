//! ランタイムプロファイラのメインスレッド公開 API (M3 可視化基盤)。
//!
//! Editor の可視化ウィンドウが数 Hz〜数十 Hz でポーリングする想定。
//! `profiler_frame()` は triple buffer の newest-wins 読みなので、
//! どの頻度で呼んでもサウンドスレッドを妨げない。

use std::sync::atomic::Ordering;

use super::SoundEngine;
use super::profiler::ProfilerFrame;

impl SoundEngine {
    /// プロファイラの publish を有効/無効にする。
    ///
    /// 無効時のサウンドスレッド追加コストは atomic load 1 回のみ。
    /// 可視化ウィンドウを開いたときだけ有効にする運用を想定する。
    pub fn set_profiling_enabled(&self, enabled: bool) {
        self.profiler_enabled.store(enabled, Ordering::Relaxed);
    }

    /// プロファイラ publish が有効か。
    #[must_use]
    pub fn profiling_enabled(&self) -> bool {
        self.profiler_enabled.load(Ordering::Relaxed)
    }

    /// 最新のプロファイラフレームを取得する。
    ///
    /// triple buffer を update してから参照を返す (newest-wins、tearing なし)。
    /// `set_profiling_enabled(true)` 前は初期値 (空フレーム) が返る。
    #[must_use]
    pub fn profiler_frame(&mut self) -> &ProfilerFrame {
        // read() = 新フレームがあれば取り込み + 最新参照 (newest-wins)。
        self.profiler_output.read()
    }
}
