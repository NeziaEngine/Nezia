//! Source 関連の公開 API。play 系統別にサブモジュールへ分割。
//!
//! - [`play`] — 基本 Rust API: play / play_with_callback / play_to_bus(_with_callback) /
//!   play_with_handle(_and_callback) (Rust クロージャ版、Box 経由)
//! - [`play_native`] — FFI 用 alloc-free 版: `*_with_callback_native` 3 種
//!   (関数ポインタ + user_data を固定スロット書き込み)
//! - [`play_scheduled`] — 予約再生 (Phase 3-4 PlayScheduled): `_in(seconds)` /
//!   `_at_frame(u64)` 形式の各バリエーション
//! - [`control`] — ライブ制御: stop_all / set_source_* / seek / pause / resume /
//!   stop / set_source_loop / set_source_priority

mod control;
mod play;
mod play_native;
mod play_scheduled;

use std::ffi::c_void;

/// FFI 用の C 関数ポインタコールバック型。
///
/// `play_*_with_callback_native` 系で受け取る型。`extern "C"` のため
/// クロージャキャプチャはできず、`user_data` を経由して呼出側のコンテキストを伝える。
pub type NativeFinishFn = unsafe extern "C" fn(user_data: *mut c_void);
