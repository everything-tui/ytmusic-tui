use crate::action::AppEvent;
use crate::api::model::upscale_thumbnail_url;
use crate::paths::Paths;
use anyhow::{Context, Result};
use sha1::{Digest, Sha1};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::debug;

pub async fn fetch_art(
    paths: Arc<Paths>,
    video_id: String,
    url: String,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    match fetch_art_inner(&paths, &video_id, &url).await {
        Ok((path, accent, pixels, image)) => {
            let _ = tx.send(AppEvent::ArtLoaded {
                video_id,
                path,
                accent,
                pixels: pixels.map(Arc::new),
                image: image.map(Arc::new),
            });
        }
        Err(e) => debug!("art fetch failed: {e}"),
    }
}

/// Decoded artwork shared by every place a cover is drawn.
#[derive(Debug, Clone, Default)]
pub struct Cover {
    pub pixels: Option<Arc<ArtPixels>>,
    pub image: Option<Arc<image::DynamicImage>>,
    /// Accent color derived from the cover's dominant hue.
    pub accent: Option<(u8, u8, u8)>,
}

/// Longest side of the full-resolution cover kept for graphics protocols.
const COVER_MAX_PX: u32 = 600;

/// Center-cropped square image for kitty/iTerm2/sixel rendering.
pub fn load_cover_image(path: &Path) -> Result<image::DynamicImage> {
    let img = image::open(path)?;
    let (w, h) = (img.width(), img.height());
    let side = w.min(h);
    let square = img.crop_imm((w - side) / 2, (h - side) / 2, side, side);
    Ok(if side > COVER_MAX_PX {
        square.resize_exact(COVER_MAX_PX, COVER_MAX_PX, image::imageops::FilterType::Lanczos3)
    } else {
        square
    })
}

/// Square RGB thumbnail used for half-block rendering in the terminal.
#[derive(Debug, Clone)]
pub struct ArtPixels {
    pub size: u32,
    pub rgb: Vec<u8>,
}

impl ArtPixels {
    /// Higher than a typical half-block cover so small Home cards stay crisp.
    pub const SIZE: u32 = 192;

    pub fn load(path: &Path) -> Result<Self> {
        let img = image::open(path)?.into_rgb8();
        let (w, h) = img.dimensions();
        let side = w.min(h);
        let cropped =
            image::imageops::crop_imm(&img, (w - side) / 2, (h - side) / 2, side, side).to_image();
        let small = image::imageops::resize(
            &cropped,
            Self::SIZE,
            Self::SIZE,
            image::imageops::FilterType::Lanczos3,
        );
        Ok(Self {
            size: Self::SIZE,
            rgb: small.into_raw(),
        })
    }

    /// Sample the region covering fractional coords [x0,x1)×[y0,y1).
    ///
    /// Uses a tight box around the cell center so half-block covers stay sharp
    /// instead of looking like a soft average of many source pixels.
    pub fn sample(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> (u8, u8, u8) {
        let s = self.size as f32;
        let cx = (x0 + x1) * 0.5;
        let cy = (y0 + y1) * 0.5;
        let rw = ((x1 - x0) * s).max(1.0);
        let rh = ((y1 - y0) * s).max(1.0);
        // Keep ~35% of the cell so edges stay defined without full-box blur.
        let half_w = (rw * 0.175).max(0.5);
        let half_h = (rh * 0.175).max(0.5);
        let xa = ((cx * s - half_w).floor()).clamp(0.0, s - 1.0) as u32;
        let ya = ((cy * s - half_h).floor()).clamp(0.0, s - 1.0) as u32;
        let xb = ((cx * s + half_w).ceil() as u32).clamp(xa + 1, self.size);
        let yb = ((cy * s + half_h).ceil() as u32).clamp(ya + 1, self.size);
        let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
        for y in ya..yb {
            for x in xa..xb {
                let i = ((y * self.size + x) * 3) as usize;
                r += self.rgb[i] as u32;
                g += self.rgb[i + 1] as u32;
                b += self.rgb[i + 2] as u32;
                n += 1;
            }
        }
        let n = n.max(1);
        ((r / n) as u8, (g / n) as u8, (b / n) as u8)
    }
}

async fn fetch_art_inner(
    paths: &Paths,
    video_id: &str,
    url: &str,
) -> Result<(
    PathBuf,
    Option<(u8, u8, u8)>,
    Option<ArtPixels>,
    Option<image::DynamicImage>,
)> {
    let url = upscale_thumbnail_url(url);
    let hash = {
        let mut h = Sha1::new();
        h.update(url.as_bytes());
        hex::encode(h.finalize())
    };
    let path = paths.art_cache_dir().join(format!("{hash}.jpg"));

    if !path.exists() {
        let client = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (compatible; yt-tui/0.1)")
            .build()?;
        let bytes = download_art_bytes(&client, &url).await?;
        std::fs::write(&path, &bytes).context("write art cache")?;
        // Also alias by video_id for quick lookup; detail keys ("detail:<id>") are not file-safe.
        if !video_id.contains(':') {
            let alias = paths.art_cache_dir().join(format!("{video_id}.jpg"));
            let _ = std::fs::copy(&path, alias);
        }
    }

    let (accent, pixels, image) = tokio::task::spawn_blocking({
        let path = path.clone();
        move || {
            (
                extract_accent(&path).ok(),
                ArtPixels::load(&path).ok(),
                load_cover_image(&path).ok(),
            )
        }
    })
    .await
    .unwrap_or((None, None, None));

    Ok((path, accent, pixels, image))
}

/// Fetch image bytes, falling back through common YT thumbnail sizes on 404.
async fn download_art_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let mut candidates = vec![url.to_string()];
    if url.contains("i.ytimg.com") {
        for name in ["hqdefault.jpg", "mqdefault.jpg", "sddefault.jpg", "default.jpg"] {
            if let Some(base) = url.rsplit_once('/') {
                let alt = format!("{}/{}", base.0, name);
                if alt != url {
                    candidates.push(alt);
                }
            }
        }
    }
    let mut last_err = anyhow::anyhow!("no art URL candidates");
    for candidate in candidates {
        match client.get(&candidate).send().await {
            Ok(resp) if resp.status().is_success() => {
                return Ok(resp.bytes().await.context("read art body")?.to_vec());
            }
            Ok(resp) => {
                last_err = anyhow::anyhow!("art HTTP {} for {candidate}", resp.status());
            }
            Err(e) => last_err = e.into(),
        }
    }
    Err(last_err)
}

/// Picks the hue that covers the most of the artwork (weighted by saturation), so a cover
/// that is mostly blue yields blue even when it has a small bright accent elsewhere.
pub fn extract_accent(path: &Path) -> Result<(u8, u8, u8)> {
    const BINS: usize = 36;
    const SIDE: u32 = 64;
    let img = image::open(path)?.into_rgb8();
    let small = image::imageops::resize(&img, SIDE, SIDE, image::imageops::FilterType::Triangle);

    let mut weight = [0f32; BINS];
    let mut sat = [0f32; BINS];
    let mut lit = [0f32; BINS];
    let mut sin = [0f32; BINS];
    let mut cos = [0f32; BINS];
    let mut chromatic = 0f32;
    for p in small.pixels() {
        let (h, s, l) = rgb_to_hsl(p[0], p[1], p[2]);
        if s < 0.2 || !(0.1..=0.93).contains(&l) {
            continue;
        }
        chromatic += 1.0;
        let w = s * (1.0 - (2.0 * (l - 0.5)).abs() * 0.5);
        let bin = ((h * BINS as f32) as usize).min(BINS - 1);
        let ang = h * std::f32::consts::TAU;
        weight[bin] += w;
        sat[bin] += s * w;
        lit[bin] += l * w;
        sin[bin] += ang.sin() * w;
        cos[bin] += ang.cos() * w;
    }

    // Black-and-white or very muted covers: a soft neutral instead of a random hue.
    if chromatic / ((SIDE * SIDE) as f32) < 0.08 {
        let (r, g, b) = hsl_to_rgb(0.0, 0.0, 0.82);
        return Ok((r, g, b));
    }

    // Strongest hue neighbourhood (three adjacent bins, wrapping around the colour wheel).
    let window = |i: usize| [(i + BINS - 1) % BINS, i, (i + 1) % BINS];
    let center = (0..BINS)
        .max_by(|&a, &b| {
            let wa: f32 = window(a).iter().map(|&k| weight[k]).sum();
            let wb: f32 = window(b).iter().map(|&k| weight[k]).sum();
            wa.total_cmp(&wb)
        })
        .unwrap_or(0);
    let (mut w, mut s, mut l, mut sn, mut cs) = (0f32, 0f32, 0f32, 0f32, 0f32);
    for k in window(center) {
        w += weight[k];
        s += sat[k];
        l += lit[k];
        sn += sin[k];
        cs += cos[k];
    }
    let w = w.max(f32::EPSILON);
    let hue = (sn.atan2(cs) / std::f32::consts::TAU).rem_euclid(1.0);
    // Keep the accent vivid and readable on the dark UI without changing its hue.
    let sat = (s / w).clamp(0.5, 0.9);
    let light = (l / w).clamp(0.56, 0.7);
    Ok(hsl_to_rgb(hue, sat, light))
}

fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let r = r as f32 / 255.0;
    let g = g as f32 / 255.0;
    let b = b as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if (max - min).abs() < f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if (max - r).abs() < f32::EPSILON {
        let mut h = (g - b) / d;
        if g < b {
            h += 6.0;
        }
        h
    } else if (max - g).abs() < f32::EPSILON {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } / 6.0;
    (h, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let hue2rgb = |p: f32, q: f32, mut t: f32| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            return p + (q - p) * 6.0 * t;
        }
        if t < 1.0 / 2.0 {
            return q;
        }
        if t < 2.0 / 3.0 {
            return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
        }
        p
    };
    let (r, g, b) = if s == 0.0 {
        (l, l, l)
    } else {
        let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
        let p = 2.0 * l - q;
        (
            hue2rgb(p, q, h + 1.0 / 3.0),
            hue2rgb(p, q, h),
            hue2rgb(p, q, h - 1.0 / 3.0),
        )
    };
    (
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
    )
}

pub fn art_path_for(paths: &Paths, video_id: &str) -> Option<PathBuf> {
    let p = paths.art_cache_dir().join(format!("{video_id}.jpg"));
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

#[cfg(test)]
mod accent_tests {
    use super::*;

    fn accent_of(name: &str, fill: impl Fn(u32, u32) -> [u8; 3]) -> (u8, u8, u8) {
        let img = image::RgbImage::from_fn(100, 100, |x, y| image::Rgb(fill(x, y)));
        let path = std::env::temp_dir().join(format!("yt-tui-accent-{}-{name}.png", std::process::id()));
        img.save(&path).unwrap();
        let out = extract_accent(&path).unwrap();
        let _ = std::fs::remove_file(path);
        out
    }

    #[test]
    fn mostly_blue_cover_with_red_spot_is_blue() {
        let (r, _, b) = accent_of("blue", |x, y| if x < 8 && y < 8 { [230, 20, 20] } else { [30, 80, 200] });
        assert!(b > r, "expected blue accent, got r={r} b={b}");
    }

    #[test]
    fn grayscale_cover_is_neutral() {
        let (r, g, b) = accent_of("gray", |x, _| { let v = (x * 2) as u8; [v, v, v] });
        assert!(r.abs_diff(g) < 6 && g.abs_diff(b) < 6);
    }
}
