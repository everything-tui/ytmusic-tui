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

/// First range is small so decoding can start as soon as ~96KB arrives.
const FIRST_CHUNK: u64 = 512 * 1024;
const CHUNK_SIZE: u64 = 4 * 1024 * 1024;

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
        let client = reqwest::Client::builder()
            .user_agent("Mozilla/5.0")
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
        let gf = {
            let mut flight = self.in_flight.lock();
            if let Some(existing) = flight.get(video_id) {
                if !existing.is_failed() {
                    return Ok(existing.clone());
                }
                // Supersede the failed handle so its still-running task stops writing.
                flight.remove(video_id);
            }
            // Finished between the exists() check and this lock.
            if final_path.exists() {
                on_progress(1.0);
                return GrowingFile::open_complete(&final_path);
            }
            let gf = GrowingFile::create(&part_path)?;
            flight.insert(video_id.to_string(), gf.clone());
            gf
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
            let result = download_chunks(
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
                    } else if part_path2.exists() && gf_dl.len() > 0 {
                        // Partial file still usable for playback — don't poison waiters yet.
                        warn!("download {video_id} incomplete: {e}");
                        gf_dl.set_failed(e.to_string());
                    } else {
                        warn!("download {video_id} failed: {e}");
                        gf_dl.set_failed(e.to_string());
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

async fn download_chunks(
    client: &reqwest::Client,
    url: &str,
    headers: &HashMap<String, String>,
    filesize: Option<u64>,
    gf: &GrowingFile,
    generation: u64,
    final_path: &Path,
    on_progress: &impl Fn(f32),
) -> Result<()> {
    let mut start: u64 = 0;
    let mut total = filesize;
    let mut first = true;
    // Throttle UI progress to whole-percent changes (≤ ~100 events per track).
    let mut last_pct_reported: i32 = -1;

    let report = |pct: f32, last: &mut i32| {
        let whole = (pct * 100.0).floor() as i32;
        if whole != *last {
            *last = whole;
            on_progress(pct.min(1.0));
        }
    };

    loop {
        if gf.is_stale_generation(generation) {
            anyhow::bail!("download superseded");
        }

        let chunk = if first { FIRST_CHUNK } else { CHUNK_SIZE };
        first = false;
        let end = start + chunk - 1;
        let range_url = if url.contains('?') {
            format!("{url}&range={start}-{end}")
        } else {
            format!("{url}?range={start}-{end}")
        };

        let mut req = client.get(&range_url);
        for (k, v) in headers {
            req = req.header(k, v);
        }

        let resp = req.send().await.context("chunk request")?;
        if !resp.status().is_success() && resp.status().as_u16() != 206 {
            anyhow::bail!("chunk HTTP {}", resp.status());
        }

        // Try to learn total size from Content-Range
        if total.is_none() {
            if let Some(cr) = resp.headers().get("content-range") {
                if let Ok(s) = cr.to_str() {
                    if let Some(t) = s.split('/').nth(1) {
                        if let Ok(n) = t.parse::<u64>() {
                            total = Some(n);
                            gf.set_expected_len(n);
                        }
                    }
                }
            }
        }

        // Stream bytes as they arrive so decoding can start before the range finishes.
        let mut stream = resp.bytes_stream();
        let mut got: u64 = 0;
        while let Some(item) = stream.next().await {
            if gf.is_stale_generation(generation) {
                anyhow::bail!("download superseded");
            }
            let bytes = item.context("chunk body")?;
            if bytes.is_empty() {
                continue;
            }
            got += bytes.len() as u64;
            if let Err(e) = gf.append(&bytes) {
                if final_path.exists() {
                    gf.adopt_complete_file(final_path)?;
                    on_progress(1.0);
                    return Ok(());
                }
                return Err(e);
            }
            start += bytes.len() as u64;
            if let Some(t) = total {
                report((start as f32 / t as f32).min(1.0), &mut last_pct_reported);
            } else if last_pct_reported < 0 {
                last_pct_reported = 0;
                on_progress(0.35);
            }
        }

        if got == 0 {
            break;
        }

        if let Some(t) = total {
            if start >= t {
                break;
            }
        } else if got < chunk {
            // Unknown size: short read ⇒ EOF
            break;
        }
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
