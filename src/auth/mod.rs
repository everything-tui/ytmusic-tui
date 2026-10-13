pub mod browser;
pub mod manual;
pub mod sapisid;

use crate::action::{AppEvent, AuthStateEvent};
use crate::api::model::Account;
use crate::paths::Paths;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthSession {
    pub cookies: String,
    pub browser: Option<String>,
    pub profile: Option<String>,
    pub account_name: Option<String>,
    /// From ytcfg `VISITOR_DATA` / `X-Goog-Visitor-Id` — required by modern Music clients.
    #[serde(default)]
    pub visitor_id: Option<String>,
    /// From ytcfg `DATASYNC_ID` — enables `SAPISIDHASH …_u` auth used by Firefox/Chrome.
    #[serde(default)]
    pub user_session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthState {
    Unknown,
    Guest,
    SignedIn,
    WaitingForBrowser,
}

pub struct AuthService {
    paths: Arc<Paths>,
    session: Option<AuthSession>,
}

impl AuthService {
    pub fn new(paths: Arc<Paths>) -> Self {
        let session = Self::load_session(&paths).ok().flatten();
        Self { paths, session }
    }

    pub fn session(&self) -> Option<&AuthSession> {
        self.session.as_ref()
    }

    pub fn cookie_header(&self) -> Option<&str> {
        self.session.as_ref().map(|s| s.cookies.as_str())
    }

    pub fn is_signed_in(&self) -> bool {
        self.session.is_some()
    }

    fn load_session(paths: &Paths) -> Result<Option<AuthSession>> {
        // Try keyring first
        if let Ok(entry) = keyring::Entry::new("yt-tui", "cookies") {
            if let Ok(json) = entry.get_password() {
                if let Ok(session) = serde_json::from_str::<AuthSession>(&json) {
                    return Ok(Some(session));
                }
            }
        }
        // File fallback
        let path = paths.auth_file();
        if path.exists() {
            let text = fs::read_to_string(&path)?;
            let session: AuthSession = serde_json::from_str(&text)?;
            return Ok(Some(session));
        }
        Ok(None)
    }

    pub fn save_session(&mut self, session: AuthSession) -> Result<()> {
        let json = serde_json::to_string(&session)?;
        let mut saved = false;
        if let Ok(entry) = keyring::Entry::new("yt-tui", "cookies") {
            if entry.set_password(&json).is_ok() {
                saved = true;
            }
        }
        if !saved {
            let path = self.paths.auth_file();
            fs::write(&path, &json)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
            }
        }
        self.session = Some(session);
        Ok(())
    }

    pub fn logout(&mut self) -> Result<()> {
        if let Ok(entry) = keyring::Entry::new("yt-tui", "cookies") {
            let _ = entry.delete_credential();
        }
        let path = self.paths.auth_file();
        if path.exists() {
            fs::remove_file(&path)?;
        }
        crate::paths::cleanup_legacy(&self.paths);
        self.session = None;
        Ok(())
    }

    /// Refresh cookies from the remembered browser if possible.
    pub fn silent_refresh(&mut self) -> Result<bool> {
        let browser = self
            .session
            .as_ref()
            .and_then(|s| s.browser.clone());
        let found = browser::read_youtube_cookies();
        let match_c = if let Some(ref b) = browser {
            found.into_iter().find(|c| &c.browser == b)
        } else {
            found.into_iter().next()
        };
        if let Some(c) = match_c {
            let mut session = self.session.clone().unwrap_or(AuthSession {
                cookies: String::new(),
                browser: Some(c.browser.clone()),
                profile: c.profile.clone(),
                account_name: None,
                visitor_id: None,
                user_session_id: None,
            });
            session.cookies = c.cookie_header;
            session.browser = Some(c.browser);
            self.save_session(session)?;
            return Ok(true);
        }
        Ok(false)
    }
}

/// Poll browsers for a valid YouTube Music session.
pub async fn poll_browser_login(
    paths: Arc<Paths>,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    let _ = tx.send(AppEvent::AuthState(AuthStateEvent::WaitingForBrowser {
        browsers: vec![
            "brave".into(),
            "chrome".into(),
            "firefox".into(),
            "safari".into(),
        ],
    }));
    let _ = tx.send(AppEvent::AuthState(AuthStateEvent::Progress(
        "Opening music.youtube.com…\n  Log in there, then return here.\n  \
         If macOS asks for Keychain access, click Always Allow."
            .into(),
    )));

    if let Err(e) = browser::open_music_youtube() {
        let _ = tx.send(AppEvent::AuthState(AuthStateEvent::Failed(format!(
            "Could not open browser: {e}"
        ))));
        return;
    }

    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(300);
    let mut last_status = String::new();
    let mut ticks: u32 = 0;
    loop {
        if tokio::time::Instant::now() > deadline {
            let _ = tx.send(AppEvent::AuthState(AuthStateEvent::Failed(
                "Timed out waiting for login (5 minutes).\n  Press P to paste a cookie, or Enter to retry."
                    .into(),
            )));
            return;
        }

        let (found, attempts) = tokio::task::spawn_blocking(browser::read_youtube_cookies_with_status)
            .await
            .unwrap_or_else(|_| (Vec::new(), Vec::new()));

        ticks += 1;
        let summary = browser::summarize_attempts(&attempts);
        let status = format!(
            "Waiting for YouTube Music login… ({ticks})\n{summary}\n\n  \
             Tip: stay logged in, then come back. Press P to paste a cookie instead."
        );
        if status != last_status {
            last_status = status.clone();
            let _ = tx.send(AppEvent::AuthState(AuthStateEvent::Progress(status)));
        }

        if let Some(c) = found.into_iter().next() {
            let _ = tx.send(AppEvent::AuthState(AuthStateEvent::Progress(format!(
                "Found session in {} — validating…",
                c.browser
            ))));
            match validate_and_save(paths.clone(), c.cookie_header, Some(c.browser), c.profile)
                .await
            {
                Ok(session) => {
                    info!("signed in via browser");
                    let _ = tx.send(AppEvent::AuthState(AuthStateEvent::SignedIn(session)));
                    return;
                }
                Err(e) => {
                    warn!("cookie validation failed: {e}");
                    let _ = tx.send(AppEvent::AuthState(AuthStateEvent::Progress(format!(
                        "Found cookies but validation failed: {e}\n  \
                         Make sure you're logged into music.youtube.com, then wait…"
                    ))));
                }
            }
        }

        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    }
}

pub async fn validate_and_save(
    paths: Arc<Paths>,
    cookies: String,
    browser: Option<String>,
    profile: Option<String>,
) -> Result<AuthSession> {
    validate_and_save_with_visitor(paths, cookies, browser, profile, None).await
}

pub async fn validate_and_save_with_visitor(
    paths: Arc<Paths>,
    cookies: String,
    browser: Option<String>,
    profile: Option<String>,
    visitor_hint: Option<String>,
) -> Result<AuthSession> {
    let (account, visitor_id, user_session_id) =
        validate_cookies_full(&cookies, visitor_hint).await?;
    let session = AuthSession {
        cookies,
        browser,
        profile,
        account_name: Some(account.name.clone()),
        visitor_id,
        user_session_id,
    };
    let mut svc = AuthService::new(paths);
    svc.save_session(session.clone())?;
    Ok(session)
}

pub async fn validate_cookies(cookies: &str) -> Result<Account> {
    let (account, _, _) = validate_cookies_full(cookies, None).await?;
    Ok(account)
}

/// Validate cookies and collect visitor / user-session ids used by modern Music auth.
pub async fn validate_cookies_full(
    cookies: &str,
    visitor_hint: Option<String>,
) -> Result<(Account, Option<String>, Option<String>)> {
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(std::time::Duration::from_secs(20))
        .build()?;

    let (mut visitor_id, mut user_session_id) =
        fetch_ytcfg_ids(&client, cookies).await.unwrap_or((None, None));
    if visitor_id.is_none() {
        visitor_id = visitor_hint;
    }

    let sapisid = manual::sapisid_from_cookies(cookies)
        .context("missing SAPISID / __Secure-3PAPISID in cookies")?;
    let auth = sapisid::authorization_header_parts(
        &sapisid,
        manual::papisid_1p_from_cookies(cookies).as_deref(),
        manual::papisid_3p_from_cookies(cookies).as_deref(),
        user_session_id.as_deref(),
    );

    let client_version = format!("1.{}.01.00", chrono::Utc::now().format("%Y%m%d"));
    let body = serde_json::json!({
        "context": {
            "client": {
                "clientName": "WEB_REMIX",
                "clientVersion": client_version,
                "hl": "en",
                "gl": "US"
            }
        }
    });

    let mut req = client
        .post("https://music.youtube.com/youtubei/v1/account/account_menu?prettyPrint=false")
        .header("Cookie", cookies)
        .header("Authorization", auth)
        .header("X-Origin", "https://music.youtube.com")
        .header("Origin", "https://music.youtube.com")
        .header("Referer", "https://music.youtube.com/")
        .header("X-Goog-AuthUser", "0")
        .header("X-Youtube-Client-Name", "67")
        .header("X-Youtube-Client-Version", &client_version)
        .json(&body);
    if let Some(ref v) = visitor_id {
        req = req.header("X-Goog-Visitor-Id", v);
    }

    let resp = req.send().await?;

    if !resp.status().is_success() {
        anyhow::bail!("auth validation HTTP {}", resp.status());
    }

    let json: serde_json::Value = resp.json().await?;
    if logged_in_from_response(&json) == Some(false) {
        anyhow::bail!(
            "YouTube says this session is signed out (logged_in=0). \
             Re-sign in at music.youtube.com, then press L here (or paste a fresh cookie)."
        );
    }
    let name = extract_account_name(&json).context(
        "Could not read account name — cookies look incomplete or expired. \
         Re-sign in at music.youtube.com, then press L (or paste a fresh cookie).",
    )?;

    // Prefer ids from the authenticated response when present.
    if let Some(v) = json
        .pointer("/responseContext/visitorData")
        .and_then(|v| v.as_str())
    {
        visitor_id = Some(v.to_string());
    }
    if user_session_id.is_none() {
        if let Some(ds) = find_string_by_key(&json, "datasyncId")
            .or_else(|| find_string_by_key(&json, "dataSyncId"))
        {
            user_session_id = user_session_id_from_datasync(&ds);
        }
    }

    Ok((
        Account {
            name,
            channel_id: None,
            thumbnails: vec![],
        },
        visitor_id,
        user_session_id,
    ))
}

/// Pull `VISITOR_DATA` + `DATASYNC_ID` from the Music homepage ytcfg blob.
pub async fn fetch_ytcfg_ids(
    client: &reqwest::Client,
    cookies: &str,
) -> Result<(Option<String>, Option<String>)> {
    let resp = client
        .get("https://music.youtube.com/")
        .header("Cookie", cookies)
        .header("Referer", "https://music.youtube.com/")
        .send()
        .await?;
    let html = resp.text().await?;
    Ok(parse_ytcfg_ids(&html))
}

pub fn parse_ytcfg_ids(html: &str) -> (Option<String>, Option<String>) {
    // ytcfg.set({...}); — pull a few known keys with a light scan.
    let visitor = capture_ytcfg_string(html, "VISITOR_DATA");
    let datasync = capture_ytcfg_string(html, "DATASYNC_ID");
    let user_session = datasync
        .as_deref()
        .and_then(user_session_id_from_datasync);
    (visitor, user_session)
}

fn capture_ytcfg_string(html: &str, key: &str) -> Option<String> {
    // "KEY":"value" or "KEY": "value"
    let patterns = [
        format!("\"{key}\":\""),
        format!("\"{key}\": \""),
        format!("'{key}':'"),
    ];
    for p in patterns {
        if let Some(i) = html.find(&p) {
            let rest = &html[i + p.len()..];
            let end = rest.find(['"', '\'']).unwrap_or(0);
            let val = &rest[..end];
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

pub fn user_session_id_from_datasync(data_sync_id: &str) -> Option<String> {
    // "delegated||user" → user; "user||" → user
    let (first, second) = data_sync_id
        .split_once("||")
        .map(|(a, b)| (a, b))
        .unwrap_or((data_sync_id, ""));
    if !second.is_empty() {
        Some(second.to_string())
    } else if !first.is_empty() {
        Some(first.to_string())
    } else {
        None
    }
}

/// Read `logged_in` / `yt_li` from Innertube `responseContext.serviceTrackingParams`.
pub fn logged_in_from_response(json: &serde_json::Value) -> Option<bool> {
    let params = json.pointer("/responseContext/serviceTrackingParams")?.as_array()?;
    for group in params {
        let Some(arr) = group.get("params").and_then(|v| v.as_array()) else {
            continue;
        };
        for kv in arr {
            let key = kv.get("key").and_then(|v| v.as_str()).unwrap_or("");
            if key == "logged_in" || key == "yt_li" {
                return match kv.get("value").and_then(|v| v.as_str()) {
                    Some("1") => Some(true),
                    Some("0") => Some(false),
                    _ => None,
                };
            }
        }
    }
    None
}

fn extract_account_name(json: &serde_json::Value) -> Option<String> {
    // Walk a few known paths
    json.pointer("/actions/0/openPopupAction/popup/multiPageMenuRenderer/header/activeAccountHeaderRenderer/accountName/runs/0/text")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            // Fallback: any "accountName"
            find_text_by_key(json, "accountName")
        })
}

fn find_text_by_key(v: &serde_json::Value, key: &str) -> Option<String> {
    match v {
        serde_json::Value::Object(map) => {
            if let Some(val) = map.get(key) {
                if let Some(s) = val.as_str() {
                    return Some(s.to_string());
                }
                if let Some(s) = val.pointer("/runs/0/text").and_then(|x| x.as_str()) {
                    return Some(s.to_string());
                }
            }
            for val in map.values() {
                if let Some(s) = find_text_by_key(val, key) {
                    return Some(s);
                }
            }
            None
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                if let Some(s) = find_text_by_key(item, key) {
                    return Some(s);
                }
            }
            None
        }
        _ => None,
    }
}

fn find_string_by_key(v: &serde_json::Value, key: &str) -> Option<String> {
    match v {
        serde_json::Value::Object(map) => {
            if let Some(s) = map.get(key).and_then(|x| x.as_str()) {
                return Some(s.to_string());
            }
            for val in map.values() {
                if let Some(s) = find_string_by_key(val, key) {
                    return Some(s);
                }
            }
            None
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                if let Some(s) = find_string_by_key(item, key) {
                    return Some(s);
                }
            }
            None
        }
        _ => None,
    }
}

pub const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datasync_primary_channel() {
        assert_eq!(
            user_session_id_from_datasync("abc123||").as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn datasync_delegated_channel() {
        assert_eq!(
            user_session_id_from_datasync("delegated||user99").as_deref(),
            Some("user99")
        );
    }
}
