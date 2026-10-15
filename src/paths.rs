use anyhow::{Context, Result};
use directories::ProjectDirs;
use std::fs;
use std::path::PathBuf;
use tracing::info;

#[derive(Clone)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl Paths {
    pub fn new() -> Result<Self> {
        let dirs = ProjectDirs::from("com", "yt-tui", "yt-tui")
            .context("could not determine project directories")?;
        let paths = Self {
            config_dir: dirs.config_dir().to_path_buf(),
            data_dir: dirs.data_dir().to_path_buf(),
            cache_dir: dirs.cache_dir().to_path_buf(),
        };
        paths.ensure()?;
        Ok(paths)
    }

    pub fn ensure(&self) -> Result<()> {
        for dir in [
            &self.config_dir,
            &self.data_dir,
            &self.cache_dir,
            &self.audio_cache_dir(),
            &self.art_cache_dir(),
            &self.lyrics_cache_dir(),
            &self.logs_dir(),
        ] {
            fs::create_dir_all(dir)?;
        }
        Ok(())
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn session_file(&self) -> PathBuf {
        self.data_dir.join("session.json")
    }

    pub fn auth_file(&self) -> PathBuf {
        self.data_dir.join("auth.json")
    }

    /// Port + token for the local control / MCP bridge (loopback TCP).
    pub fn control_file(&self) -> PathBuf {
        self.data_dir.join("control.json")
    }

    pub fn audio_cache_dir(&self) -> PathBuf {
        self.cache_dir.join("audio")
    }

    pub fn art_cache_dir(&self) -> PathBuf {
        self.cache_dir.join("art")
    }

    pub fn lyrics_cache_dir(&self) -> PathBuf {
        self.cache_dir.join("lyrics")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.cache_dir.join("logs")
    }

    /// Pre-native-resolver leftovers: managed yt-dlp/deno binaries and the
    /// Netscape cookie jar written only for yt-dlp.
    fn legacy_bin_dir(&self) -> PathBuf {
        self.data_dir.join("bin")
    }

    fn legacy_cookies_file(&self) -> PathBuf {
        self.cache_dir.join("cookies.txt")
    }
}

/// Delete leftover yt-dlp / deno binaries and the Netscape cookie jar from
/// older installs. Safe to call repeatedly.
pub fn cleanup_legacy(paths: &Paths) {
    let bin = paths.legacy_bin_dir();
    if bin.exists() {
        match fs::remove_dir_all(&bin) {
            Ok(()) => info!("removed legacy bin dir {}", bin.display()),
            Err(e) => tracing::warn!("could not remove {}: {e}", bin.display()),
        }
    }
    let cookies = paths.legacy_cookies_file();
    if cookies.exists() {
        match fs::remove_file(&cookies) {
            Ok(()) => info!("removed legacy cookies {}", cookies.display()),
            Err(e) => tracing::warn!("could not remove {}: {e}", cookies.display()),
        }
    }
}
