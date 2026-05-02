use std::path::Path;
use std::sync::Arc;

use arc_swap::ArcSwap;

use crate::audio::{self, AudioBuffer};

/// バッファスロットの最大数。
const MAX_BUFFERS: usize = 1024;

/// AudioBuffer を識別するハンドル。
///
/// generation によってスロット再利用時の無効化を検出する。
/// ECS の EntityId とは独立した型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BufferId {
    pub index: u32,
    pub generation: u32,
}

/// AudioBuffer のジェネレーション付きスロット管理。
///
/// スロットは安定したインデックスを持ち、削除しても詰めない。
/// generation によって古いハンドルの無効化を検出する。
/// ボイスプールのスパースセットと異なり dense packing は行わない。
/// バッファはランダムアクセスが主であり、一括イテレーションの
/// キャッシュ効率は不要なため。
pub struct AudioBufferPool {
    slots: Vec<BufferSlot>,
    buffers: Vec<Option<Arc<AudioBuffer>>>,
    shared: Arc<ArcSwap<Vec<Option<Arc<AudioBuffer>>>>>,
    free_list: Vec<u32>,
    next_index: u32,
}

#[derive(Clone, Copy)]
struct BufferSlot {
    generation: u32,
    occupied: bool,
}

impl AudioBufferPool {
    pub fn new(shared: Arc<ArcSwap<Vec<Option<Arc<AudioBuffer>>>>>) -> Self {
        Self {
            slots: Vec::with_capacity(MAX_BUFFERS),
            buffers: Vec::with_capacity(MAX_BUFFERS),
            shared,
            free_list: Vec::new(),
            next_index: 0,
        }
    }

    /// オーディオファイルをロードし、ハンドルを返す。
    pub fn load<P: AsRef<Path>>(
        &mut self,
        path: P,
    ) -> Result<BufferId, Box<dyn std::error::Error>> {
        let buffer = Arc::new(audio::load(path)?);
        Ok(self.insert(buffer))
    }

    /// メモリ上のエンコード済みバイト列からロードし、ハンドルを返す。
    ///
    /// `NeziaAudioClip.encodedBytes` / Resources / Addressables / WebRequest 経由の
    /// バイト列を直接デコードする経路。Symphonia がフォーマットを自動判別する。
    pub fn load_from_memory(
        &mut self,
        bytes: &[u8],
    ) -> Result<BufferId, Box<dyn std::error::Error>> {
        let buffer = Arc::new(audio::load_from_memory(bytes)?);
        Ok(self.insert(buffer))
    }

    /// 既にデコード済みの PCM サンプル列からロードし、ハンドルを返す。
    ///
    /// Unity の `AudioClip.GetData()` 結果を直接アップロードする経路（移行用ブリッジ）。
    /// `samples` はインターリーブ形式。
    pub fn load_from_pcm(
        &mut self,
        samples: Vec<f32>,
        channels: u16,
        sample_rate: u32,
    ) -> BufferId {
        let buffer = Arc::new(AudioBuffer::from_pcm(samples, channels, sample_rate));
        self.insert(buffer)
    }

    /// バッファをアンロードする。
    ///
    /// 再生中のボイスがこのバッファを参照していた場合、
    /// 次の update で自動的に despawn される。
    pub fn unload(&mut self, id: BufferId) -> bool {
        let Some(slot) = self.slots.get_mut(id.index as usize) else {
            return false;
        };
        if slot.generation != id.generation || !slot.occupied {
            return false;
        }

        self.buffers[id.index as usize] = None;
        slot.generation += 1;
        slot.occupied = false;
        self.free_list.push(id.index);
        self.sync_shared();
        true
    }

    /// 共有 buffers のスナップショットを取得する。
    ///
    /// `BufferReader` 等が任意スレッドから直接バッファを参照するために使う。
    /// 内部 `ArcSwap` のロード（lock-free）。
    pub fn shared_snapshot(&self) -> arc_swap::Guard<Arc<Vec<Option<Arc<AudioBuffer>>>>> {
        self.shared.load()
    }

    /// ハンドルを検証し、有効ならスロットインデックスを返す。
    pub fn resolve(&self, id: BufferId) -> Option<u32> {
        let slot = self.slots.get(id.index as usize)?;
        if slot.generation != id.generation || !slot.occupied {
            return None;
        }
        Some(id.index)
    }

    /// デコード済みバッファをスロットに格納してハンドルを返す内部実装。
    fn insert(&mut self, buffer: Arc<AudioBuffer>) -> BufferId {
        let (index, generation) = self.allocate_slot();
        self.buffers[index as usize] = Some(buffer);
        self.sync_shared();
        BufferId { index, generation }
    }

    fn allocate_slot(&mut self) -> (u32, u32) {
        if let Some(index) = self.free_list.pop() {
            let generation = self.slots[index as usize].generation;
            self.slots[index as usize] = BufferSlot {
                generation,
                occupied: true,
            };
            (index, generation)
        } else {
            let index = self.next_index;
            self.next_index += 1;
            self.slots.push(BufferSlot {
                generation: 0,
                occupied: true,
            });
            self.buffers.push(None);
            (index, 0)
        }
    }

    fn sync_shared(&self) {
        self.shared.store(Arc::new(self.buffers.clone()));
    }
}
