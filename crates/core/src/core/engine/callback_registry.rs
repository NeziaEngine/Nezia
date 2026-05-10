//! 再生終了コールバックの登録テーブル。
//!
//! `play_with_callback` などで採番したトークン（`u32`、`0` はコールバックなしの予約値）と
//! コールバック実体を対応付けて保持する。サウンドスレッドからは
//! `Event::SourceFinished { token }` 経由で完了通知が届くため、メインスレッド側で
//! `complete()` を呼んでコールバックを取り出して実行する。
//!
//! ## データ構造
//!
//! HashMap ではなく **固定サイズのスロット配列**（`[CallbackKind; self.callback_slots]`）を
//! 使う。spawn ごとに `Box::new(closure) + HashMap::insert` していた旧実装と比較し、
//! Native（FFI 経由）コールバックでは spawn ごとの alloc がゼロになる。
//! Rust API 経由の closure は `dyn FnOnce` の動的サイズ要件で `Box` が原理上必要。
//!
//! ## トークン形式
//!
//! `token: u32 = (generation: u16) << 16 | (slot index: u16)`
//!
//! - `generation` はスロット再利用ごとに +1（0 は欠番）。
//!   旧 token を持っている呼出側が新スロットを誤って取り出すのを防ぐ。
//! - `slot index` はスロット配列の直接 index（HashMap lookup 不要）。
//! - 全 generation スロットの初期値を 1 にすることで、有効 token は `>> 16 != 0`
//!   が保証される。`token == 0` は「コールバックなし」の予約値として使える。

use std::ffi::c_void;

/// 1 スロットの中身。
pub(in crate::core) enum CallbackKind {
    /// 未使用。
    Empty,
    /// FFI 経由の関数ポインタ + 不透明 user data（alloc 不要）。
    Native {
        f: unsafe extern "C" fn(*mut c_void),
        /// `*mut c_void` を `Send` 安全に保つため `usize` で保持し、呼び出し直前に復元。
        /// 呼出側が「コールバック発火時まで `user_data` を有効に保つ」契約を守る前提。
        user_data: usize,
    },
    /// Rust API 経由の任意 closure（`dyn FnOnce` 仕様で `Box` は不可避）。
    Rust(Box<dyn FnOnce() + Send>),
}

pub(in crate::core) struct CallbackRegistry {
    slots: Vec<CallbackKind>,
    generation: Vec<u16>,
    free_list: Vec<u16>,
    next_index: u16,
    /// スロット数 = 同時 callback 数の上限 = `EngineConfig::max_sources`。
    callback_slots: usize,
}

impl CallbackRegistry {
    #[cfg(test)]
    pub(super) fn new() -> Self {
        Self::with_capacity(crate::source::DEFAULT_MAX_SOURCES)
    }

    pub(super) fn with_capacity(callback_slots: usize) -> Self {
        let mut slots = Vec::with_capacity(callback_slots);
        for _ in 0..callback_slots {
            slots.push(CallbackKind::Empty);
        }
        Self {
            slots,
            generation: vec![1; callback_slots], // 0 を欠番にして token != 0 を保証
            free_list: Vec::with_capacity(callback_slots),
            next_index: 0,
            callback_slots,
        }
    }

    /// 内部 `Vec` の確保ヒープ実バイト数 (`memory_stats` walker 用)。
    /// `Box<dyn FnOnce>` 個別の確保サイズは追跡不能 (アロケータ側でしか分からない) ので、
    /// ポインタ分のみ計上する。
    pub(crate) fn memory_bytes(&self) -> usize {
        use crate::memory::vec_cap_bytes;
        vec_cap_bytes(&self.slots)
            + vec_cap_bytes(&self.generation)
            + vec_cap_bytes(&self.free_list)
    }

    /// FFI 用: 関数ポインタ + user_data を直接登録する（**alloc なし**）。
    /// 失敗時（スロット枯渇）は `None`。
    pub(super) fn register_native(
        &mut self,
        f: unsafe extern "C" fn(*mut c_void),
        user_data: *mut c_void,
    ) -> Option<u32> {
        let idx = self.alloc_slot()?;
        self.slots[idx as usize] = CallbackKind::Native {
            f,
            user_data: user_data as usize,
        };
        Some(pack_token(self.generation[idx as usize], idx))
    }

    /// Rust API 用: 任意 closure を登録する（`Box` 1 個 alloc）。
    /// 失敗時（スロット枯渇）は `None`。
    pub(super) fn register_rust(&mut self, callback: Box<dyn FnOnce() + Send>) -> Option<u32> {
        let idx = self.alloc_slot()?;
        self.slots[idx as usize] = CallbackKind::Rust(callback);
        Some(pack_token(self.generation[idx as usize], idx))
    }

    /// 登録を取り消す（コマンド push 失敗時のロールバック用）。
    /// generation を進めてスロットを返却する。
    pub(super) fn cancel(&mut self, token: u32) {
        if let Some((g, idx)) = unpack_token(token)
            && self.generation[idx as usize] == g
        {
            self.release_slot(idx);
        }
    }

    /// 完了通知を受けてコールバック実体を取り出す。
    /// stale token / 未登録なら `Empty`。
    pub(super) fn complete(&mut self, token: u32) -> CallbackKind {
        let Some((g, idx)) = unpack_token(token) else {
            return CallbackKind::Empty;
        };
        if self.generation[idx as usize] != g {
            return CallbackKind::Empty;
        }
        let cb = std::mem::replace(&mut self.slots[idx as usize], CallbackKind::Empty);
        self.release_slot(idx);
        cb
    }

    /// 全コールバックを破棄する（`StopAll` 時のクリア用）。
    pub(super) fn clear(&mut self) {
        for i in 0..self.callback_slots {
            if !matches!(self.slots[i], CallbackKind::Empty) {
                self.slots[i] = CallbackKind::Empty;
                bump_generation(&mut self.generation[i]);
            }
        }
        self.free_list.clear();
        self.next_index = 0;
    }

    fn alloc_slot(&mut self) -> Option<u16> {
        if let Some(idx) = self.free_list.pop() {
            return Some(idx);
        }
        if (self.next_index as usize) < self.callback_slots {
            let i = self.next_index;
            self.next_index += 1;
            return Some(i);
        }
        None
    }

    fn release_slot(&mut self, idx: u16) {
        bump_generation(&mut self.generation[idx as usize]);
        self.free_list.push(idx);
    }
}

#[inline]
fn pack_token(generation: u16, idx: u16) -> u32 {
    ((generation as u32) << 16) | (idx as u32)
}

/// `token == 0`（コールバックなし）は `None`。`generation == 0` は欠番のため `None`。
#[inline]
fn unpack_token(token: u32) -> Option<(u16, u16)> {
    if token == 0 {
        return None;
    }
    let g = (token >> 16) as u16;
    if g == 0 {
        return None;
    }
    Some((g, token as u16))
}

#[inline]
fn bump_generation(g: &mut u16) {
    *g = g.wrapping_add(1);
    if *g == 0 {
        *g = 1; // 欠番をスキップ
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::c_void;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn register_rust_complete_roundtrip() {
        let mut reg = CallbackRegistry::new();
        let flag = Arc::new(AtomicBool::new(false));
        let f = flag.clone();
        let token = reg
            .register_rust(Box::new(move || f.store(true, Ordering::Relaxed)))
            .expect("register");
        assert_ne!(token, 0);
        match reg.complete(token) {
            CallbackKind::Rust(cb) => cb(),
            _ => panic!("expected Rust"),
        }
        assert!(flag.load(Ordering::Relaxed));
    }

    unsafe extern "C" fn noop(_: *mut c_void) {}

    #[test]
    fn register_native_complete_roundtrip() {
        let mut reg = CallbackRegistry::new();
        let token = reg
            .register_native(noop, 0xDEAD as *mut c_void)
            .expect("register");
        assert_ne!(token, 0);
        match reg.complete(token) {
            CallbackKind::Native { f: _, user_data } => {
                assert_eq!(user_data, 0xDEAD);
            }
            _ => panic!("expected Native"),
        }
    }

    #[test]
    fn complete_with_stale_token_returns_empty() {
        let mut reg = CallbackRegistry::new();
        let t1 = reg.register_native(noop, std::ptr::null_mut()).unwrap();
        // 完了して slot を解放
        let _ = reg.complete(t1);
        // 古い token で complete してもなにも返らない
        assert!(matches!(reg.complete(t1), CallbackKind::Empty));
        // 新しい登録は新 token でのみ取り出せる
        let t2 = reg.register_native(noop, std::ptr::null_mut()).unwrap();
        assert_ne!(t1, t2);
    }

    #[test]
    fn cancel_returns_slot_to_free_list() {
        let mut reg = CallbackRegistry::new();
        let t1 = reg.register_native(noop, std::ptr::null_mut()).unwrap();
        reg.cancel(t1);
        // cancel 後はその token は無効
        assert!(matches!(reg.complete(t1), CallbackKind::Empty));
        // 解放された slot が free_list にある
        let t2 = reg.register_native(noop, std::ptr::null_mut()).unwrap();
        assert_ne!(t1, t2);
    }

    #[test]
    fn slot_exhaustion_returns_none() {
        let mut reg = CallbackRegistry::new();
        for _ in 0..crate::source::DEFAULT_MAX_SOURCES {
            assert!(reg.register_native(noop, std::ptr::null_mut()).is_some());
        }
        assert!(reg.register_native(noop, std::ptr::null_mut()).is_none());
    }

    #[test]
    fn token_never_zero() {
        let mut reg = CallbackRegistry::new();
        for _ in 0..crate::source::DEFAULT_MAX_SOURCES {
            let t = reg.register_native(noop, std::ptr::null_mut()).unwrap();
            assert_ne!(t, 0);
        }
    }

    #[test]
    fn clear_invalidates_all_tokens() {
        let mut reg = CallbackRegistry::new();
        let t1 = reg.register_native(noop, std::ptr::null_mut()).unwrap();
        let t2 = reg.register_native(noop, std::ptr::null_mut()).unwrap();
        reg.clear();
        assert!(matches!(reg.complete(t1), CallbackKind::Empty));
        assert!(matches!(reg.complete(t2), CallbackKind::Empty));
    }
}
