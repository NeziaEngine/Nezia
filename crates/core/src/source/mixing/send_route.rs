//! Source 起点 Send (User-Defined Aux Send) の宛先解決と書き出し。
//!
//! 宛先は `BusWorld::mix_buffer` の一部か `CompressorWorld::sidechain_buffer` の一部。
//! `apply_send_outputs` で post-spatial mono 信号を `(left_gain, right_gain) × gain`
//! で interleaved に加算ミックスする。raw ptr を使うのは「宛先と本線 bus_buf が重複しないこと」
//! (source の `output_bus_dense` と send `dest_dense` が異なる、もしくは `CompressorWorld`
//! 側) を呼出側で保証しているため、Rust の借用チェッカでは表現できない 2 個目以上の
//! `&mut [f32]` を持ち回るのを避けるため。

#[derive(Copy, Clone)]
pub(super) struct SendOutput {
    pub(super) dest_ptr: *mut f32,
    pub(super) dest_len: usize,
    pub(super) gain: f32,
}

impl SendOutput {
    pub(super) const NULL: SendOutput = SendOutput {
        dest_ptr: std::ptr::null_mut(),
        dest_len: 0,
        gain: 0.0,
    };
}

/// `mono_scratch[..total_frames]` を `(left_gain, right_gain) × send.gain` で interleaved に
/// 各 send 宛先へ加算する。`device_channels >= 3` のときは L/R 平均をその他チャネルに流す
/// (本線 bus_buf と同一規約)。
#[inline]
pub(super) fn apply_send_outputs(
    mono_scratch: &[f32],
    total_frames: usize,
    left_gain: f32,
    right_gain: f32,
    device_channels: usize,
    send_outputs: &[SendOutput],
) {
    if send_outputs.is_empty() {
        return;
    }
    for out in send_outputs {
        if out.dest_ptr.is_null() || out.dest_len == 0 {
            continue;
        }
        // SAFETY: 呼出側が dest_ptr/dest_len の有効性と非エイリアスを保証する。
        let dest = unsafe { std::slice::from_raw_parts_mut(out.dest_ptr, out.dest_len) };
        let l = left_gain * out.gain;
        let r = right_gain * out.gain;
        for (n, frame) in dest
            .chunks_mut(device_channels)
            .take(total_frames)
            .enumerate()
        {
            let s = mono_scratch[n];
            for (ch, slot) in frame.iter_mut().enumerate() {
                let g = match ch {
                    0 => l,
                    1 => r,
                    _ => (l + r) * 0.5,
                };
                *slot += s * g;
            }
        }
    }
}
