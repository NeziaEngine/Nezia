mod biquad;
mod hpf;
mod lpf;
mod reverb;
mod system;
mod world;

pub use hpf::HpfWorld;
pub use lpf::LpfWorld;
pub use reverb::ReverbWorld;
pub use system::EffectSystem;
pub use world::{EffectKind, EffectPosition, EffectTarget, EffectWorld, Owner};

use crate::entity::EntityId;

/// メタ層エフェクトプール上限 (種別合算)。後から定数調整可能。
pub const MAX_EFFECTS: usize = 256;

/// バス内チェーン上限 (Pre + Post 合算ではなく **各 chain あたり**の上限)。
/// 設計ドキュメント (docs/design/core/dsp.md) を参照。
pub const MAX_EFFECTS_PER_BUS: usize = 8;

/// Source 内チェーン上限 (Phase 2-3 で実装する Pre-Spatial chain の上限)。
pub const MAX_EFFECTS_PER_SOURCE: usize = 4;

/// LPF/HPF プール上限 (Source 単位 LPF の最悪ケース 256 ソース全数を許容)。
pub const MAX_LPF: usize = 256;
pub const MAX_HPF: usize = 256;

/// Reverb プール上限 (Bus 専用、遅延ラインメモリが大きいため少数)。
pub const MAX_REVERBS: usize = 16;

/// EffectId は EntityId を再利用する (sparse-set ベースで二層 ID を踏襲)。
pub type EffectId = EntityId;

/// LPF パラメータインデックス (`set_effect_param` の `param: u8` で使用)。
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LpfParam {
    Cutoff = 0,
    Q = 1,
}

/// HPF パラメータインデックス。
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HpfParam {
    Cutoff = 0,
    Q = 1,
}

/// Reverb パラメータインデックス。すべて正規化値 `[0.0, 1.0]`。
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReverbParam {
    RoomSize = 0,
    Damping = 1,
    Wet = 2,
    Dry = 3,
    Width = 4,
}

/// 型安全パラメータ ID トレイト。種別ごとの enum がこれを実装する。
pub trait EffectParamId: Copy {
    const KIND: EffectKind;
    fn as_u8(self) -> u8;
}

impl EffectParamId for LpfParam {
    const KIND: EffectKind = EffectKind::Lpf;
    fn as_u8(self) -> u8 {
        self as u8
    }
}

impl EffectParamId for HpfParam {
    const KIND: EffectKind = EffectKind::Hpf;
    fn as_u8(self) -> u8 {
        self as u8
    }
}

impl EffectParamId for ReverbParam {
    const KIND: EffectKind = EffectKind::Reverb;
    fn as_u8(self) -> u8 {
        self as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::EntityId;

    #[test]
    fn effect_world_spawn_resolve_despawn() {
        let mut meta = EffectWorld::new();
        let id = EntityId {
            index: 0,
            generation: 0,
        };
        assert!(meta.spawn_with_id(
            id,
            EffectKind::Lpf,
            0,
            Owner::Bus(0),
            EffectPosition::Pre,
            0,
            0
        ));
        assert!(meta.contains(id));
        assert_eq!(meta.kind(id), Some(EffectKind::Lpf));
        let removed = meta.despawn(id).expect("despawn");
        assert_eq!(removed, (EffectKind::Lpf, 0));
        assert!(!meta.contains(id));
    }

    #[test]
    fn lpf_chain_attenuates_high_freq_in_buf() {
        // 1 LPF が 10kHz サイン波を 1kHz cutoff で減衰させることをチェーン経由で確認。
        let mut meta = EffectWorld::new();
        let mut lpf = LpfWorld::new();
        let mut hpf = HpfWorld::new();
        let id = EntityId {
            index: 0,
            generation: 0,
        };
        let state_idx = lpf.spawn(id, 1000.0, 0.707).unwrap();
        meta.spawn_with_id(
            id,
            EffectKind::Lpf,
            0,
            Owner::Bus(0),
            EffectPosition::Pre,
            0,
            state_idx,
        );
        lpf.flush_dirty(44100.0);

        // 10kHz サイン波を 4096 サンプル生成 (mono)
        let n = 4096;
        let mut buf: Vec<f32> = (0..n)
            .map(|k| (2.0 * std::f32::consts::PI * 10_000.0 * k as f32 / 44100.0).sin())
            .collect();
        let mut reverb = ReverbWorld::new();
        EffectSystem::apply_chain(&meta, &mut lpf, &mut hpf, &mut reverb, &[id], &mut buf, 1);
        let max_abs = buf[2048..].iter().map(|v| v.abs()).fold(0.0_f32, f32::max);
        assert!(
            max_abs < 0.2,
            "expected strong LPF attenuation, got {max_abs}"
        );
    }

    #[test]
    fn disabled_effect_passes_through() {
        let mut meta = EffectWorld::new();
        let mut lpf = LpfWorld::new();
        let mut hpf = HpfWorld::new();
        let id = EntityId {
            index: 0,
            generation: 0,
        };
        let state_idx = lpf.spawn(id, 1000.0, 0.707).unwrap();
        meta.spawn_with_id(
            id,
            EffectKind::Lpf,
            0,
            Owner::Bus(0),
            EffectPosition::Pre,
            0,
            state_idx,
        );
        lpf.flush_dirty(44100.0);
        meta.set_enabled(id, false);

        let mut buf = vec![0.5_f32; 256];
        let original = buf.clone();
        let mut reverb = ReverbWorld::new();
        EffectSystem::apply_chain(&meta, &mut lpf, &mut hpf, &mut reverb, &[id], &mut buf, 1);
        assert_eq!(buf, original, "disabled effect must pass through");
    }
}
