//! オーディオバッファのロード・アンロード。

use std::slice;

use crate::engine::NeziaEngine;
use crate::panic::{guard_buffer, guard_result};
use crate::types::{NeziaBufferId, NeziaResult};

/// オーディオファイルをロードしてハンドルを返す。失敗時は `INVALID` を返す。
///
/// # 引数
/// - `path_ptr` / `path_len`: UTF-8 のファイルパス。NUL 終端は不要。
///
/// # 安全性
/// `path_ptr` は `path_len` バイト読める有効な領域を指すこと。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_buffer_load(
    engine: *mut NeziaEngine,
    path_ptr: *const u8,
    path_len: usize,
) -> NeziaBufferId {
    guard_buffer(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaBufferId::INVALID;
        };
        if path_ptr.is_null() {
            return NeziaBufferId::INVALID;
        }
        // SAFETY: 呼出側契約により path_ptr は path_len バイトの有効領域。
        let bytes = unsafe { slice::from_raw_parts(path_ptr, path_len) };
        let Ok(path) = std::str::from_utf8(bytes) else {
            return NeziaBufferId::INVALID;
        };
        match engine.inner.load(path) {
            Ok(id) => NeziaBufferId::from_core(id),
            Err(_) => NeziaBufferId::INVALID,
        }
    })
}

/// メモリ上のエンコード済みバイト列からロードする。失敗時は INVALID。
///
/// 統合層からの主要ロード経路。`NeziaAudioClip.encodedBytes` や Addressables /
/// `UnityWebRequest` で取得した byte 配列をそのまま渡す。フォーマットは symphonia が
/// マジックバイトから自動判別する。
///
/// # 安全性
/// `bytes_ptr` は `bytes_len` バイト読める有効な領域を指すこと。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_buffer_load_from_memory(
    engine: *mut NeziaEngine,
    bytes_ptr: *const u8,
    bytes_len: usize,
) -> NeziaBufferId {
    guard_buffer(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaBufferId::INVALID;
        };
        if bytes_len == 0 || bytes_ptr.is_null() {
            return NeziaBufferId::INVALID;
        }
        // SAFETY: 呼出側契約により bytes_ptr は bytes_len バイトの有効領域。
        let bytes = unsafe { slice::from_raw_parts(bytes_ptr, bytes_len) };
        match engine.inner.load_from_memory(bytes) {
            Ok(id) => NeziaBufferId::from_core(id),
            Err(_) => NeziaBufferId::INVALID,
        }
    })
}

/// 既にデコード済みの PCM サンプル列からロードする。失敗時は INVALID。
///
/// Unity 標準 `AudioClip.GetData()` 結果を渡す移行期間用ブリッジ。
/// `samples_ptr` はインターリーブ形式の f32 PCM（ステレオなら `[L0, R0, L1, R1, ...]`）。
///
/// # 安全性
/// `samples_ptr` は `samples_len` 要素分の有効領域を指すこと。
/// `samples_len` は `channels` の倍数であること（呼出側責任）。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_buffer_load_from_pcm(
    engine: *mut NeziaEngine,
    samples_ptr: *const f32,
    samples_len: usize,
    channels: u16,
    sample_rate: u32,
) -> NeziaBufferId {
    guard_buffer(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaBufferId::INVALID;
        };
        if channels == 0 || sample_rate == 0 {
            return NeziaBufferId::INVALID;
        }
        if samples_len == 0 {
            return NeziaBufferId::from_core(engine.inner.load_from_pcm(
                Vec::new(),
                channels,
                sample_rate,
            ));
        }
        if samples_ptr.is_null() {
            return NeziaBufferId::INVALID;
        }
        // SAFETY: 呼出側契約により samples_ptr は samples_len 要素分の有効領域。
        let samples = unsafe { slice::from_raw_parts(samples_ptr, samples_len) }.to_vec();
        NeziaBufferId::from_core(engine.inner.load_from_pcm(samples, channels, sample_rate))
    })
}

/// バッファをアンロードする。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_buffer_unload(
    engine: *mut NeziaEngine,
    buffer: NeziaBufferId,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine.inner.unload(buffer.to_core()) {
            NeziaResult::Ok
        } else {
            NeziaResult::InvalidHandle
        }
    })
}
