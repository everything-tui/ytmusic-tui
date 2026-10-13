//! Session debug NDJSON logger (debug mode only).
use std::fs::OpenOptions;
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

const PATH: &str = "/Users/francoisegner/Desktop/yt-tui/.cursor/debug-afafdc.log";

pub fn log(hypothesis_id: &str, location: &str, message: &str, data: serde_json::Value) {
    // #region agent log
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let payload = serde_json::json!({
        "sessionId": "afafdc",
        "hypothesisId": hypothesis_id,
        "location": location,
        "message": message,
        "data": data,
        "timestamp": ts,
    });
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(PATH) {
        let _ = writeln!(f, "{payload}");
    }
    // #endregion
}
