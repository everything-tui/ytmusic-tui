//! Adaptive EQ: nudge the 10-band graphic EQ toward a taste profile
//! based on the currently playing track's spectral balance.

use crate::player::eq::{BAND_COUNT, BAND_FREQS_HZ};
use rustfft::{num_complex::Complex, FftPlanner};
use std::f32::consts::PI;

/// Default max correction for Balanced / Vocal / Bright.
pub const ADAPTIVE_MAX_DB: f32 = 8.0;
/// Bass Heavy is allowed to push harder (still within EQ ±12).
pub const BASS_MAX_DB: f32 = 12.0;
/// Smoothing toward new gains each update (0..1). Higher = snappier.
const SMOOTH: f32 = 0.35;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AdaptiveProfile {
    #[default]
    Off,
    Balanced,
    Bass,
    Vocal,
    Bright,
}

impl AdaptiveProfile {
    pub const ALL: &[AdaptiveProfile] = &[
        AdaptiveProfile::Off,
        AdaptiveProfile::Balanced,
        AdaptiveProfile::Bass,
        AdaptiveProfile::Vocal,
        AdaptiveProfile::Bright,
    ];

    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Balanced,
            Self::Balanced => Self::Bass,
            Self::Bass => Self::Vocal,
            Self::Vocal => Self::Bright,
            Self::Bright => Self::Off,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Balanced => "balanced",
            Self::Bass => "bass",
            Self::Vocal => "vocal",
            Self::Bright => "bright",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Balanced => "Balanced",
            Self::Bass => "Bass Heavy",
            Self::Vocal => "Vocal",
            Self::Bright => "Bright",
        }
    }

    pub fn from_config(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "balanced" => Self::Balanced,
            "bass" | "bass_heavy" | "bass-heavy" => Self::Bass,
            "vocal" => Self::Vocal,
            "bright" => Self::Bright,
            _ => Self::Off,
        }
    }

    pub fn as_config(self) -> &'static str {
        self.label()
    }

    /// Target relative curve in dB vs mid reference (500 Hz + 1 kHz).
    /// Indices match `BAND_FREQS_HZ`: 32, 64, 125, 250, 500, 1k, 2k, 4k, 8k, 16k.
    pub fn target_relative_db(self) -> [f32; BAND_COUNT] {
        match self {
            Self::Off => [0.0; BAND_COUNT],
            Self::Balanced => [2.0, 1.5, 0.5, 0.0, 0.0, 0.0, 0.0, 0.5, 1.0, 1.5],
            // Aggressive club curve: big sub/bass, scooped low-mids so bass reads louder.
            Self::Bass => [11.0, 10.0, 7.0, 2.0, -1.5, -2.0, -1.5, 0.5, 1.5, 1.0],
            Self::Vocal => [-3.0, -2.5, -1.0, 0.5, 1.0, 2.5, 3.5, 2.5, 1.0, 0.0],
            Self::Bright => [-1.0, -0.5, 0.0, 0.0, 0.0, 0.5, 1.5, 3.0, 4.5, 4.0],
        }
    }

    /// Always-on bias stacked on top of relative matching.
    /// Bass Heavy keeps audible weight even when the track is already warm.
    pub fn preference_bias_db(self) -> [f32; BAND_COUNT] {
        match self {
            Self::Bass => [4.0, 3.5, 2.5, 1.0, 0.0, -0.5, -1.0, 0.0, 0.5, 0.0],
            _ => [0.0; BAND_COUNT],
        }
    }

    pub fn max_db(self) -> f32 {
        match self {
            Self::Bass => BASS_MAX_DB,
            _ => ADAPTIVE_MAX_DB,
        }
    }
}

/// Stateful analyzer + smoothed gain output.
pub struct AdaptiveEq {
    planner: FftPlanner<f32>,
    fft_buf: Vec<Complex<f32>>,
    /// Smoothed gains currently applied (or last computed).
    pub gains_db: [f32; BAND_COUNT],
    last_update: Option<std::time::Instant>,
}

impl Default for AdaptiveEq {
    fn default() -> Self {
        Self {
            planner: FftPlanner::new(),
            fft_buf: Vec::new(),
            gains_db: [0.0; BAND_COUNT],
            last_update: None,
        }
    }
}

impl AdaptiveEq {
    pub fn reset(&mut self) {
        self.gains_db = [0.0; BAND_COUNT];
        self.last_update = None;
    }

    /// Measure spectrum, compute target gains, smooth into `self.gains_db`.
    /// Returns `true` if gains changed enough to re-apply (> 0.15 dB any band).
    pub fn update(
        &mut self,
        samples: &[f32],
        sample_rate: u32,
        profile: AdaptiveProfile,
    ) -> bool {
        if profile == AdaptiveProfile::Off || samples.len() < 256 {
            return false;
        }
        // Throttle heavy FFT work.
        if let Some(t) = self.last_update {
            if t.elapsed() < std::time::Duration::from_millis(300) {
                return false;
            }
        }
        self.last_update = Some(std::time::Instant::now());

        let measured = measure_relative_db(self, samples, sample_rate);
        let desired = compute_gains_for_profile(&measured, profile);

        let mut changed = false;
        for i in 0..BAND_COUNT {
            let next = self.gains_db[i] * (1.0 - SMOOTH) + desired[i] * SMOOTH;
            if (next - self.gains_db[i]).abs() > 0.15 {
                changed = true;
            }
            self.gains_db[i] = next;
        }
        changed
    }
}

/// FFT energy at each EQ centre, converted to dB relative to mid (500+1k).
pub fn measure_relative_db(
    state: &mut AdaptiveEq,
    samples: &[f32],
    sample_rate: u32,
) -> [f32; BAND_COUNT] {
    let n = 2048.min(samples.len());
    let slice = &samples[samples.len() - n..];
    let sr = sample_rate.max(8_000) as f32;

    state.fft_buf.clear();
    let denom = (n as f32 - 1.0).max(1.0);
    for (i, &s) in slice.iter().enumerate() {
        let w = 0.5 * (1.0 - (2.0 * PI * i as f32 / denom).cos());
        state.fft_buf.push(Complex {
            re: s * w,
            im: 0.0,
        });
    }
    let size = n.next_power_of_two().max(256);
    state.fft_buf.resize(size, Complex { re: 0.0, im: 0.0 });
    let fft = state.planner.plan_fft_forward(size);
    fft.process(&mut state.fft_buf);

    let nyquist = sr / 2.0;
    let mut energy = [0.0f32; BAND_COUNT];
    for (bi, &fc) in BAND_FREQS_HZ.iter().enumerate() {
        if fc >= nyquist * 0.98 {
            energy[bi] = 1e-12;
            continue;
        }
        // ~⅔-octave window around centre.
        let f0 = (fc / 1.4).max(20.0);
        let f1 = (fc * 1.4).min(nyquist);
        let i0 = ((f0 / nyquist) * (size as f32 / 2.0)) as usize;
        let i1 = (((f1 / nyquist) * (size as f32 / 2.0)) as usize + 1).min(size / 2);
        let mut sum = 0.0f32;
        let mut count = 0usize;
        for i in i0..i1.max(i0 + 1) {
            let c = state.fft_buf[i];
            sum += c.re * c.re + c.im * c.im;
            count += 1;
        }
        energy[bi] = if count > 0 {
            (sum / count as f32).max(1e-12)
        } else {
            1e-12
        };
    }

    // Absolute dB then relative to mid reference (500 Hz idx 4, 1k idx 5).
    let mut db = [0.0f32; BAND_COUNT];
    for i in 0..BAND_COUNT {
        db[i] = 10.0 * energy[i].log10();
    }
    let mid_ref = (db[4] + db[5]) * 0.5;
    for d in &mut db {
        *d -= mid_ref;
    }
    db
}

/// `gain = clamp(target - measured + bias, ±max)`.
pub fn compute_gains_for_profile(
    measured_rel_db: &[f32; BAND_COUNT],
    profile: AdaptiveProfile,
) -> [f32; BAND_COUNT] {
    let target = profile.target_relative_db();
    let bias = profile.preference_bias_db();
    let max = profile.max_db();
    let mut out = [0.0f32; BAND_COUNT];
    for i in 0..BAND_COUNT {
        out[i] = (target[i] - measured_rel_db[i] + bias[i]).clamp(-max, max);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, sr: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * PI * freq * i as f32 / sr).sin())
            .collect()
    }

    #[test]
    fn profile_cycle() {
        assert_eq!(AdaptiveProfile::Off.next(), AdaptiveProfile::Balanced);
        assert_eq!(AdaptiveProfile::Bright.next(), AdaptiveProfile::Off);
    }

    #[test]
    fn from_config_roundtrip() {
        for p in AdaptiveProfile::ALL {
            assert_eq!(AdaptiveProfile::from_config(p.as_config()), *p);
        }
    }

    #[test]
    fn bass_sine_measures_high_low_bands() {
        let sr = 44_100u32;
        let mut low = sine(60.0, sr as f32, 4096);
        // Mix a little mid so reference isn't silent.
        for (i, s) in low.iter_mut().enumerate() {
            *s += 0.15 * (2.0 * PI * 800.0 * i as f32 / sr as f32).sin();
        }
        let mut state = AdaptiveEq::default();
        let m = measure_relative_db(&mut state, &low, sr);
        // 64 Hz band (idx 1) should sit above the mid reference (≈0).
        assert!(
            m[1] > 2.0,
            "expected bass-heavy relative measure, got {:?}",
            m
        );
    }

    #[test]
    fn bass_profile_boosts_thin_track() {
        let measured = [0.0; BAND_COUNT];
        let g = compute_gains_for_profile(&measured, AdaptiveProfile::Bass);
        // Target + bias → strong sub lift on a flat mix.
        assert!(g[0] >= 10.0, "g[0]={}", g[0]);
        assert!(g[1] >= 9.0, "g[1]={}", g[1]);
        assert!(g[0] <= BASS_MAX_DB);
    }

    #[test]
    fn already_bassy_still_gets_weight() {
        let mut measured = [0.0; BAND_COUNT];
        // Track already matches the aggressive Bass target curve.
        measured[0] = 11.0;
        measured[1] = 10.0;
        measured[2] = 7.0;
        let g = compute_gains_for_profile(&measured, AdaptiveProfile::Bass);
        // Preference bias keeps audible low-end even when spectrum is warm.
        assert!(g[0] >= 3.0, "g[0]={}", g[0]);
        assert!(g[1] >= 3.0, "g[1]={}", g[1]);
    }
}
