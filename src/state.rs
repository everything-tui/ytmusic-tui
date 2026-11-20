use crate::api::model::Track;
use crate::paths::Paths;
use crate::player::queue::RepeatMode;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionState {
    pub queue: Vec<Track>,
    pub index: usize,
    pub position_ms: u64,
    pub volume: f32,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub last_tab: usize,
    pub autoplay: bool,
}

impl SessionState {
    pub fn load(paths: &Paths) -> Result<Self> {
        let path = paths.session_file();
        if !path.exists() {
            return Ok(Self {
                volume: 0.8,
                autoplay: true,
                ..Default::default()
            });
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("reading session {}", path.display()))?;
        let mut s: SessionState = serde_json::from_str(&text)?;
        if s.volume <= 0.0 {
            s.volume = 0.8;
        }
        Ok(s)
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        let text = serde_json::to_string_pretty(self)?;
        fs::write(paths.session_file(), text)?;
        Ok(())
    }

    pub fn position(&self) -> Duration {
        Duration::from_millis(self.position_ms)
    }

    pub fn set_position(&mut self, d: Duration) {
        self.position_ms = d.as_millis() as u64;
    }
}
