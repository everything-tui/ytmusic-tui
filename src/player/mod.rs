pub mod download;
pub mod engine;
pub mod queue;
pub mod resolver;
pub mod source;

pub use engine::SampleBuffer;

use crate::action::{AppEvent, PlayerEvent};
use crate::api::model::Track;
use crate::paths::Paths;
use crate::player::download::Downloader;
use crate::player::engine::{EngineCmd, EngineHandle};
use crate::player::queue::{PreviousResult, Queue, RepeatMode};
use crate::player::resolver::Resolver;
use crate::player::source::GrowingFile;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    Stopped,
    Loading,
    Playing,
    Paused,
    Buffering,
}

pub struct PlayerService {
    pub queue: Queue,
    pub state: PlaybackState,
    pub position: Duration,
    pub duration: Option<Duration>,
    pub volume: f32,
    pub muted: bool,
    pub volume_before_mute: f32,
    pub autoplay: bool,
    pub engine: EngineHandle,
    resolver: Resolver,
    downloader: Downloader,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    paths: Arc<Paths>,
    current: Option<Track>,
    /// Bumped on every new play request so in-flight resolve/download tasks
    /// can bail before calling `EngineCmd::Load` (avoids playing a stale track).
    play_epoch: Arc<AtomicU64>,
}

impl PlayerService {
    pub fn new(
        paths: Arc<Paths>,
        ytdlp: PathBuf,
        js_runtime: Option<String>,
        volume: f32,
        autoplay: bool,
        cache_mb: u64,
        event_tx: mpsc::UnboundedSender<AppEvent>,
        viz_samples: SampleBuffer,
    ) -> Self {
        let (engine_tx, engine_rx) = crossbeam_channel::unbounded::<PlayerEvent>();
        let engine = EngineHandle::spawn_with_samples(engine_tx, viz_samples);

        // Forward engine events to app
        let app_tx = event_tx.clone();
        std::thread::spawn(move || {
            while let Ok(ev) = engine_rx.recv() {
                let _ = app_tx.send(AppEvent::Player(ev));
            }
        });

        let cookies = resolver::cookies_path_if_exists(&paths.cookies_tmp_file());
        let resolver = Resolver::new(ytdlp, js_runtime, cookies);
        let downloader = Downloader::new(paths.audio_cache_dir(), cache_mb);

        Self {
            queue: Queue::default(),
            state: PlaybackState::Stopped,
            position: Duration::ZERO,
            duration: None,
            volume,
            muted: false,
            volume_before_mute: volume,
            autoplay,
            engine,
            resolver,
            downloader,
            event_tx,
            paths,
            current: None,
            play_epoch: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn set_ytdlp(&mut self, path: PathBuf, js: Option<String>) {
        self.resolver.set_ytdlp(path);
        self.resolver.set_js_runtime(js);
    }

    pub fn refresh_cookies(&mut self) {
        self.resolver
            .set_cookies(resolver::cookies_path_if_exists(&self.paths.cookies_tmp_file()));
    }

    pub fn take_samples(&self, out: &mut Vec<f32>) {
        self.engine.take_samples(out);
    }

    pub fn current_track(&self) -> Option<&Track> {
        self.current.as_ref().or_else(|| self.queue.current())
    }

    pub fn play_track(&mut self, track: Track) {
        self.queue.play_track_now(track.clone());
        self.start_current(Duration::ZERO);
    }

    pub fn play_tracks(&mut self, tracks: Vec<Track>, start: usize) {
        if tracks.is_empty() {
            return;
        }
        self.queue.replace(tracks, start);
        self.start_current(Duration::ZERO);
    }

    /// Begin resolving/loading the current queue item without blocking the
    /// player command loop. A newer `start_current` bumps `play_epoch` so any
    /// older task drops its result instead of loading the wrong song.
    pub fn start_current(&mut self, start: Duration) {
        let Some(track) = self.queue.current().cloned() else {
            return;
        };
        let epoch = self.play_epoch.fetch_add(1, Ordering::SeqCst) + 1;
        // Cut the previous stream immediately so a slow resolve can't keep
        // playing a song the user already navigated away from.
        self.engine.send(EngineCmd::Stop);
        self.current = Some(track.clone());
        self.state = PlaybackState::Loading;
        self.position = start;
        self.duration = track.duration;

        let next = self
            .queue
            .tracks_in_play_order()
            .get(self.queue.cursor() + 1)
            .map(|t| (*t).clone());

        let resolver = self.resolver.clone();
        let downloader = self.downloader.clone();
        let engine_tx = self.engine.cmd_tx.clone();
        let event_tx = self.event_tx.clone();
        let play_epoch = self.play_epoch.clone();
        let volume = self.effective_volume();

        tokio::spawn(async move {
            let video_id = track.video_id.clone();
            let title = track.title.clone();

            // Cached files skip yt-dlp entirely (~5s cold-start win on replays).
            let need_resolve = !downloader.is_cached(&video_id);
            let file = if !need_resolve {
                match GrowingFile::open_complete(&downloader.cached_path(&video_id)) {
                    Ok(f) => Some(f),
                    Err(e) => {
                        warn!("open cached {video_id}: {e}; re-resolving");
                        None
                    }
                }
            } else {
                None
            };
            let file = match file {
                Some(f) => f,
                None => {
                    match resolve_download(
                        epoch,
                        &play_epoch,
                        &video_id,
                        &title,
                        &resolver,
                        &downloader,
                        &event_tx,
                    )
                    .await
                    {
                        Some(f) => f,
                        None => return,
                    }
                }
            };
            if play_epoch.load(Ordering::SeqCst) != epoch {
                return;
            }

            let _ = engine_tx.send(EngineCmd::Load {
                track,
                file,
                start,
                volume,
            });

            let downloader2 = downloader.clone();
            tokio::spawn(async move {
                let _ = downloader2.enforce_lru();
            });

            if let Some(next) = next {
                prefetch_track(next, resolver, downloader);
            }
        });
    }

    pub fn toggle(&mut self) {
        match self.state {
            PlaybackState::Playing => {
                self.engine.send(EngineCmd::Pause);
                self.state = PlaybackState::Paused;
            }
            PlaybackState::Paused => {
                self.engine.send(EngineCmd::Play);
                self.state = PlaybackState::Playing;
            }
            PlaybackState::Stopped => {
                if self.queue.current().is_some() {
                    self.start_current(self.position);
                }
            }
            _ => {
                self.engine.send(EngineCmd::Toggle);
            }
        }
    }

    fn skip_to_next(&mut self) {
        if self.queue.next().is_some() {
            self.start_current(Duration::ZERO);
        } else {
            self.engine.send(EngineCmd::Stop);
            self.state = PlaybackState::Stopped;
            self.current = None;
            self.play_epoch.fetch_add(1, Ordering::SeqCst);
        }
    }

    pub fn next(&mut self) {
        self.skip_to_next();
    }

    pub fn previous(&mut self) {
        match self.queue.previous(self.position.as_secs_f64()) {
            PreviousResult::Restart => {
                self.engine.send(EngineCmd::Seek(Duration::ZERO));
                self.position = Duration::ZERO;
            }
            PreviousResult::Track => {
                self.start_current(Duration::ZERO);
            }
            PreviousResult::None => {}
        }
    }

    pub fn seek(&mut self, pos: Duration) {
        self.position = pos;
        self.engine.send(EngineCmd::Seek(pos));
    }

    pub fn seek_relative(&mut self, delta: i64) {
        let ms = self.position.as_millis() as i64 + delta * 1000;
        let ms = ms.max(0) as u64;
        let mut pos = Duration::from_millis(ms);
        if let Some(d) = self.duration {
            if pos > d {
                pos = d;
            }
        }
        self.seek(pos);
    }

    pub fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 1.0);
        self.muted = false;
        self.engine.send(EngineCmd::SetVolume(self.volume));
    }

    pub fn volume_delta(&mut self, delta: f32) {
        self.set_volume(self.volume + delta);
    }

    pub fn toggle_mute(&mut self) {
        if self.muted {
            self.muted = false;
            self.volume = self.volume_before_mute;
        } else {
            self.muted = true;
            self.volume_before_mute = self.volume;
            self.volume = 0.0;
        }
        self.engine
            .send(EngineCmd::SetVolume(self.effective_volume()));
    }

    fn effective_volume(&self) -> f32 {
        if self.muted {
            0.0
        } else {
            self.volume
        }
    }

    pub fn toggle_shuffle(&mut self) {
        self.queue.toggle_shuffle();
    }

    pub fn cycle_repeat(&mut self) {
        self.queue.cycle_repeat();
    }

    pub fn on_ended(&mut self) {
        // Called from app when PlayerEvent::Ended arrives
    }

    pub fn handle_ended(&mut self) {
        if self.queue.advance_on_end().is_some() {
            self.start_current(Duration::ZERO);
        } else {
            self.state = PlaybackState::Stopped;
            self.current = None;
            self.play_epoch.fetch_add(1, Ordering::SeqCst);
        }
    }

    pub fn append_radio(&mut self, tracks: Vec<Track>) {
        self.queue.append_radio(tracks);
    }

    pub fn restore_session(
        &mut self,
        tracks: Vec<Track>,
        index: usize,
        position: Duration,
        shuffle: bool,
        repeat: RepeatMode,
    ) {
        self.queue = Queue::from_session(tracks, index, shuffle, repeat);
        self.position = position;
        self.state = PlaybackState::Paused;
        self.current = self.queue.current().cloned();
        info!(
            "restored session: {} tracks, index {index}, paused",
            self.queue.len()
        );
    }

    pub fn shutdown(&self) {
        self.engine.send(EngineCmd::Shutdown);
    }
}

async fn resolve_download(
    epoch: u64,
    play_epoch: &AtomicU64,
    video_id: &str,
    title: &str,
    resolver: &Resolver,
    downloader: &Downloader,
    event_tx: &mpsc::UnboundedSender<AppEvent>,
) -> Option<std::sync::Arc<GrowingFile>> {
    let alive = || play_epoch.load(Ordering::SeqCst) == epoch;
    let stream = match resolver.resolve(video_id).await {
        Ok(s) => s,
        Err(e) => {
            if !alive() {
                return None;
            }
            warn!("resolve failed for {title}: {e}, retrying…");
            resolver.invalidate(video_id).await;
            match resolver.resolve(video_id).await {
                Ok(s) => s,
                Err(e2) => {
                    if !alive() {
                        return None;
                    }
                    warn!("resolve retry failed: {e2}");
                    let _ = event_tx.send(AppEvent::Player(PlayerEvent::Error(format!(
                        "Couldn't play {title} — skipping"
                    ))));
                    return None;
                }
            }
        }
    };
    if !alive() {
        return None;
    }
    let tx = event_tx.clone();
    let vid = video_id.to_string();
    match downloader
        .get(video_id, &stream, move |pct| {
            let _ = tx.send(AppEvent::Player(PlayerEvent::DownloadProgress {
                video_id: vid.clone(),
                pct,
            }));
        })
        .await
    {
        Ok(f) => Some(f),
        Err(e) => {
            if alive() {
                warn!("download failed: {e}");
                let _ = event_tx.send(AppEvent::Player(PlayerEvent::Error(format!(
                    "Couldn't play {title} — skipping"
                ))));
            }
            None
        }
    }
}

fn prefetch_track(track: Track, resolver: Resolver, downloader: Downloader) {
    if downloader.is_cached(&track.video_id) {
        return;
    }
    let video_id = track.video_id;
    tokio::spawn(async move {
        if let Ok(stream) = resolver.resolve(&video_id).await {
            let _ = downloader.get(&video_id, &stream, |_| {}).await;
        }
    });
}
