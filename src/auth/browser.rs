use crate::auth::manual::{cookie_map, sapisid_from_cookies};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

#[derive(Debug, Clone)]
pub struct BrowserCookies {
    pub browser: String,
    pub profile: Option<String>,
    pub cookie_header: String,
}

#[derive(Debug, Clone)]
pub struct BrowserAttempt {
    pub browser: String,
    pub status: String,
    pub ok: bool,
}

/// Try to read YouTube cookies from installed browsers.
/// Chromium cookie DBs are copied first so a running browser lock does not block us.
pub fn read_youtube_cookies() -> Vec<BrowserCookies> {
    read_youtube_cookies_with_status().0
}

pub fn read_youtube_cookies_with_status() -> (Vec<BrowserCookies>, Vec<BrowserAttempt>) {
    let mut found = Vec::new();
    let mut attempts = Vec::new();
    let domains = Some(vec![
        "youtube.com".into(),
        ".youtube.com".into(),
        "music.youtube.com".into(),
        "google.com".into(),
    ]);

    for target in chromium_targets() {
        let label = match &target.profile {
            Some(p) if p != "Default" => format!("{} ({p})", target.browser),
            _ => target.browser.clone(),
        };
        match read_chromium_copied(&target, domains.clone()) {
            Ok(header) if is_valid_session(&header) => {
                info!("valid YouTube session from {label}");
                attempts.push(BrowserAttempt {
                    browser: label.clone(),
                    status: "session found".into(),
                    ok: true,
                });
                found.push(BrowserCookies {
                    browser: target.browser,
                    profile: target.profile,
                    cookie_header: header,
                });
            }
            Ok(header) if !header.is_empty() => {
                attempts.push(BrowserAttempt {
                    browser: label,
                    status: "opened browser DB, but not logged into YouTube Music yet".into(),
                    ok: false,
                });
                debug!("incomplete session cookies");
                let _ = header;
            }
            Ok(_) => {
                attempts.push(BrowserAttempt {
                    browser: label,
                    status: "no youtube.com cookies — finish login in this browser".into(),
                    ok: false,
                });
            }
            Err(e) => {
                warn!("{label}: {e}");
                attempts.push(BrowserAttempt {
                    browser: label,
                    status: e,
                    ok: false,
                });
            }
        }
    }

    match rookie::firefox(domains.clone()) {
        Ok(cookies) if !cookies.is_empty() => {
            let header = cookie_vec_to_header(&cookies);
            if is_valid_session(&header) {
                attempts.push(BrowserAttempt {
                    browser: "firefox".into(),
                    status: "session found".into(),
                    ok: true,
                });
                found.push(BrowserCookies {
                    browser: "firefox".into(),
                    profile: None,
                    cookie_header: header,
                });
            } else {
                attempts.push(BrowserAttempt {
                    browser: "firefox".into(),
                    status: "cookies present but not a YouTube Music login".into(),
                    ok: false,
                });
            }
        }
        Ok(_) => {}
        Err(e) => {
            let msg = e.to_string();
            if !msg.to_lowercase().contains("can't find")
                && !msg.to_lowercase().contains("not found")
            {
                attempts.push(BrowserAttempt {
                    browser: "firefox".into(),
                    status: msg,
                    ok: false,
                });
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        match rookie::safari(domains) {
            Ok(cookies) if !cookies.is_empty() => {
                let header = cookie_vec_to_header(&cookies);
                if is_valid_session(&header) {
                    attempts.push(BrowserAttempt {
                        browser: "safari".into(),
                        status: "session found".into(),
                        ok: true,
                    });
                    found.push(BrowserCookies {
                        browser: "safari".into(),
                        profile: None,
                        cookie_header: header,
                    });
                }
            }
            Ok(_) => {}
            Err(e) => {
                let msg = e.to_string().to_lowercase();
                if msg.contains("permission")
                    || msg.contains("denied")
                    || msg.contains("operation not permitted")
                {
                    attempts.push(BrowserAttempt {
                        browser: "safari".into(),
                        status: "needs Full Disk Access for your terminal app".into(),
                        ok: false,
                    });
                }
            }
        }
    }

    (found, attempts)
}

struct ChromiumTarget {
    browser: String,
    profile: Option<String>,
    cookies_path: PathBuf,
}

fn chromium_targets() -> Vec<ChromiumTarget> {
    let home = directories::BaseDirs::new()
        .map(|b| b.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    #[cfg(target_os = "macos")]
    let roots: Vec<(&str, PathBuf)> = {
        let support = home.join("Library/Application Support");
        vec![
            ("brave", support.join("BraveSoftware/Brave-Browser")),
            ("chrome", support.join("Google/Chrome")),
            ("chrome", support.join("Google/Chrome Beta")),
            ("chromium", support.join("Chromium")),
            ("edge", support.join("Microsoft Edge")),
            ("vivaldi", support.join("Vivaldi")),
            ("opera", support.join("com.operasoftware.Opera")),
            ("arc", support.join("Arc/User Data")),
        ]
    };

    #[cfg(target_os = "linux")]
    let roots: Vec<(&str, PathBuf)> = {
        let config = home.join(".config");
        vec![
            ("brave", config.join("BraveSoftware/Brave-Browser")),
            ("chrome", config.join("google-chrome")),
            ("chromium", config.join("chromium")),
            ("edge", config.join("microsoft-edge")),
            ("vivaldi", config.join("vivaldi")),
        ]
    };

    #[cfg(target_os = "windows")]
    let roots: Vec<(&str, PathBuf)> = {
        let local = home.join("AppData/Local");
        vec![
            (
                "brave",
                local.join("BraveSoftware/Brave-Browser/User Data"),
            ),
            ("chrome", local.join("Google/Chrome/User Data")),
            ("edge", local.join("Microsoft/Edge/User Data")),
        ]
    };

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let roots: Vec<(&str, PathBuf)> = vec![];

    let mut out = Vec::new();
    for (browser, root) in roots {
        if !root.exists() {
            continue;
        }
        let mut profiles = vec![root.join("Default")];
        if let Ok(rd) = fs::read_dir(&root) {
            for entry in rd.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("Profile") {
                    profiles.push(entry.path());
                }
            }
        }
        for profile_dir in profiles {
            let candidates = [
                profile_dir.join("Network/Cookies"),
                profile_dir.join("Cookies"),
            ];
            for cookies_path in candidates {
                if cookies_path.exists() {
                    let profile = profile_dir
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string());
                    out.push(ChromiumTarget {
                        browser: browser.to_string(),
                        profile,
                        cookies_path,
                    });
                    break;
                }
            }
        }
    }
    out
}

fn read_chromium_copied(
    target: &ChromiumTarget,
    domains: Option<Vec<String>>,
) -> Result<String, String> {
    let tmp = tempfile::tempdir().map_err(|e| format!("temp dir: {e}"))?;
    let dest = tmp.path().join("Cookies");

    copy_cookie_db(&target.cookies_path, &dest)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let src = PathBuf::from(format!(
            "{}{suffix}",
            target.cookies_path.to_string_lossy()
        ));
        if src.exists() {
            let _ = fs::copy(&src, tmp.path().join(format!("Cookies{suffix}")));
        }
    }

    let path_str = dest.to_string_lossy().to_string();
    match rookie::any_browser(&path_str, domains.clone(), None) {
        Ok(cookies) if !cookies.is_empty() => {
            return Ok(cookie_vec_to_header(&cookies));
        }
        Ok(_) => {
            // Empty from copy — try live API as fallback
        }
        Err(e) => {
            debug!("any_browser on copy failed for {}: {e}", target.browser);
        }
    }

    // Live fallback (often fails while browser is open)
    let live = match target.browser.as_str() {
        "brave" => rookie::brave(domains),
        "chrome" => rookie::chrome(domains),
        "chromium" => rookie::chromium(domains),
        "edge" => rookie::edge(domains),
        "vivaldi" => rookie::vivaldi(domains),
        "opera" => rookie::opera(domains),
        #[cfg(target_os = "macos")]
        "arc" => rookie::arc(domains),
        other => return Err(format!("unsupported browser helper: {other}")),
    };

    match live {
        Ok(cookies) if !cookies.is_empty() => Ok(cookie_vec_to_header(&cookies)),
        Ok(_) => Ok(String::new()),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("unable to open") || msg.contains("locked") || msg.contains("busy") {
                Err(format!(
                    "DB locked by running browser — quit {} briefly, or press P to paste",
                    target.browser
                ))
            } else if msg.to_lowercase().contains("keychain")
                || msg.to_lowercase().contains("safe storage")
                || msg.to_lowercase().contains("decrypt")
            {
                Err(format!(
                    "allow Keychain access for {} Safe Storage (Always Allow)",
                    title_case(&target.browser)
                ))
            } else {
                Err(msg)
            }
        }
    }
}

fn copy_cookie_db(src: &Path, dest: &Path) -> Result<(), String> {
    match fs::copy(src, dest) {
        Ok(_) => Ok(()),
        Err(e) => match fs::read(src) {
            Ok(bytes) => {
                fs::write(dest, bytes).map_err(|e2| format!("write cookie copy: {e2}"))?;
                Ok(())
            }
            Err(e2) => Err(format!(
                "cannot read cookie DB ({e} / {e2}). Quit the browser or grant Full Disk Access"
            )),
        },
    }
}

fn cookie_vec_to_header(cookies: &[rookie::enums::Cookie]) -> String {
    cookies
        .iter()
        .map(|c| format!("{}={}", c.name, c.value))
        .collect::<Vec<_>>()
        .join("; ")
}

fn title_case(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

pub fn is_valid_session(cookie: &str) -> bool {
    let map = cookie_map(cookie);
    let has_sid = map.contains_key("SID")
        || map.contains_key("__Secure-3PSID")
        || map.contains_key("__Secure-1PSID");
    let has_sapisid = sapisid_from_cookies(cookie).is_some();
    has_sid && has_sapisid
}

pub fn open_music_youtube() -> anyhow::Result<()> {
    webbrowser::open("https://music.youtube.com")?;
    Ok(())
}

pub fn open_full_disk_access_settings() {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
            .spawn();
    }
}

pub fn summarize_attempts(attempts: &[BrowserAttempt]) -> String {
    if attempts.is_empty() {
        return "  No supported browsers with cookie DBs found.".into();
    }
    attempts
        .iter()
        .map(|a| {
            let mark = if a.ok { "✓" } else { "·" };
            format!("  {mark} {}: {}", a.browser, a.status)
        })
        .collect::<Vec<_>>()
        .join("\n")
}
