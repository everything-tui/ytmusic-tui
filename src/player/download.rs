use crate::player::resolver::ResolvedStream;
use crate::player::source::GrowingFile;
use anyhow::{Context, Result};
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use futures::StreamExt;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tracing::{debug, warn};

/// Wait for this many bytes (or EOF/failure) before returning from `get`,
/// so CDN 403s surface to the resolve failover instead of a silent GrowingFile.
const READY_BYTES: u64 = 64 * 1024;

/// Prewarmed start of a track kept in memory: enough to open the decoder and play
/// several seconds while the rest of the download connects.
const HEAD_BYTES: u64 = 256 * 1024;
/// Prewarmed heads kept at once; the oldest goes first.
const MAX_HEADS: usize = 24;
/// googlevideo sometimes paces a URL at playback speed or slower. Without the first
/// bytes by then, race a backup stream from another client.
const HEDGE_AFTER: Duration = Duration::from_millis(700);
/// Download speed is judged over windows this long.
const RATE_WINDOW: Duration = Duration::from_secs(3);
/// Below this multiple of the track's bitrate a download risks stalling playback.
const MIN_RATE_FACTOR: f64 = 1.5;
/// No bytes for this long counts as a slow window right away.
const STALL_AFTER: Duration = Duration::from_secs(3);
/// Fresh-URL switches per download before settling for what we have.
const MAX_RESUMES: u32 = 3;

/// Cached audio is `{video_id}.{ext}`: m4a (AAC), webm (Opus) or mp4 (muxed).
const AUDIO_EXTS: [&str; 3] = ["m4a", "webm", "mp4"];

/// Video ids whose cached file is already the best rendition YouTube offers,
/// one per line in the audio cache dir.
/// (v2: judged with Opus available, which beats the AAC 128 kbps v1 settled for.)
const BEST_AVAILABLE_FILE: &str = "best-available-v2.txt";

/// What a download may ask the resolver for while it runs.
pub enum Refetch {
    /// Another client's stream for a hedged start; may be a different rendition.
    Alternate { exclude_client: String },
    /// A fresh URL serving the byte-identical file, to resume a slow download.
    SameFile { client: String, filesize: u64 },
}

/// Resolver callback for [`Refetch`] requests; `None` when nothing suitable exists.
pub type Refetcher =
    Arc<dyn Fn(Refetch) -> BoxFuture<'static, Option<ResolvedStream>> + Send + Sync>;

struct Head {
    filesize: u64,
    bytes: Vec<u8>,
    at: Instant,
}

#[derive(Clone)]
pub struct Downloader {
    cache_dir: PathBuf,
    max_bytes: u64,
    client: reqwest::Client,
    /// Coalesce concurrent gets for the same video so only one writer touches `.part`.
    in_flight: Arc<Mutex<HashMap<String, Arc<GrowingFile>>>>,
    /// Prewarmed first bytes by video id, handed to the next `get` of that file.
    heads: Arc<Mutex<HashMap<String, Head>>>,
    /// Loaded on first use from [`BEST_AVAILABLE_FILE`].
    best_available: Arc<Mutex<Option<HashSet<String>>>>,
}

impl Downloader {
    pub fn new(cache_dir: PathBuf, max_mb: u64) -> Self {
        // Default UA is overwritten on every request by `ResolvedStream.headers`.
        // No gzip: googlevideo Range/206 bodies are already compressed audio; asking
        // for content-encoding has caused intermittent CDN 403s / truncated reads.
        // Without timeouts a stalled CDN connection never fails, so readers
        // waiting on the GrowingFile (and the next track switch) hang forever.
        let client = reqwest::Client::builder()
            .gzip(false)
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(20))
            .build()
            .expect("reqwest client");
        Self {
            cache_dir,
            max_bytes: max_mb * 1024 * 1024,
            client,
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            heads: Arc::new(Mutex::new(HashMap::new())),
            best_available: Arc::new(Mutex::new(None)),
        }
    }

    fn with_best_available<T>(&self, f: impl FnOnce(&mut HashSet<String>) -> T) -> T {
        let mut guard = self.best_available.lock();
        let set = guard.get_or_insert_with(|| {
            fs::read_to_string(self.cache_dir.join(BEST_AVAILABLE_FILE))
                .map(|s| s.lines().map(str::to_string).collect())
                .unwrap_or_default()
        });
        f(set)
    }

    /// The cached file is known to be the best rendition there is.
    pub fn is_best_available(&self, video_id: &str) -> bool {
        self.with_best_available(|set| set.contains(video_id))
    }

    /// Remember that no better rendition exists, so it isn't asked for again.
    pub fn mark_best_available(&self, video_id: &str) {
        let added = self.with_best_available(|set| set.insert(video_id.to_string()));
        if added {
            let path = self.cache_dir.join(BEST_AVAILABLE_FILE);
            let line = format!("{video_id}\n");
            if let Err(e) = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()))
            {
                warn!("remembering {video_id} as best available: {e}");
            }
        }
    }

    /// Replace the cached file of `video_id` with `stream` (a better rendition).
    /// Downloads beside it and swaps atomically, so a reader playing the old file
    /// keeps its handle and the next play gets the new one.
    pub async fn upgrade(&self, video_id: &str, stream: &ResolvedStream, refetch: Option<Refetcher>) -> Result<()> {
        let key = format!("{video_id}#upgrade");
        let tmp_path = self.cache_dir.join(format!("{video_id}.upgrade"));
        let gf = {
            let mut flight = self.in_flight.lock();
            if flight.contains_key(&key) || flight.contains_key(video_id) {
                return Ok(());
            }
            let gf = GrowingFile::create(&tmp_path)?;
            flight.insert(key.clone(), gf.clone());
            gf
        };
        let result = download_stream(
            &self.client,
            stream.clone(),
            0,
            &gf,
            gf.generation(),
            None,
            &|_| {},
            refetch,
        )
        .await
        .and_then(|ext| {
            let ext = ext.context("upgrade download did not finish")?;
            let len = fs::metadata(&tmp_path)?.len();
            if stream.filesize.is_some_and(|n| n != len) {
                anyhow::bail!("upgrade size {len} != {:?}", stream.filesize);
            }
            gf.mark_complete();
            let final_path = self.path_for(video_id, &ext);
            fs::rename(&tmp_path, &final_path).context("swapping in upgraded file")?;
            // The old file may be in another container (m4a → webm).
            for old in AUDIO_EXTS.iter().filter(|e| **e != ext) {
                let _ = fs::remove_file(self.path_for(video_id, old));
            }
            Ok(())
        });
        self.in_flight.lock().remove(&key);
        if result.is_err() {
            let _ = fs::remove_file(&tmp_path);
        }
        result
    }

    /// Fetch the first bytes of `video_id` so a later `get` can start playing at once.
    pub async fn prewarm(&self, video_id: &str, stream: &ResolvedStream) -> Result<()> {
        // Without a size a later stream can't be checked to be the same file.
        let Some(filesize) = stream.filesize.filter(|&n| n > 0) else {
            return Ok(());
        };
        if self.is_cached(video_id)
            || self.in_flight.lock().contains_key(video_id)
            || self
                .heads
                .lock()
                .get(video_id)
                .is_some_and(|h| h.filesize == filesize)
        {
            return Ok(());
        }
        let end = HEAD_BYTES.min(filesize) - 1;
        let resp = range_request(&self.client, stream, 0, Some(end))
            .send()
            .await
            .context("prewarm request")?;
        let status = resp.status();
        if !(status.is_success() || status.as_u16() == 206) {
            anyhow::bail!("prewarm HTTP {status}");
        }
        let bytes = resp.bytes().await.context("prewarm body")?;
        if bytes.len() as u64 != end + 1 {
            anyhow::bail!("prewarm got {} of {} bytes", bytes.len(), end + 1);
        }
        let mut heads = self.heads.lock();
        if heads.len() >= MAX_HEADS {
            if let Some(oldest) = heads.iter().min_by_key(|(_, h)| h.at).map(|(k, _)| k.clone()) {
                heads.remove(&oldest);
            }
        }
        heads.insert(
            video_id.to_string(),
            Head {
                filesize,
                bytes: bytes.to_vec(),
                at: Instant::now(),
            },
        );
        debug!("prewarmed {video_id} ({} bytes)", bytes.len());
        Ok(())
    }

    /// The cached file for `video_id`, whatever its container.
    pub fn cached_file(&self, video_id: &str) -> Option<PathBuf> {
        AUDIO_EXTS
            .iter()
            .map(|ext| self.path_for(video_id, ext))
            .find(|p| p.exists())
    }

    /// The cached file, or where an m4a one would go.
    pub fn cached_path(&self, video_id: &str) -> PathBuf {
        self.cached_file(video_id)
            .unwrap_or_else(|| self.path_for(video_id, "m4a"))
    }

    fn path_for(&self, video_id: &str, ext: &str) -> PathBuf {
        self.cache_dir.join(format!("{video_id}.{ext}"))
    }

    pub fn is_cached(&self, video_id: &str) -> bool {
        self.cached_file(video_id).is_some()
    }

    /// Download (or open cached) and return a GrowingFile ready for decoding.
    /// Returns `Err` if the CDN rejects the URL before enough bytes arrive (e.g. HTTP 403).
    /// With a `refetch`, a slow start races a backup stream and a slow download
    /// resumes on a fresh URL.
    pub async fn get(
        &self,
        video_id: &str,
        stream: &ResolvedStream,
        refetch: Option<Refetcher>,
        on_progress: impl Fn(f32) + Send + Sync + 'static,
    ) -> Result<Arc<GrowingFile>> {
        if let Some(done) = self.cached_file(video_id) {
            self.in_flight.lock().remove(video_id);
            on_progress(1.0);
            return GrowingFile::open_complete(&done);
        }

        // The container (and so the final name) is only known once a stream wins.
        let part_path = self.cache_dir.join(format!("{video_id}.part"));

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
                    if let Some(done) = self.cached_file(video_id) {
                        drop(flight);
                        on_progress(1.0);
                        return GrowingFile::open_complete(&done);
                    }
                    let gf = GrowingFile::create(&part_path)?;
                    flight.insert(video_id.to_string(), gf.clone());
                    Start::Fresh(gf)
                }
            } else if let Some(done) = self.cached_file(video_id) {
                drop(flight);
                on_progress(1.0);
                return GrowingFile::open_complete(&done);
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

        // A prewarmed head of this very file makes the start instant.
        let head = self
            .heads
            .lock()
            .remove(video_id)
            .filter(|h| Some(h.filesize) == stream.filesize);
        let start_at = match head {
            Some(h) => {
                gf.set_expected_len(h.filesize);
                gf.append(&h.bytes)?;
                debug!("starting {video_id} from prewarmed head");
                h.bytes.len() as u64
            }
            None => 0,
        };

        let client = self.client.clone();
        let stream = stream.clone();
        let gf_dl = gf.clone();
        let cache = self.clone();
        let part_path2 = part_path.clone();
        let video_id = video_id.to_string();
        let in_flight = self.in_flight.clone();
        let generation = gf.generation();

        tokio::spawn(async move {
            // One connection with a Range header. Do not use `&range=` query
            // chunks — and do not prefer ANDROID_VR URLs (those 403 without PO tokens).
            let twin = {
                let cache = cache.clone();
                let video_id = video_id.clone();
                move || cache.cached_file(&video_id)
            };
            let result = download_stream(
                &client,
                stream,
                start_at,
                &gf_dl,
                generation,
                Some(&twin),
                &on_progress,
                refetch,
            )
            .await;

            // A newer get() may have replaced us — don't touch the jar.
            if gf_dl.is_stale_generation(generation) {
                debug!("download {video_id} superseded — ignoring result");
                return;
            }

            match result {
                // A twin finished first and was adopted; our partial is not needed.
                Ok(None) => {
                    let _ = fs::remove_file(&part_path2);
                }
                Ok(Some(ext)) => {
                    gf_dl.mark_complete();
                    let final_path2 = cache.path_for(&video_id, &ext);
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
                    if let Some(done) = cache.cached_file(&video_id) {
                        match gf_dl.adopt_complete_file(&done) {
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
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !AUDIO_EXTS.contains(&ext) {
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

fn range_request(
    client: &reqwest::Client,
    stream: &ResolvedStream,
    from: u64,
    to: Option<u64>,
) -> reqwest::RequestBuilder {
    let mut req = client.get(&stream.url);
    for (k, v) in &stream.headers {
        req = req.header(k, v);
    }
    // Single Range so googlevideo returns 206 and we can start decoding early.
    let range = match to {
        Some(to) => format!("bytes={from}-{to}"),
        None => format!("bytes={from}-"),
    };
    req.header(reqwest::header::RANGE, range)
}

/// An open download connection plus whatever it delivered before winning a race.
struct Body {
    stream: ResolvedStream,
    total: Option<u64>,
    prefix: Vec<u8>,
    rest: BoxStream<'static, reqwest::Result<bytes::Bytes>>,
}

/// Connect from byte `from` and read until `want` bytes arrived (or EOF).
async fn open_body(client: reqwest::Client, stream: ResolvedStream, from: u64, want: u64) -> Result<Body> {
    // Prefer the known content length as the end; open-ended otherwise.
    let to = stream.filesize.filter(|&n| n > from).map(|n| n - 1);
    let resp = range_request(&client, &stream, from, to)
        .send()
        .await
        .context("download request")?;
    let status = resp.status();
    // #region agent log
    crate::debug_log::log(
        if status.as_u16() == 403 { "A" } else { "D" },
        "player/download.rs:download_stream",
        "cdn response",
        serde_json::json!({
            "http_status": status.as_u16(),
            "ok": status.is_success() || status.as_u16() == 206,
            "url_host": stream.url.split('/').nth(2).unwrap_or(""),
            "client": stream.client_name,
            "from": from,
        }),
    );
    // #endregion
    if !(status.is_success() || status.as_u16() == 206) {
        anyhow::bail!("download HTTP {status}");
    }
    // Content-Range: bytes FROM-N/TOTAL
    let content_range = resp
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if from > 0 {
        let start = content_range
            .as_deref()
            .and_then(|s| s.strip_prefix("bytes ")?.split('-').next()?.parse::<u64>().ok());
        if start != Some(from) {
            anyhow::bail!("server ignored resume offset {from} ({content_range:?})");
        }
    }
    let total = stream.filesize.or_else(|| {
        content_range
            .as_deref()
            .and_then(|s| s.split('/').nth(1)?.parse().ok())
            .or_else(|| resp.content_length().map(|n| n + from))
    });

    let mut rest = resp.bytes_stream().boxed();
    let mut prefix = Vec::new();
    while (prefix.len() as u64) < want {
        match rest.next().await {
            Some(chunk) => prefix.extend_from_slice(&chunk.context("download body")?),
            None => break,
        }
    }
    Ok(Body {
        stream,
        total,
        prefix,
        rest,
    })
}

/// Open the download; when the first bytes are slow, race a backup stream from
/// another client and keep whichever delivers first.
async fn open_hedged(client: &reqwest::Client, stream: ResolvedStream, refetch: Option<&Refetcher>) -> Result<Body> {
    let primary = open_body(client.clone(), stream.clone(), 0, READY_BYTES);
    tokio::pin!(primary);
    let Some(refetch) = refetch else {
        return primary.await;
    };
    tokio::select! {
        res = &mut primary => return res,
        _ = tokio::time::sleep(HEDGE_AFTER) => {}
    }

    let backup = {
        let ask = refetch(Refetch::Alternate {
            exclude_client: stream.client_name.clone(),
        });
        let client = client.clone();
        async move {
            let alt = ask.await?;
            debug!("hedging slow start with {}", alt.client_name);
            match open_body(client, alt, 0, READY_BYTES).await {
                Ok(body) => Some(body),
                Err(e) => {
                    debug!("hedge failed: {e:#}");
                    None
                }
            }
        }
    };
    tokio::pin!(backup);
    let body = tokio::select! {
        res = &mut primary => match res {
            Ok(body) => body,
            Err(e) => backup.await.ok_or(e)?,
        },
        alt = &mut backup => match alt {
            Some(body) => body,
            None => primary.await?,
        },
    };
    // #region agent log
    crate::debug_log::log(
        "D",
        "player/download.rs:open_hedged",
        "hedged start",
        serde_json::json!({
            "primary": stream.client_name,
            "winner": body.stream.client_name,
        }),
    );
    // #endregion
    Ok(body)
}

/// Finds the cached file a twin download may have finished meanwhile.
type Twin<'a> = &'a (dyn Fn() -> Option<PathBuf> + Send + Sync);

/// Append `bytes`; if the file was already finished by a twin, adopt that instead.
/// Returns `true` when the download is done because of that.
fn write(gf: &GrowingFile, twin: Option<Twin>, bytes: &[u8]) -> Result<bool> {
    match gf.append(bytes) {
        Ok(()) => Ok(false),
        Err(e) => match twin.and_then(|find| find()) {
            Some(done) => {
                gf.adopt_complete_file(&done)?;
                Ok(true)
            }
            None => Err(e),
        },
    }
}

#[allow(clippy::too_many_arguments)]
async fn download_stream(
    client: &reqwest::Client,
    stream: ResolvedStream,
    start_at: u64,
    gf: &GrowingFile,
    generation: u64,
    // Finds a finished twin download; `None` when replacing that file.
    twin: Option<Twin<'_>>,
    on_progress: &impl Fn(f32),
    refetch: Option<Refetcher>,
) -> Result<Option<String>> {
    if gf.is_stale_generation(generation) {
        anyhow::bail!("download superseded");
    }

    // One connection with a Range header. Do not use `&range=` query chunks.
    let body = if start_at == 0 {
        open_hedged(client, stream, refetch.as_ref()).await?
    } else {
        open_body(client.clone(), stream, start_at, 0).await?
    };
    let Body {
        stream: mut current,
        total,
        prefix,
        mut rest,
    } = body;
    if let Some(t) = total {
        gf.set_expected_len(t);
    }

    let mut written = start_at;
    let mut last_pct_reported: i32 = -1;
    let mut report = |written: u64| {
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
    };
    if !prefix.is_empty() {
        if write(gf, twin, &prefix)? {
            on_progress(1.0);
            return Ok(None);
        }
        written += prefix.len() as u64;
        report(written);
    }

    // Watchdog: a download slower than playback needs gets a fresh URL and
    // resumes where it is (same file, so the bytes line up).
    let min_bps = match (total, current.duration) {
        (Some(t), Some(d)) if d > 1.0 => Some(t as f64 * 8.0 / d * MIN_RATE_FACTOR),
        _ => None,
    };
    let mut window = (Instant::now(), 0u64);
    let mut resumes = 0u32;
    loop {
        if gf.is_stale_generation(generation) {
            anyhow::bail!("download superseded");
        }
        let mut slow = false;
        match tokio::time::timeout(STALL_AFTER, rest.next()).await {
            Ok(Some(item)) => {
                let bytes = item.context("download body")?;
                if !bytes.is_empty() {
                    if write(gf, twin, &bytes)? {
                        on_progress(1.0);
                        return Ok(None);
                    }
                    written += bytes.len() as u64;
                    window.1 += bytes.len() as u64;
                    report(written);
                }
            }
            Ok(None) => break,
            Err(_) => slow = true,
        }
        let elapsed = window.0.elapsed();
        if !slow && elapsed >= RATE_WINDOW {
            if let Some(min) = min_bps {
                slow = (window.1 as f64 * 8.0 / elapsed.as_secs_f64()) < min;
            }
            window = (Instant::now(), 0);
        }
        let (Some(refetch), Some(t)) = (refetch.as_ref(), total) else {
            continue;
        };
        if !slow || resumes >= MAX_RESUMES || written >= t {
            continue;
        }
        resumes += 1;
        window = (Instant::now(), 0);
        let ask = refetch(Refetch::SameFile {
            client: current.client_name.clone(),
            filesize: t,
        });
        let resumed = match ask.await {
            Some(fresh) => open_body(client.clone(), fresh, written, 0).await,
            None => Err(anyhow::anyhow!("no fresh url for the same file")),
        };
        // #region agent log
        crate::debug_log::log(
            "D",
            "player/download.rs:download_stream",
            "slow download",
            serde_json::json!({
                "client": current.client_name,
                "written": written,
                "total": t,
                "resume": resumes,
                "resumed": resumed.as_ref().map(|_| ()).map_err(|e| e.to_string()),
            }),
        );
        // #endregion
        match resumed {
            Ok(body) => {
                debug!("download slow; resumed at {written} via {}", body.stream.client_name);
                current = body.stream;
                rest = body.rest;
            }
            // Keep the connection we have; it is still making progress (or will time out).
            Err(e) => debug!("download slow; staying on current connection: {e:#}"),
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
    Ok(Some(current.ext))
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// How the fake CDN serves a path.
    #[derive(Clone, Copy)]
    enum Serve {
        Fast,
        /// Headers only after this long.
        SlowStart(Duration),
        /// The first bytes fast, then a trickle.
        Throttle { fast: usize },
    }

    /// Minimal googlevideo stand-in: honours `Range`, answers 206.
    async fn cdn(routes: Vec<(&'static str, Serve, Arc<Vec<u8>>)>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let routes = Arc::new(routes);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let routes = routes.clone();
                tokio::spawn(async move {
                    let mut req = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                        let Ok(n) = sock.read(&mut buf).await else { return };
                        if n == 0 {
                            return;
                        }
                        req.extend_from_slice(&buf[..n]);
                    }
                    let req = String::from_utf8_lossy(&req).to_string();
                    let path = req.split_whitespace().nth(1).unwrap_or("").to_string();
                    let Some((_, serve, data)) = routes.iter().find(|(p, ..)| *p == path) else {
                        let _ = sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n").await;
                        return;
                    };
                    let range = req
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("range: bytes=").map(str::to_string))
                        .unwrap_or_else(|| "0-".into());
                    let (a, b) = range.split_once('-').unwrap();
                    let a: usize = a.trim().parse().unwrap();
                    let b: usize = b.trim().parse().unwrap_or(data.len() - 1).min(data.len() - 1);
                    if let Serve::SlowStart(d) = serve {
                        tokio::time::sleep(*d).await;
                    }
                    let head = format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {a}-{b}/{}\r\nConnection: close\r\n\r\n",
                        b + 1 - a,
                        data.len()
                    );
                    if sock.write_all(head.as_bytes()).await.is_err() {
                        return;
                    }
                    let body = &data[a..=b];
                    match serve {
                        Serve::Throttle { fast } => {
                            let fast = (*fast).min(body.len());
                            if sock.write_all(&body[..fast]).await.is_err() {
                                return;
                            }
                            for chunk in body[fast..].chunks(1024) {
                                tokio::time::sleep(Duration::from_millis(100)).await;
                                if sock.write_all(chunk).await.is_err() {
                                    return;
                                }
                            }
                        }
                        _ => {
                            let _ = sock.write_all(body).await;
                        }
                    }
                });
            }
        });
        base
    }

    fn data(len: usize, seed: u8) -> Arc<Vec<u8>> {
        Arc::new((0..len).map(|i| (i % 251) as u8 ^ seed).collect())
    }

    fn stream(url: String, client: &str, data: &[u8], secs: f64) -> ResolvedStream {
        ResolvedStream {
            url,
            headers: HashMap::new(),
            filesize: Some(data.len() as u64),
            duration: Some(secs),
            ext: "m4a".into(),
            resolved_at: Instant::now(),
            expires_at: Instant::now() + Duration::from_secs(3600),
            client_name: client.into(),
            itag: None,
        }
    }

    fn downloader(name: &str) -> Downloader {
        let dir = std::env::temp_dir().join(format!("yt-tui-dl-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Downloader::new(dir, 100)
    }

    /// Wait for the background download to finish and return the cached file.
    async fn finished(dl: &Downloader, gf: &GrowingFile, vid: &str) -> Vec<u8> {
        for _ in 0..200 {
            if gf.is_complete() && dl.is_cached(vid) {
                return fs::read(dl.cached_path(vid)).unwrap();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("download of {vid} never finished ({} bytes)", gf.len());
    }

    fn refetcher(alt: Option<ResolvedStream>, same: Option<ResolvedStream>) -> Refetcher {
        Arc::new(move |want| {
            let s = match want {
                Refetch::Alternate { .. } => alt.clone(),
                Refetch::SameFile { filesize, .. } => same.clone().filter(|s| s.filesize == Some(filesize)),
            };
            Box::pin(async move { s })
        })
    }

    #[tokio::test]
    async fn upgrade_swaps_cached_file_under_a_playing_reader() {
        use std::io::Read;
        let old = data(300_000, 4);
        let better = data(600_000, 5);
        let base = cdn(vec![("/better", Serve::Fast, better.clone())]).await;
        let dl = downloader("upgrade");
        fs::write(dl.cached_path("v"), &*old).unwrap();
        // Playing the old file: its handle must keep reading the old bytes.
        let playing = GrowingFile::open_complete(&dl.cached_path("v")).unwrap();
        let mut reader = playing.reader(crate::player::source::CancelToken::default());
        let mut first = vec![0u8; 1000];
        reader.read_exact(&mut first).unwrap();

        let s = stream(format!("{base}/better"), "WEB_REMIX", &better, 60.0);
        dl.upgrade("v", &s, None).await.unwrap();
        assert_eq!(fs::read(dl.cached_path("v")).unwrap(), *better);
        assert!(!dl.cache_dir.join("v.upgrade").exists());

        let mut rest = Vec::new();
        reader.read_to_end(&mut rest).unwrap();
        first.extend(rest);
        assert_eq!(first, *old);
    }

    #[tokio::test]
    async fn failed_upgrade_keeps_cached_file() {
        let old = data(300_000, 4);
        let base = cdn(vec![]).await;
        let dl = downloader("upgrade-fail");
        fs::write(dl.cached_path("v"), &*old).unwrap();
        let s = stream(format!("{base}/missing"), "WEB_REMIX", &old, 60.0);
        assert!(dl.upgrade("v", &s, None).await.is_err());
        assert_eq!(fs::read(dl.cached_path("v")).unwrap(), *old);
        assert!(!dl.cache_dir.join("v.upgrade").exists());
    }

    #[tokio::test]
    async fn files_are_named_by_container() {
        let opus = data(300_000, 6);
        let base = cdn(vec![("/opus", Serve::Fast, opus.clone())]).await;
        let dl = downloader("ext");
        let mut s = stream(format!("{base}/opus"), "VISIONOS", &opus, 60.0);
        s.ext = "webm".into();
        // Fresh download lands as .webm.
        let gf = dl.get("v", &s, None, |_| {}).await.unwrap();
        assert_eq!(finished(&dl, &gf, "v").await, *opus);
        assert_eq!(dl.cached_file("v").unwrap(), dl.cache_dir.join("v.webm"));
        // An upgrade across containers leaves exactly one file.
        fs::remove_file(dl.cache_dir.join("v.webm")).unwrap();
        fs::write(dl.cache_dir.join("w.m4a"), b"aac").unwrap();
        dl.upgrade("w", &s, None).await.unwrap();
        assert_eq!(dl.cached_file("w").unwrap(), dl.cache_dir.join("w.webm"));
        assert!(!dl.cache_dir.join("w.m4a").exists());
    }

    #[test]
    fn best_available_is_remembered_across_restarts() {
        let dl = downloader("best");
        assert!(!dl.is_best_available("ugc"));
        dl.mark_best_available("ugc");
        dl.mark_best_available("ugc");
        assert!(dl.is_best_available("ugc"));
        let again = Downloader::new(dl.cache_dir.clone(), 100);
        assert!(again.is_best_available("ugc") && !again.is_best_available("other"));
        let lines = fs::read_to_string(dl.cache_dir.join(BEST_AVAILABLE_FILE)).unwrap();
        assert_eq!(lines, "ugc\n");
    }

    #[tokio::test]
    async fn prewarmed_head_starts_at_once() {
        let file = data(700_000, 1);
        let base = cdn(vec![("/a", Serve::SlowStart(Duration::from_millis(1500)), file.clone())]).await;
        let dl = downloader("head");
        let s = stream(format!("{base}/a"), "VISIONOS", &file, 60.0);
        // Prewarming waits out the slow start; playing afterwards doesn't.
        dl.prewarm("v", &s).await.unwrap();
        let t = Instant::now();
        let gf = dl.get("v", &s, None, |_| {}).await.unwrap();
        assert!(t.elapsed() < Duration::from_millis(300), "{:?}", t.elapsed());
        assert!(gf.len() >= HEAD_BYTES);
        assert_eq!(finished(&dl, &gf, "v").await, *file);
    }

    #[tokio::test]
    async fn slow_start_is_hedged_with_another_client() {
        let primary = data(600_000, 1);
        let backup = data(500_000, 2);
        let base = cdn(vec![
            ("/slow", Serve::SlowStart(Duration::from_secs(10)), primary.clone()),
            ("/fast", Serve::Fast, backup.clone()),
        ])
        .await;
        let dl = downloader("hedge");
        let s = stream(format!("{base}/slow"), "WEB_REMIX", &primary, 60.0);
        let alt = stream(format!("{base}/fast"), "VISIONOS", &backup, 60.0);
        let t = Instant::now();
        let gf = dl.get("v", &s, Some(refetcher(Some(alt), None)), |_| {}).await.unwrap();
        assert!(t.elapsed() < Duration::from_millis(1500), "{:?}", t.elapsed());
        assert_eq!(finished(&dl, &gf, "v").await, *backup);
    }

    #[tokio::test]
    async fn fast_start_is_not_hedged() {
        let primary = data(300_000, 1);
        let backup = data(200_000, 2);
        let base = cdn(vec![("/fast", Serve::Fast, primary.clone()), ("/other", Serve::Fast, backup.clone())]).await;
        let dl = downloader("nohedge");
        let s = stream(format!("{base}/fast"), "WEB_REMIX", &primary, 60.0);
        let alt = stream(format!("{base}/other"), "VISIONOS", &backup, 60.0);
        let gf = dl.get("v", &s, Some(refetcher(Some(alt), None)), |_| {}).await.unwrap();
        assert_eq!(finished(&dl, &gf, "v").await, *primary);
    }

    #[tokio::test]
    async fn slow_download_resumes_on_fresh_url() {
        // 600 KB over 60 s needs 80 kbit/s; the trickle gives 10 KB/s.
        let file = data(600_000, 3);
        let base = cdn(vec![
            ("/throttled", Serve::Throttle { fast: 100_000 }, file.clone()),
            ("/fresh", Serve::Fast, file.clone()),
        ])
        .await;
        let dl = downloader("resume");
        let s = stream(format!("{base}/throttled"), "VISIONOS", &file, 60.0);
        let fresh = stream(format!("{base}/fresh"), "VISIONOS", &file, 60.0);
        let t = Instant::now();
        let gf = dl.get("v", &s, Some(refetcher(None, Some(fresh))), |_| {}).await.unwrap();
        // Byte-identical across the splice, and far sooner than the trickle's ~50 s.
        assert_eq!(finished(&dl, &gf, "v").await, *file);
        assert!(t.elapsed() < Duration::from_secs(8), "{:?}", t.elapsed());
    }
}
