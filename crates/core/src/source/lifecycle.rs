use std::sync::Arc;

use crate::audio::AudioBuffer;
use crate::event::Event;
use crate::spatial::SpatialWorld;

use super::world::{SourceState, SourceWorld};

/// Source ライフサイクル管理システム。
///
/// 再生が完了した・停止済みの Source を検出し、
/// `SourceWorld` と `SpatialWorld` から一括 despawn する。
/// `SourceMixingSystem` のミキシングとは責務を分離している。
pub struct SourceLifecycleSystem;

impl SourceLifecycleSystem {
    /// 毎オーディオコールバックで `SourceMixingSystem::update()` の直後に呼び出す。
    ///
    /// 逆順で走査することで swap-remove によるインデックスずれを防ぐ。
    /// `emit_event` は `SourceFinished` イベントをリングバッファに push するクロージャ。
    pub fn update(
        world: &mut SourceWorld,
        spatial: &mut SpatialWorld,
        buffers: &[Option<Arc<AudioBuffer>>],
        emit_event: &mut dyn FnMut(Event),
    ) {
        for source_i in (0..world.vol.len()).rev() {
            let natural_finish = match world.state[source_i] {
                SourceState::Stopped => false,
                SourceState::Playing => {
                    if world.looping[source_i] {
                        false
                    } else {
                        let buf_idx = world.audio_buffer_index[source_i] as usize;
                        match buffers.get(buf_idx).and_then(|b| b.as_ref()) {
                            Some(ab) => world.sample_offset[source_i] as usize >= ab.frame_count(),
                            None => true,
                        }
                    }
                }
                SourceState::Pausing => false,
            };
            let should_despawn =
                natural_finish || matches!(world.state[source_i], SourceState::Stopped);

            if should_despawn {
                let id = world.entity_at_dense(source_i);
                if natural_finish {
                    let token = world.token[source_i];
                    if token != 0 {
                        emit_event(Event::SourceFinished { token });
                    }
                }
                if let Some(id) = id {
                    emit_event(Event::SourceDespawned { id });
                }
                world.despawn_by_dense_index(source_i);
                spatial.swap_remove(source_i);
            }
        }
    }
}
