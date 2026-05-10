//! `play_with_handle_init` (= spawn 時 spatial 一括初期化) の結合テスト。
//!
//! 旧経路は spawn 後に `set_source_priority` / `set_source_spatial_params` /
//! `set_source_doppler_level` を別コマンドで送る設計のため、1 ボイスで 4 コマンドを
//! SPSC リングに積んでいた。本 API は同梱して 1 コマンドに圧縮する。
//!
//! 実機オーディオデバイスが無い環境では `SoundEngine::new()` が失敗するため
//! early return で skip する (CI 互換、既存テストと同じパターン)。

use nezia::{AttenuationModel, SoundEngine, SpawnSpatialInit};

fn make_buffer(engine: &mut SoundEngine) -> nezia::BufferId {
    engine.load_from_pcm(vec![0.5_f32; 100], 1, 48_000)
}

#[test]
fn play_with_handle_init_returns_id_for_2d_source() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let buf = make_buffer(&mut engine);
    let bus = engine.master_bus();
    // 2D ソース: NONE で旧 play_with_handle と同等。
    let id = engine.play_with_handle_init(buf, 1.0, 1.0, bus, false, 128, SpawnSpatialInit::NONE);
    assert!(id.is_some());
}

#[test]
fn play_with_handle_init_returns_id_for_3d_source() {
    let mut engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    let buf = make_buffer(&mut engine);
    let bus = engine.master_bus();
    // 3D ソース: 旧経路なら spawn 後に 4 コマンド送るところを 1 コマンドにまとめる。
    let init = SpawnSpatialInit {
        enabled: true,
        model: AttenuationModel::InverseDistance,
        min_distance: 1.0,
        max_distance: 50.0,
        rolloff: 1.0,
        doppler_level: 1.0,
        curve_index: u32::MAX,
    };
    let id = engine.play_with_handle_init(buf, 1.0, 1.0, bus, true, 200, init);
    assert!(id.is_some());
}

#[test]
fn dropouts_exposes_command_queue_full_counter() {
    let engine = match SoundEngine::new() {
        Ok(e) => e,
        Err(_) => return,
    };
    // 起動直後はゼロ (バーストなし)。フィールドが配線されていることだけ確認する。
    let d = engine.dropouts();
    assert_eq!(d.command_queue_full, 0);
}
