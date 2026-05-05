//! Custom Attenuation Curve 関連 FFI (Phase 3-1)。
//!
//! 設計詳細は `docs/design/core/spatial.md` 参照。

use std::slice;

use nezia_core::AttenuationCurveId;

use crate::engine::NeziaEngine;
use crate::panic::{guard_result, guard_value};
use crate::types::{NeziaEntityId, NeziaResult};

/// Custom Attenuation Curve ハンドル (`core::AttenuationCurveId` の ABI ミラー)。
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NeziaAttenuationCurveId {
    pub index: u32,
    pub generation: u32,
}

impl NeziaAttenuationCurveId {
    pub(crate) const INVALID: Self = Self {
        index: u32::MAX,
        generation: 0,
    };

    #[inline]
    fn from_core(id: AttenuationCurveId) -> Self {
        Self {
            index: id.index,
            generation: id.generation,
        }
    }

    #[inline]
    fn to_core(self) -> AttenuationCurveId {
        AttenuationCurveId {
            index: self.index,
            generation: self.generation,
        }
    }

    fn is_invalid(self) -> bool {
        self.index == u32::MAX
    }
}

/// Custom Attenuation Curve を作成してハンドルを返す。
///
/// `points` は `[0.0, 1.0]` の正規化距離に対応する uniform sample。内部で 64 サンプル
/// LUT に再サンプリングされる。`points_len == 0` または容量超過で `INVALID`。
///
/// # 安全性
/// `points_ptr` は `points_len` 個の `f32` を読める有効領域を指すこと。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_attenuation_curve_create(
    engine: *mut NeziaEngine,
    points_ptr: *const f32,
    points_len: usize,
) -> NeziaAttenuationCurveId {
    guard_value(NeziaAttenuationCurveId::INVALID, || {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaAttenuationCurveId::INVALID;
        };
        if points_len == 0 || points_ptr.is_null() {
            return NeziaAttenuationCurveId::INVALID;
        }
        // SAFETY: 呼出側契約。
        let points = unsafe { slice::from_raw_parts(points_ptr, points_len) };
        engine
            .inner
            .create_attenuation_curve(points)
            .map(NeziaAttenuationCurveId::from_core)
            .unwrap_or(NeziaAttenuationCurveId::INVALID)
    })
}

/// Custom Attenuation Curve を破棄する。参照中のソースは silent fallback する。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_attenuation_curve_destroy(
    engine: *mut NeziaEngine,
    id: NeziaAttenuationCurveId,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine.inner.destroy_attenuation_curve(id.to_core()) {
            NeziaResult::Ok
        } else {
            NeziaResult::InvalidHandle
        }
    })
}

/// ソースに Custom Attenuation Curve を割り当てる。
/// `curve` に `INVALID` を渡すと curve を外す (silent fallback)。
/// 別途 `set_source_spatial_params` で `model = Custom` を設定する必要がある。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_set_attenuation_curve(
    engine: *mut NeziaEngine,
    source: NeziaEntityId,
    curve: NeziaAttenuationCurveId,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        let curve_arg = if curve.is_invalid() {
            None
        } else {
            Some(curve.to_core())
        };
        if engine
            .inner
            .set_source_attenuation_curve(source.to_core(), curve_arg)
        {
            NeziaResult::Ok
        } else {
            NeziaResult::QueueFull
        }
    })
}
