use crate::auth::{manual, sapisid};
use crate::player::jsc::Jsc;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{watch, Mutex};
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Client constants — mirror yt-dlp INNERTUBE_CLIENTS (no GVS PO token required).
// ANDROID / IOS require PO tokens and are intentionally omitted.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct PlayerClient {
    name: &'static str,
    /// `X-Youtube-Client-Name` numeric id.
    client_id: &'static str,
    version: &'static str,
    user_agent: &'static str,
    /// Send Cookie header when we have a session.
    wants_cookies: bool,
    /// Attach Origin/Referer suitable for web_embedded CDN fetches.
    web_headers: bool,
}

/// yt-dlp JS-less default — plaintext adaptive audio, no PO token.
const VISIONOS: PlayerClient = PlayerClient {
    name: "VISIONOS",
    client_id: "101",
    version: "1.02",
    user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15",
    wants_cookies: false,
    web_headers: false,
};

/// yt-dlp `tv_downgraded` — authenticated fallback (cookies); newer TVHTML5 often DRM-only.
const TV_DOWNGRADED: PlayerClient = PlayerClient {
    name: "TVHTML5",
    client_id: "7",
    version: "5.20260707",
    user_agent: "Mozilla/5.0 (ChromiumStylePlatform) Cobalt/Version",
    wants_cookies: true,
    web_headers: false,
};

/// Embed client — useful for some age-gated / embeddable videos; many Music tracks are unavailable.
const WEB_EMBEDDED: PlayerClient = PlayerClient {
    name: "WEB_EMBEDDED",
    client_id: "56",
    version: "2.20260708.00.00",
    user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/15.5 Safari/605.1.15,gzip(gfe)",
    wants_cookies: true,
    web_headers: true,
};

/// Prefer VISIONOS first so visitor_id is minted for the same client we download with.
/// WEB_EMBEDDED first caused mismatched visitor ↔ CDN URL pairs (HTTP 403).
const CLIENTS: &[PlayerClient] = &[VISIONOS, TV_DOWNGRADED, WEB_EMBEDDED];

/// YouTube Music web. Honors a signed-in session (Premium-only and age-gated
/// tracks, 256 kbps audio), but its URLs are signature-ciphered and need the
/// player JS solved. Without Premium, googlevideo also wants a PO token, so it
/// only goes first for Premium accounts and is a fallback otherwise.
const WEB_REMIX_NAME: &str = "WEB_REMIX";
const WEB_REMIX_ID: &str = "67";
const WEB_REMIX_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";
const MUSIC_ORIGIN: &str = "https://music.youtube.com";
const WEB_REMIX_PLAYER_URL: &str = "https://music.youtube.com/youtubei/v1/player?prettyPrint=false";
/// ~256 kbps Opus and 256 kbps AAC — only served to Premium accounts.
const PREMIUM_OPUS_ITAG: i64 = 774;
const PREMIUM_ITAG: i64 = 141;
/// ~150 kbps Opus (WebM): the best rendition without Premium, better than AAC 128.
const OPUS_ITAG: i64 = 251;

const PLAYER_URL: &str = "https://www.youtube.com/youtubei/v1/player?prettyPrint=false";
const VISITOR_URL: &str = "https://www.youtube.com/youtubei/v1/visitor_id?prettyPrint=false";

const PREFERRED_ITAG: i64 = 140;
/// Progressive muxed mp4 (H.264 + AAC) — works without PO on some clients.
const PROGRESSIVE_ITAG: i64 = 18;

/// Plaintext clients: Opus 251, then AAC 140, then the best other audio.
const PLAIN_PREFERENCE: &[i64] = &[OPUS_ITAG, PREFERRED_ITAG];
/// YouTube Music: Premium Opus, Premium AAC, then as for plaintext clients.
const REMIX_PREFERENCE: &[i64] = &[PREMIUM_OPUS_ITAG, PREMIUM_ITAG, OPUS_ITAG, PREFERRED_ITAG];

/// Nominal `(codec, kbps)` of the audio renditions YouTube serves, by itag.
fn itag_quality(itag: i64) -> Option<(&'static str, u32)> {
    Some(match itag {
        PREMIUM_OPUS_ITAG => ("Opus", 256),
        PREMIUM_ITAG => ("AAC", 256),
        OPUS_ITAG => ("Opus", 150),
        PREFERRED_ITAG => ("AAC", 128),
        250 => ("Opus", 70),
        249 => ("Opus", 50),
        139 => ("AAC", 48),
        PROGRESSIVE_ITAG => ("AAC", 96),
        _ => return None,
    })
}

/// Container of a format: WebM for Opus, mp4 for progressive video, m4a otherwise.
fn format_ext(f: &StreamFormat) -> &'static str {
    if f.mime_type.as_deref().is_some_and(|m| m.starts_with("audio/webm")) {
        "webm"
    } else if f.itag == Some(PROGRESSIVE_ITAG) {
        "mp4"
    } else {
        "m4a"
    }
}

/// WEB_REMIX responses without the Premium itag before an account counts as free.
/// One is not enough: a track may simply lack a 256 kbps rendition.
const PREMIUM_MISSES: u8 = 3;

/// Always offered at 256 kbps to Premium accounts; probed right after sign-in.
const PREMIUM_PROBE_ID: &str = "dQw4w9WgXcQ";

/// What WEB_REMIX responses revealed about the signed-in account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Premium {
    Unknown { misses: u8 },
    Yes,
    No,
}

impl Premium {
    pub const UNKNOWN: Premium = Premium::Unknown { misses: 0 };

    /// `Some(is_premium)` once settled.
    pub fn known(self) -> Option<bool> {
        match self {
            Premium::Yes => Some(true),
            Premium::No => Some(false),
            Premium::Unknown { .. } => None,
        }
    }

    /// Fold in whether a playable WEB_REMIX response offered the Premium itag.
    fn observe(self, offered: bool) -> Premium {
        match (self, offered) {
            (_, true) => Premium::Yes,
            (Premium::Unknown { misses }, false) if misses + 1 >= PREMIUM_MISSES => Premium::No,
            (Premium::Unknown { misses }, false) => Premium::Unknown { misses: misses + 1 },
            (known, false) => known,
        }
    }
}

/// `STATUS: reason` from a player response, as opposed to network/solver trouble.
/// A signed-in WEB_REMIX request may get past any of these: besides Premium and
/// age gates, some Music-only releases are refused as "not available" elsewhere.
fn is_playability_error(err: &str) -> bool {
    ["UNPLAYABLE:", "LOGIN_REQUIRED:", "ERROR:", "AGE_", "CONTENT_CHECK_REQUIRED:"]
        .iter()
        .any(|p| err.starts_with(p))
}

/// YouTube refused the video for every client — retrying won't help.
#[derive(Debug)]
pub struct Unplayable(pub &'static str);

impl std::fmt::Display for Unplayable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Unplayable {}

/// Classify per-client errors as a permanent refusal. Bot checks and
/// "reload" errors are transient and stay retryable.
///
/// Only meaningful once WEB_REMIX had its say for a signed-in session — the
/// plaintext clients ignore the session and refuse Premium/age-gated tracks.
fn unplayable_reason(errors: &[String], signed_in: bool) -> Option<&'static str> {
    let any = |needle: &str| errors.iter().any(|e| e.contains(needle));
    if any("Music Premium") {
        Some("requires Music Premium")
    } else if any("confirm your age") {
        Some(if signed_in {
            "age-restricted, not playable for this account"
        } else {
            "age-restricted, sign in to play"
        })
    } else if !errors.is_empty()
        && errors
            .iter()
            .all(|e| e.contains("not available") || e.contains("unavailable"))
    {
        Some("not available on YouTube")
    } else {
        None
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedStream {
    pub url: String,
    pub headers: HashMap<String, String>,
    pub filesize: Option<u64>,
    pub duration: Option<f64>,
    pub ext: String,
    pub resolved_at: Instant,
    /// Absolute expiry; derived from `streamingData.expiresInSeconds` when present.
    pub expires_at: Instant,
    /// Innertube client that produced this URL.
    pub client_name: String,
    pub itag: Option<i64>,
}

impl ResolvedStream {
    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.expires_at
    }

    /// Nominal `(codec, kbps)` of this rendition, when its itag is a known one.
    pub fn quality(&self) -> Option<(&'static str, u32)> {
        itag_quality(self.itag?)
    }
}

#[derive(Debug, Deserialize)]
struct PlayerResponse {
    #[serde(rename = "playabilityStatus")]
    playability: Option<PlayabilityStatus>,
    #[serde(rename = "streamingData")]
    streaming: Option<StreamingData>,
    #[serde(rename = "responseContext")]
    response_context: Option<ResponseContext>,
}

#[derive(Debug, Deserialize)]
struct PlayabilityStatus {
    status: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StreamingData {
    #[serde(rename = "expiresInSeconds")]
    expires_in_seconds: Option<String>,
    #[serde(rename = "adaptiveFormats", default)]
    adaptive_formats: Vec<StreamFormat>,
    #[serde(rename = "formats", default)]
    formats: Vec<StreamFormat>,
}

#[derive(Debug, Deserialize, Clone)]
struct StreamFormat {
    itag: Option<i64>,
    url: Option<String>,
    #[serde(rename = "mimeType")]
    mime_type: Option<String>,
    bitrate: Option<u64>,
    #[serde(rename = "contentLength")]
    content_length: Option<serde_json::Value>,
    #[serde(rename = "approxDurationMs")]
    approx_duration_ms: Option<serde_json::Value>,
    /// `s=…&sp=…&url=…` instead of `url` on ciphered clients (WEB_REMIX).
    #[serde(rename = "signatureCipher")]
    signature_cipher: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResponseContext {
    #[serde(rename = "visitorData")]
    visitor_data: Option<String>,
}

#[derive(Clone)]
pub struct Resolver {
    http: reqwest::Client,
    visitor_data: Arc<Mutex<Option<String>>>,
    cookies: Arc<Mutex<Option<String>>>,
    cache: Arc<Mutex<HashMap<String, ResolvedStream>>>,
    /// Client names skipped on the next resolve (CDN 403 failover).
    skip_clients: Arc<Mutex<HashMap<String, Vec<String>>>>,
    premium: Arc<watch::Sender<Premium>>,
    jsc: Jsc,
}

impl Resolver {
    pub fn new(player_js_dir: PathBuf) -> Self {
        let http = reqwest::Client::builder()
            .gzip(true)
            .timeout(Duration::from_secs(15))
            .build()
            .expect("reqwest client");
        Self {
            jsc: Jsc::new(http.clone(), player_js_dir),
            http,
            visitor_data: Arc::new(Mutex::new(None)),
            cookies: Arc::new(Mutex::new(None)),
            cache: Arc::new(Mutex::new(HashMap::new())),
            skip_clients: Arc::new(Mutex::new(HashMap::new())),
            premium: Arc::new(watch::channel(Premium::UNKNOWN).0),
        }
    }

    pub async fn set_cookies(&self, cookies: Option<String>) {
        let signed_in = cookies.as_deref().and_then(manual::sapisid_from_cookies).is_some();
        *self.cookies.lock().await = cookies.clone();
        // Auth change invalidates any cookie-bound URLs / visitors.
        self.cache.lock().await.clear();
        self.skip_clients.lock().await.clear();
        self.update_premium(|_| Premium::UNKNOWN);
        *self.visitor_data.lock().await = None;

        if let (true, Some(cookies)) = (signed_in, cookies) {
            // Signed in: WEB_REMIX may be needed, so pay the ~2s player
            // preprocessing now instead of on the first restricted track,
            // then settle Premium so it shows before anything plays.
            let resolver = self.clone();
            tokio::spawn(async move {
                if let Err(e) = resolver.jsc.warm_up().await {
                    warn!("warming up stream deciphering: {e:#}");
                }
                resolver.probe_premium(&cookies).await;
            });
        }
    }

    /// The best rendition this account can get for `video_id` right now, fetched
    /// fresh. `Ok(None)` when YouTube refuses it outright; `Err` is trouble worth
    /// retrying another time.
    pub async fn best_stream(&self, video_id: &str) -> Result<Option<ResolvedStream>> {
        match self.resolve_uncached(video_id, None).await {
            Ok(stream) => Ok(Some(stream)),
            Err(e) if e.downcast_ref::<Unplayable>().is_some() => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Premium status as learned so far; changes whenever it does.
    pub fn subscribe_premium(&self) -> watch::Receiver<Premium> {
        self.premium.subscribe()
    }

    fn update_premium(&self, f: impl FnOnce(Premium) -> Premium) -> Premium {
        let mut now = Premium::UNKNOWN;
        self.premium.send_if_modified(|p| {
            now = f(*p);
            let changed = now != *p;
            if changed {
                info!("Music Premium for this account: {now:?}");
            }
            *p = now;
            changed
        });
        now
    }

    /// Ask for a track every Premium account gets at 256 kbps; its absence
    /// settles the account as free.
    async fn probe_premium(&self, cookies: &str) {
        if !matches!(*self.premium.borrow(), Premium::Unknown { .. }) {
            return;
        }
        let result = self.resolve_web_remix(PREMIUM_PROBE_ID).await;
        // Signed out or switched accounts meanwhile: the answer is about someone else.
        if self.cookies.lock().await.as_deref() != Some(cookies) {
            return;
        }
        match result {
            Ok((_, offered)) => {
                self.update_premium(|p| match p {
                    Premium::Unknown { .. } if offered => Premium::Yes,
                    Premium::Unknown { .. } => Premium::No,
                    known => known,
                });
            }
            Err(e) => warn!("checking Music Premium: {e:#}"),
        }
    }

    /// Drop sticky visitor so the next resolve mints a fresh one (CDN 403 recovery).
    pub async fn clear_visitor(&self) {
        *self.visitor_data.lock().await = None;
    }

    pub async fn resolve(&self, video_id: &str) -> Result<ResolvedStream> {
        {
            let cache = self.cache.lock().await;
            if let Some(s) = cache.get(video_id) {
                if !s.is_expired() {
                    let skip = self.skip_clients.lock().await;
                    let skipped = skip
                        .get(video_id)
                        .map(|v| v.iter().any(|c| c == &s.client_name))
                        .unwrap_or(false);
                    if !skipped {
                        return Ok(s.clone());
                    }
                }
            }
        }

        let result = self.resolve_uncached(video_id, None).await;
        if let Ok(ref stream) = result {
            self.cache
                .lock()
                .await
                .insert(video_id.to_string(), stream.clone());
        }
        result
    }

    pub async fn invalidate(&self, video_id: &str) {
        self.cache.lock().await.remove(video_id);
    }

    /// Drop a cached URL and skip that client on the next resolve (CDN 403).
    pub async fn invalidate_skipping_client(&self, video_id: &str, client_name: &str) {
        self.cache.lock().await.remove(video_id);
        let mut skip = self.skip_clients.lock().await;
        let entry = skip.entry(video_id.to_string()).or_default();
        if !entry.iter().any(|c| c == client_name) {
            entry.push(client_name.to_string());
        }
    }

    /// Clear CDN-403 skips after a download actually succeeds.
    pub async fn clear_client_skips(&self, video_id: &str) {
        self.skip_clients.lock().await.remove(video_id);
    }

    /// A stream from any client but `exclude`, bypassing the cache — the backup
    /// in a hedged start. It may be a different rendition (e.g. 128 vs 256 kbps).
    pub async fn resolve_alternate(&self, video_id: &str, exclude: &str) -> Result<ResolvedStream> {
        self.resolve_uncached(video_id, Some(exclude)).await
    }

    /// A fresh URL for the byte-identical file (same size), to resume a slow
    /// download mid-way. The original client is asked first: it serves the same
    /// rendition; other clients only count when the size matches exactly.
    pub async fn resolve_same_file(
        &self,
        video_id: &str,
        client: &str,
        filesize: u64,
    ) -> Option<ResolvedStream> {
        let others = CLIENTS.iter().map(|c| c.name).chain([WEB_REMIX_NAME]);
        let names = std::iter::once(client).chain(others.filter(|n| *n != client));
        for name in names {
            let result = if name == WEB_REMIX_NAME {
                if !self.signed_in().await {
                    continue;
                }
                self.resolve_web_remix(video_id).await.map(|(s, _)| s)
            } else {
                let Some(&c) = CLIENTS.iter().find(|c| c.name == name) else {
                    continue;
                };
                self.resolve_with_client(video_id, c).await
            };
            match result {
                Ok(stream) if stream.filesize == Some(filesize) => {
                    self.cache
                        .lock()
                        .await
                        .insert(video_id.to_string(), stream.clone());
                    return Some(stream);
                }
                Ok(_) => debug!("{name} serves a different rendition of {video_id}"),
                Err(e) => debug!("{name} fresh url for {video_id}: {e}"),
            }
        }
        None
    }

    async fn signed_in(&self) -> bool {
        self.cookies
            .lock()
            .await
            .as_deref()
            .and_then(manual::sapisid_from_cookies)
            .is_some()
    }

    async fn resolve_uncached(&self, video_id: &str, exclude: Option<&str>) -> Result<ResolvedStream> {
        let mut last_err: Option<anyhow::Error> = None;
        let mut errors: Vec<String> = Vec::new();
        let mut retried_visitor = false;
        let mut skipped = self
            .skip_clients
            .lock()
            .await
            .get(video_id)
            .cloned()
            .unwrap_or_default();
        skipped.extend(exclude.map(str::to_string));
        let signed_in = self.signed_in().await;
        let remix_allowed = signed_in && !skipped.iter().any(|c| c == WEB_REMIX_NAME);

        // Premium (or not yet known): go straight to YouTube Music for 256 kbps.
        let mut remix_tried = false;
        let mut remix_spare: Option<ResolvedStream> = None;
        if remix_allowed && *self.premium.borrow() != Premium::No {
            remix_tried = true;
            match self.resolve_web_remix(video_id).await {
                Ok((stream, offered)) => {
                    let premium = self.update_premium(|p| p.observe(offered));
                    if premium == Premium::Yes {
                        return Ok(stream);
                    }
                    // Likely a free account: googlevideo wants a PO token for this
                    // URL, so prefer the plaintext clients and keep it as a last resort.
                    remix_spare = Some(stream);
                }
                Err(e) => {
                    // Plain YouTube uploads aren't on Music; solver trouble may pass.
                    // Either way the plaintext clients get their turn.
                    let msg = e.to_string();
                    warn!("{WEB_REMIX_NAME} failed for {video_id}: {msg}; trying other clients");
                    if is_playability_error(&msg) {
                        errors.push(msg);
                        last_err = Some(e);
                    }
                }
            }
        }

        for &client in CLIENTS {
            if skipped.iter().any(|c| c == client.name) {
                debug!("skipping {} for {video_id} (prior CDN 403 or excluded)", client.name);
                continue;
            }
            match self.resolve_with_client(video_id, client).await {
                Ok(stream) => return Ok(stream),
                Err(e) => {
                    let msg = e.to_string();
                    if !retried_visitor && msg.contains("LOGIN_REQUIRED") {
                        retried_visitor = true;
                        *self.visitor_data.lock().await = None;
                        match self.resolve_with_client(video_id, client).await {
                            Ok(stream) => return Ok(stream),
                            Err(e2) => {
                                warn!("{} after visitor refresh: {e2}", client.name);
                                errors.push(e2.to_string());
                                last_err = Some(e2);
                                continue;
                            }
                        }
                    }
                    warn!("{} failed for {video_id}: {e}", client.name);
                    errors.push(msg);
                    last_err = Some(e);
                }
            }
        }

        if let Some(stream) = remix_spare {
            return Ok(stream);
        }
        if remix_allowed && !remix_tried && errors.iter().any(|e| is_playability_error(e)) {
            match self.resolve_web_remix(video_id).await {
                Ok((stream, _)) => return Ok(stream),
                Err(e) => {
                    let msg = e.to_string();
                    warn!("{WEB_REMIX_NAME} failed for {video_id}: {msg}");
                    // Solver/network trouble is worth a retry, not a verdict.
                    if !is_playability_error(&msg) {
                        return Err(e);
                    }
                    errors.push(msg);
                    last_err = Some(e);
                }
            }
        }

        if let Some(reason) = unplayable_reason(&errors, signed_in) {
            return Err(Unplayable(reason).into());
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no player client succeeded")))
    }

    async fn resolve_with_client(
        &self,
        video_id: &str,
        client: PlayerClient,
    ) -> Result<ResolvedStream> {
        let visitor = self.ensure_visitor(client).await?;
        let body = player_body(video_id, client, &visitor);
        let cookies = self.cookies.lock().await.clone();

        debug!("player {} video_id={video_id}", client.name);
        let mut req = self
            .http
            .post(PLAYER_URL)
            .header("Content-Type", "application/json")
            .header("User-Agent", client.user_agent)
            .header("X-Youtube-Client-Name", client.client_id)
            .header("X-Youtube-Client-Version", client.version)
            .header("X-Goog-Visitor-Id", &visitor);

        if client.web_headers {
            req = req
                .header("Origin", "https://www.youtube.com")
                .header("Referer", "https://www.youtube.com/");
        }

        if client.wants_cookies {
            if let Some(ref c) = cookies {
                req = req.header("Cookie", c);
            }
        }

        let resp = req
            .json(&body)
            .send()
            .await
            .with_context(|| format!("player {} request", client.name))?;

        let status = resp.status();
        let text = resp.text().await.context("player response body")?;
        if !status.is_success() {
            bail!(
                "player {} HTTP {status}: {}",
                client.name,
                truncate(&text, 200)
            );
        }

        let parsed: PlayerResponse =
            serde_json::from_str(&text).context("parsing player JSON")?;

        // Bind CDN fetches to the visitor that minted THIS url — a sticky visitor
        // from another client (e.g. WEB_EMBEDDED) is a common googlevideo 403 cause.
        let visitor_for_cdn = parsed
            .response_context
            .as_ref()
            .and_then(|c| c.visitor_data.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| visitor.clone());
        if !visitor_for_cdn.is_empty() {
            *self.visitor_data.lock().await = Some(visitor_for_cdn.clone());
        }

        let playability = parsed.playability.as_ref();
        let status_str = playability
            .and_then(|p| p.status.as_deref())
            .unwrap_or("UNKNOWN");
        if status_str != "OK" {
            let reason = playability
                .and_then(|p| p.reason.as_deref())
                .unwrap_or(status_str);
            // #region agent log
            crate::debug_log::log(
                "B",
                "player/resolver.rs:resolve_with_client",
                "playability not OK",
                serde_json::json!({
                    "video_id": video_id,
                    "client": client.name,
                    "status": status_str,
                    "reason": reason,
                    "had_cookies": cookies.is_some(),
                }),
            );
            // #endregion
            bail!("{status_str}: {reason}");
        }

        let streaming = parsed
            .streaming
            .as_ref()
            .context("player response missing streamingData")?;

        let adaptive_n = streaming.adaptive_formats.len();
        let formats_n = streaming.formats.len();
        let adaptive_with_url = streaming
            .adaptive_formats
            .iter()
            .filter(|f| f.url.as_ref().is_some_and(|u| u.starts_with("http")))
            .count();
        let formats_with_url = streaming
            .formats
            .iter()
            .filter(|f| f.url.as_ref().is_some_and(|u| u.starts_with("http")))
            .count();

        let format = match select_format(&streaming.adaptive_formats, &streaming.formats) {
            Some(f) => f,
            None => {
                // #region agent log
                crate::debug_log::log(
                    "C",
                    "player/resolver.rs:resolve_with_client",
                    "no plaintext format",
                    serde_json::json!({
                        "video_id": video_id,
                        "client": client.name,
                        "adaptive_n": adaptive_n,
                        "formats_n": formats_n,
                        "adaptive_with_url": adaptive_with_url,
                        "formats_with_url": formats_with_url,
                        "had_cookies": cookies.is_some(),
                    }),
                );
                // #endregion
                bail!("no playable plaintext audio/mp4 (or itag 18) url");
            }
        };

        let url = format.url.clone().context("selected format missing url")?;
        if !url.starts_with("http") {
            bail!("format url is not http(s): {url}");
        }
        // #region agent log
        crate::debug_log::log(
            "C",
            "player/resolver.rs:resolve_with_client",
            "format selected",
            serde_json::json!({
                "video_id": video_id,
                "client": client.name,
                "itag": format.itag,
                "mime": format.mime_type,
                "adaptive_n": adaptive_n,
                "formats_n": formats_n,
                "had_cookies": cookies.is_some(),
            }),
        );
        // #endregion

        let filesize = parse_u64_value(format.content_length.as_ref()).filter(|&n| n > 0);
        let duration =
            parse_u64_value(format.approx_duration_ms.as_ref()).map(|ms| ms as f64 / 1000.0);

        let expires_in = streaming
            .expires_in_seconds
            .as_deref()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|&s| s > 60)
            .unwrap_or(5 * 3600);
        let expires_at = Instant::now() + Duration::from_secs(expires_in.saturating_sub(60));

        let ext = format_ext(format);

        let mut headers = HashMap::new();
        headers.insert("User-Agent".into(), client.user_agent.to_string());
        if !visitor_for_cdn.is_empty() {
            headers.insert("X-Goog-Visitor-Id".into(), visitor_for_cdn);
        }
        if client.web_headers {
            headers.insert("Origin".into(), "https://www.youtube.com".into());
            headers.insert("Referer".into(), "https://www.youtube.com/".into());
        }

        Ok(ResolvedStream {
            url,
            headers,
            filesize,
            duration,
            ext: ext.into(),
            resolved_at: Instant::now(),
            expires_at,
            client_name: client.name.to_string(),
            itag: format.itag,
        })
    }

    /// Signed-in YouTube Music web request; deciphers the chosen format's URL.
    /// Also reports whether the Premium itag was offered (a Premium account).
    async fn resolve_web_remix(&self, video_id: &str) -> Result<(ResolvedStream, bool)> {
        let cookies = self
            .cookies
            .lock()
            .await
            .clone()
            .context("not signed in")?;
        let sap = manual::sapisid_from_cookies(&cookies).context("no SAPISID cookie")?;
        let player = self.jsc.player().await?;

        let version = format!("1.{}.01.00", chrono::Utc::now().format("%Y%m%d"));
        let body = serde_json::json!({
            "context": { "client": {
                "clientName": WEB_REMIX_NAME,
                "clientVersion": version,
                "hl": "en",
                "gl": "US",
                "userAgent": WEB_REMIX_UA,
            }},
            "videoId": video_id,
            "contentCheckOk": true,
            "racyCheckOk": true,
            "playbackContext": { "contentPlaybackContext": {
                "signatureTimestamp": player.sts,
                "html5Preference": "HTML5_PREF_WANTS",
            }},
        });
        let auth = sapisid::authorization_header_parts(
            &sap,
            manual::papisid_1p_from_cookies(&cookies).as_deref(),
            manual::papisid_3p_from_cookies(&cookies).as_deref(),
            None,
        );
        let resp = self
            .http
            .post(WEB_REMIX_PLAYER_URL)
            .header("Content-Type", "application/json")
            .header("User-Agent", WEB_REMIX_UA)
            .header("X-Youtube-Client-Name", WEB_REMIX_ID)
            .header("X-Youtube-Client-Version", &version)
            .header("Origin", MUSIC_ORIGIN)
            .header("X-Origin", MUSIC_ORIGIN)
            .header("Referer", format!("{MUSIC_ORIGIN}/"))
            .header("Cookie", &cookies)
            .header("Authorization", auth)
            .header("X-Goog-AuthUser", "0")
            .json(&body)
            .send()
            .await
            .context("player WEB_REMIX request")?;
        let status = resp.status();
        let text = resp.text().await.context("player response body")?;
        if !status.is_success() {
            bail!("player WEB_REMIX HTTP {status}: {}", truncate(&text, 200));
        }
        let parsed: PlayerResponse = serde_json::from_str(&text).context("parsing player JSON")?;

        let playability = parsed.playability.as_ref();
        let status_str = playability
            .and_then(|p| p.status.as_deref())
            .unwrap_or("UNKNOWN");
        if status_str != "OK" {
            let reason = playability
                .and_then(|p| p.reason.as_deref())
                .unwrap_or(status_str);
            // #region agent log
            crate::debug_log::log(
                "B",
                "player/resolver.rs:resolve_web_remix",
                "playability not OK",
                serde_json::json!({
                    "video_id": video_id,
                    "client": WEB_REMIX_NAME,
                    "status": status_str,
                    "reason": reason,
                    "had_cookies": true,
                }),
            );
            // #endregion
            bail!("{status_str}: {reason}");
        }

        let streaming = parsed
            .streaming
            .as_ref()
            .context("player response missing streamingData")?;
        let format = select_remix_format(&streaming.adaptive_formats)
            .context("no playable audio format in WEB_REMIX response")?;
        let premium_offered = matches!(format.itag, Some(PREMIUM_OPUS_ITAG | PREMIUM_ITAG));

        let (base_url, cipher) = match (&format.url, &format.signature_cipher) {
            (Some(url), _) => (url.clone(), None),
            (None, Some(sc)) => {
                let fields: HashMap<String, String> =
                    url::form_urlencoded::parse(sc.as_bytes()).into_owned().collect();
                let url = fields.get("url").cloned().context("signatureCipher without url")?;
                let s = fields.get("s").cloned().context("signatureCipher without s")?;
                let sp = fields.get("sp").cloned().unwrap_or_else(|| "signature".into());
                (url, Some((s, sp)))
            }
            (None, None) => bail!("format has neither url nor signatureCipher"),
        };
        let mut url = url::Url::parse(&base_url).context("format url")?;
        let n = url
            .query_pairs()
            .find(|(k, _)| k == "n")
            .map(|(_, v)| v.into_owned());

        let solved = self
            .jsc
            .solve(
                &player,
                cipher.iter().map(|(s, _)| s.clone()).collect(),
                n.iter().cloned().collect(),
            )
            .await;
        let solved = match solved {
            Ok(s) => s,
            Err(e) => {
                // The player may have rotated under us; re-check next time.
                self.jsc.invalidate().await;
                return Err(e.context("deciphering WEB_REMIX url"));
            }
        };

        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .map(|(k, v)| {
                let v = if k == "n" {
                    solved.n.get(v.as_ref()).cloned().unwrap_or_else(|| v.into_owned())
                } else {
                    v.into_owned()
                };
                (k.into_owned(), v)
            })
            .collect();
        {
            let mut q = url.query_pairs_mut();
            q.clear();
            q.extend_pairs(&pairs);
            if let Some((s, sp)) = &cipher {
                let sig = solved.sig.get(s).context("signature not solved")?;
                q.append_pair(sp, sig);
            }
        }

        // #region agent log
        crate::debug_log::log(
            "C",
            "player/resolver.rs:resolve_web_remix",
            "format selected",
            serde_json::json!({
                "video_id": video_id,
                "client": WEB_REMIX_NAME,
                "itag": format.itag,
                "player": player.id,
                "ciphered": cipher.is_some(),
            }),
        );
        // #endregion

        let expires_in = streaming
            .expires_in_seconds
            .as_deref()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|&s| s > 60)
            .unwrap_or(5 * 3600);
        let mut headers = HashMap::new();
        headers.insert("User-Agent".into(), WEB_REMIX_UA.to_string());
        headers.insert("Origin".into(), MUSIC_ORIGIN.to_string());
        headers.insert("Referer".into(), format!("{MUSIC_ORIGIN}/"));

        let stream = ResolvedStream {
            url: url.into(),
            headers,
            filesize: parse_u64_value(format.content_length.as_ref()).filter(|&n| n > 0),
            duration: parse_u64_value(format.approx_duration_ms.as_ref())
                .map(|ms| ms as f64 / 1000.0),
            ext: format_ext(format).into(),
            resolved_at: Instant::now(),
            expires_at: Instant::now() + Duration::from_secs(expires_in.saturating_sub(60)),
            client_name: WEB_REMIX_NAME.to_string(),
            itag: format.itag,
        };
        Ok((stream, premium_offered))
    }

    async fn ensure_visitor(&self, client: PlayerClient) -> Result<String> {
        {
            let slot = self.visitor_data.lock().await;
            if let Some(ref v) = *slot {
                return Ok(v.clone());
            }
        }

        match self.fetch_visitor(client).await {
            Ok(v) => {
                *self.visitor_data.lock().await = Some(v.clone());
                Ok(v)
            }
            Err(e) => {
                warn!("visitor_id fetch failed ({e}); continuing without");
                Ok(String::new())
            }
        }
    }

    async fn fetch_visitor(&self, client: PlayerClient) -> Result<String> {
        let body = serde_json::json!({
            "context": client_context(client, "")
        });
        let resp = self
            .http
            .post(VISITOR_URL)
            .header("Content-Type", "application/json")
            .header("User-Agent", client.user_agent)
            .header("X-Youtube-Client-Name", client.client_id)
            .header("X-Youtube-Client-Version", client.version)
            .json(&body)
            .send()
            .await
            .context("visitor_id request")?;
        let text = resp.text().await.context("visitor_id body")?;
        let json: serde_json::Value = serde_json::from_str(&text).context("visitor_id JSON")?;
        json.pointer("/responseContext/visitorData")
            .or_else(|| json.get("visitorData"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .context("visitorData missing from visitor_id response")
    }
}

fn player_body(video_id: &str, client: PlayerClient, visitor: &str) -> serde_json::Value {
    let mut body = serde_json::json!({
        "context": client_context(client, visitor),
        "videoId": video_id,
        "contentCheckOk": true,
        "racyCheckOk": true,
        "playbackContext": {
            "contentPlaybackContext": {
                "html5Preference": "HTML5_PREF_WANTS",
                // Empty is fine; yt-dlp includes the field for web_embedded always.
                "encryptedHostFlags": ""
            }
        }
    });
    // WEB_EMBEDDED_PLAYER uses Innertube name WEB_EMBEDDED_PLAYER, not WEB_EMBEDDED.
    if client.name == "WEB_EMBEDDED" {
        if let Some(ctx) = body.get_mut("context").and_then(|c| c.as_object_mut()) {
            if let Some(client_obj) = ctx.get_mut("client").and_then(|c| c.as_object_mut()) {
                client_obj.insert(
                    "clientName".into(),
                    serde_json::Value::String("WEB_EMBEDDED_PLAYER".into()),
                );
            }
        }
    }
    body
}

fn client_context(client: PlayerClient, visitor: &str) -> serde_json::Value {
    let mut client_obj = match client.name {
        "WEB_EMBEDDED" => serde_json::json!({
            "clientName": "WEB_EMBEDDED_PLAYER",
            "clientVersion": client.version,
            "hl": "en",
            "gl": "US",
            "userAgent": client.user_agent
        }),
        "VISIONOS" => serde_json::json!({
            "clientName": "VISIONOS",
            "clientVersion": client.version,
            "hl": "en",
            "gl": "US",
            "deviceMake": "Apple",
            "deviceModel": "RealityDevice17,1",
            "osName": "visionOS",
            "osVersion": "26.5.23O471",
            "userAgent": client.user_agent
        }),
        // tv_downgraded — no Samsung device fields; Cobalt UA matches yt-dlp.
        _ => serde_json::json!({
            "clientName": "TVHTML5",
            "clientVersion": client.version,
            "hl": "en",
            "gl": "US",
            "userAgent": client.user_agent
        }),
    };

    if !visitor.is_empty() {
        if let Some(obj) = client_obj.as_object_mut() {
            obj.insert(
                "visitorData".into(),
                serde_json::Value::String(visitor.into()),
            );
        }
    }

    let mut ctx = serde_json::json!({ "client": client_obj });
    if client.name == "WEB_EMBEDDED" {
        if let Some(obj) = ctx.as_object_mut() {
            obj.insert(
                "thirdParty".into(),
                serde_json::json!({
                    "embedUrl": "https://www.reddit.com/"
                }),
            );
        }
    }
    ctx
}

/// Audio we can decode: AAC in mp4, Opus in WebM.
fn is_playable_audio(f: &StreamFormat) -> bool {
    let mime = f.mime_type.as_deref().unwrap_or("");
    mime.starts_with("audio/mp4") || (mime.starts_with("audio/webm") && mime.contains("opus"))
}

/// The first `preference` itag on offer, else the highest-bitrate audio.
fn pick_audio<'a>(
    candidates: impl Iterator<Item = &'a StreamFormat>,
    preference: &[i64],
) -> Option<&'a StreamFormat> {
    let audio: Vec<&StreamFormat> = candidates.filter(|f| is_playable_audio(f)).collect();
    preference
        .iter()
        .find_map(|itag| audio.iter().find(|f| f.itag == Some(*itag)).copied())
        .or_else(|| audio.iter().max_by_key(|f| f.bitrate.unwrap_or(0)).copied())
}

/// Best plaintext audio (see [`PLAIN_PREFERENCE`]), else progressive itag 18.
fn select_format<'a>(
    adaptive: &'a [StreamFormat],
    progressive: &'a [StreamFormat],
) -> Option<&'a StreamFormat> {
    if let Some(f) = pick_audio(adaptive.iter().filter(|f| has_http_url(f)), PLAIN_PREFERENCE) {
        return Some(f);
    }
    progressive.iter().find(|f| {
        f.itag == Some(PROGRESSIVE_ITAG)
            && has_http_url(f)
            && f.mime_type
                .as_deref()
                .map(|m| m.starts_with("video/mp4"))
                .unwrap_or(false)
    })
}

/// Best YouTube Music audio (see [`REMIX_PREFERENCE`]); URLs may be ciphered.
fn select_remix_format(adaptive: &[StreamFormat]) -> Option<&StreamFormat> {
    pick_audio(
        adaptive.iter().filter(|f| has_http_url(f) || f.signature_cipher.is_some()),
        REMIX_PREFERENCE,
    )
}

fn has_http_url(f: &StreamFormat) -> bool {
    f.url
        .as_ref()
        .map(|u| u.starts_with("http"))
        .unwrap_or(false)
}

fn parse_u64_value(v: Option<&serde_json::Value>) -> Option<u64> {
    match v? {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..max]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_fixture(name: &str) -> PlayerResponse {
        let path = format!(
            "{}/tests/fixtures/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        );
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {path}: {e}"))
    }

    #[test]
    fn classifies_permanent_refusals() {
        let e = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            unplayable_reason(&e(&[
                "UNPLAYABLE: This video is only available to Music Premium members",
                "ERROR: This video is unavailable",
            ]), true),
            Some("requires Music Premium")
        );
        assert_eq!(
            unplayable_reason(&e(&[
                "LOGIN_REQUIRED: Sign in to confirm your age",
                "ERROR: This video is unavailable",
            ]), false),
            Some("age-restricted, sign in to play")
        );
        assert_eq!(
            unplayable_reason(&e(&[
                "UNPLAYABLE: This video is not available",
                "ERROR: This video is unavailable",
            ]), true),
            Some("not available on YouTube")
        );
        assert_eq!(
            unplayable_reason(&e(&[
                "LOGIN_REQUIRED: Sign in to confirm you’re not a bot",
                "ERROR: This video is unavailable",
            ]), true),
            None
        );
    }

    /// Network + a real Premium session: `YT_TUI_AUTH_JSON=<auth.json> cargo test live_ -- --ignored`.
    /// Signed out, a normal track (3QY9AzTHoio) takes the plain path. Signed in, it and
    /// Premium-only (QpXD1Hfvy28), age-gated (4avspFPZOWs) and Music-only (CT_J9G6LHfQ)
    /// tracks resolve via WEB_REMIX at 256 kbps; a plain upload (jNQXAC9IVRw) isn't on
    /// Music and falls back to the plain path. All must download, and a cached
    /// AAC 128 file upgrades to the Premium Opus rendition where YouTube has it.
    #[tokio::test]
    #[ignore]
    async fn live_signed_in_restricted_tracks() {
        let Ok(path) = std::env::var("YT_TUI_AUTH_JSON") else {
            return;
        };
        let auth: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let cookies = auth["cookies"].as_str().unwrap().to_string();
        let resolver = Resolver::new(std::env::temp_dir().join("yt-tui-jsc-test"));

        // Signed out first: restrictions apply with a clear reason.
        let stream = resolver.resolve("3QY9AzTHoio").await.unwrap();
        assert_eq!(stream.client_name, "VISIONOS");
        let err = resolver.resolve("QpXD1Hfvy28").await.unwrap_err();
        assert_eq!(err.to_string(), "requires Music Premium");
        let err = resolver.resolve("4avspFPZOWs").await.unwrap_err();
        assert_eq!(err.to_string(), "age-restricted, sign in to play");

        resolver.set_cookies(Some(cookies)).await;
        for (vid, client) in [
            ("3QY9AzTHoio", WEB_REMIX_NAME),
            ("QpXD1Hfvy28", WEB_REMIX_NAME),
            ("4avspFPZOWs", WEB_REMIX_NAME),
            ("CT_J9G6LHfQ", WEB_REMIX_NAME),
            ("jNQXAC9IVRw", "VISIONOS"),
        ] {
            let stream = resolver.resolve(vid).await.unwrap();
            assert_eq!(stream.client_name, client, "{vid}");
            if client == WEB_REMIX_NAME {
                assert!(matches!(stream.itag, Some(PREMIUM_OPUS_ITAG | PREMIUM_ITAG)), "{vid} not Premium");
            }
            let mut req = reqwest::Client::new()
                .get(&stream.url)
                .header("Range", "bytes=0-65535");
            for (k, v) in &stream.headers {
                req = req.header(k, v);
            }
            let resp = req.send().await.unwrap();
            assert_eq!(resp.status().as_u16(), 206, "{vid} via {client}");
            assert_eq!(resp.bytes().await.unwrap().len(), 65536);
        }
        assert_eq!(*resolver.premium.borrow(), Premium::Yes);

        // Cache upgrades: official releases have a Premium rendition to swap in; user
        // uploads (Mq0aagbZgUM) and plain uploads top out at Opus ~150 kbps.
        let better = resolver.best_stream("3QY9AzTHoio").await.unwrap().expect("stream");
        assert_eq!(better.quality(), Some(("Opus", 256)));
        for ugc in ["Mq0aagbZgUM", "jNQXAC9IVRw"] {
            let best = resolver.best_stream(ugc).await.unwrap().expect("stream");
            assert_eq!(best.quality(), Some(("Opus", 150)), "{ugc}");
        }
        let dir = std::env::temp_dir().join(format!("yt-tui-live-upgrade-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dl = crate::player::download::Downloader::new(dir.clone(), 100);
        std::fs::write(dl.cached_path("3QY9AzTHoio"), b"128 kbps stand-in").unwrap();
        dl.upgrade("3QY9AzTHoio", &better, None).await.unwrap();
        // m4a stand-in replaced by the WebM file.
        let cached = dl.cached_file("3QY9AzTHoio").unwrap();
        assert_eq!(cached.extension().unwrap(), "webm");
        assert_eq!(Some(std::fs::metadata(&cached).unwrap().len()), better.filesize);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn premium_detection() {
        let free = (0..PREMIUM_MISSES).fold(Premium::UNKNOWN, |p, _| p.observe(false));
        assert_eq!(free, Premium::No);
        // A single track without 256 kbps doesn't settle it, nor demote Premium.
        assert_eq!(Premium::UNKNOWN.observe(false), Premium::Unknown { misses: 1 });
        assert_eq!(Premium::UNKNOWN.observe(false).observe(true), Premium::Yes);
        assert_eq!(Premium::Yes.observe(false), Premium::Yes);
    }

    #[test]
    fn remix_format_prefers_premium_then_140() {
        let f = |itag: i64, bitrate: u64, cipher: bool| StreamFormat {
            itag: Some(itag),
            url: (!cipher).then(|| "https://x/".into()),
            mime_type: Some("audio/mp4; codecs=\"mp4a.40.2\"".into()),
            bitrate: Some(bitrate),
            content_length: None,
            approx_duration_ms: None,
            signature_cipher: cipher.then(|| "s=abc&sp=sig&url=https%3A%2F%2Fx%2F".into()),
        };
        let all = [f(140, 130_000, true), f(141, 260_000, true)];
        assert_eq!(select_remix_format(&all).unwrap().itag, Some(141));
        let free = [f(139, 50_000, true), f(140, 130_000, true)];
        assert_eq!(select_remix_format(&free).unwrap().itag, Some(140));

        // Opus in WebM: Premium 774 first, and 251 beats AAC 128.
        let opus = |itag: i64, bitrate: u64| StreamFormat {
            mime_type: Some("audio/webm; codecs=\"opus\"".into()),
            ..f(itag, bitrate, true)
        };
        let premium = [f(140, 130_000, true), f(141, 260_000, true), opus(251, 150_000), opus(774, 270_000)];
        let best = select_remix_format(&premium).unwrap();
        assert_eq!((best.itag, format_ext(best)), (Some(774), "webm"));
        let free = [f(140, 130_000, true), opus(250, 70_000), opus(251, 150_000)];
        assert_eq!(select_remix_format(&free).unwrap().itag, Some(251));
        assert_eq!(itag_quality(774), Some(("Opus", 256)));
    }

    #[test]
    fn prefers_opus_251() {
        let resp = load_fixture("player_ok_itag140.json");
        let streaming = resp.streaming.unwrap();
        let f = select_format(&streaming.adaptive_formats, &streaming.formats).expect("format");
        assert_eq!(f.itag, Some(251));
        assert_eq!(format_ext(f), "webm");
        assert!(f.url.as_ref().unwrap().starts_with("https://"));
        // Without Opus, AAC 140.
        let aac: Vec<_> = streaming.adaptive_formats.into_iter().filter(|f| f.itag != Some(251)).collect();
        let f = select_format(&aac, &[]).expect("format");
        assert_eq!((f.itag, format_ext(f)), (Some(140), "m4a"));
    }

    #[test]
    fn rejects_cipher_only() {
        let resp = load_fixture("player_cipher_only.json");
        let streaming = resp.streaming.unwrap();
        assert!(select_format(&streaming.adaptive_formats, &streaming.formats).is_none());
    }

    #[test]
    fn bitrate_fallback_without_140() {
        let resp = load_fixture("player_no_140.json");
        let streaming = resp.streaming.unwrap();
        let f = select_format(&streaming.adaptive_formats, &streaming.formats).expect("format");
        assert_eq!(f.itag, Some(251));
        let aac: Vec<_> = streaming.adaptive_formats.into_iter().filter(|f| f.itag != Some(251)).collect();
        let f = select_format(&aac, &[]).expect("format");
        assert_eq!(f.itag, Some(141));
        assert_eq!(f.bitrate, Some(256000));
    }

    #[test]
    fn login_required_status() {
        let resp = load_fixture("player_login_required.json");
        let status = resp.playability.unwrap().status.unwrap();
        assert_eq!(status, "LOGIN_REQUIRED");
    }

    #[test]
    fn prefers_140_over_higher_bitrate() {
        let formats = vec![
            StreamFormat {
                itag: Some(141),
                url: Some("https://googlevideo.com/a".into()),
                mime_type: Some("audio/mp4; codecs=\"mp4a.40.2\"".into()),
                bitrate: Some(256000),
                content_length: None,
                approx_duration_ms: None,
                signature_cipher: None,
            },
            StreamFormat {
                itag: Some(140),
                url: Some("https://googlevideo.com/b".into()),
                mime_type: Some("audio/mp4; codecs=\"mp4a.40.2\"".into()),
                bitrate: Some(128000),
                content_length: None,
                approx_duration_ms: None,
                signature_cipher: None,
            },
        ];
        let f = select_format(&formats, &[]).unwrap();
        assert_eq!(f.itag, Some(140));
    }

    #[test]
    fn last_resort_139() {
        let formats = vec![StreamFormat {
            itag: Some(139),
            url: Some("https://googlevideo.com/c".into()),
            mime_type: Some("audio/mp4; codecs=\"mp4a.40.5\"".into()),
            bitrate: Some(48000),
            content_length: None,
            approx_duration_ms: None,
            signature_cipher: None,
        }];
        let f = select_format(&formats, &[]).unwrap();
        assert_eq!(f.itag, Some(139));
    }

    #[test]
    fn progressive_itag_18_fallback() {
        let progressive = vec![StreamFormat {
            itag: Some(18),
            url: Some("https://googlevideo.com/d".into()),
            mime_type: Some("video/mp4; codecs=\"avc1.42001E, mp4a.40.2\"".into()),
            bitrate: Some(500000),
            content_length: None,
            approx_duration_ms: None,
            signature_cipher: None,
        }];
        let f = select_format(&[], &progressive).unwrap();
        assert_eq!(f.itag, Some(18));
    }

    #[test]
    fn web_embedded_context_has_embed_url() {
        let ctx = client_context(WEB_EMBEDDED, "vis");
        assert_eq!(
            ctx.pointer("/thirdParty/embedUrl")
                .and_then(|v| v.as_str()),
            Some("https://www.reddit.com/")
        );
        assert_eq!(
            ctx.pointer("/client/clientName").and_then(|v| v.as_str()),
            Some("WEB_EMBEDDED_PLAYER")
        );
    }
}
