use crate::spatial::SpatialWorld;

use super::world::{SourceState, SourceWorld};

/// Voice Virtualization システム。
///
/// 毎フレーム冒頭で各 Playing ソースの「実効可聴度 (effective audibility)」を計算し、
/// 上位 `MAX_PHYSICAL_VOICES` 件を物理ボイス、それ以外を仮想ボイスとする。
///
/// 実効可聴度の指標:
/// ```text
///   audibility = vol * gain_avg * priority_weight
/// ```
/// - `vol`: ユーザー設定音量 (`SourceWorld::vol`)
/// - `gain_avg`: 空間ゲイン平均 `(left_gain + right_gain) / 2`。`spatial_enabled = false` のソースは `vol` 自体が代入されているため自然に整合する
/// - `priority_weight`: `priority / 255`、Wwise / CRI ADX2 互換 (高い priority = 高優先 = 大きい重み)
///
/// `SpatialSystem::compute_gains()` の **後**、`SourceMixingSystem::update()` の **前** に呼び出す。
pub struct VoiceVirtualizer;

impl VoiceVirtualizer {
    /// 全 Playing ソースの可聴度をスコアリングし、上位 `MAX_PHYSICAL_VOICES` を物理化する。
    ///
    /// 内部で `is_virtual` SoA を全更新する。Pausing/Stopped ソースは常に `is_virtual = false`
    /// とする (ミキシング段で state チェックでスキップされるため、virtual フラグは無関係)。
    ///
    /// # 計算量
    /// O(N + N log N)。N = `source_world.len() <= MAX_SOURCES = 256` で実用上は微少。
    /// quickselect で O(N) にできるが Phase 2 では sort で十分。
    pub fn rebalance(world: &mut SourceWorld, spatial: &SpatialWorld, max_physical_voices: usize) {
        let n = world.len();
        if n == 0 {
            return;
        }
        // Playing ソースが max_physical_voices 以下なら全部物理化して即終了 (高速経路)。
        let playing_count = world
            .states()
            .iter()
            .filter(|s| **s == SourceState::Playing)
            .count();
        if playing_count <= max_physical_voices {
            let virtuals = world.is_virtuals_mut();
            virtuals[..n].fill(false);
            return;
        }

        // スコアリング: (audibility, dense_index) のタプル配列を作る。
        // alloc を毎フレーム避けるため固定長配列でスタック確保 (MAX_SOURCES = 256)。
        // N=256 で 256 * 8 = 2 KB なのでスタックに置ける。
        let mut scores: [(f32, u32); super::MAX_SOURCES] = [(0.0, 0); super::MAX_SOURCES];
        let mut score_count = 0usize;

        // 借用順序: 不変借用でスコアを作り終えてから可変借用に切り替える。
        {
            let vols = &world.vol;
            let priorities = world.priorities();
            let states = world.states();
            let left_gains = &spatial.left_gains;
            let right_gains = &spatial.right_gains;

            for i in 0..n {
                if states[i] != SourceState::Playing {
                    continue;
                }
                let gain_avg = 0.5 * (left_gains[i] + right_gains[i]);
                let priority_weight = priorities[i] as f32 / 255.0;
                let audibility = vols[i] * gain_avg.abs() * priority_weight;
                scores[score_count] = (audibility, i as u32);
                score_count += 1;
            }
        }

        // 降順ソート。NaN は 0.0 として扱う (NaN を含むスコアは末尾に来るが致命傷ではない)。
        scores[..score_count]
            .sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        // state 配列のスナップショットを取得 (可変借用前に読み取る)。
        // 固定長 MAX_SOURCES で alloc なし。
        let mut state_snap: [SourceState; super::MAX_SOURCES] =
            [SourceState::Stopped; super::MAX_SOURCES];
        state_snap[..n].copy_from_slice(&world.states()[..n]);

        // 可変借用に切り替えて is_virtual を更新。
        let virtuals = world.is_virtuals_mut();
        // 一旦すべて virtual 候補に。
        virtuals[..n].fill(true);
        // 上位 max_physical_voices を物理化。
        let physical_count = score_count.min(max_physical_voices);
        for &(_, dense) in scores[..physical_count].iter() {
            virtuals[dense as usize] = false;
        }
        // Pausing/Stopped は virtual フラグの意味がないが、整合性のため false に戻す。
        for (i, vflag) in virtuals.iter_mut().enumerate().take(n) {
            if state_snap[i] != SourceState::Playing {
                *vflag = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::DEFAULT_MAX_PHYSICAL_VOICES as MAX_PHYSICAL_VOICES;
    use crate::source::world::SourceComponent;
    use crate::spatial::SpatialWorld;

    fn make_world(n: usize) -> (SourceWorld, SpatialWorld) {
        let mut world = SourceWorld::new();
        let mut spatial = SpatialWorld::new();
        for _ in 0..n {
            world
                .spawn(SourceComponent {
                    vol: 1.0,
                    ..SourceComponent::default()
                })
                .unwrap();
            spatial.push_defaults();
        }
        // 全ソースを 2D (spatial_enabled=false 既定) で left_gain = right_gain = vol 相当にしたい。
        // SpatialSystem::compute_gains は呼ばないので left/right を手で設定。
        for i in 0..n {
            spatial.left_gains[i] = 1.0;
            spatial.right_gains[i] = 1.0;
        }
        (world, spatial)
    }

    #[test]
    fn under_budget_keeps_all_physical() {
        let (mut world, spatial) = make_world(MAX_PHYSICAL_VOICES);
        VoiceVirtualizer::rebalance(&mut world, &spatial, MAX_PHYSICAL_VOICES);
        for &v in world.is_virtuals() {
            assert!(!v);
        }
    }

    #[test]
    fn over_budget_virtualizes_excess() {
        let (mut world, spatial) = make_world(MAX_PHYSICAL_VOICES + 8);
        VoiceVirtualizer::rebalance(&mut world, &spatial, MAX_PHYSICAL_VOICES);
        let phys = world.is_virtuals().iter().filter(|v| !**v).count();
        let virt = world.is_virtuals().iter().filter(|v| **v).count();
        assert_eq!(phys, MAX_PHYSICAL_VOICES);
        assert_eq!(virt, 8);
    }

    #[test]
    fn higher_priority_protected_from_virtualization() {
        // 全部 priority=50 (低優先) にし、最後の 1 体だけ priority=255 (最高優先) にする。
        // MAX_PHYSICAL_VOICES より多く生成しても、priority=255 のソースは必ず物理化される。
        let n = MAX_PHYSICAL_VOICES + 16;
        let mut world = SourceWorld::new();
        let mut spatial = SpatialWorld::new();
        let mut high_prio_id = None;
        for i in 0..n {
            let prio = if i == n - 1 { 255 } else { 50 };
            let id = world
                .spawn(SourceComponent {
                    vol: 1.0,
                    priority: prio,
                    ..SourceComponent::default()
                })
                .unwrap();
            spatial.push_defaults();
            spatial.left_gains[i] = 1.0;
            spatial.right_gains[i] = 1.0;
            if prio == 255 {
                high_prio_id = Some(id);
            }
        }
        VoiceVirtualizer::rebalance(&mut world, &spatial, MAX_PHYSICAL_VOICES);
        let high_dense = world.resolve(high_prio_id.unwrap()).unwrap();
        assert!(
            !world.is_virtuals()[high_dense],
            "highest priority source must be physical"
        );
    }

    #[test]
    fn louder_source_protected_over_quieter() {
        // 全ソース priority 同一、vol だけ違う。最大 vol のソースは物理化される。
        let n = MAX_PHYSICAL_VOICES + 4;
        let mut world = SourceWorld::new();
        let mut spatial = SpatialWorld::new();
        let mut loud_id = None;
        for i in 0..n {
            let vol = if i == 0 { 1.0 } else { 0.05 };
            let id = world
                .spawn(SourceComponent {
                    vol,
                    ..SourceComponent::default()
                })
                .unwrap();
            spatial.push_defaults();
            spatial.left_gains[i] = vol;
            spatial.right_gains[i] = vol;
            if i == 0 {
                loud_id = Some(id);
            }
        }
        VoiceVirtualizer::rebalance(&mut world, &spatial, MAX_PHYSICAL_VOICES);
        let dense = world.resolve(loud_id.unwrap()).unwrap();
        assert!(
            !world.is_virtuals()[dense],
            "loudest source must be physical"
        );
    }

    #[test]
    fn paused_sources_excluded_from_budget() {
        // MAX_PHYSICAL_VOICES + 16 体生成し、ほとんどを Pausing にする。
        // Playing 数が予算内なら全 Playing が物理化される。
        let (mut world, spatial) = make_world(MAX_PHYSICAL_VOICES + 16);
        let total = world.len();
        for dense in MAX_PHYSICAL_VOICES..total {
            let id = world.entity_at_dense(dense).unwrap();
            world.set_state(id, SourceState::Pausing);
        }
        VoiceVirtualizer::rebalance(&mut world, &spatial, MAX_PHYSICAL_VOICES);
        for dense in 0..MAX_PHYSICAL_VOICES {
            assert!(
                !world.is_virtuals()[dense],
                "playing source {} should be physical",
                dense
            );
        }
    }
}
