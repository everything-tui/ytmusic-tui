use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{debug, warn};

// ---------------------------------------------------------------------------
// Client constants — update these together when YouTube breaks mobile /player.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct PlayerClient {
    name: &'static str,
    /// `X-Youtube-Client-Name` numeric id.
    client_id: &'static str,
    version: &'static str,
    user_agent: &'static str,
}

/// ANDROID_VR returns plaintext adaptive URLs without PO tokens / signatureCipher.
const ANDROID_VR: PlayerClient = PlayerClient {
    name: "ANDROID_VR",
    client_id: "28",
    version: "1.65.10",
    user_agent: "com.google.android.apps.youtube.vr.oculus/1.65.10 (Linux; U; Android 12L; Quest 3 Build/SQ3A.220605.009.A1) gzip",
};

const IOS: PlayerClient = PlayerClient {
    name: "IOS",
    client_id: "5",
    version: "20.10.4",
    user_agent: "com.google.ios.youtube/20.10.4 (iPhone16,2; U; CPU iOS 18_3_2 like Mac OS X;)",
};

const ANDROID: PlayerClient = PlayerClient {
    name: "ANDROID",
    client_id: "3",
    version: "20.10.38",
    user_agent: "com.google.android.youtube/20.10.38 (Linux; U; Android 14; en_US) gzip",
};

const CLIENTS: &[PlayerClient] = &[ANDROID_VR, IOS, ANDROID];

const PLAYER_URL: &str = "https://youtubei.googleapis.com/youtubei/v1/player?prettyPrint=false";
const VISITOR_URL: &str = "https://youtubei.googleapis.com/youtubei/v1/visitor_id?prettyPrint=false";

// Prefer itag 140 (AAC-LC 128k). Avoid preferring 139 (HE-AAC) — half-rate SBR
// confuses some decoders into 2× duration with trailing silence.
const PREFERRED_ITAG: i64 = 140;
const LAST_RESORT_ITAG: i64 = 139;

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
    adaptive_formats: Vec<AdaptiveFormat>,
}

#[derive(Debug, Deserialize, Clone)]
struct AdaptiveFormat {
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
    cache: Arc<Mutex<HashMap<String, ResolvedStream>>>,
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
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
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

    async fn resolve_uncached(&self, video_id: &str) -> Result<ResolvedStream> {
        let mut last_err: Option<anyhow::Error> = None;
        let mut retried_visitor = false;

        for &client in CLIENTS {
            match self.resolve_with_client(video_id, client).await {
                Ok(stream) => return Ok(stream),
                Err(e) => {
                    let msg = e.to_string();
                    // Refresh visitor once on LOGIN_REQUIRED, then retry the same client.
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

        debug!("player {} video_id={video_id}", client.name);
        let resp = self
            .http
            .post(PLAYER_URL)
            .header("Content-Type", "application/json")
            .header("User-Agent", client.user_agent)
            .header("X-Youtube-Client-Name", client.client_id)
            .header("X-Youtube-Client-Version", client.version)
            .header("X-Goog-Visitor-Id", &visitor)
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

        // Absorb visitor from response if we didn't have one.
        if let Some(vd) = parsed
            .response_context
            .as_ref()
            .and_then(|c| c.visitor_data.clone())
        {
            let mut slot = self.visitor_data.lock().await;
            if slot.is_none() {
                *slot = Some(vd);
            }
        }

        let playability = parsed.playability.as_ref();
        let status_str = playability
            .and_then(|p| p.status.as_deref())
            .unwrap_or("UNKNOWN");
        if status_str != "OK" {
            let reason = playability
                .and_then(|p| p.reason.as_deref())
                .unwrap_or(status_str);
            bail!("{status_str}: {reason}");
        }

        let streaming = parsed
            .streaming
            .as_ref()
            .context("player response missing streamingData")?;

        let format = select_format(&streaming.adaptive_formats)
            .context("no playable audio/mp4 format with plaintext url")?;

        let url = format.url.clone().context("selected format missing url")?;
        if !url.starts_with("http") {
            bail!("format url is not http(s): {url}");
        }

        let filesize = parse_u64_value(format.content_length.as_ref()).filter(|&n| n > 0);
        let duration =
            parse_u64_value(format.approx_duration_ms.as_ref()).map(|ms| ms as f64 / 1000.0);

        let expires_in = streaming
            .expires_in_seconds
            .as_deref()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|&s| s > 60)
            .unwrap_or(5 * 3600);
        // Trim 60s so we re-resolve before the CDN rejects the URL.
        let expires_at = Instant::now() + Duration::from_secs(expires_in.saturating_sub(60));

        let mut headers = HashMap::new();
        headers.insert("User-Agent".into(), client.user_agent.to_string());

        Ok(ResolvedStream {
            url,
            headers,
            filesize,
            duration,
            ext: "m4a".into(),
            resolved_at: Instant::now(),
            expires_at,
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
                // Empty visitor still works for some clients; player response may fill it.
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
    serde_json::json!({
        "context": client_context(client, visitor),
        "videoId": video_id,
        "contentCheckOk": true,
        "racyCheckOk": true
    })
}

fn client_context(client: PlayerClient, visitor: &str) -> serde_json::Value {
    let mut c = match client.name {
        "ANDROID_VR" => serde_json::json!({
            "clientName": client.name,
            "clientVersion": client.version,
            "hl": "en",
            "gl": "US",
            "deviceMake": "Oculus",
            "deviceModel": "Quest 3",
            "androidSdkVersion": 32,
            "osName": "Android",
            "osVersion": "12L",
            "platform": "MOBILE"
        }),
        "IOS" => serde_json::json!({
            "clientName": client.name,
            "clientVersion": client.version,
            "hl": "en",
            "gl": "US",
            "deviceMake": "Apple",
            "deviceModel": "iPhone16,2",
            "osName": "iPhone",
            "osVersion": "18.3.2",
            "platform": "MOBILE"
        }),
        _ => serde_json::json!({
            "clientName": client.name,
            "clientVersion": client.version,
            "hl": "en",
            "gl": "US",
            "androidSdkVersion": 34,
            "osName": "Android",
            "osVersion": "14",
            "platform": "MOBILE"
        }),
    };
    if !visitor.is_empty() {
        if let Some(obj) = c.as_object_mut() {
            obj.insert(
                "visitorData".into(),
                serde_json::Value::String(visitor.into()),
            );
        }
    }
    serde_json::json!({ "client": c })
}

/// Pick itag 140, else best non-139 audio/mp4 with plaintext url, else 139.
fn select_format(formats: &[AdaptiveFormat]) -> Option<&AdaptiveFormat> {
    let mut best: Option<&AdaptiveFormat> = None;
    let mut fallback_139: Option<&AdaptiveFormat> = None;

    for f in formats {
        let has_url = f
            .url
            .as_ref()
            .map(|u| u.starts_with("http"))
            .unwrap_or(false);
        if !has_url {
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
    best.or(fallback_139)
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
        let formats = &resp.streaming.unwrap().adaptive_formats;
        let f = select_format(formats).expect("format");
        assert_eq!(f.itag, Some(140));
        assert!(f.url.as_ref().unwrap().starts_with("https://"));
    }

    #[test]
    fn rejects_cipher_only() {
        let resp = load_fixture("player_cipher_only.json");
        let formats = &resp.streaming.unwrap().adaptive_formats;
        assert!(select_format(formats).is_none());
    }

    #[test]
    fn bitrate_fallback_without_140() {
        let resp = load_fixture("player_no_140.json");
        let formats = &resp.streaming.unwrap().adaptive_formats;
        let f = select_format(formats).expect("format");
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
            AdaptiveFormat {
                itag: Some(141),
                url: Some("https://googlevideo.com/a".into()),
                mime_type: Some("audio/mp4; codecs=\"mp4a.40.2\"".into()),
                bitrate: Some(256000),
                content_length: None,
                approx_duration_ms: None,
            },
            AdaptiveFormat {
                itag: Some(140),
                url: Some("https://googlevideo.com/b".into()),
                mime_type: Some("audio/mp4; codecs=\"mp4a.40.2\"".into()),
                bitrate: Some(128000),
                content_length: None,
                approx_duration_ms: None,
            },
        ];
        let f = select_format(&formats).unwrap();
        assert_eq!(f.itag, Some(140));
    }

    #[test]
    fn last_resort_139() {
        let formats = vec![AdaptiveFormat {
            itag: Some(139),
            url: Some("https://googlevideo.com/c".into()),
            mime_type: Some("audio/mp4; codecs=\"mp4a.40.5\"".into()),
            bitrate: Some(48000),
            content_length: None,
            approx_duration_ms: None,
        }];
        let f = select_format(&formats).unwrap();
        assert_eq!(f.itag, Some(139));
    }
}
