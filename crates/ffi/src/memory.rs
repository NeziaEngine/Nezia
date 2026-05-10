//! メモリ計測 FFI。
//!
//! `nezia_engine_get_memory_stats` で `NeziaMemoryStats` 全項目を一度に取り出す。
//! グローバルアロケータ (`TrackingAllocator`) は `mem-tracking` feature 有効時のみ
//! cdylib の `#[global_allocator]` に登録され、`heap_bytes_in_use` 等が live で更新される。

use nezia_core::NeziaMemoryStats;

use crate::engine::NeziaEngine;
use crate::panic::guard_result;
use crate::types::NeziaResult;

/// `mem-tracking` feature 有効時のみ cdylib にグローバルアロケータを登録する。
///
/// rlib / staticlib として他クレートに静的リンクされた場合はホスト側の
/// `#[global_allocator]` 設定が優先されるため、ここでの登録は cdylib 経由でのみ
/// 効果を持つ。
#[cfg(feature = "mem-tracking")]
#[global_allocator]
static GLOBAL: nezia_core::TrackingAllocator = nezia_core::TrackingAllocator::new();

/// C ABI ミラー。`NeziaMemoryStats` と同じレイアウト (`#[repr(C)]`) なので
/// メンバを 1 対 1 でコピーする。bool は C99 `_Bool` (= 1 byte) と揃える。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NeziaMemoryStatsC {
    pub heap_bytes_in_use: u64,
    pub heap_bytes_peak: u64,
    pub alloc_count: u64,
    pub free_count: u64,
    /// 0 = 無効 (`mem-tracking` feature 無効 or staticlib リンク等) / 1 = 有効。
    pub heap_tracked: u8,
    pub voices_bytes: u64,
    pub buffers_bytes: u64,
    pub effects_bytes: u64,
    pub graph_bytes: u64,
}

impl From<NeziaMemoryStats> for NeziaMemoryStatsC {
    fn from(s: NeziaMemoryStats) -> Self {
        Self {
            heap_bytes_in_use: s.heap_bytes_in_use,
            heap_bytes_peak: s.heap_bytes_peak,
            alloc_count: s.alloc_count,
            free_count: s.free_count,
            heap_tracked: u8::from(s.heap_tracked),
            voices_bytes: s.voices_bytes,
            buffers_bytes: s.buffers_bytes,
            effects_bytes: s.effects_bytes,
            graph_bytes: s.graph_bytes,
        }
    }
}

/// Nezia エンジンのメモリ使用量スナップショットを取得する。
///
/// - `heap_*` / `alloc_count` / `free_count`: cdylib をビルドした場合のみ有効
///   (`heap_tracked = 1`)。rlib / staticlib リンク時は 0 + `heap_tracked = 0`。
/// - `voices_bytes` / `buffers_bytes` / `effects_bytes` / `graph_bytes`:
///   各サブシステムが確保している `Vec` / `Box<[T]>` の capacity ベースの実バイト合計。
///   常時取得可能。
///
/// 取得コストは μs 未満。任意スレッドから呼べる。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_engine_get_memory_stats(
    engine: *mut NeziaEngine,
    out_stats: *mut NeziaMemoryStatsC,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_ref() }) else {
            return NeziaResult::NullPointer;
        };
        if out_stats.is_null() {
            return NeziaResult::NullPointer;
        }
        let stats: NeziaMemoryStatsC = engine.inner.memory_stats().into();
        unsafe { *out_stats = stats };
        NeziaResult::Ok
    })
}
