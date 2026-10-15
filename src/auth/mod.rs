pub mod manual;
pub mod sapisid;

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
        let path = paths.auth_file();
        if !path.exists() {
            return Ok(None);
        }
        let text = fs::read_to_string(&path)?;
        Ok(Some(serde_json::from_str(&text)?))
    }

    /// Saved in a private (0600) file in the app data directory; the OS keychain is not used,
    /// so no password prompts can ever come from storing or reading the session.
    pub fn save_session(&mut self, session: AuthSession) -> Result<()> {
        let json = serde_json::to_string(&session)?;
        let path = self.paths.auth_file();
        let tmp = path.with_extension("json.tmp");
        {
            use std::io::Write;
            let mut opts = fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts.open(&tmp)?;
            f.write_all(json.as_bytes())?;
        }
        fs::rename(&tmp, &path)?;
        self.session = Some(session);
        Ok(())
    }

    pub fn logout(&mut self) -> Result<()> {
        let path = self.paths.auth_file();
        if path.exists() {
            fs::remove_file(&path)?;
        }
        crate::paths::cleanup_legacy(&self.paths);
        self.session = None;
        Ok(())
    }
}

/// Serialises read-modify-write of the saved session so the keepalive task, API responses and
/// manual logins can never overwrite each other's freshly rotated cookies.
static SESSION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Merges cookie changes (name, value) into the *currently saved* session and returns it.
/// Only the changed cookies are applied, so a stale in-memory jar can never roll back a
/// newer `__Secure-1PSIDTS` that another task already saved.
pub fn apply_cookie_updates(
    paths: Arc<Paths>,
    updates: &[(String, String)],
) -> Result<Option<AuthSession>> {
    if updates.is_empty() {
        return Ok(None);
    }
    let _guard = SESSION_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut svc = AuthService::new(paths);
    let Some(mut session) = svc.session.clone() else {
        return Ok(None);
    };
    session.cookies = manual::merge_cookie_header(&session.cookies, updates);
    svc.save_session(session.clone())?;
    Ok(Some(session))
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
    {
        let _guard = SESSION_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut svc = AuthService::new(paths);
        svc.save_session(session.clone())?;
    }
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

const ROTATE_URL: &str = "https://accounts.google.com/RotateCookies";

/// Asks Google to rotate the short-lived session cookies, exactly as a browser tab does in
/// the background. Returns the cookies Google (re)set; empty when nothing changed.
pub async fn rotate_cookies(cookies: &str) -> Result<Vec<(String, String)>> {
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(std::time::Duration::from_secs(20))
        .build()?;
    let resp = client
        .post(ROTATE_URL)
        .header("Content-Type", "application/json")
        .header("Origin", "https://accounts.google.com")
        .header("Cookie", cookies)
        .body(r#"[000,"-0000000000000000000"]"#)
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("RotateCookies HTTP {}", resp.status());
    }
    let mut updates = Vec::new();
    for v in resp.headers().get_all(reqwest::header::SET_COOKIE) {
        if let Some(pair) = v.to_str().ok().and_then(manual::parse_set_cookie_pair) {
            updates.push(pair);
        }
    }
    Ok(updates)
}

/// Keeps the saved session alive on its own: rotates cookies shortly after launch and then
/// periodically, saving the result and handing it to the API client.
pub async fn keepalive_loop(
    paths: Arc<Paths>,
    api: mpsc::UnboundedSender<crate::action::Command>,
) {
    let mut wait = std::time::Duration::from_secs(5);
    loop {
        tokio::time::sleep(wait).await;
        wait = std::time::Duration::from_secs(10 * 60);
        match rotate_saved_session(paths.clone()).await {
            Ok(Some(session)) => {
                let _ = api.send(crate::action::Command::ApplyAuth(session));
                info!("session cookies rotated");
            }
            Ok(None) => {}
            Err(e) => warn!("cookie rotation failed: {e}"),
        }
    }
}

/// Rotates the saved session's cookies and persists the result. `None` = nothing to do.
async fn rotate_saved_session(paths: Arc<Paths>) -> Result<Option<AuthSession>> {
    let p = paths.clone();
    let Some(session) =
        tokio::task::spawn_blocking(move || AuthService::new(p).session().cloned()).await?
    else {
        return Ok(None);
    };
    let updates = rotate_cookies(&session.cookies).await?;
    tokio::task::spawn_blocking(move || apply_cookie_updates(paths, &updates)).await?
}

/// Outcome of trying to revive the saved session without user input.
pub enum Recovery {
    Refreshed(AuthSession),
    /// Network / server trouble: the session may still be fine, so do not ask for cookies.
    Transient(String),
    /// YouTube definitively treats the session as signed out.
    Rejected(String),
}

/// Whether a validation failure means the session is really dead (as opposed to the
/// network being down or YouTube returning a hiccup).
fn is_definitive_rejection(e: &anyhow::Error) -> bool {
    let m = format!("{e:#}");
    m.contains("signed out") || m.contains("HTTP 401") || m.contains("HTTP 403")
        || m.contains("missing SAPISID")
}

/// Tries to revive the saved session without any user input: rotates its cookies the way a
/// browser would and re-validates.
pub async fn recover_session(paths: Arc<Paths>) -> Recovery {
    let p = paths.clone();
    let session = match tokio::task::spawn_blocking(move || AuthService::new(p).session().cloned())
        .await
    {
        Ok(Some(s)) => s,
        _ => return Recovery::Rejected("no saved session".into()),
    };
    // A failed rotation (e.g. 401) is not fatal: the old cookies may still be accepted.
    let cookies = match rotate_saved_session(paths.clone()).await {
        Ok(Some(updated)) => updated.cookies,
        _ => session.cookies.clone(),
    };
    match validate_and_save_with_visitor(
        paths,
        cookies,
        session.browser,
        session.profile,
        session.visitor_id,
    )
    .await
    {
        Ok(s) => Recovery::Refreshed(s),
        Err(e) if is_definitive_rejection(&e) => Recovery::Rejected(e.to_string()),
        Err(e) => Recovery::Transient(e.to_string()),
    }
}
