//! FFI 境界の lifecycle スモークテスト。
//!
//! `extern "C"` 関数を Rust 側から直接呼び、生成・破棄・基本制御が動くことを確認する。
//! オーディオデバイスがない CI 等では `nezia_engine_new` が NULL を返す可能性があるため、
//! その場合は早期 return する。

use nezia::nezia_bus_create;
use nezia::nezia_bus_destroy;
use nezia::nezia_bus_set_gain;
use nezia::nezia_engine_free;
use nezia::nezia_engine_master_bus;
use nezia::nezia_engine_new;
use nezia::nezia_engine_poll_events;
use nezia::nezia_engine_set_volume;
use nezia::nezia_engine_stop_all;
use nezia::nezia_listener_set;
use nezia::{NeziaResult, NeziaVec3};

#[test]
fn lifecycle_smoke() {
    let engine = nezia_engine_new();
    if engine.is_null() {
        eprintln!("no audio device available; skipping smoke test");
        return;
    }

    unsafe {
        // master bus が有効ハンドルを返すこと
        let master = nezia_engine_master_bus(engine);
        assert_ne!(master.index, u32::MAX, "master bus must be valid");

        // 音量設定
        assert_eq!(nezia_engine_set_volume(engine, 0.5), NeziaResult::Ok);

        // バス作成・gain 設定・破棄
        let bus = nezia_bus_create(engine, 1.0);
        assert_ne!(bus.index, u32::MAX);
        assert_eq!(nezia_bus_set_gain(engine, bus, 0.8), NeziaResult::Ok);
        assert_eq!(nezia_bus_destroy(engine, bus), NeziaResult::Ok);

        // listener 設定
        assert_eq!(
            nezia_listener_set(
                engine,
                NeziaVec3 {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0
                },
                NeziaVec3 {
                    x: 0.0,
                    y: 0.0,
                    z: -1.0
                },
                NeziaVec3 {
                    x: 0.0,
                    y: 1.0,
                    z: 0.0
                },
            ),
            NeziaResult::Ok
        );

        // poll は空でも OK
        assert_eq!(nezia_engine_poll_events(engine), NeziaResult::Ok);

        // stop_all
        assert_eq!(nezia_engine_stop_all(engine), NeziaResult::Ok);

        // NULL ハンドルは NullPointer
        assert_eq!(
            nezia_engine_set_volume(std::ptr::null_mut(), 0.5),
            NeziaResult::NullPointer
        );

        nezia_engine_free(engine);
        // double-free 防止: NULL 渡しは無視されるべき
        nezia_engine_free(std::ptr::null_mut());
    }
}
