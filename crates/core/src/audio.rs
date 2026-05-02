use std::fs::File;
use std::io::Cursor;
use std::path::Path;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// デコード済みの PCM データ。
///
/// 全サンプルをインターリーブ形式でメモリ上に展開して保持する。
/// ストリーミング再生ではなく、効果音のように全体をメモリに載せるユースケース向け。
pub struct AudioBuffer {
    /// インターリーブされた PCM サンプル（f32）。
    /// ステレオの場合: [L0, R0, L1, R1, ...]
    pub samples: Vec<f32>,
    /// チャンネル数（1 = モノラル, 2 = ステレオ）。
    pub channels: u16,
    /// サンプルレート（Hz）。
    pub sample_rate: u32,
}

impl AudioBuffer {
    /// 総フレーム数（= samples.len() / channels）。
    pub fn frame_count(&self) -> usize {
        self.samples.len() / self.channels as usize
    }

    /// 既にデコード済みの PCM サンプル列から `AudioBuffer` を構築する。
    ///
    /// Unity の `AudioClip.GetData()` 結果のような、ホスト側で既にデコード済みの
    /// データを Nezia バッファに取り込む経路で利用する。
    ///
    /// `samples` はインターリーブ形式（ステレオなら `[L0, R0, L1, R1, ...]`）。
    /// `channels` は 1 以上、`sample_rate` も 1 以上を想定する。
    pub fn from_pcm(samples: Vec<f32>, channels: u16, sample_rate: u32) -> Self {
        Self {
            samples,
            channels,
            sample_rate,
        }
    }
}

/// オーディオファイルを読み込み、デコードして `AudioBuffer` を返す。
///
/// Symphonia が対応するフォーマット（MP3, WAV, FLAC, OGG Vorbis 等）を
/// 自動判別してデコードする。
pub fn load<P: AsRef<Path>>(path: P) -> Result<AudioBuffer, Box<dyn std::error::Error>> {
    let file = File::open(path.as_ref())?;
    let extension_hint = path
        .as_ref()
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_string());
    decode(Box::new(file), extension_hint.as_deref())
}

/// オーディオデータをメモリ上のバイト列から読み込み、デコードして `AudioBuffer` を返す。
///
/// `Resources` / `Addressables` / `UnityWebRequest` 等で取得したエンコード済みバイト列や、
/// `NeziaAudioClip` が保持する元ファイルバイト列をそのままデコードする経路。
/// バイト列はコピーされるため、呼出後に `bytes` を解放してよい。
pub fn load_from_memory(bytes: &[u8]) -> Result<AudioBuffer, Box<dyn std::error::Error>> {
    // symphonia の MediaSource は 'static を要求する。`Cursor<Vec<u8>>` を所有させる。
    let cursor = Cursor::new(bytes.to_vec());
    decode(Box::new(cursor), None)
}

/// オーディオファイルのメタデータ。
#[derive(Debug, Clone, Copy)]
pub struct AudioMetadata {
    pub sample_rate: u32,
    pub channels: u16,
    /// 総フレーム数（チャンネル数で割る前のサンプル数）。
    /// コンテナがフレーム数を持たない場合は 0。
    pub total_frames: u64,
}

/// メモリ上のバイト列からオーディオメタデータのみを取得する（フルデコードしない）。
///
/// `NeziaAudioImporter` が ScriptableObject 化する際に sample rate / channels /
/// total frames を埋めるために使う。`AudioBuffer::create_audio_clip_proxy()` 相当の
/// API も同じメタデータでサイズが決まる。
pub fn peek_metadata(bytes: &[u8]) -> Result<AudioMetadata, Box<dyn std::error::Error>> {
    let cursor = Cursor::new(bytes.to_vec());
    let mss = MediaSourceStream::new(Box::new(cursor), Default::default());

    let probed = symphonia::default::get_probe().format(
        &Hint::new(),
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;

    let track = probed
        .format
        .default_track()
        .ok_or("no audio track found")?;
    let codec = &track.codec_params;

    Ok(AudioMetadata {
        sample_rate: codec.sample_rate.unwrap_or(0),
        channels: codec.channels.map(|c| c.count() as u16).unwrap_or(0),
        total_frames: codec.n_frames.unwrap_or(0),
    })
}

/// MediaSource からデコードする内部実装。
fn decode(
    source: Box<dyn MediaSource>,
    extension: Option<&str>,
) -> Result<AudioBuffer, Box<dyn std::error::Error>> {
    let mss = MediaSourceStream::new(source, Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = extension {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;

    let mut format = probed.format;

    let track = format.default_track().ok_or("no audio track found")?;

    let channels = track
        .codec_params
        .channels
        .map(|ch| ch.count() as u16)
        .unwrap_or(2);
    let sample_rate = track.codec_params.sample_rate.unwrap_or(44100);
    let track_id = track.id;

    let mut decoder =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;

    let mut all_samples = Vec::new();

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => return Err(e.into()),
        };

        if packet.track_id() != track_id {
            continue;
        }

        let decoded = decoder.decode(&packet)?;
        let spec = *decoded.spec();
        let duration = decoded.capacity();

        let mut sample_buf = SampleBuffer::<f32>::new(duration as u64, spec);
        sample_buf.copy_interleaved_ref(decoded);
        all_samples.extend_from_slice(sample_buf.samples());
    }

    Ok(AudioBuffer {
        samples: all_samples,
        channels,
        sample_rate,
    })
}
