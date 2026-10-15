pub mod innertube;
pub mod model;
pub mod ytmapi;

use crate::action::{AppEvent, AuthStateEvent, Command, SearchFilter};
use crate::api::innertube::InnertubeClient;
use crate::api::model::*;
use crate::auth::AuthSession;
use crate::paths::Paths;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use anyhow::Result;
use tokio::sync::mpsc;
use tracing::warn;


pub struct ApiService {
    client: InnertubeClient,
    tx: mpsc::UnboundedSender<AppEvent>,
    paths: Arc<Paths>,
    /// Guards browser-based session recovery so parallel failures trigger it only once.
    recovery: Arc<Mutex<RecoveryState>>,
}

#[derive(Default)]
struct RecoveryState {
    running: bool,
    last_ok: Option<Instant>,
}

impl ApiService {
    pub fn new(
        cookies: Option<String>,
        tx: mpsc::UnboundedSender<AppEvent>,
        paths: Arc<Paths>,
    ) -> Result<Self> {
        Ok(Self {
            client: InnertubeClient::new(cookies)?,
            tx,
            paths,
            recovery: Arc::new(Mutex::new(RecoveryState::default())),
        })
    }

    pub fn set_cookies(&mut self, cookies: Option<String>) {
        self.client.set_cookies(cookies);
    }

    pub fn apply_session(&mut self, session: Option<&AuthSession>) {
        if let Some(s) = session {
            self.client.apply_session(s);
        } else {
            self.client.set_cookies(None);
            self.client.set_auth_meta(None, None);
        }
    }

    fn branch(&self) -> Self {
        Self {
            client: self.client.clone(),
            tx: self.tx.clone(),
            paths: self.paths.clone(),
            recovery: self.recovery.clone(),
        }
    }

    /// Reports an API failure. A signed-out response first tries to re-read the session
    /// from the browser so the user is only asked to log in when that really fails.
    async fn emit_api_error(&mut self, e: anyhow::Error) {
        let msg = e.to_string();
        if !msg.contains("SESSION_EXPIRED") {
            let _ = self.tx.send(AppEvent::Error(msg));
            return;
        }
        {
            let mut r = self.recovery.lock().unwrap();
            if r.running {
                return;
            }
            let recent = r.last_ok.is_some_and(|t| t.elapsed() < Duration::from_secs(60));
            if recent {
                drop(r);
                let _ = self.tx.send(AppEvent::AuthState(AuthStateEvent::SessionExpired(msg)));
                return;
            }
            r.running = true;
        }
        let recovered = crate::auth::recover_session(self.paths.clone()).await;
        {
            let mut r = self.recovery.lock().unwrap();
            r.running = false;
            if matches!(recovered, crate::auth::Recovery::Refreshed(_)) {
                r.last_ok = Some(Instant::now());
            }
        }
        match recovered {
            crate::auth::Recovery::Refreshed(session) => {
                self.client.apply_session(&session);
                let _ = self.tx.send(AppEvent::AuthState(AuthStateEvent::SignedIn(session)));
            }
            crate::auth::Recovery::Transient(e) => {
                warn!("session check failed, keeping saved session: {e}");
                let _ = self.tx.send(AppEvent::Error(
                    "Could not reach YouTube to refresh the session — will retry".into(),
                ));
            }
            crate::auth::Recovery::Rejected(_) => {
                let _ = self.tx.send(AppEvent::AuthState(AuthStateEvent::SessionExpired(msg)));
            }
        }
    }

    /// Saves session cookie changes Google sent since the last call.
    fn persist_rotated_cookies(&self) {
        let updates = self.client.take_cookie_updates();
        if updates.is_empty() {
            return;
        }
        let paths = self.paths.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(e) = crate::auth::apply_cookie_updates(paths, &updates) {
                warn!("could not persist rotated cookies: {e}");
            }
        });
    }

    pub async fn handle(&mut self, cmd: Command) {
        self.handle_inner(cmd).await;
        self.persist_rotated_cookies();
    }

    async fn handle_inner(&mut self, cmd: Command) {
        match cmd {
            Command::Search { query, filter } => {
                let filter_str = match filter {
                    SearchFilter::Songs => Some("songs"),
                    SearchFilter::Albums => Some("albums"),
                    SearchFilter::Artists => Some("artists"),
                    SearchFilter::Playlists => Some("playlists"),
                };
                match self.client.search(&query, filter_str).await {
                    Ok(bundle) => {
                        let _ = self.tx.send(AppEvent::SearchResults {
                            query,
                            filter,
                            tracks: bundle.tracks,
                            albums: bundle.albums,
                            artists: bundle.artists,
                            playlists: bundle.playlists,
                        });
                    }
                    Err(e) => {
                        let _ = self.tx.send(AppEvent::Error(format!("Search failed: {e}")));
                    }
                }
            }
            Command::SearchSuggestions(query) => {
                match self.client.search_suggestions(&query).await {
                    Ok(s) => {
                        let _ = self.tx.send(AppEvent::SearchSuggestions(s));
                    }
                    Err(e) => warn!("suggestions: {e}"),
                }
            }
            Command::LoadHome { gen } => match self.client.home().await {
                Ok(shelves) => {
                    let _ = self.tx.send(AppEvent::Home { shelves, gen });
                }
                Err(e) => {
                    let _ = self.tx.send(AppEvent::Error(format!("Home failed: {e}")));
                }
            },
            Command::LoadAlbum(id) => match self.client.album(&id).await {
                Ok(a) => {
                    let _ = self.tx.send(AppEvent::AlbumLoaded(a));
                }
                Err(e) => {
                    let _ = self.tx.send(AppEvent::Error(format!("Album failed: {e}")));
                }
            },
            Command::LoadArtist(id) => match self.client.artist(&id).await {
                Ok(a) => {
                    let _ = self.tx.send(AppEvent::ArtistLoaded(a));
                }
                Err(e) => {
                    let _ = self.tx.send(AppEvent::Error(format!("Artist failed: {e}")));
                }
            },
            Command::LoadPlaylist(id) => match self.client.playlist(&id).await {
                Ok(p) => {
                    let _ = self.tx.send(AppEvent::PlaylistLoaded(p));
                }
                Err(e) => {
                    let _ = self.tx.send(AppEvent::Error(format!("Playlist failed: {e}")));
                }
            },
            Command::LoadLibraryPlaylists => match self.client.library_playlists().await {
                Ok(p) => {
                    let _ = self.tx.send(AppEvent::LibraryPlaylists(p));
                }
                Err(e) => {
                    self.emit_api_error(e.context("Library playlists")).await;
                }
            },
            Command::LoadLikedSongs => match self.client.liked_songs().await {
                Ok(t) => {
                    let _ = self.tx.send(AppEvent::LikedSongs(t));
                }
                Err(e) => {
                    self.emit_api_error(e.context("Liked songs")).await;
                }
            },
            Command::LoadLibraryAlbums => match self.client.library_albums().await {
                Ok(a) => {
                    let _ = self.tx.send(AppEvent::LibraryAlbums(a));
                }
                Err(e) => {
                    self.emit_api_error(e.context("Library albums")).await;
                }
            },
            Command::LoadLibraryArtists => match self.client.library_artists().await {
                Ok(a) => {
                    let _ = self.tx.send(AppEvent::LibraryArtists(a));
                }
                Err(e) => {
                    self.emit_api_error(e.context("Library artists")).await;
                }
            },
            Command::LoadHistory => match self.client.history().await {
                Ok(t) => {
                    let _ = self.tx.send(AppEvent::History(t));
                }
                Err(e) => {
                    self.emit_api_error(e.context("History")).await;
                }
            },
            Command::LoadRadio(video_id) => match self.client.watch_playlist(&video_id).await {
                Ok((tracks, _)) => {
                    let _ = self.tx.send(AppEvent::RadioTracks {
                        seed: video_id,
                        tracks,
                    });
                }
                Err(e) => {
                    let _ = self.tx.send(AppEvent::Error(format!("Radio failed: {e}")));
                }
            },
            Command::Rate { video_id, liked } => match self.client.rate(&video_id, liked).await {
                Ok(()) => {
                    let _ = self.tx.send(AppEvent::RateDone { video_id, liked });
                }
                Err(e) => {
                    let _ = self.tx.send(AppEvent::Error(format!("Rate failed: {e}")));
                }
            },
            Command::AddToPlaylist {
                playlist_id,
                video_ids,
            } => match self.client.add_to_playlist(&playlist_id, &video_ids).await {
                Ok(()) => {
                    let _ = self.tx.send(AppEvent::AddedToPlaylist);
                }
                Err(e) => {
                    let _ = self.tx.send(AppEvent::Error(format!("Add to playlist: {e}")));
                }
            },
            Command::CreatePlaylist { title } => match self.client.create_playlist(&title).await {
                Ok(p) => {
                    let _ = self.tx.send(AppEvent::PlaylistCreated(p));
                }
                Err(e) => {
                    let _ = self.tx.send(AppEvent::Error(format!("Create playlist: {e}")));
                }
            },
            Command::LoadLyrics {
                video_id,
                browse_id,
                track: _,
            } => {
                if let Some(bid) = browse_id {
                    if let Ok(Some(plain)) = self.client.lyrics(&bid).await {
                        let _ = self.tx.send(AppEvent::LyricsLoaded {
                            video_id,
                            lyrics: Lyrics {
                                synced: None,
                                plain: Some(plain),
                                source: "YouTube Music".into(),
                            },
                        });
                    }
                }
            }
            Command::ValidateCookies { cookies, .. } => {
                self.set_cookies(Some(cookies));
            }
            Command::ApplyAuth(session) => {
                self.client.apply_session(&session);
            }
            Command::ClearAuth => {
                self.apply_session(None);
            }
            _ => {}
        }
    }
}

pub fn spawn_api_worker(
    paths: Arc<Paths>,
    cookies: Option<String>,
    mut cmd_rx: mpsc::UnboundedReceiver<Command>,
    event_tx: mpsc::UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        let api = match ApiService::new(cookies, event_tx.clone(), paths) {
            Ok(a) => a,
            Err(e) => {
                let _ = event_tx.send(AppEvent::Error(format!("API init failed: {e}")));
                return;
            }
        };
        // Auth updates run inline; browse/search/library run concurrently so a playlist
        // open is not stuck behind a large library or home fetch.
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                Command::ApplyAuth(_) | Command::ClearAuth | Command::ValidateCookies { .. } => {
                    let mut svc = api.branch();
                    svc.handle(cmd).await;
                }
                other => {
                    let mut svc = api.branch();
                    tokio::spawn(async move {
                        svc.handle(other).await;
                    });
                }
            }
        }
    });
}
