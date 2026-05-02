//! パニック越境遮断ヘルパ。
//!
//! Rust のパニックを C 境界を超えて伝播させると未定義動作になる。各 `extern "C"`
//! エントリポイントは `guard` 系のヘルパで包むことで `catch_unwind` で捕捉する。

use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::types::{NeziaBufferId, NeziaEntityId, NeziaResult};

/// `NeziaResult` を返す関数をパニックガードする。
#[inline]
pub(crate) fn guard_result<F>(f: F) -> NeziaResult
where
    F: FnOnce() -> NeziaResult,
{
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(_) => NeziaResult::Panic,
    }
}

/// `NeziaEntityId` を返す関数をパニックガードする（失敗時は INVALID）。
#[inline]
pub(crate) fn guard_entity<F>(f: F) -> NeziaEntityId
where
    F: FnOnce() -> NeziaEntityId,
{
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(NeziaEntityId::INVALID)
}

/// `NeziaBufferId` を返す関数をパニックガードする（失敗時は INVALID）。
#[inline]
pub(crate) fn guard_buffer<F>(f: F) -> NeziaBufferId
where
    F: FnOnce() -> NeziaBufferId,
{
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(NeziaBufferId::INVALID)
}

/// 任意の値を返す関数をパニックガードする（失敗時は `default`）。
#[inline]
pub(crate) fn guard_value<T, F>(default: T, f: F) -> T
where
    F: FnOnce() -> T,
{
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(default)
}
