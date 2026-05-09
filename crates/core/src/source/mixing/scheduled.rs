//! Phase 0.5 (Phase 3-4): 予約再生 (PlayScheduled) の発音判定。
//!
//! `state == Scheduled` のソースを走査し、`start_dsp_frame` が当該 callback 区間
//! `[clock, clock + frames_in_callback)` に到達していれば `Playing` 化する。過去指定
//! (`start_dsp_frame <= clock`) は offset 0 で即時発音、callback 区間内
//! (`clock < start_dsp_frame < clock + frames_in_callback`) は frames 単位の sub-callback
//! offset を `start_offset_scratch` に書き込む。Phase 2 mix がこれを読んで bus_buf を
//! シフトする。
//!
//! `start_offset_scratch` は呼出側で長さ >= `world.len()` を保証する。書き込まれる前に
//! すべて 0 にリセットしておく必要があり、その責務もここで負う (毎 callback 冒頭で実施)。

use crate::source::world::{SourceState, SourceWorld};

pub(super) fn activate_scheduled(
    world: &mut SourceWorld,
    start_offset_scratch: &mut [u32],
    clock: u64,
    frames_in_callback: u64,
) {
    let n = world.len();
    // Phase 2 mix が参照するため、Scheduled が無いソースも含めて全件 0 リセット。
    // MAX_SOURCES = 256 で memset O(1024 byte)。
    for slot in start_offset_scratch.iter_mut().take(n) {
        *slot = 0;
    }
    // 早期リターン: Scheduled が一つも無ければ走査しない (典型的な状態)。
    let has_scheduled = world
        .states()
        .iter()
        .take(n)
        .any(|s| *s == SourceState::Scheduled);
    if !has_scheduled {
        return;
    }
    // SourceWorld のフィールドを直接借りることで `&[u64]` と `&mut [SourceState]` の
    // 同時借用を許可する (公開 accessor 経由だと world 全体の借用になり競合する)。
    let starts = &world.start_dsp_frame;
    let states = &mut world.state;
    let callback_end = clock.saturating_add(frames_in_callback);
    for i in 0..n {
        if states[i] != SourceState::Scheduled {
            continue;
        }
        let ts = starts[i];
        if ts >= callback_end {
            // この callback では発音しない (将来 callback で再評価)。
            continue;
        }
        // ts < callback_end → 発音開始。
        states[i] = SourceState::Playing;
        if ts <= clock {
            // 過去指定 / callback 冒頭ぴったり: offset 0 で即時発音。
            start_offset_scratch[i] = 0;
        } else {
            // callback 区間内: (ts - clock) frame 進んだ位置から発音開始。
            // frames_in_callback <= u32::MAX (callback サイズは数千 frame まで) なので u32 で安全。
            start_offset_scratch[i] = (ts - clock) as u32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::world::{SourceComponent, SourceWorld};

    fn world_with(start_dsp_frames: &[u64]) -> SourceWorld {
        let mut w = SourceWorld::new();
        for &t in start_dsp_frames {
            w.spawn(SourceComponent {
                vol: 1.0,
                pitch: 1.0,
                start_dsp_frame: t,
                ..SourceComponent::default()
            })
            .unwrap();
        }
        w
    }

    #[test]
    fn future_schedule_stays_scheduled() {
        let mut w = world_with(&[10_000]);
        let mut scratch = [0u32; 1];
        // clock = 0, callback covers [0, 512). start = 10000 → 未来。
        activate_scheduled(&mut w, &mut scratch, 0, 512);
        assert_eq!(w.states()[0], SourceState::Scheduled);
        assert_eq!(scratch[0], 0);
    }

    #[test]
    fn past_schedule_becomes_playing_with_zero_offset() {
        let mut w = world_with(&[100]);
        let mut scratch = [0u32; 1];
        // clock = 1000 (start=100 は過去), callback covers [1000, 1512).
        activate_scheduled(&mut w, &mut scratch, 1000, 512);
        assert_eq!(w.states()[0], SourceState::Playing);
        assert_eq!(scratch[0], 0);
    }

    #[test]
    fn within_callback_sets_sub_offset() {
        let mut w = world_with(&[1200]);
        let mut scratch = [0u32; 1];
        // clock = 1000, callback covers [1000, 1512). start = 1200 → 200 frame 目。
        activate_scheduled(&mut w, &mut scratch, 1000, 512);
        assert_eq!(w.states()[0], SourceState::Playing);
        assert_eq!(scratch[0], 200);
    }

    #[test]
    fn boundary_start_equals_clock_starts_at_zero_offset() {
        let mut w = world_with(&[1000]);
        let mut scratch = [0u32; 1];
        activate_scheduled(&mut w, &mut scratch, 1000, 512);
        assert_eq!(w.states()[0], SourceState::Playing);
        assert_eq!(scratch[0], 0);
    }

    #[test]
    fn boundary_start_equals_callback_end_stays_scheduled() {
        let mut w = world_with(&[1512]);
        let mut scratch = [0u32; 1];
        activate_scheduled(&mut w, &mut scratch, 1000, 512);
        // ts >= callback_end (= clock + frames) なので Scheduled のまま。
        assert_eq!(w.states()[0], SourceState::Scheduled);
    }

    #[test]
    fn already_playing_sources_are_untouched() {
        let mut w = SourceWorld::new();
        w.spawn(SourceComponent {
            vol: 1.0,
            pitch: 1.0,
            start_dsp_frame: 0, // 即時 → spawn 時点で Playing
            ..SourceComponent::default()
        })
        .unwrap();
        let mut scratch = [0u32; 1];
        activate_scheduled(&mut w, &mut scratch, 5000, 512);
        assert_eq!(w.states()[0], SourceState::Playing);
        assert_eq!(scratch[0], 0);
    }

    #[test]
    fn scratch_is_reset_each_call() {
        // 前 callback の残骸 (99) が確実に上書きされることを確認する。
        let mut w = world_with(&[10_000, 200]);
        let mut scratch = [99u32; 2];
        activate_scheduled(&mut w, &mut scratch, 0, 512);
        // 0番: 未来 → Scheduled。scratch は 0 にリセット (99 が残らない)。
        assert_eq!(scratch[0], 0);
        // 1番: clock=0, start=200 → callback 区間内、sub_offset=200。
        assert_eq!(scratch[1], 200);
        assert_eq!(w.states()[1], SourceState::Playing);
    }

    #[test]
    fn mixed_states_handled_per_source() {
        let mut w = world_with(&[10_000, 1100, 50, 0]);
        // index 3 は start=0 (sentinel) → spawn 時点で Playing。
        let mut scratch = [0u32; 4];
        activate_scheduled(&mut w, &mut scratch, 1000, 512);
        assert_eq!(w.states()[0], SourceState::Scheduled);
        assert_eq!(w.states()[1], SourceState::Playing);
        assert_eq!(scratch[1], 100);
        assert_eq!(w.states()[2], SourceState::Playing);
        assert_eq!(scratch[2], 0); // 過去
        assert_eq!(w.states()[3], SourceState::Playing); // 既に Playing
        assert_eq!(scratch[3], 0);
    }
}
