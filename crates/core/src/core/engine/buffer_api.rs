use std::path::Path;

use crate::buffer_pool::BufferId;

use super::SoundEngine;
use super::buffer_reader::BufferReader;

impl SoundEngine {
    /// オーディオファイルをロードし、ハンドルを返す。
    pub fn load<P: AsRef<Path>>(
        &mut self,
        path: P,
    ) -> Result<BufferId, Box<dyn std::error::Error>> {
        self.buffer_pool.load(path)
    }

    /// メモリ上のエンコード済みバイト列からロードし、ハンドルを返す。
    ///
    /// 統合層からの主要ロード経路。`NeziaAudioClip` の保持バイト列、Addressables、
    /// `UnityWebRequest` などホスト側で取得したバイト列をそのままデコードする。
    pub fn load_from_memory(
        &mut self,
        bytes: &[u8],
    ) -> Result<BufferId, Box<dyn std::error::Error>> {
        self.buffer_pool.load_from_memory(bytes)
    }

    /// 既にデコード済みの PCM サンプル列からロードし、ハンドルを返す。
    ///
    /// Unity 標準 `AudioClip.GetData()` 結果のような、ホスト側で既に展開済みの
    /// PCM を Nezia バッファに取り込む経路（移行期間用ブリッジ）。
    /// `samples` はインターリーブ形式（ステレオなら `[L0, R0, L1, R1, ...]`）。
    pub fn load_from_pcm(
        &mut self,
        samples: Vec<f32>,
        channels: u16,
        sample_rate: u32,
    ) -> BufferId {
        self.buffer_pool
            .load_from_pcm(samples, channels, sample_rate)
    }

    /// バッファをアンロードする。
    pub fn unload(&mut self, id: BufferId) -> bool {
        self.buffer_pool.unload(id)
    }

    /// 指定バッファに対する読み取り専用ハンドルを開く。
    ///
    /// `BufferReader` は内部で `Arc<AudioBuffer>` を保持するため、main thread が
    /// `unload(id)` してもハンドルが生きている間はバッファのメモリは解放されない。
    /// **任意のスレッドから `read_frames` を呼べる** のが特徴で、Unity の
    /// `AudioClip.Create(stream: true, pcmReadCallback)` のように、main thread と
    /// 別のスレッドから PCM をストリーム供給したいケース向け。
    pub fn open_buffer_reader(&self, id: BufferId) -> Option<BufferReader> {
        let index = self.buffer_pool.resolve(id)? as usize;
        let snapshot = self.buffer_pool.shared_snapshot();
        let buf = snapshot.get(index).and_then(|slot| slot.clone())?;
        Some(BufferReader { buffer: buf })
    }
}
