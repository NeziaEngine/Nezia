//! マスター出力段の soft limiter。
//!
//! 多重再生でサンプル総和が ±1.0 を超えるとデバイス側でハードクリップして「音割れ」になる。
//! 本モジュールは BusSystem の出力 (= デバイスへ渡す直前の f32 PCM) に対し、
//! 透過域 + rational soft knee で ±1.0 に漸近する 1-段リミッタを提供する。
//!
//! - 透過: `|x| <= KNEE_START` (= 0.8) は完全パススルー (ゲイン段に影響を与えない)
//! - knee: `KNEE_START < |x|` で signum × (T + (1-T)(over)/(over + (1-T))) に置換
//! - 漸近: `|x| -> ∞` で出力は `±1.0` に漸近する (= デバイス飽和を防ぐ)
//!
//! 連続関数 (微分不連続は KNEE_START でのみ) で、Unity 等のドロップイン互換時に
//! "他のエンジンと音量感が揃う" 程度の自然な飽和挙動を出す。
//!
//! 計算量はサンプルあたり数 op (abs/cmp/2 mul/1 div) で、1024 サンプル/コールバックでも
//! 数 µs 以内。SIMD 化はしていないが、`#[inline]` + 単純な算術なので autovectorizer に
//! 任せれば AVX/SSE の `vpd*` 系で並列化される想定。

const KNEE_START: f32 = 0.8;
/// 漸近するセイリング (ハード上限。出力はここに無限に近づくが届かない)。
const CEILING: f32 = 1.0;

/// 1 サンプルに soft limiter を適用する。
///
/// `|x| <= 0.8` は恒等写像、それ以上は ±1.0 に漸近する rational soft knee。
#[inline]
#[must_use]
pub fn soft_clip_sample(x: f32) -> f32 {
    let ax = x.abs();
    if ax <= KNEE_START {
        return x;
    }
    let over = ax - KNEE_START;
    let range = CEILING - KNEE_START;
    // y = T + range * over / (over + range)
    //   over=0    -> y = T            (連続)
    //   over=∞    -> y = T + range    = CEILING (漸近)
    let knee = range * over / (over + range);
    let mag = KNEE_START + knee;
    if x.is_sign_negative() { -mag } else { mag }
}

/// バッファ全体にインプレースで soft limiter をかける。
///
/// チャンネルインターリーブの形式に依存しない (サンプル単位で独立した処理)。
#[inline]
pub fn apply_soft_clip(buffer: &mut [f32]) {
    for s in buffer.iter_mut() {
        *s = soft_clip_sample(*s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transparent_below_knee() {
        for &x in &[
            0.0_f32,
            0.1,
            0.5,
            0.79,
            -0.5,
            -0.79,
            KNEE_START,
            -KNEE_START,
        ] {
            let y = soft_clip_sample(x);
            assert!(
                (y - x).abs() < 1e-7,
                "expected pass-through for x={x}, got {y}"
            );
        }
    }

    #[test]
    fn limits_above_knee() {
        // 1.0 -> 0.8 + 0.2 * 0.2/0.4 = 0.9
        let y = soft_clip_sample(1.0);
        assert!((y - 0.9).abs() < 1e-6, "y(1.0)={y}");

        // 1.5 -> 0.8 + 0.2 * 0.7/0.9 = 0.9555..
        let y = soft_clip_sample(1.5);
        assert!((y - (0.8 + 0.2 * 0.7 / 0.9)).abs() < 1e-6, "y(1.5)={y}");
    }

    #[test]
    fn never_exceeds_ceiling() {
        // 極端に大きな入力では f32 精度の上で `1.0` に丸まり得るが、`> 1.0` には
        // ならない (デバイス側ハードクリップを避けるという目的を満たす)。
        for x in [1.0_f32, 2.0, 5.0, 100.0, 1.0e6, f32::MAX / 2.0] {
            let y = soft_clip_sample(x);
            assert!(y <= CEILING, "y({x})={y} should be <= {CEILING}");
            let y = soft_clip_sample(-x);
            assert!(y >= -CEILING, "y({})={y} should be >= -{CEILING}", -x);
        }
    }

    #[test]
    fn asymptotes_to_ceiling() {
        // 大きな入力に対して 1.0 にどんどん近づく。
        let y100 = soft_clip_sample(100.0);
        let y1k = soft_clip_sample(1000.0);
        assert!(
            y100 < y1k,
            "monotonic toward ceiling: y100={y100}, y1k={y1k}"
        );
        assert!(y1k > 0.999, "should be very close to ceiling: y1k={y1k}");
    }

    #[test]
    fn continuity_at_knee() {
        // KNEE_START 上下で連続性を確認 (| y(T+ε) - y(T-ε) | が小さい)。
        let eps = 1e-4;
        let lo = soft_clip_sample(KNEE_START - eps);
        let hi = soft_clip_sample(KNEE_START + eps);
        assert!(
            (hi - lo).abs() < 5.0 * eps,
            "discontinuity at knee: lo={lo}, hi={hi}"
        );
    }

    #[test]
    fn sign_preserving() {
        for x in [-2.0_f32, -1.5, -1.0, -0.5, 0.5, 1.0, 1.5, 2.0] {
            let y = soft_clip_sample(x);
            if x > 0.0 {
                assert!(y > 0.0, "x={x} y={y}");
            } else {
                assert!(y < 0.0, "x={x} y={y}");
            }
        }
    }

    #[test]
    fn buffer_apply_matches_per_sample() {
        let input = vec![0.0_f32, 0.5, 0.9, 1.0, 1.5, -1.0, -2.0, 0.3];
        let mut buf = input.clone();
        apply_soft_clip(&mut buf);
        for (i, (&x, &y)) in input.iter().zip(buf.iter()).enumerate() {
            assert!(
                (y - soft_clip_sample(x)).abs() < 1e-7,
                "mismatch at [{i}]: x={x} buf={y} expected={}",
                soft_clip_sample(x)
            );
        }
    }
}
