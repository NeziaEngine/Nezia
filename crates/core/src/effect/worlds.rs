//! 種別別 World (LPF / HPF / Reverb / Compressor / PeakingEq / Limiter) を 1 構造体に束ねる。
//!
//! `EffectKind` ごとに独立した `*World` を全関数の引数に個別に糸通すと、種別追加のたびに
//! 数十ファイルのシグネチャに変更が波及していた。本構造体はそれらを単一引数で持ち回るための
//! 単純な「束」であり、フィールドアクセスは inline 展開されるためパフォーマンス影響はない。
//!
//! 借用面は **すべての呼び出し側が 6 World を同時に `&mut` で扱う** 既存挙動を踏襲しているため、
//! 種別単位の split borrow には依存していない。必要な箇所では `&mut worlds.compressor` のように
//! フィールド単位で借用すれば従来 API もそのまま呼べる。

use super::{CompressorWorld, HpfWorld, LimiterWorld, LpfWorld, PeakingEqWorld, ReverbWorld};

/// 種別別 World の束。
///
/// フィールドは公開してあるので、種別単独の借用が必要な箇所 (例: bus_system::apply_sends の
/// CompressorSidechain dest) からは `&mut worlds.compressor` のように直接アクセスできる。
pub struct EffectWorlds {
    pub lpf: LpfWorld,
    pub hpf: HpfWorld,
    pub reverb: ReverbWorld,
    pub compressor: CompressorWorld,
    pub peq: PeakingEqWorld,
    pub limiter: LimiterWorld,
}

impl Default for EffectWorlds {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectWorlds {
    pub fn new() -> Self {
        Self {
            lpf: LpfWorld::new(),
            hpf: HpfWorld::new(),
            reverb: ReverbWorld::new(),
            compressor: CompressorWorld::new(),
            peq: PeakingEqWorld::new(),
            limiter: LimiterWorld::new(),
        }
    }

    /// 全種別の `flush_dirty` をまとめて呼ぶ (コールバック冒頭の係数再計算)。
    /// `ReverbWorld::flush_dirty` のみ sample_rate 不要だが内側で吸収する。
    #[inline]
    pub fn flush_dirty(&mut self, sample_rate: f32) {
        self.lpf.flush_dirty(sample_rate);
        self.hpf.flush_dirty(sample_rate);
        self.reverb.flush_dirty();
        self.compressor.flush_dirty(sample_rate);
        self.peq.flush_dirty(sample_rate);
        self.limiter.flush_dirty(sample_rate);
    }

    /// Compressor sidechain buffer のゼロクリア (Send tap 書き込み前の必須前処理)。
    #[inline]
    pub fn clear_compressor_sidechain_buffers(&mut self, sample_count: usize) {
        self.compressor.clear_sidechain_buffers(sample_count);
    }

    /// 全種別 World が確保しているヒープ実バイト数の合計 (`memory_stats` walker 用)。
    pub(crate) fn memory_bytes(&self) -> usize {
        self.lpf.memory_bytes()
            + self.hpf.memory_bytes()
            + self.reverb.memory_bytes()
            + self.compressor.memory_bytes()
            + self.peq.memory_bytes()
            + self.limiter.memory_bytes()
    }
}
