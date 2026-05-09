//! メインスレッドからソース状態を問い合わせる公開 API。
//!
//! いずれも `poll_events()` で更新された `source_state_cache` を参照する。
//! 最後の poll 以降の生成・終了は反映されない。フレーム末尾で poll する想定。

use crate::entity::EntityId;

use super::SoundEngine;

impl SoundEngine {
    /// ソースが現在 SourceWorld に存在するかを最新スナップショットで確認する。
    #[must_use]
    pub fn is_source_alive(&self, id: EntityId) -> bool {
        self.source_state_cache.find(id).is_some()
    }

    /// ソースの再生位置（フレーム単位）を最新スナップショットから取得する。
    #[must_use]
    pub fn source_position(&self, id: EntityId) -> Option<f32> {
        self.source_state_cache
            .find(id)
            .map(|i| self.source_state_cache.sample_offsets[i])
    }

    /// 複数ソースの生存を一括判定する。
    ///
    /// `ids` と `out_alive` は同じ長さを持つ前提。`out_alive[i]` には
    /// `ids[i]` が現在の最新スナップショットに存在し generation も一致する場合 `1`、
    /// それ以外は `0` が書き込まれる。
    pub fn batch_is_source_alive(&self, ids: &[EntityId], out_alive: &mut [u8]) {
        let n = ids.len().min(out_alive.len());
        for i in 0..n {
            out_alive[i] = self.source_state_cache.find(ids[i]).is_some() as u8;
        }
    }

    /// 複数ソースの再生位置を一括取得する。
    ///
    /// `ids` / `out_positions` / `out_alive` は同じ長さを持つ前提。
    /// alive でない場合は `out_positions[i]` に `f32::NAN`、`out_alive[i]` に `0`。
    /// `out_alive` を不要なら `&mut []` を渡してもよい（その場合は alive 判定は
    /// `out_positions[i].is_nan()` で代替できる）。
    pub fn batch_source_positions(
        &self,
        ids: &[EntityId],
        out_positions: &mut [f32],
        out_alive: &mut [u8],
    ) {
        let n = ids.len().min(out_positions.len());
        for i in 0..n {
            match self.source_state_cache.find(ids[i]) {
                Some(idx) => {
                    out_positions[i] = self.source_state_cache.sample_offsets[idx];
                    if i < out_alive.len() {
                        out_alive[i] = 1;
                    }
                }
                None => {
                    out_positions[i] = f32::NAN;
                    if i < out_alive.len() {
                        out_alive[i] = 0;
                    }
                }
            }
        }
    }
}
