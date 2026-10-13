use rustfft::{num_complex::Complex, FftPlanner};
use std::f32::consts::PI;

const BARS: &'static str = "▁▂▃▄▅▆▇█";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisualizerStyle {
    Bars,
    Mirror,
    Wave,
    Off,
}

impl VisualizerStyle {
    pub fn from_config(s: &str) -> Self {
        match s {
            "mirror" => Self::Mirror,
            "wave" => Self::Wave,
            "off" => Self::Off,
            _ => Self::Bars,
        }
    }
}

pub struct Visualizer {
    pub style: VisualizerStyle,
    bands: Vec<f32>,
    peaks: Vec<f32>,
    fft_buf: Vec<Complex<f32>>,
    planner: FftPlanner<f32>,
    /// Smoothed peak used for AGC so bars show shape, not a solid block.
    agc_peak: f32,
    sample_rate: u32,
}

impl Visualizer {
    pub fn new(style: VisualizerStyle) -> Self {
        Self {
            style,
            bands: vec![0.0; 64],
            peaks: vec![0.0; 64],
            fft_buf: Vec::new(),
            planner: FftPlanner::new(),
            agc_peak: 1e-3,
            sample_rate: 44100,
        }
    }

    pub fn set_band_count(&mut self, n: usize) {
        let n = n.clamp(8, 128);
        if self.bands.len() != n {
            self.bands.resize(n, 0.0);
            self.peaks.resize(n, 0.0);
        }
    }

    /// Update from latest mono samples (any length; uses last 2048).
    pub fn update(&mut self, samples: &[f32], playing: bool) {
        if self.style == VisualizerStyle::Off {
            return;
        }
        if !playing || samples.len() < 64 {
            for b in &mut self.bands {
                *b *= 0.85;
            }
            for p in &mut self.peaks {
                *p *= 0.95;
            }
            self.agc_peak *= 0.98;
            return;
        }

        let n = 2048.min(samples.len());
        let slice = &samples[samples.len() - n..];

        self.fft_buf.clear();
        self.fft_buf.reserve(n);
        let denom = (n as f32 - 1.0).max(1.0);
        for (i, &s) in slice.iter().enumerate() {
            let w = 0.5 * (1.0 - (2.0 * PI * i as f32 / denom).cos());
            self.fft_buf.push(Complex {
                re: s * w,
                im: 0.0,
            });
        }
        let mut size = n.next_power_of_two();
        if size < 256 {
            size = 256;
        }
        self.fft_buf.resize(size, Complex { re: 0.0, im: 0.0 });

        let fft = self.planner.plan_fft_forward(size);
        fft.process(&mut self.fft_buf);

        let bands_n = self.bands.len();
        let mut mags = vec![0.0f32; bands_n];
        let nyquist = self.sample_rate as f32 / 2.0;
        let f_min = 40.0f32;
        let f_max = 16_000.0f32.min(nyquist);

        for (bi, mag) in mags.iter_mut().enumerate() {
            let t0 = bi as f32 / bands_n as f32;
            let t1 = (bi + 1) as f32 / bands_n as f32;
            let f0 = f_min * (f_max / f_min).powf(t0);
            let f1 = f_min * (f_max / f_min).powf(t1);
            let i0 = ((f0 / nyquist) * (size as f32 / 2.0)) as usize;
            let i1 = (((f1 / nyquist) * (size as f32 / 2.0)) as usize + 1).min(size / 2);
            let mut sum = 0.0;
            let mut count = 0;
            for i in i0..i1 {
                let c = self.fft_buf[i];
                sum += (c.re * c.re + c.im * c.im).sqrt();
                count += 1;
            }
            *mag = if count > 0 { sum / count as f32 } else { 0.0 };
        }

        // Peak AGC: normalize to recent max so the spectrum shape animates instead
        // of clipping every band to full height (or sitting near zero).
        let frame_peak = mags.iter().copied().fold(0.0f32, f32::max);
        if frame_peak > self.agc_peak {
            self.agc_peak = frame_peak;
        } else {
            self.agc_peak = self.agc_peak * 0.92 + frame_peak * 0.08;
        }
        let norm = self.agc_peak.max(1e-6);
        for mag in &mut mags {
            // Mild gamma so quieter bands still read.
            *mag = ((*mag / norm).clamp(0.0, 1.0)).powf(0.7);
        }

        for i in 0..bands_n {
            if mags[i] >= self.bands[i] {
                self.bands[i] = mags[i];
            } else {
                self.bands[i] = self.bands[i] * 0.72 + mags[i] * 0.28;
            }
            if self.bands[i] >= self.peaks[i] {
                self.peaks[i] = self.bands[i];
            } else {
                self.peaks[i] *= 0.96;
            }
        }
    }

    /// Render into a string of bar characters for `width` columns.
    pub fn render_line(&self, width: usize) -> String {
        if self.style == VisualizerStyle::Off || width == 0 {
            return String::new();
        }
        let n = self.bands.len().min(width);
        let mut out = String::with_capacity(width);
        for i in 0..width {
            let bi = i * n / width;
            let v = match self.style {
                VisualizerStyle::Wave => {
                    let phase = i as f32 / width as f32 * PI * 2.0;
                    ((self.bands.get(bi).copied().unwrap_or(0.0) * phase.sin().abs()) * 7.0)
                        as usize
                }
                VisualizerStyle::Mirror => {
                    let mid = width / 2;
                    let dist = if i < mid { mid - i } else { i - mid };
                    let bi = dist * n / mid.max(1);
                    (self.bands.get(bi).copied().unwrap_or(0.0) * 7.0) as usize
                }
                _ => (self.bands.get(bi).copied().unwrap_or(0.0) * 7.0) as usize,
            };
            let idx = v.min(7);
            out.push(BARS.chars().nth(idx).unwrap_or('▁'));
        }
        out
    }

    pub fn band_levels(&self) -> &[f32] {
        &self.bands
    }
}
