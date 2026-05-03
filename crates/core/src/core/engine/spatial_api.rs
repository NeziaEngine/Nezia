use ringbuf::traits::Producer;

use crate::command::Command;
use crate::entity::EntityId;
use crate::source::MAX_SOURCES;
use crate::spatial::AttenuationModel;

use super::SoundEngine;

impl SoundEngine {
    /// リスナーの位置・向きを更新する（毎フレーム呼び出す）。
    ///
    /// triple buffer 経由で publish するため、リングバッファ詰まりで失敗しない。
    /// `forward` / `up` はメインスレッドで正規化してから受け渡す。
    pub fn set_listener(&mut self, position: [f32; 3], forward: [f32; 3], up: [f32; 3]) {
        let buf = self.listener_input.input_buffer_mut();
        buf.update(position, forward, up);
        self.listener_input.publish();
    }

    /// ソースの距離減衰パラメータを設定する（初期化・変更時のみ）。
    #[must_use]
    pub fn set_source_spatial_params(
        &mut self,
        id: EntityId,
        model: AttenuationModel,
        min_distance: f32,
        max_distance: f32,
        rolloff: f32,
    ) -> bool {
        self.command_producer
            .try_push(Command::SetSourceSpatialParams {
                id,
                model,
                min_distance,
                max_distance,
                rolloff,
            })
            .is_ok()
    }

    /// ソースの空間演算を有効化・無効化する。
    #[must_use]
    pub fn set_source_spatial_enabled(&mut self, id: EntityId, enabled: bool) -> bool {
        self.command_producer
            .try_push(Command::SetSourceSpatialEnabled { id, enabled })
            .is_ok()
    }

    /// 複数ソースの位置を一括更新する（毎フレーム用）。
    ///
    /// triple buffer 経由で publish するため、リングバッファ詰まりで失敗しない。
    /// `MAX_SOURCES` を超える分は切り捨てる（事前確保された容量を超えると
    /// メインスレッド側で realloc が発生し、リアルタイム制約とは関係ないが
    /// alloc コストが上がるため）。
    pub fn batch_set_source_positions(&mut self, updates: &[(EntityId, [f32; 3])]) {
        let buf = self.position_updates_input.input_buffer_mut();
        buf.clear();
        let take = updates.len().min(MAX_SOURCES);
        buf.extend_from_slice(&updates[..take]);
        self.position_updates_input.publish();
    }
}
