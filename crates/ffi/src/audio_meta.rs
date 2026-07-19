//! オーディオメタデータの先読み。
//!
//! `SoundEngine` インスタンス無しで呼べる、Editor / Importer 向けの API。

use std::slice;

use crate::panic::guard_result;
use crate::types::{NeziaAudioMetadata, NeziaResult};

/// メモリ上のバイト列からオーディオメタデータのみを取得する（フルデコードしない）。
///
/// `NeziaAudioImporter` が ScriptedImporter で `.wav` 等を読み込む際、
/// sample rate / channels / total frames を取得するために使う。
/// `out_metadata` には成功時にメタデータが書き込まれる。
///
/// # 安全性
/// - `bytes_ptr` は `bytes_len` バイト読める有効な領域を指すこと。
/// - `out_metadata` は `NeziaAudioMetadata` を 1 個書ける有効な領域を指すこと。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_audio_peek_metadata(
    bytes_ptr: *const u8,
    bytes_len: usize,
    out_metadata: *mut NeziaAudioMetadata,
) -> NeziaResult {
    guard_result(|| {
        if out_metadata.is_null() {
            return NeziaResult::NullPointer;
        }
        if bytes_len == 0 || bytes_ptr.is_null() {
            return NeziaResult::NullPointer;
        }
        // SAFETY: 呼出側契約により bytes_ptr は bytes_len バイトの有効領域。
        let bytes = unsafe { slice::from_raw_parts(bytes_ptr, bytes_len) };
        let meta = match nezia_core::peek_metadata(bytes) {
            Ok(m) => m,
            Err(_) => return NeziaResult::DecodeError,
        };
        // SAFETY: 呼出側契約により out_metadata は書き込み可能。
        unsafe {
            out_metadata.write(NeziaAudioMetadata {
                sample_rate: meta.sample_rate,
                channels: meta.channels,
                _pad: 0,
                total_frames: meta.total_frames,
            });
        }
        NeziaResult::Ok
    })
}
