//! Source の生成・再生・空間パラメータ・バッチ更新。

use core::ffi::c_void;
use std::slice;

use crate::engine::NeziaEngine;
use crate::panic::{guard_entity, guard_result, guard_value};
use crate::types::{
    NeziaAttenuationModel, NeziaBufferId, NeziaEntityId, NeziaFinishCallback, NeziaResult,
    NeziaSourcePositionUpdate, NeziaSpawnSpatialInit,
};
use nezia_core::SourcePositionUpdate;

// `NeziaSourcePositionUpdate` と `core::SourcePositionUpdate` は両方 `#[repr(C)]` で
// `{ EntityId index/generation: u32, position: [f32;3] }` という同一バイト並びを持つ。
// 下記 const アサーションが通る限り、FFI から受け取った配列を変換コピーなしで
// そのまま core に渡せる。レイアウトを崩す変更があればここでコンパイルエラーになる。
const _: () = {
    use core::mem::{align_of, size_of};
    assert!(
        size_of::<NeziaSourcePositionUpdate>() == size_of::<SourcePositionUpdate>(),
        "NeziaSourcePositionUpdate と core::SourcePositionUpdate のサイズが一致しない"
    );
    assert!(
        align_of::<NeziaSourcePositionUpdate>() == align_of::<SourcePositionUpdate>(),
        "NeziaSourcePositionUpdate と core::SourcePositionUpdate のアラインが一致しない"
    );
    assert!(size_of::<NeziaSourcePositionUpdate>() == 20);

    // `NeziaEntityId` と `core::EntityId` のレイアウト一致を保証する。
    // batch query API（`nezia_source_batch_is_alive` 等）が `&[NeziaEntityId]` を
    // `&[core::EntityId]` にゼロコピー cast するために必要。
    assert!(
        size_of::<NeziaEntityId>() == size_of::<nezia_core::EntityId>(),
        "NeziaEntityId と core::EntityId のサイズが一致しない"
    );
    assert!(
        align_of::<NeziaEntityId>() == align_of::<nezia_core::EntityId>(),
        "NeziaEntityId と core::EntityId のアラインが一致しない"
    );
    assert!(size_of::<NeziaEntityId>() == 8);
};

/// マスターバスにボイスを再生する（fire-and-forget）。
///
/// `looping` は 0 = 一度きり再生 / 非 0 = ループ再生。
/// 戻り値: 1 = 受理、0 = 失敗（無効バッファ / コマンドキュー満杯）。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_play(
    engine: *mut NeziaEngine,
    buffer: NeziaBufferId,
    volume: f32,
    pitch: f32,
    looping: u8,
) -> u8 {
    guard_value(0, || {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return 0;
        };
        engine
            .inner
            .play(buffer.to_core(), volume, pitch, looping != 0) as u8
    })
}

/// マスターバスにボイスを再生し、自然終了時にコールバックを呼ぶ。
///
/// `user_data` のライフタイムはコールバック発火まで呼出側が保証する。
/// `looping != 0` の場合は終了通知が発火しないためコールバックは呼ばれない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_play_with_callback(
    engine: *mut NeziaEngine,
    buffer: NeziaBufferId,
    volume: f32,
    pitch: f32,
    looping: u8,
    callback: NeziaFinishCallback,
    user_data: *mut c_void,
) -> u8 {
    guard_value(0, || {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return 0;
        };
        // callback が None の場合はコールバック無し再生にフォールバック（alloc ナシ）。
        let Some(f) = callback else {
            return engine
                .inner
                .play(buffer.to_core(), volume, pitch, looping != 0) as u8;
        };
        // SAFETY: 呼出側契約により f / user_data は発火時まで有効。
        unsafe {
            engine.inner.play_with_callback_native(
                buffer.to_core(),
                volume,
                pitch,
                looping != 0,
                f,
                user_data,
            ) as u8
        }
    })
}

/// 指定バスにボイスを再生する。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_play_to_bus(
    engine: *mut NeziaEngine,
    buffer: NeziaBufferId,
    volume: f32,
    pitch: f32,
    bus: NeziaEntityId,
    looping: u8,
) -> u8 {
    guard_value(0, || {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return 0;
        };
        engine
            .inner
            .play_to_bus(buffer.to_core(), volume, pitch, bus.to_core(), looping != 0) as u8
    })
}

/// 指定バスにボイスを再生し、自然終了時にコールバックを呼ぶ。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_play_to_bus_with_callback(
    engine: *mut NeziaEngine,
    buffer: NeziaBufferId,
    volume: f32,
    pitch: f32,
    bus: NeziaEntityId,
    looping: u8,
    callback: NeziaFinishCallback,
    user_data: *mut c_void,
) -> u8 {
    guard_value(0, || {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return 0;
        };
        let Some(f) = callback else {
            return engine.inner.play_to_bus(
                buffer.to_core(),
                volume,
                pitch,
                bus.to_core(),
                looping != 0,
            ) as u8;
        };
        // SAFETY: 呼出側契約により f / user_data は発火時まで有効。
        unsafe {
            engine.inner.play_to_bus_with_callback_native(
                buffer.to_core(),
                volume,
                pitch,
                bus.to_core(),
                looping != 0,
                f,
                user_data,
            ) as u8
        }
    })
}

/// 再生を開始し、制御用ハンドル（EntityId）を返す。失敗時は INVALID。
///
/// Source は 1 回の発音インスタンスを表す。バッファ末尾到達 (`looping = 0`) または
/// `nezia_source_stop()` で despawn され、その時点で EntityId は無効化される。
/// 再生し直す場合は再度この関数を呼んで新しい EntityId を取得する。
///
/// `priority` / `spatial_init` は spawn 時に `Command::SpawnSource` に同梱して 1 コマンドで
/// 送る。3D ソース 1 ボイスあたり 4〜5 コマンドを消費していた旧経路 (spawn 後に
/// `set_priority` / `set_spatial_params` / `set_doppler_level` / `set_spatial_enabled` を
/// 個別 push) を置き換える。`spatial_init.enabled = 0` の 2D ソースで spatial 系
/// プロパティはダミー値で構わない。
///
/// `callback` が `Some` のとき、自然終了時に `nezia_engine_poll_events()` 経由で
/// 1 度だけ呼ばれる（`looping != 0` の場合は呼ばれない）。`user_data` のライフタイムは
/// コールバック発火まで呼出側が保証する。
///
/// 失敗時 (バッファ不正・MAX_SOURCES 到達・**コマンドリング満杯**) は `INVALID` を返す。
/// リング満杯のケースは `nezia_engine_get_dropouts` の `out_command_queue_full` で
/// 観測できる。
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn nezia_source_play_with_handle(
    engine: *mut NeziaEngine,
    buffer: NeziaBufferId,
    volume: f32,
    pitch: f32,
    bus: NeziaEntityId,
    looping: u8,
    priority: u8,
    spatial_init: NeziaSpawnSpatialInit,
    callback: NeziaFinishCallback,
    user_data: *mut c_void,
) -> NeziaEntityId {
    guard_entity(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaEntityId::INVALID;
        };
        let core_init = spatial_init.to_core();
        let result = match callback {
            Some(f) => unsafe {
                // SAFETY: 呼出側契約により f / user_data は発火時まで有効。
                engine.inner.play_with_handle_and_callback_native(
                    buffer.to_core(),
                    volume,
                    pitch,
                    bus.to_core(),
                    looping != 0,
                    priority,
                    core_init,
                    f,
                    user_data,
                )
            },
            None => engine.inner.play_with_handle(
                buffer.to_core(),
                volume,
                pitch,
                bus.to_core(),
                looping != 0,
                priority,
                core_init,
            ),
        };
        result
            .map(NeziaEntityId::from_core)
            .unwrap_or(NeziaEntityId::INVALID)
    })
}

/// 既存ソースのループフラグを動的に変更する。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_set_loop(
    engine: *mut NeziaEngine,
    source: NeziaEntityId,
    looping: u8,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine.inner.set_source_loop(source.to_core(), looping != 0) {
            NeziaResult::Ok
        } else {
            NeziaResult::QueueFull
        }
    })
}

/// Voice Virtualization 用優先度を設定する (Wwise / CRI ADX2 互換)。
///
/// 0..255、**高いほど高優先**。既定 128 (中央値)。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_set_priority(
    engine: *mut NeziaEngine,
    source: NeziaEntityId,
    priority: u8,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine.inner.set_source_priority(source.to_core(), priority) {
            NeziaResult::Ok
        } else {
            NeziaResult::QueueFull
        }
    })
}

/// ソースが現在 SourceWorld に存在するか確認する。
///
/// 1 = 存在、0 = 不在 / generation 不一致 / NULL ポインタ。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_is_alive(
    engine: *mut NeziaEngine,
    source: NeziaEntityId,
) -> u8 {
    guard_value(0, || {
        let Some(engine) = (unsafe { engine.as_ref() }) else {
            return 0;
        };
        engine.inner.is_source_alive(source.to_core()) as u8
    })
}

/// 複数ソースの生存を一括判定する。
///
/// `out_alive_ptr[i]` には `ids_ptr[i]` が現在の最新スナップショットに存在し
/// generation も一致する場合 `1`、それ以外は `0` が書き込まれる。
/// 内部は単発 `nezia_source_is_alive` 呼び出しの繰り返しを 1 回の P/Invoke に
/// 集約しつつ、メインスレッド側のスキャンも 1 ループで処理する（FFI 越境の
/// オーバーヘッドが N 倍 → 1 倍）。
///
/// # 安全性
/// - `ids_ptr` は `len` 要素分の有効領域を指すこと。
/// - `out_alive_ptr` は `len` 要素分の書き込み可能領域を指すこと。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_batch_is_alive(
    engine: *mut NeziaEngine,
    ids_ptr: *const NeziaEntityId,
    len: usize,
    out_alive_ptr: *mut u8,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_ref() }) else {
            return NeziaResult::NullPointer;
        };
        if len == 0 {
            return NeziaResult::Ok;
        }
        if ids_ptr.is_null() || out_alive_ptr.is_null() {
            return NeziaResult::NullPointer;
        }
        // SAFETY: 呼出側契約 + NeziaEntityId と core::EntityId の repr(C) レイアウト一致。
        let ids: &[nezia_core::EntityId] =
            unsafe { slice::from_raw_parts(ids_ptr.cast::<nezia_core::EntityId>(), len) };
        let out: &mut [u8] = unsafe { slice::from_raw_parts_mut(out_alive_ptr, len) };
        engine.inner.batch_is_source_alive(ids, out);
        NeziaResult::Ok
    })
}

/// 現在のソース再生位置（フレーム単位）を取得する。
///
/// 取得タイミングはサウンドスレッドからスナップショットされた共有状態に依存し、
/// 厳密には数ミリ秒の遅延がある。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_get_position(
    engine: *mut NeziaEngine,
    source: NeziaEntityId,
    out_frames: *mut f32,
) -> NeziaResult {
    guard_result(|| {
        if out_frames.is_null() {
            return NeziaResult::NullPointer;
        }
        let Some(engine) = (unsafe { engine.as_ref() }) else {
            return NeziaResult::NullPointer;
        };
        match engine.inner.source_position(source.to_core()) {
            Some(frames) => {
                // SAFETY: out_frames は呼出側契約により書き込み可能。
                unsafe { out_frames.write(frames) };
                NeziaResult::Ok
            }
            None => NeziaResult::InvalidHandle,
        }
    })
}

/// 複数ソースの再生位置を一括取得する。
///
/// alive でないソースは `out_positions_ptr[i] = NaN` / `out_alive_ptr[i] = 0`。
/// `out_alive_ptr` を不要なら NULL を渡してもよい（その場合 alive 判定は
/// `out_positions_ptr[i]` が NaN かで代替できる）。
///
/// # 安全性
/// - `ids_ptr` は `len` 要素分の有効領域を指すこと。
/// - `out_positions_ptr` は `len` 要素分の書き込み可能領域を指すこと。
/// - `out_alive_ptr` は NULL か、`len` 要素分の書き込み可能領域を指すこと。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_batch_get_positions(
    engine: *mut NeziaEngine,
    ids_ptr: *const NeziaEntityId,
    len: usize,
    out_positions_ptr: *mut f32,
    out_alive_ptr: *mut u8,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_ref() }) else {
            return NeziaResult::NullPointer;
        };
        if len == 0 {
            return NeziaResult::Ok;
        }
        if ids_ptr.is_null() || out_positions_ptr.is_null() {
            return NeziaResult::NullPointer;
        }
        // SAFETY: 呼出側契約 + NeziaEntityId と core::EntityId の repr(C) レイアウト一致。
        let ids: &[nezia_core::EntityId] =
            unsafe { slice::from_raw_parts(ids_ptr.cast::<nezia_core::EntityId>(), len) };
        let out_pos: &mut [f32] = unsafe { slice::from_raw_parts_mut(out_positions_ptr, len) };
        // out_alive は NULL 許容。
        let mut empty: [u8; 0] = [];
        let out_alive: &mut [u8] = if out_alive_ptr.is_null() {
            &mut empty
        } else {
            unsafe { slice::from_raw_parts_mut(out_alive_ptr, len) }
        };
        engine.inner.batch_source_positions(ids, out_pos, out_alive);
        NeziaResult::Ok
    })
}

/// ソースの距離減衰パラメータを設定する（初期化・変更時のみ）。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_set_spatial_params(
    engine: *mut NeziaEngine,
    source: NeziaEntityId,
    model: NeziaAttenuationModel,
    min_distance: f32,
    max_distance: f32,
    rolloff: f32,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine.inner.set_source_spatial_params(
            source.to_core(),
            model.to_core(),
            min_distance,
            max_distance,
            rolloff,
        ) {
            NeziaResult::Ok
        } else {
            NeziaResult::QueueFull
        }
    })
}

/// ソースの空間演算を有効化・無効化する。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_set_spatial_enabled(
    engine: *mut NeziaEngine,
    source: NeziaEntityId,
    enabled: u8,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine
            .inner
            .set_source_spatial_enabled(source.to_core(), enabled != 0)
        {
            NeziaResult::Ok
        } else {
            NeziaResult::QueueFull
        }
    })
}

// ── ライブソース制御 ──

/// 既存ソースの音量を設定する（spawn 後の動的変更）。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_set_volume(
    engine: *mut NeziaEngine,
    source: NeziaEntityId,
    volume: f32,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine.inner.set_source_volume(source.to_core(), volume) {
            NeziaResult::Ok
        } else {
            NeziaResult::QueueFull
        }
    })
}

/// 既存ソースのピッチを設定する。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_set_pitch(
    engine: *mut NeziaEngine,
    source: NeziaEntityId,
    pitch: f32,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine.inner.set_source_pitch(source.to_core(), pitch) {
            NeziaResult::Ok
        } else {
            NeziaResult::QueueFull
        }
    })
}

/// ソースの再生位置（フレーム単位）をシークする。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_seek(
    engine: *mut NeziaEngine,
    source: NeziaEntityId,
    frame_offset: f32,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine.inner.seek_source(source.to_core(), frame_offset) {
            NeziaResult::Ok
        } else {
            NeziaResult::QueueFull
        }
    })
}

/// ソースを一時停止する。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_pause(
    engine: *mut NeziaEngine,
    source: NeziaEntityId,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine.inner.pause_source(source.to_core()) {
            NeziaResult::Ok
        } else {
            NeziaResult::QueueFull
        }
    })
}

/// 一時停止中のソースを再開する。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_resume(
    engine: *mut NeziaEngine,
    source: NeziaEntityId,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine.inner.resume_source(source.to_core()) {
            NeziaResult::Ok
        } else {
            NeziaResult::QueueFull
        }
    })
}

/// ソースを停止する（次の audio callback で despawn）。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_stop(
    engine: *mut NeziaEngine,
    source: NeziaEntityId,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if engine.inner.stop_source(source.to_core()) {
            NeziaResult::Ok
        } else {
            NeziaResult::QueueFull
        }
    })
}

/// 複数ソースの位置を一括更新する（毎フレーム想定）。
///
/// # 安全性
/// `updates_ptr` は `updates_len` 要素分の有効領域を指すこと。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nezia_source_batch_set_positions(
    engine: *mut NeziaEngine,
    updates_ptr: *const NeziaSourcePositionUpdate,
    updates_len: usize,
) -> NeziaResult {
    guard_result(|| {
        let Some(engine) = (unsafe { engine.as_mut() }) else {
            return NeziaResult::NullPointer;
        };
        if updates_len == 0 {
            engine.inner.batch_set_source_positions(&[]);
            return NeziaResult::Ok;
        }
        if updates_ptr.is_null() {
            return NeziaResult::NullPointer;
        }
        // SAFETY:
        // - `updates_ptr` は呼出側契約により `updates_len` 要素分の有効領域。
        // - `NeziaSourcePositionUpdate` と `SourcePositionUpdate` は上の const アサーションで
        //   レイアウト一致を保証しているので、要素ごとの再解釈はトリビアルに安全。
        // これにより毎フレームの Vec alloc / 要素コピーがゼロになる（純粋なポインタ読み替え）。
        let updates: &[SourcePositionUpdate] = unsafe {
            slice::from_raw_parts(updates_ptr.cast::<SourcePositionUpdate>(), updates_len)
        };
        engine.inner.batch_set_source_positions(updates);
        NeziaResult::Ok
    })
}
