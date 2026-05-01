use std::sync::Arc;

use crate::audio::AudioBuffer;
use crate::spatial::SpatialWorld;

use super::world::{SourceState, SourceWorld};

/// Source ライフサイクル管理システム。
///
/// 再生が完了した・停止済み・Free 状態の Source を検出し、
/// `SourceWorld` と `SpatialWorld` から一括 despawn する。
/// `SourceMixingSystem` のミキシングとは責務を分離している。
pub struct SourceLifecycleSystem;

impl SourceLifecycleSystem {
    /// 毎オーディオコールバックで `SourceMixingSystem::update()` の直後に呼び出す。
    ///
    /// 逆順で走査することで swap-remove によるインデックスずれを防ぐ。
    pub fn update(
        world: &mut SourceWorld,
        spatial: &mut SpatialWorld,
        buffers: &[Option<Arc<AudioBuffer>>],
    ) {
        for source_i in (0..world.vol.len()).rev() {
            let should_despawn = match world.state[source_i] {
                SourceState::Stopped | SourceState::Free => true,
                SourceState::Playing => {
                    let buf_idx = world.audio_buffer_index[source_i] as usize;
                    match buffers.get(buf_idx).and_then(|b| b.as_ref()) {
                        Some(ab) => world.sample_offset[source_i] as usize >= ab.frame_count(),
                        None => true,
                    }
                }
                SourceState::Pausing => false,
            };
            if should_despawn {
                world.despawn_by_dense_index(source_i);
                spatial.swap_remove(source_i);
            }
        }
    }
}
