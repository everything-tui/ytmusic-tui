//! 10-band peaking equalizer (RBJ cookbook biquads).

use rodio::Source;
use std::f32::consts::PI;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// ISO-ish octave centres spanning sub-bass → air.
pub const BAND_COUNT: usize = 10;
pub const BAND_FREQS_HZ: [f32; BAND_COUNT] = [
    32.0, 64.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0,
];
/// Short labels for the TUI (freq shown separately).
pub const BAND_LABELS: [&str; BAND_COUNT] =
    ["32", "64", "125", "250", "500", "1k", "2k", "4k", "8k", "16k"];
pub const GAIN_MIN_DB: f32 = -12.0;
pub const GAIN_MAX_DB: f32 = 12.0;
/// ~1-octave bandwidth — typical graphic EQ Q.
const Q: f32 = 1.414;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EqParams {
    pub enabled: bool,
    pub gains_db: [f32; BAND_COUNT],
}

impl Default for EqParams {
    fn default() -> Self {
        Self {
            enabled: true,
            gains_db: [0.0; BAND_COUNT],
        }
    }
}

impl EqParams {
    pub fn clamped(mut self) -> Self {
        for g in &mut self.gains_db {
            *g = g.clamp(GAIN_MIN_DB, GAIN_MAX_DB);
        }
        self
    }

    pub fn is_flat(&self) -> bool {
        self.gains_db.iter().all(|g| g.abs() < 0.01)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EqPreset {
    pub name: &'static str,
    pub gains_db: [f32; BAND_COUNT],
}

/// Keys 1–5 in the equalizer overlay. Curves keep peaks ≤ ~7 dB so
/// soft-clipping stays rare and the mix stays clear.
pub const PRESETS: &[EqPreset] = &[
    EqPreset {
        name: "Flat",
        gains_db: [0.0; BAND_COUNT],
    },
    // Lift sub/bass, leave mids alone, tiny air — not a muddy shelf.
    EqPreset {
        name: "Bass",
        gains_db: [6.0, 5.0, 3.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.5, 0.0],
    },
    // Club/EDM smile: sub heavy, scooped low-mids, bright top.
    EqPreset {
        name: "Electronic",
        gains_db: [7.0, 5.5, 2.0, -1.5, -3.0, -1.5, 0.5, 2.5, 4.5, 3.5],
    },
    // Cut rumble, boost presence / intelligibility (podcasts, vocals).
    EqPreset {
        name: "Vocal",
        gains_db: [-4.0, -3.0, -1.5, 0.5, 2.0, 3.5, 4.0, 2.5, 1.0, 0.0],
    },
    // Classic rock smile with a bit of mid grit.
    EqPreset {
        name: "Rock",
        gains_db: [4.5, 3.5, 1.5, 0.0, -1.5, 0.5, 2.0, 3.5, 3.0, 1.5],
    },
];

/// Shared live EQ parameters (UI writes, audio thread reads).
#[derive(Clone, Default)]
pub struct EqHandle {
    inner: Arc<Mutex<EqParams>>,
}

impl EqHandle {
    pub fn new(params: EqParams) -> Self {
        Self {
            inner: Arc::new(Mutex::new(params.clamped())),
        }
    }

    pub fn get(&self) -> EqParams {
        self.inner.lock().map(|g| *g).unwrap_or_default()
    }

    pub fn set(&self, params: EqParams) {
        if let Ok(mut g) = self.inner.lock() {
            *g = params.clamped();
        }
    }
}

#[derive(Clone, Copy, Default)]
struct BiquadCoeffs {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl BiquadCoeffs {
    fn identity() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        }
    }

    /// Peaking EQ (Audio EQ Cookbook / RBJ).
    fn peaking(sample_rate: f32, freq: f32, gain_db: f32, q: f32) -> Self {
        if sample_rate <= 0.0 || freq <= 0.0 || freq >= sample_rate * 0.49 {
            return Self::identity();
        }
        if gain_db.abs() < 0.01 {
            return Self::identity();
        }
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * freq / sample_rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q.max(0.1));
        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * cos;
        let b2 = 1.0 - alpha * a;
        let a0 = 1.0 + alpha / a;
        let a1 = -2.0 * cos;
        let a2 = 1.0 - alpha / a;
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct BiquadState {
    coeffs: BiquadCoeffs,
    z1: f32,
    z2: f32,
}

impl BiquadState {
    fn process(&mut self, x: f32) -> f32 {
        let c = self.coeffs;
        let y = c.b0 * x + self.z1;
        self.z1 = c.b1 * x - c.a1 * y + self.z2;
        self.z2 = c.b2 * x - c.a2 * y;
        y
    }

    fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

/// Per-channel cascade of peaking bands.
#[derive(Clone)]
struct ChannelEq {
    bands: [BiquadState; BAND_COUNT],
}

impl Default for ChannelEq {
    fn default() -> Self {
        Self {
            bands: [BiquadState::default(); BAND_COUNT],
        }
    }
}

impl ChannelEq {
    fn rebuild(&mut self, sample_rate: f32, gains_db: &[f32; BAND_COUNT]) {
        for (i, band) in self.bands.iter_mut().enumerate() {
            band.coeffs = BiquadCoeffs::peaking(sample_rate, BAND_FREQS_HZ[i], gains_db[i], Q);
        }
    }

    fn process(&mut self, mut x: f32) -> f32 {
        for band in &mut self.bands {
            x = band.process(x);
        }
        // Soft clip to catch boost clipping.
        x.tanh()
    }

    fn reset(&mut self) {
        for band in &mut self.bands {
            band.reset();
        }
    }
}

/// `rodio::Source` wrapper that applies the shared EQ.
pub struct EqSource<S> {
    inner: S,
    handle: EqHandle,
    channels: Vec<ChannelEq>,
    sample_rate: u32,
    channel_idx: usize,
    last_params: EqParams,
    last_sample_rate: u32,
}

impl<S: Source<Item = f32>> EqSource<S> {
    pub fn new(inner: S, handle: EqHandle) -> Self {
        let ch = inner.channels().max(1) as usize;
        let sample_rate = inner.sample_rate();
        let params = handle.get();
        let mut channels = vec![ChannelEq::default(); ch];
        for c in &mut channels {
            c.rebuild(sample_rate as f32, &params.gains_db);
        }
        Self {
            inner,
            handle,
            channels,
            sample_rate,
            channel_idx: 0,
            last_params: params,
            last_sample_rate: sample_rate,
        }
    }

    fn sync_if_needed(&mut self) {
        let ch = self.inner.channels().max(1) as usize;
        let sr = self.inner.sample_rate();
        if ch != self.channels.len() {
            self.channels.resize_with(ch, ChannelEq::default);
            self.channel_idx = 0;
            for c in &mut self.channels {
                c.reset();
                c.rebuild(sr as f32, &self.last_params.gains_db);
            }
        }
        self.sample_rate = sr;

        let params = self.handle.get();
        let rates_changed = sr != self.last_sample_rate;
        let gains_changed = params != self.last_params;
        if rates_changed || gains_changed {
            for c in &mut self.channels {
                c.rebuild(sr as f32, &params.gains_db);
            }
            self.last_params = params;
            self.last_sample_rate = sr;
        }
    }
}

impl<S: Source<Item = f32>> Iterator for EqSource<S> {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        let sample = self.inner.next()?;
        self.sync_if_needed();
        if !self.last_params.enabled || self.last_params.is_flat() {
            if !self.last_params.enabled {
                return Some(sample);
            }
            return Some(sample);
        }
        let n = self.channels.len().max(1);
        let idx = self.channel_idx % n;
        let out = self.channels[idx].process(sample);
        self.channel_idx = (self.channel_idx + 1) % n;
        Some(out)
    }
}

impl<S: Source<Item = f32>> Source for EqSource<S> {
    fn current_frame_len(&self) -> Option<usize> {
        self.inner.current_frame_len()
    }
    fn channels(&self) -> u16 {
        self.inner.channels()
    }
    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }
    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        for c in &mut self.channels {
            c.reset();
        }
        self.channel_idx = 0;
        self.inner.try_seek(pos)
    }
}

/// Process a mono buffer with a temporary EQ (for tests).
pub fn process_mono(samples: &mut [f32], sample_rate: f32, params: &EqParams) {
    if !params.enabled || params.is_flat() {
        return;
    }
    let mut ch = ChannelEq::default();
    ch.rebuild(sample_rate, &params.gains_db);
    for s in samples {
        *s = ch.process(*s);
    }
}

/// Pad/truncate arbitrary gain lists (e.g. old 5-band config) into `BAND_COUNT`.
pub fn normalize_gains(src: &[f32]) -> [f32; BAND_COUNT] {
    let mut out = [0.0; BAND_COUNT];
    for (i, g) in src.iter().copied().take(BAND_COUNT).enumerate() {
        out[i] = g.clamp(GAIN_MIN_DB, GAIN_MAX_DB);
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

    fn rms(v: &[f32]) -> f32 {
        let s: f32 = v.iter().map(|x| x * x).sum();
        (s / v.len() as f32).sqrt()
    }

    #[test]
    fn flat_is_near_identity() {
        let sr = 44_100.0;
        let mut buf = sine(440.0, sr, 4096);
        let before = rms(&buf);
        process_mono(&mut buf, sr, &EqParams::default());
        let after = rms(&buf);
        assert!((after - before).abs() / before < 0.02, "flat changed rms {before} → {after}");
    }

    #[test]
    fn bass_boost_raises_low_vs_high() {
        let sr = 44_100.0;
        let n = 8192;
        let params = EqParams {
            enabled: true,
            gains_db: PRESETS[1].gains_db, // Bass
        };

        let mut low = sine(60.0, sr, n);
        let mut high = sine(8_000.0, sr, n);
        let low_before = rms(&low);
        let high_before = rms(&high);
        process_mono(&mut low, sr, &params);
        process_mono(&mut high, sr, &params);
        let low_ratio = rms(&low) / low_before;
        let high_ratio = rms(&high) / high_before;
        assert!(
            low_ratio > high_ratio * 1.15,
            "expected bass boost: low_ratio={low_ratio} high_ratio={high_ratio}"
        );
    }

    #[test]
    fn clamp_gains() {
        let mut gains = [0.0; BAND_COUNT];
        gains[0] = 100.0;
        gains[1] = -100.0;
        let p = EqParams {
            enabled: true,
            gains_db: gains,
        }
        .clamped();
        assert_eq!(p.gains_db[0], GAIN_MAX_DB);
        assert_eq!(p.gains_db[1], GAIN_MIN_DB);
    }

    #[test]
    fn handle_roundtrip() {
        let h = EqHandle::new(EqParams::default());
        let mut gains = [0.0; BAND_COUNT];
        gains[0] = 6.0;
        let p = EqParams {
            enabled: true,
            gains_db: gains,
        };
        h.set(p);
        assert_eq!(h.get().gains_db[0], 6.0);
    }

    #[test]
    fn normalize_pads_old_five_band() {
        let n = normalize_gains(&[6.0, 3.0, 0.0, 0.0, 0.0]);
        assert_eq!(n[0], 6.0);
        assert_eq!(n[1], 3.0);
        assert_eq!(n[9], 0.0);
    }

    #[test]
    fn presets_have_five_named() {
        assert_eq!(PRESETS.len(), 5);
        assert_eq!(PRESETS[0].name, "Flat");
        assert_eq!(PRESETS[1].name, "Bass");
        assert_eq!(PRESETS[2].name, "Electronic");
        assert_eq!(PRESETS[3].name, "Vocal");
        assert_eq!(PRESETS[4].name, "Rock");
    }
}
