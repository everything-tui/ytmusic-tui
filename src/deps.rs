use crate::action::AppEvent;
use crate::paths::Paths;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub struct Deps {
    pub ytdlp: PathBuf,
    pub js_runtime: Option<String>,
    pub js_runtime_path: Option<PathBuf>,
}

pub async fn ensure_deps(paths: Arc<Paths>, tx: mpsc::UnboundedSender<AppEvent>) -> Result<Deps> {
    let _ = tx.send(AppEvent::DepsStatus("Checking yt-dlp…".into()));

    let ytdlp = ensure_ytdlp(&paths, &tx).await?;
    let _ = tx.send(AppEvent::DepsStatus("Checking JS runtime…".into()));
    let (js_runtime, js_runtime_path) = ensure_js_runtime(&paths, &tx).await?;

    // Periodic auto-update (once per 7 days) for managed yt-dlp
    maybe_update_ytdlp(&paths, &ytdlp).await;

    let ver = ytdlp_version(&ytdlp).await.unwrap_or_else(|| "unknown".into());
    let js = js_runtime.clone().unwrap_or_else(|| "none".into());
    let _ = tx.send(AppEvent::DepsReady {
        ytdlp: ver.clone(),
        js_runtime: js.clone(),
    });
    info!("deps ready: yt-dlp={ver}, js={js}");

    Ok(Deps {
        ytdlp,
        js_runtime,
        js_runtime_path,
    })
}

async fn ensure_ytdlp(paths: &Paths, tx: &mpsc::UnboundedSender<AppEvent>) -> Result<PathBuf> {
    if let Ok(p) = which::which("yt-dlp") {
        return Ok(p);
    }
    let managed = paths.bin_dir().join(ytdlp_bin_name());
    if managed.exists() {
        return Ok(managed);
    }

    let _ = tx.send(AppEvent::DepsStatus("Downloading yt-dlp…".into()));
    download_ytdlp(&managed).await?;
    Ok(managed)
}

fn ytdlp_bin_name() -> &'static str {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "yt-dlp_macos"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "yt-dlp_macos"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "yt-dlp_linux_aarch64"
    }
    #[cfg(all(target_os = "linux", not(target_arch = "aarch64")))]
    {
        "yt-dlp_linux"
    }
    #[cfg(target_os = "windows")]
    {
        "yt-dlp.exe"
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        "yt-dlp"
    }
}

async fn download_ytdlp(dest: &Path) -> Result<()> {
    let name = ytdlp_bin_name();
    let url = format!("https://github.com/yt-dlp/yt-dlp/releases/latest/download/{name}");
    info!("downloading {url}");
    let client = reqwest::Client::builder()
        .user_agent("yt-tui/0.1")
        .build()?;
    let bytes = client
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(dest, &bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))?;
    }
    // On macOS the asset is named yt-dlp_macos — also symlink as yt-dlp
    let link = dest.with_file_name("yt-dlp");
    if link != dest {
        let _ = std::fs::remove_file(&link);
        #[cfg(unix)]
        {
            let _ = std::os::unix::fs::symlink(dest, &link);
        }
        #[cfg(not(unix))]
        {
            let _ = std::fs::copy(dest, &link);
        }
    }
    Ok(())
}

async fn ensure_js_runtime(
    paths: &Paths,
    tx: &mpsc::UnboundedSender<AppEvent>,
) -> Result<(Option<String>, Option<PathBuf>)> {
    for name in ["deno", "node", "bun"] {
        if let Ok(p) = which::which(name) {
            return Ok((Some(name.to_string()), Some(p)));
        }
    }

    let managed = paths.bin_dir().join(deno_bin_name());
    if managed.exists() {
        return Ok((Some(managed.to_string_lossy().to_string()), Some(managed)));
    }

    let _ = tx.send(AppEvent::DepsStatus("Downloading deno (for yt-dlp)…".into()));
    match download_deno(&managed).await {
        Ok(()) => Ok((Some(managed.to_string_lossy().to_string()), Some(managed))),
        Err(e) => {
            warn!("could not download deno: {e}");
            let _ = tx.send(AppEvent::DepsStatus(
                "No JS runtime found — some videos may fail".into(),
            ));
            Ok((None, None))
        }
    }
}

fn deno_bin_name() -> &'static str {
    #[cfg(windows)]
    {
        "deno.exe"
    }
    #[cfg(not(windows))]
    {
        "deno"
    }
}

async fn download_deno(dest: &Path) -> Result<()> {
    let (os, arch) = deno_target();
    let url = format!(
        "https://github.com/denoland/deno/releases/latest/download/deno-{arch}-{os}.zip"
    );
    info!("downloading {url}");
    let client = reqwest::Client::builder()
        .user_agent("yt-tui/0.1")
        .build()?;
    let bytes = client
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    let tmp = dest.with_extension("zip");
    std::fs::write(&tmp, &bytes)?;

    let file = std::fs::File::open(&tmp)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut found = false;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.name().to_string();
        if name.ends_with("deno") || name.ends_with("deno.exe") {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out = std::fs::File::create(dest)?;
            std::io::copy(&mut file, &mut out)?;
            found = true;
            break;
        }
    }
    let _ = std::fs::remove_file(&tmp);
    if !found {
        bail!("deno binary not found in zip");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

fn deno_target() -> (&'static str, &'static str) {
    let os = if cfg!(target_os = "macos") {
        "apple-darwin"
    } else if cfg!(target_os = "windows") {
        "pc-windows-msvc"
    } else {
        "unknown-linux-gnu"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };
    (os, arch)
}

async fn maybe_update_ytdlp(paths: &Paths, ytdlp: &Path) {
    // Only update managed copy
    if !ytdlp.starts_with(paths.bin_dir()) {
        return;
    }
    let stamp = paths.bin_dir().join(".ytdlp_updated");
    let should = match std::fs::metadata(&stamp).and_then(|m| m.modified()) {
        Ok(t) => SystemTime::now()
            .duration_since(t)
            .unwrap_or(Duration::ZERO)
            > Duration::from_secs(7 * 24 * 3600),
        Err(_) => true,
    };
    if !should {
        return;
    }
    info!("auto-updating yt-dlp");
    let _ = Command::new(ytdlp)
        .arg("-U")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
    let _ = std::fs::write(stamp, b"ok");
}

pub async fn force_update_ytdlp(ytdlp: &Path) -> Result<()> {
    let status = Command::new(ytdlp)
        .arg("-U")
        .status()
        .await
        .context("yt-dlp -U")?;
    if !status.success() {
        bail!("yt-dlp -U failed");
    }
    Ok(())
}

async fn ytdlp_version(ytdlp: &Path) -> Option<String> {
    let out = Command::new(ytdlp).arg("--version").output().await.ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

pub async fn doctor_report(paths: &Paths) -> String {
    let mut lines = Vec::new();
    lines.push("yt-tui doctor".into());
    lines.push(format!("config: {}", paths.config_dir.display()));
    lines.push(format!("data:   {}", paths.data_dir.display()));
    lines.push(format!("cache:  {}", paths.cache_dir.display()));

    match which::which("yt-dlp") {
        Ok(p) => lines.push(format!("yt-dlp (PATH): {} ({})", p.display(), ytdlp_version(&p).await.unwrap_or_default())),
        Err(_) => {
            let managed = paths.bin_dir().join(ytdlp_bin_name());
            if managed.exists() {
                lines.push(format!(
                    "yt-dlp (managed): {} ({})",
                    managed.display(),
                    ytdlp_version(&managed).await.unwrap_or_default()
                ));
            } else {
                lines.push("yt-dlp: not found (will download on first run)".into());
            }
        }
    }

    for name in ["deno", "node", "bun"] {
        if let Ok(p) = which::which(name) {
            lines.push(format!("{name}: {}", p.display()));
        }
    }

    let truecolor = std::env::var("COLORTERM")
        .map(|v| v == "truecolor" || v == "24bit")
        .unwrap_or(false);
    lines.push(format!("truecolor: {truecolor}"));
    lines.push(format!("TERM: {}", std::env::var("TERM").unwrap_or_default()));
    lines.push(format!("TERM_PROGRAM: {}", std::env::var("TERM_PROGRAM").unwrap_or_default()));
    if std::env::var_os("TMUX").is_some() {
        lines.push("tmux: yes (album art may fall back to half-blocks)".into());
    }

    let auth = crate::auth::AuthService::new(Arc::new(paths.clone()));
    if let Some(s) = auth.session() {
        lines.push(format!(
            "auth: signed in as {}",
            s.account_name.as_deref().unwrap_or("unknown")
        ));
    } else {
        lines.push("auth: guest / not signed in".into());
    }

    let cache = crate::player::download::cache_size_bytes(&paths.audio_cache_dir());
    lines.push(format!("audio cache: {:.1} MB", cache as f64 / 1_048_576.0));

    lines.join("\n")
}

