use std::fs::File;
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::streaming::StreamingState;

/// オーディオバッファ。静的にデコード済みの PCM、または進行中のストリーミング供給を表す。
///
/// **公開 API は静的・streaming で同形** (`channels` / `sample_rate` / `frame_count` /
/// `is_streaming`)。内部表現は `AudioBufferInner` で分離する。
///
/// ## 静的バッファ
/// 全 PCM をインターリーブ形式で保持する。効果音などフルロードするユースケース向け。
///
/// ## ストリーミングバッファ
/// 部分デコードされた PCM をミラーリングバッファ経由で供給する。
/// デコードはバックグラウンドワーカが担当 (`crate::streaming::worker`)。
/// 詳細は `docs/design/core/streaming.md` を参照。
pub struct AudioBuffer {
    /// チャンネル数 (1 = モノラル, 2 = ステレオ)。
    pub channels: u16,
    /// サンプルレート (Hz)。
    pub sample_rate: u32,
    inner: AudioBufferInner,
}

enum AudioBufferInner {
    /// インターリーブされた PCM サンプル (f32) を全保持。
    Static(Vec<f32>),
    /// ストリーミング: ワーカが供給するミラーバッファへのハンドル。
    Streaming(Arc<StreamingState>),
}

impl AudioBuffer {
    /// 静的バッファ: 総フレーム数 (= samples.len() / channels)。
    /// ストリーミングバッファ: 0 を返す (総フレーム数は不明、worker 側で管理)。
    #[must_use]
    pub fn frame_count(&self) -> usize {
        match &self.inner {
            AudioBufferInner::Static(s) => {
                if self.channels == 0 {
                    0
                } else {
                    s.len() / self.channels as usize
                }
            }
            AudioBufferInner::Streaming(_) => 0,
        }
    }

    /// ストリーミングバッファかどうか。
    #[inline]
    #[must_use]
    pub fn is_streaming(&self) -> bool {
        matches!(self.inner, AudioBufferInner::Streaming(_))
    }

    /// 静的バッファのサンプル列への参照。streaming の場合は None。
    ///
    /// ミキシングシステムが random access (looping wrap) を行うために使う。
    /// streaming は `streaming_state()` 経由でリングから読む。
    #[must_use]
    pub(crate) fn static_samples(&self) -> Option<&[f32]> {
        match &self.inner {
            AudioBufferInner::Static(s) => Some(s),
            AudioBufferInner::Streaming(_) => None,
        }
    }

    /// ストリーミング状態への参照。静的の場合は None。
    #[must_use]
    pub(crate) fn streaming_state(&self) -> Option<&Arc<StreamingState>> {
        match &self.inner {
            AudioBufferInner::Streaming(s) => Some(s),
            AudioBufferInner::Static(_) => None,
        }
    }

    /// 既にデコード済みの PCM サンプル列から `AudioBuffer` を構築する。
    ///
    /// Unity の `AudioClip.GetData()` 結果のような、ホスト側で既にデコード済みの
    /// データを Nezia バッファに取り込む経路で利用する。
    ///
    /// `samples` はインターリーブ形式 (ステレオなら `[L0, R0, L1, R1, ...]`)。
    /// `channels` は 1 以上、`sample_rate` も 1 以上を想定する。
    pub fn from_pcm(samples: Vec<f32>, channels: u16, sample_rate: u32) -> Self {
        Self {
            channels,
            sample_rate,
            inner: AudioBufferInner::Static(samples),
        }
    }

    /// ストリーミング用 `AudioBuffer` を構築する (内部 API)。
    pub(crate) fn from_streaming(
        state: Arc<StreamingState>,
        channels: u16,
        sample_rate: u32,
    ) -> Self {
        Self {
            channels,
            sample_rate,
            inner: AudioBufferInner::Streaming(state),
        }
    }
}

/// オーディオファイルを読み込み、デコードして `AudioBuffer` を返す。
///
/// Symphonia が対応するフォーマット (MP3, WAV, FLAC, OGG Vorbis 等) を
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
    /// 総フレーム数 (チャンネル数で割る前のサンプル数)。
    /// コンテナがフレーム数を持たない場合は 0。
    pub total_frames: u64,
}

/// メモリ上のバイト列からオーディオメタデータのみを取得する (フルデコードしない)。
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

    let mut probed = symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;

    // iTunSMPB タグを ID3v2 メタデータから読む (symphonia 0.5.5 は MP3 demuxer 内で
    // iTunSMPB を `delay`/`padding` に反映しないため、ここで補完する)。
    let itunsmpb = read_itunsmpb_from_probed(&mut probed);

    let mut format = probed.format;

    // format reader 側 (パケット読み込み中に出現する ID3v2) も念のため確認。
    let itunsmpb = itunsmpb.or_else(|| read_itunsmpb_from_format(&mut format));

    let track = format.default_track().ok_or("no audio track found")?;

    let channels = track
        .codec_params
        .channels
        .map(|ch| ch.count() as u16)
        .unwrap_or(2);
    let sample_rate = track.codec_params.sample_rate.unwrap_or(44100);
    let track_id = track.id;
    let mut delay_frames = track.codec_params.delay.unwrap_or(0) as usize;
    let mut padding_frames = track.codec_params.padding.unwrap_or(0) as usize;
    if let Some((priming, padding)) = itunsmpb {
        // タグ未指定の場合のみ iTunSMPB を採用 (LAME タグが優先)。
        if delay_frames == 0 {
            delay_frames = priming as usize;
        }
        if padding_frames == 0 {
            padding_frames = padding as usize;
        }
    }
    let n_frames_hint = track.codec_params.n_frames.map(|n| n as usize);

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

    let trimmed = trim_priming_padding(
        all_samples,
        channels,
        delay_frames,
        padding_frames,
        n_frames_hint,
    );
    Ok(AudioBuffer::from_pcm(trimmed, channels, sample_rate))
}

/// iTunSMPB タグ (Apple iTunes/Music が MP3 に書き込む gapless 再生用メタデータ) から
/// `(priming_frames, padding_frames)` を読み出す。
///
/// iTunSMPB は ID3v2 COMM フレームに格納された 12 個のスペース区切り 8/16 桁 hex 値で、
/// 2 番目のフィールドが encoder delay (priming)、3 番目が encoder padding。例:
///
/// ```text
/// " 00000000 00000210 0000087C 000000000008BF74 ..."
///              ^priming  ^padding
/// ```
///
/// symphonia 0.5.5 は MP3 demuxer 内で iTunSMPB を `CodecParameters::{delay, padding}` に
/// 反映しないため (LAME タグのみ対応)、ここで補完して Apple Music 互換の seamless 再生を
/// 実現する。
fn read_itunsmpb_from_probed(
    probed: &mut symphonia::core::probe::ProbeResult,
) -> Option<(u32, u32)> {
    let metadata = probed.metadata.get()?;
    let rev = metadata.current()?;
    find_itunsmpb_in_revision(rev)
}

fn read_itunsmpb_from_format(
    format: &mut Box<dyn symphonia::core::formats::FormatReader>,
) -> Option<(u32, u32)> {
    let metadata = format.metadata();
    let rev = metadata.current()?;
    find_itunsmpb_in_revision(rev)
}

fn find_itunsmpb_in_revision(rev: &symphonia::core::meta::MetadataRevision) -> Option<(u32, u32)> {
    use symphonia::core::meta::Value;
    // symphonia 0.5.5 の ID3v2 reader は COMM フレームの description フィールド
    // ("iTunSMPB" 等の識別子) を捨てるため、key 一致では判別できない。代わりに
    // value の形式 (12 個のスペース区切り hex、4 番目が 16 桁) で識別する。
    for tag in rev.tags() {
        let s = match &tag.value {
            Value::String(s) => s.as_str(),
            _ => continue,
        };
        if let Some(result) = parse_itunsmpb(s) {
            return Some(result);
        }
    }
    None
}

/// iTunSMPB の文字列表現から `(priming, padding)` を解析する。
///
/// iTunSMPB は 12 個のスペース区切り hex フィールドからなり、ユニークな署名:
/// - フィールド 0..3, 4..12: 8 桁 hex
/// - フィールド 3: 16 桁 hex (元音声の総サンプル数)
///
/// この形式に厳密一致しない文字列 (iTunNORM, 通常コメント等) は弾く。
fn parse_itunsmpb(s: &str) -> Option<(u32, u32)> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() != 12 {
        return None;
    }
    // 形式署名チェック: 4 番目だけが 16 桁、残りは 8 桁。
    if parts[3].len() != 16 {
        return None;
    }
    if parts
        .iter()
        .enumerate()
        .any(|(i, p)| i != 3 && p.len() != 8)
    {
        return None;
    }
    if parts
        .iter()
        .any(|p| !p.chars().all(|c| c.is_ascii_hexdigit()))
    {
        return None;
    }
    let priming = u32::from_str_radix(parts[1], 16).ok()?;
    let padding = u32::from_str_radix(parts[2], 16).ok()?;
    Some((priming, padding))
}

/// コーデックがデコード結果に挿入する priming (先頭) / padding (末尾) サンプルを取り除く。
///
/// MDCT overlap-add やフレーム長境界揃えのための無音であり、`CodecParameters::{delay, padding}`
/// の指示通り再生時にスキップする責務は呼出側にある。trim しないとループ再生時に
/// 「本来音 → 末尾無音 → 先頭無音 → 本来音」となり、ループ境界でクリック音が発生する
/// (静的バッファのギャップレス再生問題)。
///
/// 業界標準 (Unity / Wwise / FMOD) と同じく **コーデックのメタデータに従った範囲のみ trim** する。
/// シームレスループには loop-ready な形式 (Vorbis / WAV / Opus) と適切な authoring が必要で、
/// 任意 MP3 を seamless にする処理 (silence 検出, crossfade) は意図的に行わない。
///
/// 1. **タグベース trim** — `delay`/`padding` が指定されていれば仕様通り削る (LAME-tagged MP3 等)。
/// 2. **n_frames ベース truncation** — タグが無くても symphonia が「再生すべき総フレーム数」を
///    `n_frames` に格納していれば、それを超える末尾余剰 (encoder padding) を切り詰める。
fn trim_priming_padding(
    mut samples: Vec<f32>,
    channels: u16,
    delay_frames: usize,
    padding_frames: usize,
    n_frames_hint: Option<usize>,
) -> Vec<f32> {
    let ch = channels.max(1) as usize;

    // Step 1: タグから読み取れた値を優先して trim。
    let head = delay_frames.saturating_mul(ch).min(samples.len());
    if head > 0 {
        samples.drain(..head);
    }
    let tail = padding_frames.saturating_mul(ch).min(samples.len());
    if tail > 0 {
        samples.truncate(samples.len() - tail);
    }

    // Step 2: `n_frames` が示す長さを超える分は末尾余剰 (encoder padding) として切り詰める。
    if let Some(target_frames) = n_frames_hint {
        let target_samples = target_frames.saturating_mul(ch);
        if samples.len() > target_samples {
            samples.truncate(target_samples);
        }
    }

    samples
}
