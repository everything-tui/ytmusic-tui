//! Persistent playback debug NDJSON logger.
//!
//! Always-on instrumentation for intermittent play failures. Appends one JSON
//! object per line to `.cursor/debug-playback.log`. When play fails again, point
//! the agent at that file — no rebuild/instrumentation step needed first.
use std::fs::{create_dir_all, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const PATH: &str =
    "/Users/francoisegner/Documents/Repositories/Privat/everything-tui/ytmusic-tui/.cursor/debug-playback.log";

pub fn log(hypothesis_id: &str, location: &str, message: &str, data: serde_json::Value) {
    // #region agent log
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let payload = serde_json::json!({
        "sessionId": "playback",
        "hypothesisId": hypothesis_id,
        "location": location,
        "message": message,
        "data": data,
        "timestamp": ts,
    });
    if let Some(parent) = Path::new(PATH).parent() {
        let _ = create_dir_all(parent);
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(PATH) {
        let _ = writeln!(f, "{payload}");
    }
    // #endregion
}
