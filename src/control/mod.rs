//! Local control interface so external agents (e.g. Claude via MCP) can drive the player.
//!
//! The running TUI listens on a Unix socket (`Paths::control_socket`) and speaks
//! newline-delimited JSON: one request object per line, one response per line.
//! Playback intents are injected as regular [`Action`]s into the UI loop so app state
//! (shuffle / mute / volume) stays consistent with keyboard input.
//!
//! Authentication data (cookies, sessions) is deliberately unreachable from here, and
//! searches use a separate guest client.

pub mod mcp;

use crate::action::Action;
use crate::api::innertube::InnertubeClient;
use crate::api::model::Track;
use crate::app::{App, PlayerCommand};
use crate::paths::Paths;
use crate::player::queue::RepeatMode;
use crate::player::PlaybackState;
use anyhow::{anyhow, Result};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Cap on queue entries exposed in the snapshot.
const MAX_QUEUE: usize = 200;
/// Cap on remembered search-result tracks (so `play_track` can use full metadata).
const MAX_KNOWN_TRACKS: usize = 500;

#[derive(Debug, Clone, Serialize)]
pub struct TrackInfo {
    pub video_id: String,
    pub title: String,
    pub artists: String,
    pub album: Option<String>,
    pub duration_secs: Option<u64>,
}

impl From<&Track> for TrackInfo {
    fn from(t: &Track) -> Self {
        Self {
            video_id: t.video_id.clone(),
            title: t.title.clone(),
            artists: t.artist_names(),
            album: t.album.as_ref().map(|a| a.name.clone()),
            duration_secs: t.duration.map(|d| d.as_secs()),
        }
    }
}

/// Read-only view of player state, published by the UI loop.
#[derive(Debug, Clone, Serialize, Default)]
pub struct Snapshot {
    pub state: String,
    pub current: Option<TrackInfo>,
    pub position_secs: f64,
    pub duration_secs: Option<f64>,
    /// 0.0 – 1.0
    pub volume: f32,
    pub muted: bool,
    pub shuffle: bool,
    /// "off" | "all" | "one"
    pub repeat: String,
    pub queue: Vec<TrackInfo>,
    pub queue_cursor: usize,
}

pub type SharedSnapshot = Arc<Mutex<Snapshot>>;

fn repeat_label(r: RepeatMode) -> &'static str {
    match r {
        RepeatMode::Off => "off",
        RepeatMode::All => "all",
        RepeatMode::One => "one",
    }
}

impl Snapshot {
    pub fn from_app(app: &App) -> Self {
        Self {
            state: match app.playback_state {
                PlaybackState::Stopped => "stopped",
                PlaybackState::Loading => "loading",
                PlaybackState::Playing => "playing",
                PlaybackState::Paused => "paused",
                PlaybackState::Buffering => "buffering",
            }
            .into(),
            current: app.current.as_ref().map(TrackInfo::from),
            position_secs: app.position.as_secs_f64(),
            duration_secs: app.duration.map(|d| d.as_secs_f64()),
            volume: app.volume,
            muted: app.muted,
            shuffle: app.shuffle,
            repeat: repeat_label(app.repeat).into(),
            queue: app.queue.iter().take(MAX_QUEUE).map(TrackInfo::from).collect(),
            queue_cursor: app.queue_cursor,
        }
    }
}

struct Ctx {
    snapshot: SharedSnapshot,
    action_tx: mpsc::UnboundedSender<Action>,
    player_tx: mpsc::UnboundedSender<PlayerCommand>,
    /// Guest client: control searches never touch the user's session.
    api: InnertubeClient,
    known: Mutex<HashMap<String, Track>>,
}

impl Ctx {
    fn remember(&self, tracks: &[Track]) {
        let mut known = self.known.lock();
        if known.len() + tracks.len() > MAX_KNOWN_TRACKS {
            known.clear();
        }
        for t in tracks {
            known.insert(t.video_id.clone(), t.clone());
        }
    }

    fn track_for(&self, video_id: &str) -> Track {
        if let Some(t) = self.known.lock().get(video_id) {
            return t.clone();
        }
        Track {
            video_id: video_id.to_string(),
            title: "Remote track".into(),
            artists: vec![],
            album: None,
            duration: None,
            thumbnails: vec![],
            explicit: false,
            liked: None,
            from_radio: false,
        }
    }

    fn act(&self, a: Action) {
        let _ = self.action_tx.send(a);
    }

    fn player(&self, c: PlayerCommand) {
        let _ = self.player_tx.send(c);
    }
}

fn str_arg<'a>(req: &'a Value, key: &str) -> Result<&'a str> {
    req.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("missing string argument '{key}'"))
}

fn num_arg(req: &Value, key: &str) -> Result<f64> {
    req.get(key)
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite())
        .ok_or_else(|| anyhow!("missing numeric argument '{key}'"))
}

fn index_arg(req: &Value, key: &str) -> Result<usize> {
    let n = num_arg(req, key)?;
    if n < 0.0 {
        return Err(anyhow!("'{key}' must be >= 0"));
    }
    Ok(n as usize)
}

fn ok(msg: &str) -> Result<Value> {
    Ok(json!({ "status": msg }))
}

async fn dispatch(ctx: &Ctx, req: Value) -> Result<Value> {
    let cmd = str_arg(&req, "cmd")?.to_string();
    tracing::info!("control: {cmd}");
    match cmd.as_str() {
        "state" => Ok(serde_json::to_value(&*ctx.snapshot.lock())?),
        "queue" => {
            let s = ctx.snapshot.lock();
            Ok(json!({ "cursor": s.queue_cursor, "tracks": s.queue }))
        }

        "play" => {
            ctx.act(Action::Play);
            ok("play requested")
        }
        "pause" => {
            ctx.act(Action::Pause);
            ok("pause requested")
        }
        "toggle" => {
            ctx.act(Action::PlayPause);
            ok("toggle requested")
        }
        "next" => {
            ctx.act(Action::Next);
            ok("next requested")
        }
        "previous" => {
            ctx.act(Action::Previous);
            ok("previous requested")
        }
        "seek" => {
            let secs = num_arg(&req, "seconds")?.max(0.0);
            ctx.act(Action::SeekTo(Duration::from_secs_f64(secs)));
            ok("seek requested")
        }
        "volume" => {
            let pct = num_arg(&req, "percent")?.clamp(0.0, 100.0);
            ctx.act(Action::SetVolume((pct / 100.0) as f32));
            ok("volume set")
        }
        "mute" => {
            let want = req.get("muted").and_then(Value::as_bool).unwrap_or(true);
            if ctx.snapshot.lock().muted != want {
                ctx.act(Action::Mute);
            }
            ok("mute updated")
        }
        "shuffle" => {
            let want = req
                .get("enabled")
                .and_then(Value::as_bool)
                .ok_or_else(|| anyhow!("missing boolean argument 'enabled'"))?;
            if ctx.snapshot.lock().shuffle != want {
                ctx.act(Action::ToggleShuffle);
            }
            ok("shuffle updated")
        }
        "repeat" => {
            let want = match str_arg(&req, "mode")? {
                "off" => RepeatMode::Off,
                "all" => RepeatMode::All,
                "one" => RepeatMode::One,
                other => return Err(anyhow!("mode must be off|all|one, got '{other}'")),
            };
            let mut cur = match ctx.snapshot.lock().repeat.as_str() {
                "all" => RepeatMode::All,
                "one" => RepeatMode::One,
                _ => RepeatMode::Off,
            };
            // Cycle is Off → All → One → Off; at most two steps reach any mode.
            for _ in 0..2 {
                if cur == want {
                    break;
                }
                ctx.act(Action::CycleRepeat);
                cur = cur.next();
            }
            ok("repeat updated")
        }

        "search" => {
            let query = str_arg(&req, "query")?;
            let filter = req.get("filter").and_then(Value::as_str).unwrap_or("songs");
            if !matches!(filter, "songs" | "albums" | "artists" | "playlists") {
                return Err(anyhow!("filter must be songs|albums|artists|playlists"));
            }
            let limit = req
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(10)
                .clamp(1, 50) as usize;
            let bundle = ctx.api.search(query, Some(filter)).await?;
            ctx.remember(&bundle.tracks);
            Ok(json!({
                "tracks": bundle.tracks.iter().take(limit).map(TrackInfo::from).collect::<Vec<_>>(),
                "albums": bundle.albums.iter().take(limit).map(|a| json!({
                    "id": a.id, "title": a.title, "year": a.year,
                    "artists": a.artists.iter().map(|x| x.name.as_str()).collect::<Vec<_>>().join(", "),
                })).collect::<Vec<_>>(),
                "artists": bundle.artists.iter().take(limit).map(|a| json!({
                    "id": a.id, "name": a.name,
                })).collect::<Vec<_>>(),
                "playlists": bundle.playlists.iter().take(limit).map(|p| json!({
                    "id": p.id, "title": p.title,
                })).collect::<Vec<_>>(),
            }))
        }

        "play_track" => {
            let t = ctx.track_for(str_arg(&req, "video_id")?);
            ctx.player(PlayerCommand::PlayTrack(t));
            ok("playing track")
        }
        "play_tracks" => {
            let ids = req
                .get("video_ids")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("missing array argument 'video_ids'"))?;
            let tracks: Vec<Track> = ids
                .iter()
                .filter_map(Value::as_str)
                .map(|id| ctx.track_for(id))
                .collect();
            if tracks.is_empty() {
                return Err(anyhow!("'video_ids' is empty"));
            }
            ctx.player(PlayerCommand::PlayTracks { tracks, start: 0 });
            ok("playing tracks")
        }
        "queue_add" => {
            let t = ctx.track_for(str_arg(&req, "video_id")?);
            ctx.player(PlayerCommand::AddToQueue(t));
            ok("added to queue")
        }
        "queue_play_next" => {
            let t = ctx.track_for(str_arg(&req, "video_id")?);
            ctx.player(PlayerCommand::PlayNext(t));
            ok("queued to play next")
        }
        "queue_remove" => {
            ctx.player(PlayerCommand::RemoveAt(index_arg(&req, "index")?));
            ok("removed from queue")
        }
        "queue_jump" => {
            let i = index_arg(&req, "index")?;
            if i >= ctx.snapshot.lock().queue.len() {
                return Err(anyhow!("index {i} is outside the queue"));
            }
            ctx.player(PlayerCommand::JumpTo(i));
            ok("jumping")
        }
        "queue_clear" => {
            ctx.player(PlayerCommand::ClearUpcoming);
            ok("upcoming queue cleared")
        }
        other => Err(anyhow!("unknown command '{other}'")),
    }
}

#[cfg(unix)]
async fn serve_conn(ctx: Arc<Ctx>, stream: tokio::net::UnixStream) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (r, mut w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Value>(&line) {
            Ok(req) => match dispatch(&ctx, req).await {
                Ok(data) => json!({ "ok": true, "data": data }),
                Err(e) => json!({ "ok": false, "error": e.to_string() }),
            },
            Err(e) => json!({ "ok": false, "error": format!("invalid JSON: {e}") }),
        };
        let mut out = resp.to_string();
        out.push('\n');
        if w.write_all(out.as_bytes()).await.is_err() {
            break;
        }
    }
}

/// Starts the control socket. Returns the snapshot handle the UI should publish into
/// and the receiver for actions it should apply.
#[cfg(unix)]
pub fn start(
    paths: &Paths,
    player_tx: mpsc::UnboundedSender<PlayerCommand>,
) -> Result<(SharedSnapshot, mpsc::UnboundedReceiver<Action>)> {
    use std::os::unix::fs::PermissionsExt;

    let sock = paths.control_socket();
    if sock.exists() {
        // A live instance answers; a stale file from a crash does not.
        if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
            return Err(anyhow!(
                "another yt-tui already owns {}",
                sock.display()
            ));
        }
        let _ = std::fs::remove_file(&sock);
    }
    let listener = tokio::net::UnixListener::bind(&sock)?;
    std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600))?;

    let snapshot: SharedSnapshot = Arc::new(Mutex::new(Snapshot {
        state: "stopped".into(),
        repeat: "off".into(),
        ..Default::default()
    }));
    let (action_tx, action_rx) = mpsc::unbounded_channel();
    let ctx = Arc::new(Ctx {
        snapshot: snapshot.clone(),
        action_tx,
        player_tx,
        api: InnertubeClient::new(None)?,
        known: Mutex::new(HashMap::new()),
    });

    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    tokio::spawn(serve_conn(ctx.clone(), stream));
                }
                Err(e) => {
                    tracing::warn!("control accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
    });
    tracing::info!("control socket listening at {}", sock.display());
    Ok((snapshot, action_rx))
}

#[cfg(not(unix))]
pub fn start(
    _paths: &Paths,
    _player_tx: mpsc::UnboundedSender<PlayerCommand>,
) -> Result<(SharedSnapshot, mpsc::UnboundedReceiver<Action>)> {
    Err(anyhow!("control socket is only supported on unix platforms"))
}

pub fn cleanup(paths: &Paths) {
    let _ = std::fs::remove_file(paths.control_socket());
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    async fn roundtrip(sock: &std::path::Path, req: Value) -> Value {
        let stream = tokio::net::UnixStream::connect(sock).await.unwrap();
        let (r, mut w) = stream.into_split();
        w.write_all(format!("{req}\n").as_bytes()).await.unwrap();
        let mut line = String::new();
        BufReader::new(r).read_line(&mut line).await.unwrap();
        serde_json::from_str(&line).unwrap()
    }

    #[tokio::test]
    async fn socket_routes_commands() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            config_dir: dir.path().into(),
            data_dir: dir.path().into(),
            cache_dir: dir.path().into(),
        };
        let (player_tx, mut player_rx) = mpsc::unbounded_channel();
        let (snapshot, mut action_rx) = start(&paths, player_tx).unwrap();
        let sock = paths.control_socket();

        let r = roundtrip(&sock, json!({"cmd": "pause"})).await;
        assert_eq!(r["ok"], true);
        assert!(matches!(action_rx.recv().await, Some(Action::Pause)));

        let r = roundtrip(&sock, json!({"cmd": "volume", "percent": 150})).await;
        assert_eq!(r["ok"], true);
        match action_rx.recv().await {
            Some(Action::SetVolume(v)) => assert_eq!(v, 1.0),
            other => panic!("unexpected {other:?}"),
        }

        // Shuffle is idempotent against the published snapshot.
        let r = roundtrip(&sock, json!({"cmd": "shuffle", "enabled": false})).await;
        assert_eq!(r["ok"], true);
        assert!(action_rx.try_recv().is_err());
        snapshot.lock().shuffle = true;
        roundtrip(&sock, json!({"cmd": "shuffle", "enabled": false})).await;
        assert!(matches!(action_rx.recv().await, Some(Action::ToggleShuffle)));

        let r = roundtrip(&sock, json!({"cmd": "play_track", "video_id": "abc123"})).await;
        assert_eq!(r["ok"], true);
        match player_rx.recv().await {
            Some(PlayerCommand::PlayTrack(t)) => assert_eq!(t.video_id, "abc123"),
            other => panic!("unexpected {other:?}"),
        }

        let r = roundtrip(&sock, json!({"cmd": "queue_jump", "index": 3})).await;
        assert_eq!(r["ok"], false);

        let r = roundtrip(&sock, json!({"cmd": "login"})).await;
        assert_eq!(r["ok"], false);

        let r = roundtrip(&sock, json!({"cmd": "state"})).await;
        assert_eq!(r["data"]["state"], "stopped");
    }
}
