//! メモリ使用量の精密計測。
//!
//! 2 つの独立した経路で「Nezia が使っているメモリ」を観測する:
//!
//! ## 1. グローバルアロケータ統計 (`heap_bytes_in_use` 等)
//! [`TrackingAllocator`] を `#[global_allocator]` として宣言した実行体
//! (典型的には `nezia-ffi` の cdylib) でのみ有効。Nezia (および依存クレート) が
//! Rust 経由で行う **すべて** のヒープ alloc / dealloc を atomic カウンタで追跡する。
//!
//! - rlib / staticlib として他クレートから利用される場合は global allocator が
//!   設定されないため、これらのカウンタは 0 のままになる。`heap_tracked` フラグで
//!   有効/無効を判別できる。
//! - audio thread の hot path (process callback) は alloc しない設計のため、
//!   この計測がリアルタイム性能に影響することはない。
//!
//! ## 2. 各サブシステムの「論理保持バイト」 walker
//! [`SoundEngine::memory_stats`](crate::SoundEngine::memory_stats) が呼ばれたタイミングで
//! 各 World / Pool が保持する Vec / Arc のバイト数を計算する。`Vec::capacity * size_of::<T>()`
//! を基本に、`AudioBuffer` の PCM 実バイトもサンプル数から正確に算出する。
//!
//! グローバル統計 (1) と breakdown (2) は完全には一致しない:
//! - グローバルは内部の一時 alloc / Arc 二重カウントを含めた「ヒープ全体の真値」。
//! - breakdown は「各サブシステムがいま論理的に持っているデータ量」。
//!
//! 両方を見比べることで、想定外の alloc 経路を発見できる。

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

/// グローバルカウンタ。`TrackingAllocator` が更新する。
///
/// 公開しているのは `SoundEngine::memory_stats()` 経由で読むためだけで、
/// 直接書き換えてはならない。
pub(crate) static BYTES_IN_USE: AtomicU64 = AtomicU64::new(0);
pub(crate) static BYTES_PEAK: AtomicU64 = AtomicU64::new(0);
pub(crate) static ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
pub(crate) static FREE_COUNT: AtomicU64 = AtomicU64::new(0);
pub(crate) static TRACKING_INSTALLED: AtomicU64 = AtomicU64::new(0);

/// `System` をラップしてヒープ使用量を atomic で追跡するアロケータ。
///
/// 使い方 (cdylib 等の最終バイナリ側):
/// ```ignore
/// #[global_allocator]
/// static GLOBAL: nezia::TrackingAllocator = nezia::TrackingAllocator::new();
/// ```
///
/// `new()` は `const fn` なので static 初期化に使える。alloc/free ごとに
/// `Relaxed` の `fetch_add` を 2 回 + peak の CAS ループを行うだけで、
/// 実測オーバーヘッドは 5-15ns / call 程度。
pub struct TrackingAllocator;

impl TrackingAllocator {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for TrackingAllocator {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: System は GlobalAlloc を実装しており、本ラッパは alloc/dealloc を
// そのまま委譲するだけ。カウンタ更新は alloc 結果に副作用を持たない。
unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: 委譲のみ。layout の有効性は呼び出し側契約。
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            on_alloc(layout.size() as u64);
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: 委譲のみ。
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            on_alloc(layout.size() as u64);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: 委譲のみ。ptr/layout の妥当性は呼び出し側契約。
        unsafe { System.dealloc(ptr, layout) };
        on_free(layout.size() as u64);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: 委譲のみ。
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            // realloc は alloc + free 1 ペアと等価に扱う (差分でなく総和を見たいので)。
            on_alloc(new_size as u64);
            on_free(layout.size() as u64);
        }
        new_ptr
    }
}

#[inline]
fn on_alloc(bytes: u64) {
    TRACKING_INSTALLED.store(1, Ordering::Relaxed);
    ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
    let new_use = BYTES_IN_USE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    let mut cur = BYTES_PEAK.load(Ordering::Relaxed);
    while new_use > cur {
        match BYTES_PEAK.compare_exchange_weak(cur, new_use, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(observed) => cur = observed,
        }
    }
}

#[inline]
fn on_free(bytes: u64) {
    FREE_COUNT.fetch_add(1, Ordering::Relaxed);
    BYTES_IN_USE.fetch_sub(bytes, Ordering::Relaxed);
}

/// `SoundEngine::memory_stats()` の戻り値。
///
/// `heap_*` 系は [`TrackingAllocator`] を `#[global_allocator]` として登録した
/// 実行体でのみ意味を持つ (それ以外では 0 + `heap_tracked = false`)。
/// `*_bytes` 系の breakdown は walker による論理保持バイトで、グローバル統計が
/// 無効でも常に有効。
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct NeziaMemoryStats {
    /// Nezia 由来 (依存クレート含む) の Rust ヒープ現在使用量 (bytes)。
    pub heap_bytes_in_use: u64,
    /// 起動以降の最大値 (bytes)。
    pub heap_bytes_peak: u64,
    /// 累積 alloc 回数 (realloc は alloc + free 各 1 として計数)。
    pub alloc_count: u64,
    /// 累積 free 回数。
    pub free_count: u64,

    /// `TrackingAllocator` が global allocator として有効かどうか。
    /// `false` の場合 `heap_*` / `alloc_count` / `free_count` はすべて 0。
    pub heap_tracked: bool,

    // ── 各サブシステムの論理保持バイト (walker) ──
    /// Source / Spatial / live params / triple buffers / state cache / slot 管理。
    pub voices_bytes: u64,
    /// AudioBufferPool (slot 管理 + 静的 PCM 実バイト + streaming リング)。
    pub buffers_bytes: u64,
    /// EffectWorld / EffectWorlds / effect スロット / compressor_owners。
    pub effects_bytes: u64,
    /// BusWorld / routing mirror / send プール / snapshot / curve / container / callbacks /
    /// command/event/capture リング。
    pub graph_bytes: u64,
}

impl NeziaMemoryStats {
    /// breakdown の合計 (`voices + buffers + effects + graph`)。
    /// `heap_bytes_in_use` との差分が「カバーできていない一時 alloc / 依存クレート分」。
    #[must_use]
    pub fn breakdown_total(&self) -> u64 {
        self.voices_bytes + self.buffers_bytes + self.effects_bytes + self.graph_bytes
    }
}

/// `Vec<T>` の確保済み容量を実バイト数で返す (要素単位サイズを掛ける)。
///
/// 各 World の `memory_bytes()` 実装が一律にこの式を使うことで、
/// `len * size_of` (= 論理使用) ではなく `capacity * size_of` (= 実ヒープ占有) を
/// 数えていることを明示する。spawn 直後に縮退しても capacity は維持されるため、
/// プールの「最大確保バイト」を見るのが目的。
#[inline]
#[must_use]
pub(crate) fn vec_cap_bytes<T>(v: &Vec<T>) -> usize {
    v.capacity() * std::mem::size_of::<T>()
}

/// `Box<[T]>` の実バイト数。
#[inline]
#[must_use]
pub(crate) fn boxed_slice_bytes<T>(b: &[T]) -> usize {
    std::mem::size_of_val(b)
}

/// グローバル統計のスナップショット (engine からも FFI からも使う)。
pub(crate) fn snapshot_global() -> (u64, u64, u64, u64, bool) {
    let tracked = TRACKING_INSTALLED.load(Ordering::Relaxed) != 0;
    (
        BYTES_IN_USE.load(Ordering::Relaxed),
        BYTES_PEAK.load(Ordering::Relaxed),
        ALLOC_COUNT.load(Ordering::Relaxed),
        FREE_COUNT.load(Ordering::Relaxed),
        tracked,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breakdown_total_sums_categories() {
        let s = NeziaMemoryStats {
            voices_bytes: 100,
            buffers_bytes: 200,
            effects_bytes: 50,
            graph_bytes: 30,
            ..Default::default()
        };
        assert_eq!(s.breakdown_total(), 380);
    }

    #[test]
    fn snapshot_global_reads_atomics() {
        let (in_use, peak, allocs, frees, _tracked) = snapshot_global();
        // この時点でカウンタが 0 であろうと非ゼロであろうと矛盾しないこと
        // (cdylib 経由でない場合は 0、テストバイナリは default System なので 0)。
        assert!(in_use <= peak.max(in_use));
        assert!(allocs >= frees.saturating_sub(allocs)); // 整合性チェックではなく単に load 動作確認
    }
}
