//! FFI 境界の lifecycle スモークテスト。
//!
//! `extern "C"` 関数を Rust 側から直接呼び、生成・破棄・基本制御が動くことを確認する。
//! オーディオデバイスがない CI 等では `nezia_engine_new` が NULL を返す可能性があるため、
//! その場合は早期 return する。

use nezia::nezia_audio_peek_metadata;
use nezia::nezia_buffer_load_from_memory;
use nezia::nezia_buffer_load_from_pcm;
use nezia::nezia_buffer_reader_channels;
use nezia::nezia_buffer_reader_close;
use nezia::nezia_buffer_reader_open;
use nezia::nezia_buffer_reader_read;
use nezia::nezia_buffer_reader_sample_rate;
use nezia::nezia_buffer_reader_total_frames;
use nezia::nezia_buffer_unload;
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
use nezia::{NeziaAudioMetadata, NeziaResult, NeziaVec3};

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

        // PCM 直アップロード（パターン入り 0.1 秒、stereo, 48kHz）
        let mut pcm: Vec<f32> = vec![0.0; 48_000 / 10 * 2];
        for (i, s) in pcm.iter_mut().enumerate() {
            *s = (i as f32) * 0.001;
        }
        let pcm_id = nezia_buffer_load_from_pcm(engine, pcm.as_ptr(), pcm.len(), 2, 48_000);
        assert_ne!(pcm_id.index, u32::MAX, "load_from_pcm must succeed");

        // BufferReader: 任意スレッドから PCM を取り出せること
        let reader = nezia_buffer_reader_open(engine, pcm_id);
        assert!(!reader.is_null());
        assert_eq!(nezia_buffer_reader_channels(reader), 2);
        assert_eq!(nezia_buffer_reader_sample_rate(reader), 48_000);
        assert_eq!(
            nezia_buffer_reader_total_frames(reader),
            (pcm.len() / 2) as u64
        );
        let mut dst = vec![0.0f32; 8];
        let read = nezia_buffer_reader_read(reader, 0, dst.as_mut_ptr(), dst.len());
        assert_eq!(read, 4); // 4 frames (8 samples / 2 channels)
        assert_eq!(dst[0], 0.0);
        assert!((dst[1] - 0.001).abs() < 1e-6);
        nezia_buffer_reader_close(reader);
        nezia_buffer_reader_close(std::ptr::null_mut()); // NULL 安全

        assert_eq!(nezia_buffer_unload(engine, pcm_id), NeziaResult::Ok);

        // 不正引数: channels=0
        let bad = nezia_buffer_load_from_pcm(engine, pcm.as_ptr(), pcm.len(), 0, 48_000);
        assert_eq!(bad.index, u32::MAX);

        // load_from_memory: 不正バイト列はデコード失敗 → INVALID
        let garbage = [0u8; 32];
        let bad_mem = nezia_buffer_load_from_memory(engine, garbage.as_ptr(), garbage.len());
        assert_eq!(bad_mem.index, u32::MAX);

        // peek_metadata: 不正バイト列は DecodeError
        let mut meta = NeziaAudioMetadata {
            sample_rate: 0,
            channels: 0,
            _pad: 0,
            total_frames: 0,
        };
        let res = nezia_audio_peek_metadata(garbage.as_ptr(), garbage.len(), &mut meta);
        assert_eq!(res, NeziaResult::DecodeError);
        // NULL out_metadata
        let res2 = nezia_audio_peek_metadata(garbage.as_ptr(), garbage.len(), std::ptr::null_mut());
        assert_eq!(res2, NeziaResult::NullPointer);

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
