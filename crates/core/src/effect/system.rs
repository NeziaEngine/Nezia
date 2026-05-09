use super::compressor::CompressorWorld;
use super::hpf::HpfWorld;
use super::limiter::LimiterWorld;
use super::lpf::LpfWorld;
use super::peq::PeakingEqWorld;
use super::reverb::ReverbWorld;
use super::world::{EffectKind, EffectPosition, EffectWorld, Owner};

/// エフェクト適用 System。
///
/// バス / ソースのチェーン (固定長 slot 配列) を二段階 dispatch (`kind` → `algo`) で
/// 1 スロットずつ走査し、対応する種別 World の `apply` を呼ぶ。
/// `enabled = false` の slot はスキップ (状態は保持)。
pub struct EffectSystem;

impl EffectSystem {
    /// 1 つのチェーン (target/position 指定) に対し、`buf` を in-place 処理する。
    #[allow(clippy::too_many_arguments)]
    pub fn apply_chain(
        meta: &EffectWorld,
        lpf: &mut LpfWorld,
        hpf: &mut HpfWorld,
        reverb: &mut ReverbWorld,
        compressor: &mut CompressorWorld,
        peq: &mut PeakingEqWorld,
        limiter: &mut LimiterWorld,
        chain: &[crate::effect::EffectId],
        buf: &mut [f32],
        channels: usize,
    ) {
        for &eff_id in chain {
            let Some(meta_dense) = meta.resolve(eff_id) else {
                continue;
            };
            if !meta.enableds()[meta_dense] {
                continue;
            }
            let kind = meta.kinds()[meta_dense];
            let algo = meta.algos()[meta_dense];
            let state_index = meta.state_indices()[meta_dense];
            // 二段階 dispatch: kind → algo。Phase 2-3/3-3/3-5 では各種別 1 アルゴリズム (algo == 0) のみ。
            match kind {
                EffectKind::Lpf if algo == 0 => lpf.apply(state_index, buf, channels),
                EffectKind::Hpf if algo == 0 => hpf.apply(state_index, buf, channels),
                EffectKind::Reverb if algo == 0 => reverb.apply(state_index, buf, channels),
                EffectKind::Compressor if algo == 0 => compressor.apply(state_index, buf, channels),
                EffectKind::PeakingEq if algo == 0 => peq.apply(state_index, buf, channels),
                EffectKind::Limiter if algo == 0 => limiter.apply(state_index, buf, channels),
                _ => {}
            }
        }
    }
}

/// `EffectPosition` を別の語彙で説明する補助 (将来 doc 化用)。
#[allow(dead_code)]
pub(super) fn _owner_marker(_o: Owner, _p: EffectPosition) {}
