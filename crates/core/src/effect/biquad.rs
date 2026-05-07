/// Biquad 1 段フィルタ係数 (Direct Form I の Transposed Form II 用に正規化済み)。
///
/// Robert Bristow-Johnson "Audio EQ Cookbook" の RBJ 式に従う。
/// `a0` は normalize 時に 1.0 にしてあるため保持しない。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BiquadCoeffs {
    pub b0: f32,
    pub b1: f32,
    pub b2: f32,
    pub a1: f32,
    pub a2: f32,
}

impl BiquadCoeffs {
    /// LPF (RBJ): cutoff_hz, Q から係数を計算する。
    /// `cutoff_hz` は [20, sample_rate/2 - 100] でクランプされる。
    /// `q` は [0.05, 20.0] でクランプされる (極端な値の発散防止)。
    pub fn lpf(cutoff_hz: f32, q: f32, sample_rate: f32) -> Self {
        let (sin_w0, cos_w0, alpha) = rbj_common(cutoff_hz, q, sample_rate);
        let b1 = 1.0 - cos_w0;
        let b0 = b1 * 0.5;
        let b2 = b0;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha;
        let _ = sin_w0;
        Self::normalize(b0, b1, b2, a0, a1, a2)
    }

    /// Peaking EQ (RBJ): 中心周波数 `center_hz` 付近を `gain_db` だけ持ち上げ/削る。
    /// `gain_db = 0` で素通し (恒等)。`gain_db > 0` で boost、`< 0` で cut。
    /// `gain_db` は [-24.0, +24.0] dB にクランプ (運用上の暴走防止)。
    pub fn peaking_eq(center_hz: f32, q: f32, gain_db: f32, sample_rate: f32) -> Self {
        let (_sin_w0, cos_w0, alpha) = rbj_common(center_hz, q, sample_rate);
        let g = gain_db.clamp(-24.0, 24.0);
        // A = 10^(gain_db/40)。peaking では b 側に A、a 側に 1/A が入る非対称形。
        let a_amp = 10.0_f32.powf(g / 40.0);
        let b0 = 1.0 + alpha * a_amp;
        let b1 = -2.0 * cos_w0;
        let b2 = 1.0 - alpha * a_amp;
        let a0 = 1.0 + alpha / a_amp;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha / a_amp;
        Self::normalize(b0, b1, b2, a0, a1, a2)
    }

    /// HPF (RBJ)。
    pub fn hpf(cutoff_hz: f32, q: f32, sample_rate: f32) -> Self {
        let (_sin_w0, cos_w0, alpha) = rbj_common(cutoff_hz, q, sample_rate);
        let b0 = (1.0 + cos_w0) * 0.5;
        let b1 = -(1.0 + cos_w0);
        let b2 = b0;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha;
        Self::normalize(b0, b1, b2, a0, a1, a2)
    }

    fn normalize(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        let inv = 1.0 / a0;
        Self {
            b0: b0 * inv,
            b1: b1 * inv,
            b2: b2 * inv,
            a1: a1 * inv,
            a2: a2 * inv,
        }
    }

    /// 素通し (b0=1, それ以外 0)。dirty フラグ初期値や safe fallback に使う。
    pub const PASSTHROUGH: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };
}

fn rbj_common(cutoff_hz: f32, q: f32, sample_rate: f32) -> (f32, f32, f32) {
    let nyquist = sample_rate * 0.5;
    let f = cutoff_hz.clamp(20.0, (nyquist - 100.0).max(100.0));
    let q = q.clamp(0.05, 20.0);
    let w0 = 2.0 * std::f32::consts::PI * f / sample_rate;
    let sin_w0 = w0.sin();
    let cos_w0 = w0.cos();
    let alpha = sin_w0 / (2.0 * q);
    (sin_w0, cos_w0, alpha)
}

/// Biquad 1 段の DF-I 状態 (チャネルごと)。
/// Direct Form I: y[n] = b0*x[n] + b1*x[n-1] + b2*x[n-2] - a1*y[n-1] - a2*y[n-2]
#[derive(Debug, Clone, Copy, Default)]
pub struct BiquadState {
    pub x1: f32,
    pub x2: f32,
    pub y1: f32,
    pub y2: f32,
}

impl BiquadState {
    /// 1 サンプル処理。
    #[inline]
    pub fn process(&mut self, input: f32, c: &BiquadCoeffs) -> f32 {
        let y = c.b0 * input + c.b1 * self.x1 + c.b2 * self.x2 - c.a1 * self.y1 - c.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = input;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps
    }

    #[test]
    fn passthrough_returns_input() {
        let mut s = BiquadState::default();
        let c = BiquadCoeffs::PASSTHROUGH;
        for i in 0..8 {
            let x = i as f32 * 0.1;
            assert!(approx(s.process(x, &c), x, 1e-6));
        }
    }

    #[test]
    fn lpf_dc_gain_is_unity() {
        // LPF は DC (0 Hz) で gain = 1.0 (b0 + b1 + b2) / (1 + a1 + a2)
        let c = BiquadCoeffs::lpf(1000.0, 0.707, 44100.0);
        let dc_gain = (c.b0 + c.b1 + c.b2) / (1.0 + c.a1 + c.a2);
        assert!(approx(dc_gain, 1.0, 1e-3));
    }

    #[test]
    fn hpf_dc_gain_is_zero() {
        let c = BiquadCoeffs::hpf(1000.0, 0.707, 44100.0);
        let dc_gain = (c.b0 + c.b1 + c.b2) / (1.0 + c.a1 + c.a2);
        assert!(approx(dc_gain, 0.0, 1e-3));
    }

    #[test]
    fn lpf_attenuates_high_frequency() {
        // 10 kHz サイン波を 1 kHz cutoff の LPF に通す → 出力振幅が小さくなる
        let sr = 44100.0;
        let c = BiquadCoeffs::lpf(1000.0, 0.707, sr);
        let mut s = BiquadState::default();
        let mut max_abs = 0.0_f32;
        // 1024 サンプル分（約 23ms）走らせて定常応答を見る
        for n in 0..2048 {
            let x = (2.0 * std::f32::consts::PI * 10_000.0 * n as f32 / sr).sin();
            let y = s.process(x, &c);
            if n > 1024 {
                max_abs = max_abs.max(y.abs());
            }
        }
        assert!(
            max_abs < 0.2,
            "expected strong attenuation at 10kHz, got max_abs={max_abs}"
        );
    }

    #[test]
    fn peaking_eq_zero_db_is_passthrough() {
        // gain_db = 0 のとき peaking EQ は恒等 (DC ゲインも全帯域もユニティ)。
        let c = BiquadCoeffs::peaking_eq(1000.0, 1.0, 0.0, 44100.0);
        let dc_gain = (c.b0 + c.b1 + c.b2) / (1.0 + c.a1 + c.a2);
        assert!(approx(dc_gain, 1.0, 1e-3));
        // ナイキスト近傍 (z = -1 → 1 - z^-1 + z^-2) のゲインも 1。
        let nyq_gain = (c.b0 - c.b1 + c.b2) / (1.0 - c.a1 + c.a2);
        assert!(approx(nyq_gain, 1.0, 1e-3));
    }

    #[test]
    fn peaking_eq_boost_amplifies_at_center() {
        // 1 kHz で +12 dB ブースト → 1 kHz サイン波の出力振幅は ~4x (10^(12/20)=3.98)。
        let sr = 44100.0;
        let c = BiquadCoeffs::peaking_eq(1000.0, 1.0, 12.0, sr);
        let mut s = BiquadState::default();
        let mut max_abs = 0.0_f32;
        for n in 0..4096 {
            let x = (2.0 * std::f32::consts::PI * 1000.0 * n as f32 / sr).sin();
            let y = s.process(x, &c);
            if n > 2048 {
                max_abs = max_abs.max(y.abs());
            }
        }
        // 入力振幅 1.0、+12 dB → ~3.98 倍。許容 ±10%。
        assert!(
            (3.5..=4.5).contains(&max_abs),
            "expected ~4x boost at 1kHz, got {max_abs}"
        );
    }

    #[test]
    fn peaking_eq_cut_attenuates_at_center() {
        // 1 kHz で -12 dB カット → 1 kHz サイン波の出力振幅は ~0.25x (10^(-12/20)=0.251)。
        let sr = 44100.0;
        let c = BiquadCoeffs::peaking_eq(1000.0, 1.0, -12.0, sr);
        let mut s = BiquadState::default();
        let mut max_abs = 0.0_f32;
        for n in 0..4096 {
            let x = (2.0 * std::f32::consts::PI * 1000.0 * n as f32 / sr).sin();
            let y = s.process(x, &c);
            if n > 2048 {
                max_abs = max_abs.max(y.abs());
            }
        }
        assert!(max_abs < 0.35, "expected ~0.25x cut at 1kHz, got {max_abs}");
    }

    #[test]
    fn peaking_eq_leaves_distant_frequency_intact() {
        // 1 kHz center で +12 dB しても、100 Hz の信号は (Q=1) ほぼ素通し (1.0 付近)。
        let sr = 44100.0;
        let c = BiquadCoeffs::peaking_eq(1000.0, 1.0, 12.0, sr);
        let mut s = BiquadState::default();
        let mut max_abs = 0.0_f32;
        for n in 0..4096 {
            let x = (2.0 * std::f32::consts::PI * 100.0 * n as f32 / sr).sin();
            let y = s.process(x, &c);
            if n > 2048 {
                max_abs = max_abs.max(y.abs());
            }
        }
        // ピーキング EQ は中心から離れるとユニティに収束。
        assert!(
            (0.7..=1.3).contains(&max_abs),
            "expected near-unity at 100Hz with 1kHz center, got {max_abs}"
        );
    }

    #[test]
    fn hpf_attenuates_low_frequency() {
        let sr = 44100.0;
        let c = BiquadCoeffs::hpf(1000.0, 0.707, sr);
        let mut s = BiquadState::default();
        let mut max_abs = 0.0_f32;
        for n in 0..2048 {
            let x = (2.0 * std::f32::consts::PI * 100.0 * n as f32 / sr).sin();
            let y = s.process(x, &c);
            if n > 1024 {
                max_abs = max_abs.max(y.abs());
            }
        }
        assert!(
            max_abs < 0.2,
            "expected strong attenuation at 100Hz, got max_abs={max_abs}"
        );
    }
}
