use crate::paths::Paths;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub ui: UiConfig,
    pub playback: PlaybackConfig,
    pub keys: KeyOverrides,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    /// "dynamic" | "static"
    pub theme: String,
    pub accent: String,
    /// "auto" | "kitty" | "iterm" | "sixel" | "halfblocks" | "off"
    pub art_protocol: String,
    /// "bars" | "mirror" | "wave" | "off"
    pub visualizer: String,
    pub show_hints: bool,
    pub card_art_halfblocks: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaybackConfig {
    pub autoplay: bool,
    pub volume: f32,
    pub seek_step_secs: u64,
    pub cache_size_mb: u64,
    pub prefetch: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct KeyOverrides {
    // reserved for future key remapping via TOML
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ui: UiConfig::default(),
            playback: PlaybackConfig::default(),
            keys: KeyOverrides::default(),
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "dynamic".into(),
            accent: "#FF0033".into(),
            art_protocol: "auto".into(),
            visualizer: "bars".into(),
            show_hints: true,
            card_art_halfblocks: true,
        }
    }
}

impl Default for PlaybackConfig {
    fn default() -> Self {
        Self {
            autoplay: true,
            volume: 0.8,
            seek_step_secs: 5,
            cache_size_mb: 1024,
            prefetch: true,
        }
    }
}

impl Config {
    pub fn load(paths: &Paths) -> Result<Self> {
        let path = paths.config_file();
        if !path.exists() {
            let cfg = Self::default();
            cfg.save(paths)?;
            return Ok(cfg);
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let cfg: Config = toml::from_str(&text).context("parsing config.toml")?;
        Ok(cfg)
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        let header = r#"# yt-tui configuration
# Docs: https://github.com/francoisegner/yt-tui

"#;
        let body = toml::to_string_pretty(self)?;
        fs::write(paths.config_file(), format!("{header}{body}"))?;
        Ok(())
    }
}

pub fn parse_hex_color(s: &str) -> Option<(u8, u8, u8)> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some((r, g, b))
}
