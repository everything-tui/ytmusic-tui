use crate::action::AppEvent;
use crate::api::model::{LyricLine, Lyrics, Track};
use crate::paths::Paths;
use anyhow::Result;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::debug;

#[derive(Debug, Deserialize)]
struct LrclibGet {
    #[serde(rename = "syncedLyrics")]
    synced_lyrics: Option<String>,
    #[serde(rename = "plainLyrics")]
    plain_lyrics: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LrclibSearchItem {
    #[serde(rename = "syncedLyrics")]
    synced_lyrics: Option<String>,
    #[serde(rename = "plainLyrics")]
    plain_lyrics: Option<String>,
    duration: Option<f64>,
}

pub async fn fetch_lyrics(
    paths: Arc<Paths>,
    track: Track,
    ytm_browse_id: Option<String>,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    let video_id = track.video_id.clone();

    if let Some(cached) = load_cache(&paths, &video_id) {
        let _ = tx.send(AppEvent::LyricsLoaded {
            video_id,
            lyrics: cached,
        });
        return;
    }

    match fetch_lrclib(&track).await {
        Ok(lyrics) => {
            let _ = save_cache(&paths, &video_id, &lyrics);
            let _ = tx.send(AppEvent::LyricsLoaded { video_id, lyrics });
            return;
        }
        Err(e) => debug!("lrclib: {e}"),
    }

    // YTM fallback — signal API via event that we need browse; for now emit empty with note
    if ytm_browse_id.is_some() {
        let _ = tx.send(AppEvent::Info("Fetching YouTube Music lyrics…".into()));
        // The app should also dispatch Command::LoadLyrics
    }

    let _ = tx.send(AppEvent::LyricsLoaded {
        video_id,
        lyrics: Lyrics {
            synced: None,
            plain: None,
            source: "none".into(),
        },
    });
}

async fn fetch_lrclib(track: &Track) -> Result<Lyrics> {
    let client = reqwest::Client::builder()
        .user_agent("yt-tui/0.1 (https://github.com/francoisegner/yt-tui)")
        .build()?;

    let artist = track.artist_names();
    let album = track
        .album
        .as_ref()
        .map(|a| a.name.as_str())
        .unwrap_or("");
    let duration = track.duration.map(|d| d.as_secs()).unwrap_or(0);

    let url = format!(
        "https://lrclib.net/api/get?track_name={}&artist_name={}&album_name={}&duration={}",
        urlencoding_encode(&track.title),
        urlencoding_encode(&artist),
        urlencoding_encode(album),
        duration
    );

    let resp = client.get(&url).send().await?;
    if resp.status().as_u16() == 404 {
        return search_lrclib(&client, track).await;
    }
    let data: LrclibGet = resp.error_for_status()?.json().await?;
    Ok(to_lyrics(data.synced_lyrics, data.plain_lyrics, "LRCLIB"))
}

async fn search_lrclib(client: &reqwest::Client, track: &Track) -> Result<Lyrics> {
    let url = format!(
        "https://lrclib.net/api/search?track_name={}&artist_name={}",
        urlencoding_encode(&track.title),
        urlencoding_encode(&track.artist_names())
    );
    let items: Vec<LrclibSearchItem> = client.get(&url).send().await?.error_for_status()?.json().await?;
    let target = track.duration.map(|d| d.as_secs_f64()).unwrap_or(0.0);
    let best = items.into_iter().min_by(|a, b| {
        let da = (a.duration.unwrap_or(0.0) - target).abs();
        let db = (b.duration.unwrap_or(0.0) - target).abs();
        da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
    });
    let Some(item) = best else {
        anyhow::bail!("no lrclib results");
    };
    if let Some(d) = item.duration {
        if target > 0.0 && (d - target).abs() > 3.0 {
            anyhow::bail!("lrclib duration mismatch");
        }
    }
    Ok(to_lyrics(item.synced_lyrics, item.plain_lyrics, "LRCLIB"))
}

fn to_lyrics(synced: Option<String>, plain: Option<String>, source: &str) -> Lyrics {
    let synced_lines = synced.as_deref().and_then(|s| {
        let lines = parse_lrc(s);
        if lines.is_empty() {
            None
        } else {
            Some(lines)
        }
    });
    Lyrics {
        synced: synced_lines,
        plain,
        source: source.into(),
    }
}

/// Parse LRC format. Supports multiple timestamps per line.
pub fn parse_lrc(text: &str) -> Vec<LyricLine> {
    let re = Regex::new(r"\[(\d{1,2}):(\d{2})(?:\.(\d{1,3}))?\]").unwrap();
    let mut lines = Vec::new();
    for raw in text.lines() {
        let mut times = Vec::new();
        for cap in re.captures_iter(raw) {
            let m: u64 = cap[1].parse().unwrap_or(0);
            let s: u64 = cap[2].parse().unwrap_or(0);
            let frac = cap.get(3).map(|m| m.as_str()).unwrap_or("0");
            let millis = match frac.len() {
                1 => frac.parse::<u64>().unwrap_or(0) * 100,
                2 => frac.parse::<u64>().unwrap_or(0) * 10,
                _ => frac.parse::<u64>().unwrap_or(0),
            };
            times.push(Duration::from_millis(m * 60_000 + s * 1000 + millis));
        }
        let text = re.replace_all(raw, "").trim().to_string();
        if text.is_empty() && times.is_empty() {
            continue;
        }
        for t in times {
            lines.push(LyricLine {
                time: t,
                text: text.clone(),
            });
        }
    }
    lines.sort_by_key(|l| l.time);
    lines
}

fn cache_path(paths: &Paths, video_id: &str) -> PathBuf {
    paths.lyrics_cache_dir().join(format!("{video_id}.json"))
}

fn load_cache(paths: &Paths, video_id: &str) -> Option<Lyrics> {
    let p = cache_path(paths, video_id);
    let text = std::fs::read_to_string(p).ok()?;
    serde_json::from_str(&text).ok()
}

fn save_cache(paths: &Paths, video_id: &str, lyrics: &Lyrics) -> Result<()> {
    let p = cache_path(paths, video_id);
    let text = serde_json::to_string(lyrics)?;
    std::fs::write(p, text)?;
    Ok(())
}

fn urlencoding_encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// Index of the current lyric line for a given position.
pub fn current_line_index(lines: &[LyricLine], pos: Duration) -> Option<usize> {
    if lines.is_empty() {
        return None;
    }
    let mut idx = 0;
    for (i, line) in lines.iter().enumerate() {
        if line.time <= pos {
            idx = i;
        } else {
            break;
        }
    }
    Some(idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_lrc() {
        let lrc = "[00:12.00]Hello\n[00:15.50]World\n[01:00.000]End";
        let lines = parse_lrc(lrc);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text, "Hello");
        assert_eq!(lines[1].time, Duration::from_millis(15500));
        assert_eq!(lines[2].time, Duration::from_secs(60));
    }

    #[test]
    fn multi_timestamp() {
        let lrc = "[00:01.00][00:02.00]Twice";
        let lines = parse_lrc(lrc);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "Twice");
        assert_eq!(lines[1].text, "Twice");
    }

    #[test]
    fn current_line() {
        let lines = parse_lrc("[00:00.00]A\n[00:10.00]B\n[00:20.00]C");
        assert_eq!(current_line_index(&lines, Duration::from_secs(5)), Some(0));
        assert_eq!(current_line_index(&lines, Duration::from_secs(15)), Some(1));
        assert_eq!(current_line_index(&lines, Duration::from_secs(25)), Some(2));
    }
}
