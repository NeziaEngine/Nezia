//! エンジン初期化時のキャパシティ設定。
//!
//! `MAX_SOURCES` / `MAX_PHYSICAL_VOICES` といった「同時発音数」「物理ボイス数」を
//! ランタイムで上書きできるようにする。`SoundEngine::with_config` に渡す。
//! デフォルト値は `crate::source::DEFAULT_MAX_SOURCES` / `DEFAULT_MAX_PHYSICAL_VOICES`
//! と同値で、`SoundEngine::new()` はこのデフォルトを使う。
//!
//! ## 配分の指針
//! - `max_sources`: 論理ボイス上限。同時に存在しうる Source の総数。仮想化されたものを含む。
//! - `max_physical_voices`: 実際にミキシング DSP を流すボイス数。`max_sources` 以下。
//!
//! モバイルなら `max_sources=512, max_physical_voices=24` 程度、PC なら
//! `max_sources=4096, max_physical_voices=64` などタイトル別にチューニングできる。

/// エンジン初期化時のキャパシティ設定。
#[derive(Debug, Clone, Copy)]
#[must_use]
pub struct EngineConfig {
    /// 論理ソース数上限 (`SourceWorld` のスロット総数)。
    pub max_sources: usize,
    /// 物理ボイス数上限 (実 DSP / ミキシングを行うボイス数)。
    /// `max_sources` を超えてはならない。
    pub max_physical_voices: usize,
}

impl EngineConfig {
    /// 設定値の整合性を検査する。違反があれば `Err(&'static str)`。
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.max_sources == 0 {
            return Err("max_sources must be > 0");
        }
        if self.max_physical_voices == 0 {
            return Err("max_physical_voices must be > 0");
        }
        if self.max_physical_voices > self.max_sources {
            return Err("max_physical_voices must be <= max_sources");
        }
        // Phase 1 の実装制約: VoiceVirtualizer や audio thread のスクラッチは
        // `DEFAULT_MAX_SOURCES` を上限とする固定長スタック配列で構成されているため、
        // それを超える `max_sources` は受け付けない (後続 PR でヒープ化予定)。
        if self.max_sources > crate::source::DEFAULT_MAX_SOURCES {
            // Phase 1 の実装制約: VoiceVirtualizer や audio thread の一部スクラッチが
            // `DEFAULT_MAX_SOURCES` を上限とする固定長スタック配列で構成されているため。
            // 後続 PR でヒープ化したら撤廃する。
            return Err("max_sources cannot exceed DEFAULT_MAX_SOURCES (compile-time cap)");
        }
        Ok(())
    }
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            max_sources: crate::source::DEFAULT_MAX_SOURCES,
            max_physical_voices: crate::source::DEFAULT_MAX_PHYSICAL_VOICES,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_valid() {
        assert!(EngineConfig::default().validate().is_ok());
    }

    #[test]
    fn rejects_zero() {
        let cfg = EngineConfig {
            max_sources: 0,
            max_physical_voices: 1,
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_physical_gt_logical() {
        let cfg = EngineConfig {
            max_sources: 8,
            max_physical_voices: 16,
        };
        assert!(cfg.validate().is_err());
    }
}
