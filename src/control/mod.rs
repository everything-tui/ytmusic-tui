//! Local control interface so external agents (e.g. Claude via MCP) can drive the player.
//!
//! The running TUI listens on a loopback TCP port (the same on Linux, macOS and Windows)
//! and records `{port, token}` in `Paths::control_file`. Clients send `{"auth": token}`
//! as their first line, then newline-delimited JSON requests, one response per line.
//! Playback intents are injected as regular [`Action`]s into the UI loop so app state
//! (shuffle / mute / volume) stays consistent with keyboard input.
//!
//! Authentication data (cookies, sessions) is deliberately unreachable from here, and
//! searches use a separate guest client. The listener only ever binds 127.0.0.1.

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

/// Connection details written for clients (`yt-tui mcp`).
#[derive(Debug, Serialize, serde::Deserialize)]
pub struct Endpoint {
    pub port: u16,
    pub token: String,
}

impl Endpoint {
    pub fn load(paths: &Paths) -> Result<Self> {
        let text = std::fs::read_to_string(paths.control_file())?;
        Ok(serde_json::from_str(&text)?)
    }
}

fn token_matches(given: &str, expected: &str) -> bool {
    // Constant-time compare: avoid leaking the token prefix through timing.
    given.len() == expected.len()
        && given
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

async fn serve_conn(ctx: Arc<Ctx>, token: Arc<String>, stream: tokio::net::TcpStream) {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    let (r, mut w) = stream.into_split();
    let mut reader = BufReader::new(r);

    // First line must authenticate; bound its size so strangers can't buffer data.
    let mut first = String::new();
    let authed = match (&mut reader).take(512).read_line(&mut first).await {
        Ok(n) if n > 0 => serde_json::from_str::<Value>(&first)
            .ok()
            .and_then(|v| v.get("auth").and_then(Value::as_str).map(str::to_owned))
            .is_some_and(|t| token_matches(&t, &token)),
        _ => false,
    };
    if !authed {
        return;
    }

    let mut lines = reader.lines();
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

/// Starts the control listener. Returns the snapshot handle the UI should publish into
/// and the receiver for actions it should apply.
/// Keeps the bridge alive; dropping it stops the listener, drops open
/// connections and removes the endpoint file.
pub struct ControlHandle {
    task: tokio::task::JoinHandle<()>,
    file: std::path::PathBuf,
}

impl Drop for ControlHandle {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.file);
    }
}

pub fn start(
    paths: &Paths,
    player_tx: mpsc::UnboundedSender<PlayerCommand>,
    action_tx: mpsc::UnboundedSender<Action>,
) -> Result<(SharedSnapshot, ControlHandle)> {
    use std::net::{Ipv4Addr, SocketAddr, TcpStream};

    let file = paths.control_file();
    if let Ok(old) = Endpoint::load(paths) {
        // A live instance accepts connections; a leftover file from a crash does not.
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, old.port));
        if TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok() {
            return Err(anyhow!("another yt-tui already owns {}", file.display()));
        }
    }
    let _ = std::fs::remove_file(&file);

    let std_listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    std_listener.set_nonblocking(true)?;
    let port = std_listener.local_addr()?.port();
    let listener = tokio::net::TcpListener::from_std(std_listener)?;

    let mut raw = [0u8; 24];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut raw);
    let token = Arc::new(hex::encode(raw));

    let endpoint = serde_json::to_vec(&Endpoint {
        port,
        token: (*token).clone(),
    })?;
    write_private(&file, &endpoint)?;

    let snapshot: SharedSnapshot = Arc::new(Mutex::new(Snapshot {
        state: "stopped".into(),
        repeat: "off".into(),
        ..Default::default()
    }));
    let ctx = Arc::new(Ctx {
        snapshot: snapshot.clone(),
        action_tx,
        player_tx,
        api: InnertubeClient::new(None)?,
        known: Mutex::new(HashMap::new()),
    });

    let task = tokio::spawn(async move {
        // Owned by this task so aborting it also drops every open connection.
        let mut conns = tokio::task::JoinSet::new();
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    conns.spawn(serve_conn(ctx.clone(), token.clone(), stream));
                }
                Err(e) => {
                    tracing::warn!("control accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
    });
    tracing::info!("control bridge listening on 127.0.0.1:{port}");
    Ok((snapshot, ControlHandle { task, file }))
}

/// Writes a file readable only by the current user (0600 on Unix; the per-user
/// profile directory ACL covers Windows).
fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(bytes)?;
    Ok(())
}



#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    async fn connect(ep: &Endpoint) -> tokio::net::TcpStream {
        tokio::net::TcpStream::connect(("127.0.0.1", ep.port))
            .await
            .unwrap()
    }

    async fn roundtrip(ep: &Endpoint, req: Value) -> Value {
        let stream = connect(ep).await;
        let (r, mut w) = stream.into_split();
        let auth = json!({ "auth": ep.token });
        w.write_all(format!("{auth}\n{req}\n").as_bytes())
            .await
            .unwrap();
        let mut line = String::new();
        BufReader::new(r).read_line(&mut line).await.unwrap();
        serde_json::from_str(&line).unwrap()
    }

    fn temp_paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            config_dir: dir.path().into(),
            data_dir: dir.path().into(),
            cache_dir: dir.path().into(),
        };
        (dir, paths)
    }

    #[tokio::test]
    async fn routes_commands() {
        let (_dir, paths) = temp_paths();
        let (player_tx, mut player_rx) = mpsc::unbounded_channel();
        let (action_tx, mut action_rx) = mpsc::unbounded_channel();
        let (snapshot, _handle) = start(&paths, player_tx, action_tx).unwrap();
        let ep = Endpoint::load(&paths).unwrap();

        let r = roundtrip(&ep, json!({"cmd": "pause"})).await;
        assert_eq!(r["ok"], true);
        assert!(matches!(action_rx.recv().await, Some(Action::Pause)));

        let r = roundtrip(&ep, json!({"cmd": "volume", "percent": 150})).await;
        assert_eq!(r["ok"], true);
        match action_rx.recv().await {
            Some(Action::SetVolume(v)) => assert_eq!(v, 1.0),
            other => panic!("unexpected {other:?}"),
        }

        // Shuffle is idempotent against the published snapshot.
        let r = roundtrip(&ep, json!({"cmd": "shuffle", "enabled": false})).await;
        assert_eq!(r["ok"], true);
        assert!(action_rx.try_recv().is_err());
        snapshot.lock().shuffle = true;
        roundtrip(&ep, json!({"cmd": "shuffle", "enabled": false})).await;
        assert!(matches!(action_rx.recv().await, Some(Action::ToggleShuffle)));

        let r = roundtrip(&ep, json!({"cmd": "play_track", "video_id": "abc123"})).await;
        assert_eq!(r["ok"], true);
        match player_rx.recv().await {
            Some(PlayerCommand::PlayTrack(t)) => assert_eq!(t.video_id, "abc123"),
            other => panic!("unexpected {other:?}"),
        }

        let r = roundtrip(&ep, json!({"cmd": "queue_jump", "index": 3})).await;
        assert_eq!(r["ok"], false);

        let r = roundtrip(&ep, json!({"cmd": "login"})).await;
        assert_eq!(r["ok"], false);

        let r = roundtrip(&ep, json!({"cmd": "state"})).await;
        assert_eq!(r["data"]["state"], "stopped");
    }

    #[tokio::test]
    async fn rejects_missing_or_wrong_token() {
        let (_dir, paths) = temp_paths();
        let (player_tx, _player_rx) = mpsc::unbounded_channel();
        let (action_tx, mut action_rx) = mpsc::unbounded_channel();
        let (_snapshot, _handle) = start(&paths, player_tx, action_tx).unwrap();
        let ep = Endpoint::load(&paths).unwrap();

        for first in [
            json!({"cmd": "pause"}),
            json!({"auth": "wrong"}),
            json!({"auth": ""}),
        ] {
            let stream = connect(&ep).await;
            let (r, mut w) = stream.into_split();
            let _ = w
                .write_all(format!("{first}\n{}\n", json!({"cmd": "pause"})).as_bytes())
                .await;
            let mut line = String::new();
            let n = BufReader::new(r).read_line(&mut line).await.unwrap_or(0);
            assert_eq!(n, 0, "unauthenticated client must get no reply");
        }
        assert!(action_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn second_instance_is_refused_and_stale_file_replaced() {
        let (_dir, paths) = temp_paths();
        let (tx, _rx) = mpsc::unbounded_channel();
        let (action_tx, _action_rx) = mpsc::unbounded_channel();
        let _first = start(&paths, tx.clone(), action_tx.clone()).unwrap();
        assert!(start(&paths, tx.clone(), action_tx.clone()).is_err());

        // A leftover file pointing at a dead port is replaced.
        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_port = dead.local_addr().unwrap().port();
        drop(dead);
        std::fs::write(
            paths.control_file(),
            json!({"port": dead_port, "token": "x"}).to_string(),
        )
        .unwrap();
        assert!(start(&paths, tx, action_tx).is_ok());
    }
}
