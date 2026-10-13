use sha1::{Digest, Sha1};

const ORIGIN: &str = "https://music.youtube.com";

/// Classic hash: `sha1("{ts} {sapisid} {origin}")` → `{ts}_{hex}`
pub fn sapisid_hash(sapisid: &str, timestamp: i64) -> String {
    let raw = format!("{timestamp} {sapisid} {ORIGIN}");
    let mut hasher = Sha1::new();
    hasher.update(raw.as_bytes());
    let digest = hasher.finalize();
    format!("{timestamp}_{}", hex::encode(digest))
}

/// Modern browser hash with user session (`_u` suffix), matching yt-dlp / Firefox:
/// `sha1("{user_session_id} {ts} {sapisid} {origin}")` → `{ts}_{hex}_u`
pub fn sapisid_hash_u(sapisid: &str, timestamp: i64, user_session_id: &str) -> String {
    let raw = format!("{user_session_id} {timestamp} {sapisid} {ORIGIN}");
    let mut hasher = Sha1::new();
    hasher.update(raw.as_bytes());
    let digest = hasher.finalize();
    format!("{timestamp}_{}_u", hex::encode(digest))
}

fn one_token(scheme: &str, sapisid: &str, ts: i64, user_session_id: Option<&str>) -> String {
    let hash = match user_session_id {
        Some(uid) if !uid.is_empty() => sapisid_hash_u(sapisid, ts, uid),
        _ => sapisid_hash(sapisid, ts),
    };
    format!("{scheme} {hash}")
}

/// Build `Authorization` the way the Music web client does:
/// `SAPISIDHASH …` plus `SAPISID1PHASH` / `SAPISID3PHASH` when those cookies exist.
pub fn authorization_header_parts(
    sapisid: &str,
    papisid_1p: Option<&str>,
    papisid_3p: Option<&str>,
    user_session_id: Option<&str>,
) -> String {
    let ts = chrono::Utc::now().timestamp();
    let mut parts = vec![one_token("SAPISIDHASH", sapisid, ts, user_session_id)];
    if let Some(s) = papisid_1p {
        parts.push(one_token("SAPISID1PHASH", s, ts, user_session_id));
    }
    if let Some(s) = papisid_3p {
        parts.push(one_token("SAPISID3PHASH", s, ts, user_session_id));
    }
    parts.join(" ")
}

pub fn authorization_header(sapisid: &str) -> String {
    authorization_header_parts(sapisid, None, None, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vector() {
        let h1 = sapisid_hash("AAA", 1_700_000_000);
        let h2 = sapisid_hash("AAA", 1_700_000_000);
        assert_eq!(h1, h2);
        assert!(h1.starts_with("1700000000_"));
        assert_eq!(h1.len(), 10 + 1 + 40);
    }

    #[test]
    fn u_suffix_changes_hash() {
        let plain = sapisid_hash("AAA", 1_700_000_000);
        let with_u = sapisid_hash_u("AAA", 1_700_000_000, "session");
        assert!(with_u.ends_with("_u"));
        assert_ne!(plain, with_u.trim_end_matches("_u"));
    }

    #[test]
    fn multi_hash_includes_variants() {
        let h = authorization_header_parts("A", Some("B"), Some("C"), Some("uid"));
        assert!(h.contains("SAPISIDHASH "));
        assert!(h.contains("SAPISID1PHASH "));
        assert!(h.contains("SAPISID3PHASH "));
        assert!(h.contains("_u"));
    }
}
