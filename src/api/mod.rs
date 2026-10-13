pub mod innertube;
pub mod model;
pub mod ytmapi;

use crate::action::{AppEvent, AuthStateEvent, Command, SearchFilter};
use crate::api::innertube::InnertubeClient;
use crate::api::model::*;
use crate::auth::AuthSession;
use anyhow::{Context, Result};
use tokio::sync::mpsc;
use tracing::warn;

fn emit_api_error(tx: &mpsc::UnboundedSender<AppEvent>, e: anyhow::Error) {
    let msg = e.to_string();
    if msg.contains("SESSION_EXPIRED") {
        let _ = tx.send(AppEvent::AuthState(AuthStateEvent::SessionExpired(msg)));
    } else {
        let _ = tx.send(AppEvent::Error(msg));
    }
}

pub struct ApiService {
    client: InnertubeClient,
    tx: mpsc::UnboundedSender<AppEvent>,
}

impl ApiService {
    pub fn new(cookies: Option<String>, tx: mpsc::UnboundedSender<AppEvent>) -> Result<Self> {
        Ok(Self {
            client: InnertubeClient::new(cookies)?,
            tx,
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
        }
    }

    pub async fn handle(&mut self, cmd: Command) {
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
                    emit_api_error(&self.tx, e.context("Library playlists"));
                }
            },
            Command::LoadLikedSongs => match self.client.liked_songs().await {
                Ok(t) => {
                    let _ = self.tx.send(AppEvent::LikedSongs(t));
                }
                Err(e) => {
                    emit_api_error(&self.tx, e.context("Liked songs"));
                }
            },
            Command::LoadLibraryAlbums => match self.client.library_albums().await {
                Ok(a) => {
                    let _ = self.tx.send(AppEvent::LibraryAlbums(a));
                }
                Err(e) => {
                    emit_api_error(&self.tx, e.context("Library albums"));
                }
            },
            Command::LoadLibraryArtists => match self.client.library_artists().await {
                Ok(a) => {
                    let _ = self.tx.send(AppEvent::LibraryArtists(a));
                }
                Err(e) => {
                    emit_api_error(&self.tx, e.context("Library artists"));
                }
            },
            Command::LoadHistory => match self.client.history().await {
                Ok(t) => {
                    let _ = self.tx.send(AppEvent::History(t));
                }
                Err(e) => {
                    emit_api_error(&self.tx, e.context("History"));
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
    cookies: Option<String>,
    mut cmd_rx: mpsc::UnboundedReceiver<Command>,
    event_tx: mpsc::UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        let api = match ApiService::new(cookies, event_tx.clone()) {
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
