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
    pub const SIZE: u32 = 96;

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

    /// Box-filtered sample of the region covering fractional coords [x0,x1)×[y0,y1).
    pub fn sample(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> (u8, u8, u8) {
        let s = self.size as f32;
        let xa = (x0 * s).floor().clamp(0.0, s - 1.0) as u32;
        let ya = (y0 * s).floor().clamp(0.0, s - 1.0) as u32;
        let xb = ((x1 * s).ceil() as u32).clamp(xa + 1, self.size);
        let yb = ((y1 * s).ceil() as u32).clamp(ya + 1, self.size);
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

pub fn extract_accent(path: &Path) -> Result<(u8, u8, u8)> {
    let img = image::open(path)?.into_rgb8();
    let (w, h) = img.dimensions();
    // Downscale for color-thief
    let small = image::imageops::resize(&img, w.min(100), h.min(100), image::imageops::FilterType::Triangle);
    let colors = color_thief::get_palette(small.as_raw(), color_thief::ColorFormat::Rgb, 10, 5)
        .map_err(|e| anyhow::anyhow!("color-thief: {e:?}"))?;

    let mut best: Option<(u8, u8, u8, f32)> = None;
    for c in colors {
        let (h, s, l) = rgb_to_hsl(c.r, c.g, c.b);
        let _ = h;
        // Prefer saturated mid-lightness
        if s < 0.25 {
            continue;
        }
        let l_clamped = l.clamp(0.55, 0.75);
        let score = s * (1.0 - (l - l_clamped).abs());
        match best {
            Some((_, _, _, bs)) if bs >= score => {}
            _ => best = Some((c.r, c.g, c.b, score)),
        }
    }

    if let Some((r, g, b, _)) = best {
        let (h, s, _) = rgb_to_hsl(r, g, b);
        let (r, g, b) = hsl_to_rgb(h, s.max(0.45), 0.62);
        return Ok((r, g, b));
    }

    // Fallback: average
    Ok((255, 0, 51))
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
