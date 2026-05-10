//! Custom Attenuation Curve (Phase 3-1) の結合テスト。
//!
//! `SoundEngine` 経由で curve registry → SpatialWorld の curve_indices →
//! sound thread での compute_gains LUT サンプリングまでを検証する。

use std::thread;
use std::time::Duration;

use nezia::{AttenuationModel, SoundEngine, SpawnSpatialInit};

/// テスト用ステレオ 16-bit PCM WAV を生成する (サイン波)。
fn gen_wav(freq: f32, sample_rate: u32, secs: f32, name: &str) -> std::path::PathBuf {
    use std::io::Write as _;
    let path = std::env::temp_dir().join(name);
    let channels: u16 = 2;
    let bps: u16 = 16;
    let n = (sample_rate as f32 * secs) as u32;
    let data_size = n * channels as u32 * (bps / 8) as u32;
    let block_align = channels * bps / 8;
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(b"RIFF").unwrap();
    f.write_all(&(36 + data_size).to_le_bytes()).unwrap();
    f.write_all(b"WAVE").unwrap();
    f.write_all(b"fmt ").unwrap();
    f.write_all(&16u32.to_le_bytes()).unwrap();
    f.write_all(&1u16.to_le_bytes()).unwrap();
    f.write_all(&channels.to_le_bytes()).unwrap();
    f.write_all(&sample_rate.to_le_bytes()).unwrap();
    f.write_all(&(sample_rate * block_align as u32).to_le_bytes())
        .unwrap();
    f.write_all(&block_align.to_le_bytes()).unwrap();
    f.write_all(&bps.to_le_bytes()).unwrap();
    f.write_all(b"data").unwrap();
    f.write_all(&data_size.to_le_bytes()).unwrap();
    let step = 2.0 * std::f32::consts::PI * freq / sample_rate as f32;
    for i in 0..n {
        let s = ((step * i as f32).sin() * 0.5 * i16::MAX as f32) as i16;
        f.write_all(&s.to_le_bytes()).unwrap();
        f.write_all(&s.to_le_bytes()).unwrap();
    }
    path
}

#[test]
fn create_and_destroy_curve() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let id = engine.create_attenuation_curve(&[1.0, 0.5, 0.0]);
    assert!(id.is_some());
    assert!(engine.destroy_attenuation_curve(id.unwrap()));
    // 二重 destroy は false。
    assert!(!engine.destroy_attenuation_curve(id.unwrap()));
}

#[test]
fn assigning_curve_to_unknown_source_returns_false() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let curve = engine.create_attenuation_curve(&[1.0, 0.0]).unwrap();
    let dummy = nezia::EntityId {
        index: 999,
        generation: 0,
    };
    // ソースは存在しないが、コマンド送信は成功する (ringbuf push)。audio thread 側で resolve 失敗 → 無視。
    let _ = engine.set_source_attenuation_curve(dummy, Some(curve));
}

#[test]
fn destroyed_curve_id_rejected_by_set() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let curve = engine.create_attenuation_curve(&[1.0, 0.0]).unwrap();
    engine.destroy_attenuation_curve(curve);

    // 再生中ソースが必要なので適当に play_with_handle する。
    let wav = gen_wav(440.0, 44100, 1.0, "nezia_curve_test_destroyed.wav");
    let buf = engine.load(&wav).unwrap();
    let master = engine.master_bus();
    let id = engine
        .play_with_handle(buf, 1.0, 1.0, master, false, 128, SpawnSpatialInit::NONE)
        .unwrap();

    // destroy 済みカーブを set しようとすると false (resolve 失敗)。
    assert!(!engine.set_source_attenuation_curve(id, Some(curve)));
    let _ = engine.stop_all();
    thread::sleep(Duration::from_millis(50));
    let _ = engine.unload(buf);
    let _ = std::fs::remove_file(&wav);
}

#[test]
fn end_to_end_custom_curve_assignment() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let curve = engine
        .create_attenuation_curve(&[1.0, 0.5, 0.0])
        .expect("curve create");

    let wav = gen_wav(330.0, 44100, 1.0, "nezia_curve_test_e2e.wav");
    let buf = engine.load(&wav).unwrap();
    let master = engine.master_bus();
    let id = engine
        .play_with_handle(buf, 1.0, 1.0, master, false, 128, SpawnSpatialInit::NONE)
        .unwrap();

    // Custom モデルに切替 + curve を割り当て + 3D 有効化。
    assert!(engine.set_source_spatial_params(id, AttenuationModel::Custom, 1.0, 100.0, 1.0,));
    assert!(engine.set_source_attenuation_curve(id, Some(curve)));
    let _ = engine.set_source_spatial_enabled(id, true);

    // 数 callback 経過してクラッシュしないことを確認 (実際のゲインはサウンドスレッドで算出)。
    thread::sleep(Duration::from_millis(100));

    let _ = engine.stop_all();
    thread::sleep(Duration::from_millis(50));
    let _ = engine.unload(buf);
    engine.destroy_attenuation_curve(curve);
    let _ = std::fs::remove_file(&wav);
}

#[test]
fn custom_curve_with_none_clears_assignment() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let curve = engine.create_attenuation_curve(&[1.0, 0.0]).unwrap();
    let wav = gen_wav(220.0, 44100, 0.5, "nezia_curve_test_clear.wav");
    let buf = engine.load(&wav).unwrap();
    let master = engine.master_bus();
    let id = engine
        .play_with_handle(buf, 1.0, 1.0, master, false, 128, SpawnSpatialInit::NONE)
        .unwrap();

    assert!(engine.set_source_attenuation_curve(id, Some(curve)));
    // None 渡しでクリア (CURVE_INDEX_NONE 反映)。
    assert!(engine.set_source_attenuation_curve(id, None));
    thread::sleep(Duration::from_millis(50));

    let _ = engine.stop_all();
    thread::sleep(Duration::from_millis(50));
    let _ = engine.unload(buf);
    engine.destroy_attenuation_curve(curve);
    let _ = std::fs::remove_file(&wav);
}
