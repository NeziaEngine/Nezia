use ringbuf::traits::Producer;

use crate::command::Command;
use crate::entity::{EntityId, SourcePositionUpdate};
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
    ///
    /// 共有 atomic スロットへ直接書き込む（コマンドキュー非経由・キュー満杯失敗なし）。
    /// 反映は次のオーディオコールバックで（典型 5〜10 ms）。
    #[must_use]
    pub fn set_source_spatial_enabled(&mut self, id: EntityId, enabled: bool) -> bool {
        self.live_params.store_spatial_enabled(id, enabled);
        true
    }

    /// SP-06: リスナーフォーカスを設定する（変更時のみ呼び出す）。
    ///
    /// `focus_point` はワールド空間の補助座標。
    /// `distance_focus_level` / `direction_focus_level` は `[0.0, 1.0]` 範囲で、
    /// それぞれ距離減衰計算とパンニング計算に使う仮想リスナー位置の補間係数。
    /// 0.0 でリスナー位置のみ使用（フォーカス無効）、1.0 でフォーカス点完全採用、
    /// 0.5 で中点。値域外は内部でクランプされる。
    ///
    /// 用途:
    /// - カメラはプレイヤー背後、聴取点はキャラクター寄りに引き寄せる
    /// - 距離はカメラ基準のまま、定位だけキャラクター基準にする（TPS 演出）
    #[must_use]
    pub fn set_listener_focus(
        &mut self,
        focus_point: [f32; 3],
        distance_focus_level: f32,
        direction_focus_level: f32,
    ) -> bool {
        self.command_producer
            .try_push(Command::SetListenerFocus {
                focus_point,
                distance_focus_level,
                direction_focus_level,
            })
            .is_ok()
    }

    /// 複数ソースの位置を一括更新する（毎フレーム用）。
    ///
    /// triple buffer 経由で publish するため、リングバッファ詰まりで失敗しない。
    /// `MAX_SOURCES` を超える分は切り捨てる（事前確保された容量を超えると
    /// メインスレッド側で realloc が発生し、リアルタイム制約とは関係ないが
    /// alloc コストが上がるため）。
    pub fn batch_set_source_positions(&mut self, updates: &[SourcePositionUpdate]) {
        let buf = self.position_updates_input.input_buffer_mut();
        buf.clear();
        let take = updates.len().min(MAX_SOURCES);
        buf.extend_from_slice(&updates[..take]);
        self.position_updates_input.publish();
    }
}
