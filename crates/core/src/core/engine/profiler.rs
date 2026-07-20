//! ランタイムプロファイラのフレーム publish (M3 可視化基盤)。
//!
//! サウンドスレッドが各コールバック末尾で [`ProfilerFrame`] を triple buffer に
//! publish し、メインスレッド (Editor の可視化ウィンドウ等) が任意頻度で読む。
//! `source_state_cache` と同じ「サウンドスレッド所有の状態を newest-wins で
//! 同期する」パターン (threading.md) を可視化向けに拡張したもの。
//!
//! ガードレール:
//! - publish は `enabled` フラグ (AtomicBool) で丸ごとゲートする。プロファイラ
//!   OFF のとき、サウンドスレッドの追加コストは atomic load 1 回のみ。
//! - 全バッファは初期化時に容量を確保し、publish 経路で再確保しない。

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::bus::{BusWorld, MAX_BUSES};
use crate::snapshot::ActiveSnapshot;
use crate::source::{SourceState, SourceWorld};

/// バス 1 本のプロファイル (dense 順)。
#[derive(Debug, Clone, Copy, Default)]
pub struct ProfilerBus {
    /// バスの EntityId (index / generation)。
    pub index: u32,
    pub generation: u32,
    /// 現在の線形ゲイン (Snapshot 補間中はその瞬間値)。
    pub gain: f32,
    pub muted: bool,
}

/// アクティブソース 1 本のプロファイル。
#[derive(Debug, Clone, Copy, Default)]
pub struct ProfilerSource {
    /// ソースの EntityId (index / generation)。
    pub index: u32,
    pub generation: u32,
    /// 出力先バスの EntityId。
    pub bus_index: u32,
    pub bus_generation: u32,
    /// 再生中バッファのプールスロット index (`BufferId.index`)。
    /// フロント側でロード済みアセットとの対応付け (クリップ名表示) に使う。
    pub buffer_index: u32,
    pub volume: f32,
    pub pitch: f32,
    /// 再生位置 (ソースフレーム、ピッチ換算前)。
    pub sample_offset: f32,
    /// 0 = Stopped / 1 = Playing / 2 = Scheduled / 3 = Pausing (`SourceState` の写像)。
    pub state: u8,
    /// virtualizer によって mix スキップされているか。
    pub is_virtual: bool,
}

/// サウンドスレッドが publish する 1 フレームぶんの一貫スナップショット。
#[derive(Debug, Clone)]
pub struct ProfilerFrame {
    /// マスター出力 (soft limiter 後) の callback 内 max |sample|。ch 0 / 1。
    /// モノラルデバイスでは [0] のみ有効。
    pub master_peak: [f32; 2],
    /// Mixer Snapshot フェードの全長 (サンプル)。0 = フェード非進行。
    pub snapshot_fade_total: u64,
    /// Mixer Snapshot フェードの残り (サンプル)。
    pub snapshot_fade_remaining: u64,
    /// 生存バス (dense 順)。
    pub buses: Vec<ProfilerBus>,
    /// 生存ソース (dense 順)。
    pub sources: Vec<ProfilerSource>,
}

impl ProfilerFrame {
    fn with_capacity(max_sources: usize) -> Self {
        Self {
            master_peak: [0.0; 2],
            snapshot_fade_total: 0,
            snapshot_fade_remaining: 0,
            buses: Vec::with_capacity(MAX_BUSES),
            sources: Vec::with_capacity(max_sources),
        }
    }

}

pub(crate) type ProfilerFrameIn = triple_buffer::Input<ProfilerFrame>;
pub(crate) type ProfilerFrameOut = triple_buffer::Output<ProfilerFrame>;

/// プロファイラの有効フラグ (メインスレッドが書き、サウンドスレッドが読む)。
pub(crate) type ProfilerEnabled = Arc<AtomicBool>;

/// プロファイラ用 triple buffer を初期化する。
/// 全 3 スロットに `MAX_BUSES` / `max_sources` ぶんの capacity を確保しておき、
/// サウンドスレッドの `clear + push` で再確保が起きないようにする。
pub(crate) fn build_profiler_buffer(max_sources: usize) -> (ProfilerFrameIn, ProfilerFrameOut) {
    let initial = ProfilerFrame::with_capacity(max_sources);
    triple_buffer::triple_buffer(&initial)
}

/// サウンドスレッド: 現在の状態を 1 フレームに詰めて publish する。
///
/// 呼び出し側 (audio_thread) が `enabled` を確認してから呼ぶこと。
/// alloc なし (capacity 済み Vec への clear + push のみ)。
pub(crate) fn publish_profiler_frame(
    input: &mut ProfilerFrameIn,
    master_data: &[f32],
    channels: usize,
    bus_world: &BusWorld,
    source_world: &SourceWorld,
    active_snapshot: &ActiveSnapshot,
) {
    let frame = input.input_buffer_mut();

    // ── master peak (soft limiter 後の callback 内 max |sample|) ──
    let mut peak = [0.0f32; 2];
    if channels >= 2 {
        for pair in master_data.chunks_exact(2) {
            let l = pair[0].abs();
            let r = pair[1].abs();
            if l > peak[0] {
                peak[0] = l;
            }
            if r > peak[1] {
                peak[1] = r;
            }
        }
    } else {
        for &s in master_data {
            let v = s.abs();
            if v > peak[0] {
                peak[0] = v;
            }
        }
        peak[1] = peak[0];
    }
    frame.master_peak = peak;

    // ── Mixer Snapshot フェード進行 ──
    frame.snapshot_fade_total = active_snapshot.fade_total_samples;
    frame.snapshot_fade_remaining = active_snapshot.fade_remaining_samples;

    // ── バス (dense 順) ──
    frame.buses.clear();
    let gains = bus_world.gains();
    let muteds = bus_world.muteds();
    for dense in 0..bus_world.len() {
        let Some(id) = bus_world.entity_at_dense(dense) else {
            continue;
        };
        frame.buses.push(ProfilerBus {
            index: id.index,
            generation: id.generation,
            gain: gains[dense],
            muted: muteds[dense],
        });
    }

    // ── ソース (dense 順) ──
    frame.sources.clear();
    let vols = source_world.vols();
    let pitches = source_world.pitches();
    let states = source_world.states();
    let virtuals = source_world.is_virtuals();
    let buses = source_world.output_buses();
    let offsets = source_world.sample_offsets();
    let buffer_indices = source_world.audio_buffer_indices();
    for dense in 0..source_world.len() {
        let Some(id) = source_world.entity_at_dense(dense) else {
            continue;
        };
        let (bus_index, bus_generation) = bus_world
            .entity_at_dense(buses[dense] as usize)
            .map(|b| (b.index, b.generation))
            .unwrap_or((u32::MAX, 0));
        frame.sources.push(ProfilerSource {
            index: id.index,
            generation: id.generation,
            bus_index,
            bus_generation,
            buffer_index: buffer_indices[dense],
            volume: vols[dense],
            pitch: pitches[dense],
            sample_offset: offsets[dense],
            state: match states[dense] {
                SourceState::Stopped => 0,
                SourceState::Playing => 1,
                SourceState::Scheduled => 2,
                SourceState::Pausing => 3,
            },
            is_virtual: virtuals[dense],
        });
    }

    input.publish();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_computed_from_interleaved_stereo() {
        let (mut input, mut output) = build_profiler_buffer(4);
        let bus_world = BusWorld::new();
        let source_world = SourceWorld::new();
        let active = ActiveSnapshot::new();
        // L=0.5 / R=-0.9 が最大になるインターリーブ列。
        let data = [0.1, -0.2, 0.5, -0.9, -0.3, 0.4];
        publish_profiler_frame(&mut input, &data, 2, &bus_world, &source_world, &active);
        let frame = output.read();
        assert_eq!(frame.master_peak, [0.5, 0.9]);
        // BusWorld::new() は Master 1 本を持つ。
        assert_eq!(frame.buses.len(), 1);
        assert!(frame.sources.is_empty());
        assert_eq!(frame.snapshot_fade_total, 0);
    }
}
