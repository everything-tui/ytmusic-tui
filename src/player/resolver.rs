use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::{debug, warn};

#[derive(Debug, Clone)]
pub struct ResolvedStream {
    pub url: String,
    pub headers: HashMap<String, String>,
    pub filesize: Option<u64>,
    pub duration: Option<f64>,
    pub ext: String,
    pub resolved_at: Instant,
}

impl ResolvedStream {
    pub fn is_expired(&self) -> bool {
        self.resolved_at.elapsed() > Duration::from_secs(5 * 3600)
    }
}

#[derive(Debug, Deserialize)]
struct YtdlpJson {
    url: Option<String>,
    #[serde(default)]
    http_headers: HashMap<String, String>,
    filesize: Option<u64>,
    filesize_approx: Option<u64>,
    duration: Option<f64>,
    ext: Option<String>,
    requested_downloads: Option<Vec<YtdlpJson>>,
}

#[derive(Clone)]
pub struct Resolver {
    ytdlp: PathBuf,
    js_runtime: Option<String>,
    cookies: Option<PathBuf>,
    cache: Arc<Mutex<HashMap<String, ResolvedStream>>>,
}

impl Resolver {
    pub fn new(ytdlp: PathBuf, js_runtime: Option<String>, cookies: Option<PathBuf>) -> Self {
        Self {
            ytdlp,
            js_runtime,
            cookies,
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn set_cookies(&mut self, cookies: Option<PathBuf>) {
        self.cookies = cookies;
    }

    pub fn set_ytdlp(&mut self, path: PathBuf) {
        self.ytdlp = path;
    }

    pub fn set_js_runtime(&mut self, rt: Option<String>) {
        self.js_runtime = rt;
    }

    pub async fn resolve(&self, video_id: &str) -> Result<ResolvedStream> {
        {
            let cache = self.cache.lock().await;
            if let Some(s) = cache.get(video_id) {
                if !s.is_expired() {
                    return Ok(s.clone());
                }
            }
        }

        let result = self.resolve_uncached(video_id).await;
        if let Ok(ref stream) = result {
            self.cache.lock().await.insert(video_id.to_string(), stream.clone());
        }
        result
    }

    async fn resolve_uncached(&self, video_id: &str) -> Result<ResolvedStream> {
        // Prefer a lightweight --print extract (much faster than full -J).
        match self.ytdlp_print(video_id).await {
            Ok(s) => Ok(s),
            Err(print_err) => {
                debug!("yt-dlp --print failed ({print_err}); falling back to -J");
                self.ytdlp_json(video_id).await
            }
        }
    }

    fn ytdlp_base_args(&self, video_id: &str) -> (Vec<String>, Option<tempfile::NamedTempFile>) {
        let url = format!("https://music.youtube.com/watch?v={video_id}");
        let mut args = vec![
            "--no-playlist".into(),
            "-f".into(),
            "140/bestaudio[ext=m4a]/bestaudio".into(),
            "--no-warnings".into(),
        ];
        // yt-dlp rewrites its cookie jar on exit and crashes if it can't, so hand it a
        // private copy; must stay alive until the process exits.
        let cookie_copy = self.cookies.as_deref().and_then(private_cookie_copy);
        if let Some(ref tmp) = cookie_copy {
            args.push("--cookies".into());
            args.push(tmp.path().to_string_lossy().to_string());
        }
        if let Some(ref rt) = self.js_runtime {
            args.push("--js-runtimes".into());
            args.push(rt.clone());
        }
        args.push(url);
        (args, cookie_copy)
    }

    async fn ytdlp_print(&self, video_id: &str) -> Result<ResolvedStream> {
        let (mut args, cookie_copy) = self.ytdlp_base_args(video_id);
        // One field per line — avoids parsing a multi-megabyte -J dump.
        args.insert(0, "--print".into());
        args.insert(1, "%(url)s".into());
        args.insert(2, "--print".into());
        args.insert(3, "%(http_headers)j".into());
        args.insert(4, "--print".into());
        args.insert(5, "%(filesize,filesize_approx|)s".into());
        args.insert(6, "--print".into());
        args.insert(7, "%(duration|)s".into());
        args.insert(8, "--print".into());
        args.insert(9, "%(ext|)s".into());

        let stdout = self.run_ytdlp(&args).await?;
        drop(cookie_copy);

        let mut lines = stdout.lines().filter(|l| !l.is_empty());
        let url = lines
            .next()
            .context("yt-dlp --print missing url")?
            .to_string();
        if !url.starts_with("http") {
            bail!("yt-dlp --print returned non-url: {url}");
        }
        let headers: HashMap<String, String> = lines
            .next()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let filesize = lines
            .next()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|&n| n > 0);
        let duration = lines.next().and_then(|s| s.parse::<f64>().ok());
        let ext = lines
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("m4a")
            .to_string();

        Ok(ResolvedStream {
            url,
            headers,
            filesize,
            duration,
            ext,
            resolved_at: Instant::now(),
        })
    }

    async fn ytdlp_json(&self, video_id: &str) -> Result<ResolvedStream> {
        let (mut args, cookie_copy) = self.ytdlp_base_args(video_id);
        args.insert(0, "-J".into());

        let stdout = self.run_ytdlp(&args).await?;
        drop(cookie_copy);

        let json: YtdlpJson =
            serde_json::from_str(&stdout).context("parsing yt-dlp JSON")?;

        let (url, headers, filesize, duration, ext) = if let Some(url) = json.url {
            (
                url,
                json.http_headers,
                json.filesize.or(json.filesize_approx),
                json.duration,
                json.ext.unwrap_or_else(|| "m4a".into()),
            )
        } else if let Some(downloads) = json.requested_downloads {
            let d = downloads
                .into_iter()
                .next()
                .context("yt-dlp returned no requested_downloads")?;
            (
                d.url.context("missing url in requested_downloads")?,
                d.http_headers,
                d.filesize.or(d.filesize_approx),
                d.duration.or(json.duration),
                d.ext.unwrap_or_else(|| "m4a".into()),
            )
        } else {
            bail!("yt-dlp JSON missing stream url");
        };

        Ok(ResolvedStream {
            url,
            headers,
            filesize,
            duration,
            ext,
            resolved_at: Instant::now(),
        })
    }

    async fn run_ytdlp(&self, args: &[String]) -> Result<String> {
        debug!("yt-dlp {:?}", args);
        let output = Command::new(&self.ytdlp)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .with_context(|| format!("spawning yt-dlp at {}", self.ytdlp.display()))?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            warn!("yt-dlp failed: {err}");
            let reason = err
                .lines()
                .rev()
                .map(str::trim)
                .find(|l| l.starts_with("ERROR:"))
                .or_else(|| {
                    err.lines()
                        .rev()
                        .map(str::trim)
                        .find(|l| !l.is_empty() && !l.starts_with("[PYI-"))
                })
                .unwrap_or("unknown error");
            bail!("yt-dlp failed: {reason}");
        }

        String::from_utf8(output.stdout).context("yt-dlp stdout utf-8")
    }

    pub async fn invalidate(&self, video_id: &str) {
        self.cache.lock().await.remove(video_id);
    }
}

fn private_cookie_copy(src: &Path) -> Option<tempfile::NamedTempFile> {
    let data = match std::fs::read(src) {
        Ok(d) => d,
        Err(e) => {
            if src.exists() {
                warn!("cannot read {}: {e} — playing without cookies", src.display());
            }
            return None;
        }
    };
    let mut tmp = tempfile::Builder::new()
        .prefix("yt-tui-cookies-")
        .suffix(".txt")
        .tempfile()
        .ok()?;
    std::io::Write::write_all(&mut tmp, &data).ok()?;
    Some(tmp)
}

pub fn cookies_path_if_exists(path: &Path) -> Option<PathBuf> {
    if path.exists() {
        Some(path.to_path_buf())
    } else {
        None
    }
}
