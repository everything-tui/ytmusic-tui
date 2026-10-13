use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{debug, warn};

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

const PLAYER_URL: &str = "https://www.youtube.com/youtubei/v1/player?prettyPrint=false";
const VISITOR_URL: &str = "https://www.youtube.com/youtubei/v1/visitor_id?prettyPrint=false";

const PREFERRED_ITAG: i64 = 140;
const LAST_RESORT_ITAG: i64 = 139;
/// Progressive muxed mp4 (H.264 + AAC) — works without PO on some clients.
const PROGRESSIVE_ITAG: i64 = 18;

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
}

impl ResolvedStream {
    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.expires_at
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
}

impl Resolver {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .gzip(true)
            .timeout(Duration::from_secs(15))
            .build()
            .expect("reqwest client");
        Self {
            http,
            visitor_data: Arc::new(Mutex::new(None)),
            cookies: Arc::new(Mutex::new(None)),
            cache: Arc::new(Mutex::new(HashMap::new())),
            skip_clients: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn set_cookies(&self, cookies: Option<String>) {
        *self.cookies.lock().await = cookies;
        // Auth change invalidates any cookie-bound URLs / visitors.
        self.cache.lock().await.clear();
        self.skip_clients.lock().await.clear();
        *self.visitor_data.lock().await = None;
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

        let result = self.resolve_uncached(video_id).await;
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

    async fn resolve_uncached(&self, video_id: &str) -> Result<ResolvedStream> {
        let mut last_err: Option<anyhow::Error> = None;
        let mut retried_visitor = false;
        let skipped = self
            .skip_clients
            .lock()
            .await
            .get(video_id)
            .cloned()
            .unwrap_or_default();

        for &client in CLIENTS {
            if skipped.iter().any(|c| c == client.name) {
                debug!("skipping {} for {video_id} (prior CDN 403)", client.name);
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
                                last_err = Some(e2);
                                continue;
                            }
                        }
                    }
                    warn!("{} failed for {video_id}: {e}", client.name);
                    last_err = Some(e);
                }
            }
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

        let ext = if format.itag == Some(PROGRESSIVE_ITAG) {
            "mp4"
        } else {
            "m4a"
        };

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
        })
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

/// Prefer itag 140, else best adaptive audio/mp4, else 139, else progressive itag 18.
fn select_format<'a>(
    adaptive: &'a [StreamFormat],
    progressive: &'a [StreamFormat],
) -> Option<&'a StreamFormat> {
    let mut best: Option<&StreamFormat> = None;
    let mut fallback_139: Option<&StreamFormat> = None;

    for f in adaptive {
        if !has_http_url(f) {
            continue;
        }
        let mime = f.mime_type.as_deref().unwrap_or("");
        if !mime.starts_with("audio/mp4") {
            continue;
        }
        match f.itag {
            Some(PREFERRED_ITAG) => return Some(f),
            Some(LAST_RESORT_ITAG) => fallback_139 = Some(f),
            _ => {
                let better = best
                    .map(|b| f.bitrate.unwrap_or(0) > b.bitrate.unwrap_or(0))
                    .unwrap_or(true);
                if better {
                    best = Some(f);
                }
            }
        }
    }
    if let Some(f) = best.or(fallback_139) {
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
    fn selects_itag_140() {
        let resp = load_fixture("player_ok_itag140.json");
        let streaming = resp.streaming.unwrap();
        let f = select_format(&streaming.adaptive_formats, &streaming.formats).expect("format");
        assert_eq!(f.itag, Some(140));
        assert!(f.url.as_ref().unwrap().starts_with("https://"));
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
            },
            StreamFormat {
                itag: Some(140),
                url: Some("https://googlevideo.com/b".into()),
                mime_type: Some("audio/mp4; codecs=\"mp4a.40.2\"".into()),
                bitrate: Some(128000),
                content_length: None,
                approx_duration_ms: None,
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
