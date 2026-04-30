/// メインスレッドからサウンドスレッドへ送るコマンド。
///
/// リングバッファ経由で送信されるため、すべてのバリアントは
/// 固定サイズかつ `Copy` でなければならない。
#[derive(Debug, Clone, Copy)]
pub enum Command {
    /// マスター音量を設定する（0.0〜1.0）。
    SetVolume(f32),
    /// ボイスを再生する。`audio_buffer_index` で AudioBuffer を指定する。
    Play {
        audio_buffer_index: u32,
        vol: f32,
        pitch: f32,
    },
    /// すべてのボイスを停止する。
    StopAll,
}
