use crate::player::resolver::ResolvedStream;
use crate::player::source::GrowingFile;
use anyhow::{Context, Result};
use futures::StreamExt;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use tracing::{debug, warn};

/// Wait for this many bytes (or EOF/failure) before returning from `get`,
/// so CDN 403s surface to the resolve failover instead of a silent GrowingFile.
const READY_BYTES: u64 = 64 * 1024;

#[derive(Clone)]
pub struct Downloader {
    cache_dir: PathBuf,
    max_bytes: u64,
    client: reqwest::Client,
    /// Coalesce concurrent gets for the same video so only one writer touches `.part`.
    in_flight: Arc<Mutex<HashMap<String, Arc<GrowingFile>>>>,
}

impl Downloader {
    pub fn new(cache_dir: PathBuf, max_mb: u64) -> Self {
        // Default UA is overwritten on every request by `ResolvedStream.headers`.
        let client = reqwest::Client::builder()
            .gzip(true)
            .build()
            .expect("reqwest client");
        Self {
            cache_dir,
            max_bytes: max_mb * 1024 * 1024,
            client,
            in_flight: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn cached_path(&self, video_id: &str) -> PathBuf {
        self.cache_dir.join(format!("{video_id}.m4a"))
    }

    pub fn is_cached(&self, video_id: &str) -> bool {
        self.cached_path(video_id).exists()
    }

    /// Download (or open cached) and return a GrowingFile ready for decoding.
    /// Returns `Err` if the CDN rejects the URL before enough bytes arrive (e.g. HTTP 403).
    pub async fn get(
        &self,
        video_id: &str,
        stream: &ResolvedStream,
        on_progress: impl Fn(f32) + Send + Sync + 'static,
    ) -> Result<Arc<GrowingFile>> {
        let final_path = self.cached_path(video_id);
        if final_path.exists() {
            self.in_flight.lock().remove(video_id);
            on_progress(1.0);
            return GrowingFile::open_complete(&final_path);
        }

        let part_path = self.cache_dir.join(format!("{video_id}.m4a.part"));

        // Hold the lock while creating/inserting so a concurrent get() cannot
        // truncate an in-progress `.part` file (that caused false "download failed").
        // Decide under the lock, then await outside (parking_lot guards are !Send).
        enum Start {
            Join(Arc<GrowingFile>),
            Fresh(Arc<GrowingFile>),
        }
        let start = {
            let mut flight = self.in_flight.lock();
            if let Some(existing) = flight.get(video_id).cloned() {
                if !existing.is_failed() {
                    Start::Join(existing)
                } else {
                    flight.remove(video_id);
                    if final_path.exists() {
                        drop(flight);
                        on_progress(1.0);
                        return GrowingFile::open_complete(&final_path);
                    }
                    let gf = GrowingFile::create(&part_path)?;
                    flight.insert(video_id.to_string(), gf.clone());
                    Start::Fresh(gf)
                }
            } else if final_path.exists() {
                drop(flight);
                on_progress(1.0);
                return GrowingFile::open_complete(&final_path);
            } else {
                let gf = GrowingFile::create(&part_path)?;
                flight.insert(video_id.to_string(), gf.clone());
                Start::Fresh(gf)
            }
        };

        let gf = match start {
            Start::Join(existing) => {
                wait_ready(&existing).await?;
                return Ok(existing);
            }
            Start::Fresh(gf) => gf,
        };

        let client = self.client.clone();
        let url = stream.url.clone();
        let headers = stream.headers.clone();
        let filesize = stream.filesize;
        let gf_dl = gf.clone();
        let final_path2 = final_path.clone();
        let part_path2 = part_path.clone();
        let video_id = video_id.to_string();
        let in_flight = self.in_flight.clone();
        let generation = gf.generation();

        tokio::spawn(async move {
            // One connection with a Range header. Do not use `&range=` query
            // chunks — and do not prefer ANDROID_VR URLs (those 403 without PO tokens).
            let result = download_stream(
                &client,
                &url,
                &headers,
                filesize,
                &gf_dl,
                generation,
                &final_path2,
                &on_progress,
            )
            .await;

            // A newer get() may have replaced us — don't touch the jar.
            if gf_dl.is_stale_generation(generation) {
                debug!("download {video_id} superseded — ignoring result");
                return;
            }

            match result {
                Ok(()) => {
                    gf_dl.mark_complete();
                    // Update logical path only after rename succeeds so readers reopen the final name.
                    match fs::rename(&part_path2, &final_path2) {
                        Ok(()) => {
                            gf_dl.set_path(final_path2.clone());
                            debug!("cached {video_id}");
                        }
                        Err(_) if final_path2.exists() => {
                            let _ = fs::remove_file(&part_path2);
                            gf_dl.set_path(final_path2);
                            debug!("cached {video_id} (already present)");
                        }
                        Err(e) => warn!("rename cache file: {e}"),
                    }
                }
                Err(e) => {
                    // If a twin finished (or we raced the rename), adopt the cache.
                    if final_path2.exists() {
                        match gf_dl.adopt_complete_file(&final_path2) {
                            Ok(()) => debug!("download {video_id} recovered from cache after: {e}"),
                            Err(adopt_err) => {
                                warn!("adopt cache after failed download: {adopt_err}");
                                gf_dl.set_failed(e.to_string());
                            }
                        }
                    } else {
                        warn!("download {video_id} failed: {e}");
                        gf_dl.set_failed(e.to_string());
                        // Remove poisoned partial so the next client starts clean.
                        let _ = fs::remove_file(&part_path2);
                    }
                }
            }

            let mut flight = in_flight.lock();
            if flight
                .get(&video_id)
                .map(|g| g.generation() == generation)
                .unwrap_or(false)
            {
                flight.remove(&video_id);
            }
        });

        wait_ready(&gf).await?;
        Ok(gf)
    }

    pub fn enforce_lru(&self) -> Result<()> {
        let mut entries: Vec<(PathBuf, SystemTime, u64)> = Vec::new();
        for entry in fs::read_dir(&self.cache_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("m4a") {
                continue;
            }
            let meta = entry.metadata()?;
            let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            entries.push((path, modified, meta.len()));
        }
        entries.sort_by_key(|(_, t, _)| *t);
        let mut total: u64 = entries.iter().map(|(_, _, l)| l).sum();
        while total > self.max_bytes {
            if let Some((path, _, len)) = entries.first().cloned() {
                let _ = fs::remove_file(&path);
                total = total.saturating_sub(len);
                entries.remove(0);
            } else {
                break;
            }
        }
        Ok(())
    }
}

async fn wait_ready(gf: &GrowingFile) -> Result<()> {
    let gf = gf.clone();
    tokio::task::spawn_blocking(move || gf.wait_bytes(READY_BYTES))
        .await
        .context("wait_ready join")?
}

async fn download_stream(
    client: &reqwest::Client,
    url: &str,
    headers: &HashMap<String, String>,
    filesize: Option<u64>,
    gf: &GrowingFile,
    generation: u64,
    final_path: &Path,
    on_progress: &impl Fn(f32),
) -> Result<()> {
    if gf.is_stale_generation(generation) {
        anyhow::bail!("download superseded");
    }

    let mut req = client.get(url);
    for (k, v) in headers {
        req = req.header(k, v);
    }
    // Single Range so googlevideo returns 206 and we can start decoding early.
    // Prefer open-ended; fall back to known content length when present.
    let range = match filesize {
        Some(n) if n > 0 => format!("bytes=0-{}", n - 1),
        _ => "bytes=0-".to_string(),
    };
    req = req.header(reqwest::header::RANGE, range);

    let resp = req.send().await.context("download request")?;
    let status = resp.status();
    // #region agent log
    crate::debug_log::log(
        if status.as_u16() == 403 { "A" } else { "D" },
        "player/download.rs:download_stream",
        "cdn response",
        serde_json::json!({
            "http_status": status.as_u16(),
            "ok": status.is_success() || status.as_u16() == 206,
            "url_host": url.split('/').nth(2).unwrap_or(""),
            "header_keys": headers.keys().cloned().collect::<Vec<_>>(),
        }),
    );
    // #endregion
    if !(status.is_success() || status.as_u16() == 206) {
        anyhow::bail!("download HTTP {status}");
    }

    let total = filesize.or_else(|| {
        // Content-Range: bytes 0-N/TOTAL
        resp.headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split('/').nth(1)?.parse().ok())
            .or_else(|| resp.content_length())
    });
    if let Some(t) = total {
        gf.set_expected_len(t);
    }

    let mut written: u64 = 0;
    let mut last_pct_reported: i32 = -1;
    let mut stream = resp.bytes_stream();
    while let Some(item) = stream.next().await {
        if gf.is_stale_generation(generation) {
            anyhow::bail!("download superseded");
        }
        let bytes = item.context("download body")?;
        if bytes.is_empty() {
            continue;
        }
        if let Err(e) = gf.append(&bytes) {
            if final_path.exists() {
                gf.adopt_complete_file(final_path)?;
                on_progress(1.0);
                return Ok(());
            }
            return Err(e);
        }
        written += bytes.len() as u64;
        if let Some(t) = total {
            let pct = (written as f32 / t as f32).min(1.0);
            let whole = (pct * 100.0).floor() as i32;
            if whole != last_pct_reported {
                last_pct_reported = whole;
                on_progress(pct);
            }
        } else if last_pct_reported < 0 {
            last_pct_reported = 0;
            on_progress(0.35);
        }
    }

    if written == 0 {
        anyhow::bail!("download returned empty body");
    }
    if let Some(t) = total {
        if written + 1024 < t {
            anyhow::bail!("download truncated ({written}/{t} bytes)");
        }
    } else {
        gf.set_expected_len(written);
    }

    on_progress(1.0);
    Ok(())
}

pub fn cache_size_bytes(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .ok()
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}
