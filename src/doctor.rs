use crate::paths::Paths;
use std::sync::Arc;

/// Environment diagnostics for `yt-tui doctor`.
pub fn doctor_report(paths: &Paths) -> String {
    let mut lines = Vec::new();
    lines.push("yt-tui doctor".into());
    lines.push(format!("config: {}", paths.config_dir.display()));
    lines.push(format!("data:   {}", paths.data_dir.display()));
    lines.push(format!("cache:  {}", paths.cache_dir.display()));
    lines.push("stream resolver: native Innertube (no yt-dlp / node / deno)".into());
    lines.push(format!(
        "signed-in fallback: WEB_REMIX via embedded QuickJS (player cache: {})",
        paths.player_js_cache_dir().display()
    ));

    let truecolor = std::env::var("COLORTERM")
        .map(|v| v == "truecolor" || v == "24bit")
        .unwrap_or(false);
    lines.push(format!("truecolor: {truecolor}"));
    lines.push(format!(
        "TERM: {}",
        std::env::var("TERM").unwrap_or_default()
    ));
    lines.push(format!(
        "TERM_PROGRAM: {}",
        std::env::var("TERM_PROGRAM").unwrap_or_default()
    ));
    if std::env::var_os("TMUX").is_some() {
        lines.push("tmux: yes (album art may fall back to half-blocks)".into());
    }

    let auth = crate::auth::AuthService::new(Arc::new(paths.clone()));
    if let Some(s) = auth.session() {
        lines.push(format!(
            "auth: signed in as {}",
            s.account_name.as_deref().unwrap_or("unknown")
        ));
    } else {
        lines.push("auth: guest / not signed in".into());
    }

    let cache = crate::player::download::cache_size_bytes(&paths.audio_cache_dir());
    lines.push(format!(
        "audio cache: {:.1} MB",
        cache as f64 / 1_048_576.0
    ));

    lines.join("\n")
}
