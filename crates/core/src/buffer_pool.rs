use std::path::Path;
use std::sync::Arc;

use arc_swap::ArcSwap;

use crate::audio::{self, AudioBuffer};
use crate::streaming::{StreamCmd, StreamingHandle, StreamingOpts, spawn_streaming_worker};

/// バッファスロットの最大数。
const MAX_BUFFERS: usize = 1024;

/// AudioBuffer を識別するハンドル。
///
/// generation によってスロット再利用時の無効化を検出する。
/// ECS の EntityId とは独立した型。
///
/// `#[repr(C)]` は FFI 層 (`NeziaBufferId`) とのゼロコピー slice cast 用
/// (例: `nezia_container_create_random` が `&[NeziaBufferId]` をそのまま `&[BufferId]`
/// として core に渡すために必要)。
#[repr(C)]
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
    /// streaming スロットの worker handle (index = buffer slot index)。
    /// 静的バッファのスロットは None。`unload` 時に worker を join する。
    streaming_handles: Vec<Option<StreamingHandle>>,
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
            streaming_handles: Vec::with_capacity(MAX_BUFFERS),
            free_list: Vec::new(),
            next_index: 0,
        }
    }

    /// プール全体のヒープ実バイト数 (`memory_stats` walker 用)。
    /// slot 管理 Vec + 各 `AudioBuffer` の PCM/リング実バイトを合算する。
    /// `Arc<AudioBuffer>` のメタ部分は 1 件あたり数十バイトなので無視。
    pub(crate) fn memory_bytes(&self) -> usize {
        use crate::memory::vec_cap_bytes;
        let mut total = vec_cap_bytes(&self.slots)
            + vec_cap_bytes(&self.buffers)
            + vec_cap_bytes(&self.streaming_handles)
            + vec_cap_bytes(&self.free_list);
        for b in self.buffers.iter().flatten() {
            total += b.payload_bytes();
        }
        total
    }

    /// オーディオファイルをストリーミング再生用にロードする (Phase 2-4)。
    ///
    /// バックグラウンドのデコードワーカを spawn し、`AudioBuffer::Streaming` を
    /// 同じ `BufferId` 空間のスロットに格納する。`play_with_handle` 等の既存 API は
    /// 静的・streaming の区別なく利用できる。
    /// 詳細は `docs/design/core/streaming.md` を参照。
    pub fn load_streaming<P: AsRef<Path>>(
        &mut self,
        path: P,
        opts: StreamingOpts,
    ) -> Result<BufferId, Box<dyn std::error::Error>> {
        let handle = spawn_streaming_worker(path, opts)?;
        let channels = handle.channels;
        let sample_rate = handle.sample_rate;
        let state = Arc::clone(&handle.state);
        let buffer = Arc::new(AudioBuffer::from_streaming(state, channels, sample_rate));
        let id = self.insert_with_streaming(buffer, Some(handle));
        Ok(id)
    }

    /// streaming source の seek (worker に転送)。
    pub fn seek_streaming(&self, id: BufferId, frame_offset: u64) {
        let Some(index) = self.resolve(id) else {
            return;
        };
        if let Some(Some(h)) = self.streaming_handles.get(index as usize) {
            h.send(StreamCmd::Seek(frame_offset));
        }
    }

    /// streaming source のループ region を更新する。`looping=true` で全体ループ、
    /// `false` でループ無効。Phase 2-4 では全体ループのみ実装。
    pub fn set_streaming_loop(&self, id: BufferId, looping: bool) {
        let Some(index) = self.resolve(id) else {
            return;
        };
        if let Some(Some(h)) = self.streaming_handles.get(index as usize) {
            let region = if looping {
                Some(crate::streaming::LoopRegion {
                    start: 0,
                    end: u64::MAX,
                })
            } else {
                None
            };
            h.send(StreamCmd::SetLoopRegion(region));
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
    /// streaming バッファの場合はワーカに Stop を送り join する (数十 ms ブロックする可能性あり)。
    pub fn unload(&mut self, id: BufferId) -> bool {
        // 検証 (slot 借用は最小範囲)。
        {
            let Some(slot) = self.slots.get(id.index as usize) else {
                return false;
            };
            if slot.generation != id.generation || !slot.occupied {
                return false;
            }
        }

        self.buffers[id.index as usize] = None;
        // streaming worker を停止 (取り出して shutdown 経由で join)。
        if let Some(slot) = self.streaming_handles.get_mut(id.index as usize)
            && let Some(handle) = slot.take()
        {
            handle.shutdown();
        }
        let slot = &mut self.slots[id.index as usize];
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
        self.insert_with_streaming(buffer, None)
    }

    /// streaming worker handle 付きでバッファを格納する内部実装。
    fn insert_with_streaming(
        &mut self,
        buffer: Arc<AudioBuffer>,
        streaming: Option<StreamingHandle>,
    ) -> BufferId {
        let (index, generation) = self.allocate_slot();
        let id = BufferId { index, generation };
        // streaming の場合、サウンドスレッドが Event::StreamingUnderrun 発火時に
        // 正しい BufferId (index + generation) を載せられるよう、状態に書き込んでおく。
        // sync_shared でサウンドスレッドから可視になる前にここで書き込むこと。
        if let Some(state) = buffer.streaming_state() {
            state.set_buffer_id(id);
        }
        self.buffers[index as usize] = Some(buffer);
        self.streaming_handles[index as usize] = streaming;
        self.sync_shared();
        id
    }

    fn allocate_slot(&mut self) -> (u32, u32) {
        if let Some(index) = self.free_list.pop() {
            let generation = self.slots[index as usize].generation;
            self.slots[index as usize] = BufferSlot {
                generation,
                occupied: true,
            };
            // 旧 streaming handle が残っていることはない (unload で take 済み) が念のため。
            self.streaming_handles[index as usize] = None;
            (index, generation)
        } else {
            let index = self.next_index;
            self.next_index += 1;
            self.slots.push(BufferSlot {
                generation: 0,
                occupied: true,
            });
            self.buffers.push(None);
            self.streaming_handles.push(None);
            (index, 0)
        }
    }

    fn sync_shared(&self) {
        self.shared.store(Arc::new(self.buffers.clone()));
    }
}
