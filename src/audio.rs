use std::fs::File;
use std::path::Path;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
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
}

/// オーディオファイルを読み込み、デコードして `AudioBuffer` を返す。
///
/// Symphonia が対応するフォーマット（MP3, WAV, FLAC, OGG Vorbis 等）を
/// 自動判別してデコードする。
pub fn load<P: AsRef<Path>>(path: P) -> Result<AudioBuffer, Box<dyn std::error::Error>> {
    let file = File::open(path.as_ref())?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.as_ref().extension().and_then(|e| e.to_str()) {
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
