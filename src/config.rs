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
    pub control: ControlConfig,
}

/// Local control interface for external agents (MCP bridge).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ControlConfig {
    /// Listen on a private Unix socket so `yt-tui mcp` can drive playback. Off by default.
    pub enabled: bool,
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
    /// When false, EQ is bypassed.
    pub eq_enabled: bool,
    /// Per-band gains in dB (10-band graphic EQ), clamped to ±12.
    /// Accepts shorter arrays from older configs (padded with 0).
    #[serde(deserialize_with = "deserialize_eq_gains", default = "default_eq_gains")]
    pub eq_gains: [f32; 10],
    /// Adaptive taste profile: "off" | "balanced" | "bass" | "vocal" | "bright".
    #[serde(default = "default_eq_adaptive")]
    pub eq_adaptive: String,
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
            control: ControlConfig::default(),
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
            eq_enabled: true,
            eq_gains: default_eq_gains(),
            eq_adaptive: default_eq_adaptive(),
        }
    }
}

fn default_eq_gains() -> [f32; 10] {
    [0.0; 10]
}

fn default_eq_adaptive() -> String {
    "off".into()
}

fn deserialize_eq_gains<'de, D>(deserializer: D) -> Result<[f32; 10], D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v: Vec<f32> = Deserialize::deserialize(deserializer)?;
    Ok(crate::player::eq::normalize_gains(&v))
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
        let mut cfg: Config = toml::from_str(&text).context("parsing config.toml")?;
        for g in &mut cfg.playback.eq_gains {
            *g = g.clamp(-12.0, 12.0);
        }
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
