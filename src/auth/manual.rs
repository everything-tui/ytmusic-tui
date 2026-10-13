use regex::Regex;
use std::collections::HashMap;

/// Result of parsing a pasted cookie / cURL, including optional visitor id.
#[derive(Debug, Clone)]
pub struct ParsedAuthInput {
    pub cookies: String,
    pub visitor_id: Option<String>,
}

/// Parse a pasted Cookie header value or a full "Copy as cURL" command.
pub fn parse_cookie_input(input: &str) -> Result<String, String> {
    Ok(parse_auth_input(input)?.cookies)
}

pub fn parse_auth_input(input: &str) -> Result<ParsedAuthInput, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("Cookie input is empty".into());
    }

    // Try to extract from cURL: -H 'cookie: ...' or -H "Cookie: ..." or -b '...'
    if input.contains("curl ") || input.contains("-H ") || input.contains("-b ") {
        let Some(c) = extract_curl_cookie(input) else {
            return Err("Could not find a Cookie header in the pasted cURL command".into());
        };
        let cookies = validate_cookie_string(&c)?;
        let visitor_id = extract_curl_header(input, "X-Goog-Visitor-Id")
            .or_else(|| extract_curl_header(input, "x-goog-visitor-id"));
        return Ok(ParsedAuthInput {
            cookies,
            visitor_id,
        });
    }

    // Strip leading "Cookie:" if present
    let cookie = input
        .trim_start_matches(|c: char| c.is_ascii_whitespace())
        .strip_prefix("Cookie:")
        .or_else(|| input.strip_prefix("cookie:"))
        .unwrap_or(input)
        .trim()
        .to_string();

    Ok(ParsedAuthInput {
        cookies: validate_cookie_string(&cookie)?,
        visitor_id: None,
    })
}

fn extract_curl_header(input: &str, name: &str) -> Option<String> {
    let re = Regex::new(&format!(
        r#"(?i)-H\s+['"]{}:\s*([^'"]+)['"]"#,
        regex::escape(name)
    ))
    .ok()?;
    re.captures(input).map(|c| c[1].to_string())
}

fn extract_curl_cookie(input: &str) -> Option<String> {
    // -b '...' or --cookie '...'
    let re_b = Regex::new(r#"(?i)(?:-b|--cookie)\s+'([^']+)'"#).ok()?;
    if let Some(c) = re_b.captures(input) {
        return Some(c[1].to_string());
    }
    let re_b2 = Regex::new(r#"(?i)(?:-b|--cookie)\s+"([^"]+)""#).ok()?;
    if let Some(c) = re_b2.captures(input) {
        return Some(c[1].to_string());
    }
    // -H 'Cookie: ...'
    let re_h = Regex::new(r#"(?i)-H\s+'[Cc]ookie:\s*([^']+)'"#).ok()?;
    if let Some(c) = re_h.captures(input) {
        return Some(c[1].to_string());
    }
    let re_h2 = Regex::new(r#"(?i)-H\s+"[Cc]ookie:\s*([^"]+)""#).ok()?;
    if let Some(c) = re_h2.captures(input) {
        return Some(c[1].to_string());
    }
    None
}

fn validate_cookie_string(cookie: &str) -> Result<String, String> {
    let map = cookie_map(cookie);
    let has_sid = map.contains_key("SID")
        || map.contains_key("__Secure-3PSID")
        || map.contains_key("__Secure-1PSID");
    let has_sapisid = map.contains_key("SAPISID") || map.contains_key("__Secure-3PAPISID");
    if !has_sid && !has_sapisid {
        return Err(
            "Cookie string looks incomplete — need SID (or __Secure-3PSID) and SAPISID (or __Secure-3PAPISID)"
                .into(),
        );
    }
    Ok(cookie.to_string())
}

pub fn cookie_map(cookie: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for part in cookie.split(';') {
        let part = part.trim();
        if let Some((k, v)) = part.split_once('=') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    map
}

/// Prefer `__Secure-3PAPISID` (what ytmusicapi / Chromium use for Music), then fall back.
pub fn sapisid_from_cookies(cookie: &str) -> Option<String> {
    let map = cookie_map(cookie);
    map.get("__Secure-3PAPISID")
        .or_else(|| map.get("SAPISID"))
        .or_else(|| map.get("__Secure-1PAPISID"))
        .cloned()
}

pub fn papisid_1p_from_cookies(cookie: &str) -> Option<String> {
    cookie_map(cookie).get("__Secure-1PAPISID").cloned()
}

pub fn papisid_3p_from_cookies(cookie: &str) -> Option<String> {
    cookie_map(cookie).get("__Secure-3PAPISID").cloned()
}

/// Merge `name=value` pairs from Set-Cookie (or a partial cookie header) into an existing jar.
pub fn merge_cookie_header(existing: &str, updates: &[(String, String)]) -> String {
    let mut map = cookie_map(existing);
    for (k, v) in updates {
        if v.is_empty() {
            map.remove(k);
        } else {
            map.insert(k.clone(), v.clone());
        }
    }
    map.into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Parse a single Set-Cookie header into (name, value), ignoring attributes.
pub fn parse_set_cookie_pair(set_cookie: &str) -> Option<(String, String)> {
    let first = set_cookie.split(';').next()?.trim();
    let (k, v) = first.split_once('=')?;
    let k = k.trim();
    if k.is_empty() {
        return None;
    }
    Some((k.to_string(), v.trim().to_string()))
}

/// Write cookies in Netscape cookie file format for yt-dlp.
pub fn write_netscape_cookies(cookie: &str, path: &std::path::Path) -> std::io::Result<()> {
    use std::io::Write;
    // Replace rather than truncate: a stale file owned by another user (e.g. after a
    // `sudo` run) can't be opened for writing, but can still be unlinked from our dir.
    let _ = std::fs::remove_file(path);
    let mut f = std::fs::File::create(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    writeln!(f, "# Netscape HTTP Cookie File")?;
    writeln!(f, "# Generated by yt-tui")?;
    let map = cookie_map(cookie);
    for (name, value) in map {
        // domain, flag, path, secure, expiry, name, value
        let secure = if name.starts_with("__Secure-") || name.starts_with("__Host-") {
            "TRUE"
        } else {
            "FALSE"
        };
        writeln!(
            f,
            ".youtube.com\tTRUE\t/\t{secure}\t2147483647\t{name}\t{value}"
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_raw_cookie() {
        let c = "SID=abc; SAPISID=xyz; other=1";
        assert!(parse_cookie_input(c).is_ok());
    }

    #[test]
    fn parse_curl() {
        let curl = r#"curl 'https://music.youtube.com' -H 'cookie: SID=abc; SAPISID=xyz'"#;
        let r = parse_cookie_input(curl).unwrap();
        assert!(r.contains("SID=abc"));
    }

    #[test]
    fn parse_curl_visitor_id() {
        let curl = r#"curl 'https://music.youtube.com/youtubei/v1/browse' \
  -H 'Cookie: SID=abc; SAPISID=xyz; __Secure-3PAPISID=xyz' \
  -H 'X-Goog-Visitor-Id: CgtVisitor123'"#;
        let parsed = parse_auth_input(curl).unwrap();
        assert!(parsed.cookies.contains("SAPISID=xyz"));
        assert_eq!(parsed.visitor_id.as_deref(), Some("CgtVisitor123"));
    }

    #[test]
    fn reject_empty() {
        assert!(parse_cookie_input("").is_err());
        assert!(parse_cookie_input("foo=bar").is_err());
    }

    #[test]
    fn prefer_secure_3papisid() {
        let c = "SAPISID=old; __Secure-3PAPISID=new; __Secure-1PAPISID=one";
        assert_eq!(sapisid_from_cookies(c).as_deref(), Some("new"));
        assert_eq!(papisid_1p_from_cookies(c).as_deref(), Some("one"));
        assert_eq!(papisid_3p_from_cookies(c).as_deref(), Some("new"));
    }

    #[test]
    fn merge_updates_cookie_jar() {
        let merged = merge_cookie_header("A=1; B=2", &[("B".into(), "9".into()), ("C".into(), "3".into())]);
        let map = cookie_map(&merged);
        assert_eq!(map.get("A").map(String::as_str), Some("1"));
        assert_eq!(map.get("B").map(String::as_str), Some("9"));
        assert_eq!(map.get("C").map(String::as_str), Some("3"));
    }
}
