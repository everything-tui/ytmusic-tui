use crate::api::model::*;
use crate::auth::{manual, sapisid, USER_AGENT};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{debug, warn};

const BASE: &str = "https://music.youtube.com/youtubei/v1";
/// Public WEB_REMIX key used by the YouTube Music web client / ytmusicapi.
const INNERTUBE_API_KEY: &str = "AIzaSyC9XL3ZjWddXya6X74dJoCTL-WEYFDNX30";
/// Upper bound on follow-up pages per list (~100 rows each), so a huge library can't stall.
const MAX_CONTINUATION_PAGES: usize = 100;

#[derive(Clone)]
pub struct InnertubeClient {
    http: reqwest::Client,
    cookies: Arc<Mutex<Option<String>>>,
    visitor_id: Arc<Mutex<Option<String>>>,
    user_session_id: Arc<Mutex<Option<String>>>,
    /// Set when a response rotated session cookies that have not been persisted yet.
    pending_cookie_updates: Arc<Mutex<Vec<(String, String)>>>,
}

impl InnertubeClient {
    pub fn new(cookies: Option<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .gzip(true)
            .timeout(Duration::from_secs(20))
            .build()?;
        Ok(Self {
            http,
            cookies: Arc::new(Mutex::new(cookies)),
            visitor_id: Arc::new(Mutex::new(None)),
            user_session_id: Arc::new(Mutex::new(None)),
            pending_cookie_updates: Arc::new(Mutex::new(Vec::new())),
        })
    }

    pub fn set_cookies(&mut self, cookies: Option<String>) {
        *self.cookies.lock().unwrap() = cookies;
    }

    pub fn set_auth_meta(&mut self, visitor_id: Option<String>, user_session_id: Option<String>) {
        *self.visitor_id.lock().unwrap() = visitor_id;
        *self.user_session_id.lock().unwrap() = user_session_id;
    }

    pub fn apply_session(&mut self, session: &crate::auth::AuthSession) {
        self.set_cookies(Some(session.cookies.clone()));
        self.set_auth_meta(session.visitor_id.clone(), session.user_session_id.clone());
    }

    /// Cookie changes Google sent since the last call, to be merged into the saved session.
    pub fn take_cookie_updates(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.pending_cookie_updates.lock().unwrap())
    }

    pub fn cookies(&self) -> Option<String> {
        self.cookies.lock().unwrap().clone()
    }

    fn client_version() -> String {
        // Match the Music web client's dated build style (e.g. 1.20261011.16.00).
        format!("1.{}.16.00", chrono::Utc::now().format("%Y%m%d"))
    }

    fn context() -> Value {
        json!({
            "client": {
                "clientName": "WEB_REMIX",
                "clientVersion": Self::client_version(),
                "hl": "en",
                "gl": "US"
            }
        })
    }

    async fn post(&self, endpoint: &str, body: Value) -> Result<Value> {
        self.post_with_query(endpoint, body, &[]).await
    }

    async fn post_with_query(
        &self,
        endpoint: &str,
        mut body: Value,
        query: &[(&str, &str)],
    ) -> Result<Value> {
        body.as_object_mut()
            .context("body object")?
            .insert("context".into(), Self::context());

        let mut url = url::Url::parse(&format!(
            "{BASE}/{endpoint}?prettyPrint=false&key={INNERTUBE_API_KEY}"
        ))?;
        url.query_pairs_mut().extend_pairs(query);
        let cookies = self.cookies();
        let visitor_id = self.visitor_id.lock().unwrap().clone();
        let user_session_id = self.user_session_id.lock().unwrap().clone();
        let client_version = Self::client_version();

        let mut req = self
            .http
            .post(url)
            .header("Origin", "https://music.youtube.com")
            .header("X-Origin", "https://music.youtube.com")
            .header("Referer", "https://music.youtube.com/")
            .header("Content-Type", "application/json")
            .header("Accept", "*/*")
            .header("X-Youtube-Client-Name", "67")
            .header("X-Youtube-Client-Version", &client_version)
            .json(&body);

        if let Some(ref v) = visitor_id {
            req = req.header("X-Goog-Visitor-Id", v);
        }

        if let Some(ref cookies) = cookies {
            req = req.header("Cookie", cookies);
            if let Some(sap) = manual::sapisid_from_cookies(cookies) {
                let auth = sapisid::authorization_header_parts(
                    &sap,
                    manual::papisid_1p_from_cookies(cookies).as_deref(),
                    manual::papisid_3p_from_cookies(cookies).as_deref(),
                    user_session_id.as_deref(),
                );
                req = req
                    .header("Authorization", auth)
                    .header("X-Goog-AuthUser", "0")
                    .header("X-Youtube-Bootstrap-Logged-In", "true");
            }
        }

        let resp = req.send().await.with_context(|| format!("innertube {endpoint} request"))?;
        let status = resp.status();

        // Absorb rotated session cookies (SIDCC / PSIDTS) so the jar stays alive longer.
        if let Some(ref current) = cookies {
            let mut updates = Vec::new();
            for val in resp.headers().get_all(reqwest::header::SET_COOKIE) {
                if let Ok(s) = val.to_str() {
                    if let Some(pair) = manual::parse_set_cookie_pair(s) {
                        updates.push(pair);
                    }
                }
            }
            if !updates.is_empty() {
                let merged = manual::merge_cookie_header(current, &updates);
                if cookie_set(&merged) != cookie_set(current) {
                    *self.cookies.lock().unwrap() = Some(merged);
                    self.pending_cookie_updates.lock().unwrap().extend(updates);
                }
            }
        }

        let text = resp.text().await?;
        if !status.is_success() {
            anyhow::bail!("innertube {endpoint} HTTP {status}: {}", &text[..text.len().min(200)]);
        }
        let json: Value = serde_json::from_str(&text).context("parse innertube json")?;

        // Keep visitor id fresh when YouTube rotates it.
        if let Some(v) = json
            .pointer("/responseContext/visitorData")
            .and_then(|v| v.as_str())
        {
            *self.visitor_id.lock().unwrap() = Some(v.to_string());
        }

        Ok(json)
    }

    /// True when YouTube explicitly reports the request as signed out.
    pub fn response_signed_out(json: &Value) -> bool {
        crate::auth::logged_in_from_response(json) == Some(false)
    }

    pub async fn search(&self, query: &str, filter: Option<&str>) -> Result<SearchBundle> {
        let mut body = json!({ "query": query });
        if let Some(params) = filter.and_then(search_params) {
            body["params"] = json!(params);
        }
        let json = self.post("search", body).await?;
        Ok(parse_search(&json))
    }

    pub async fn search_suggestions(&self, query: &str) -> Result<Vec<String>> {
        let body = json!({ "input": query });
        let json = self.post("music/get_search_suggestions", body).await?;
        Ok(parse_suggestions(&json))
    }

    pub async fn home(&self) -> Result<Vec<Shelf>> {
        let body = json!({ "browseId": "FEmusic_home" });
        let json = self.post("browse", body).await?;
        Ok(parse_home(&json))
    }

    pub async fn album(&self, browse_id: &str) -> Result<Album> {
        let body = json!({ "browseId": browse_id });
        let json = self.post("browse", body).await?;
        parse_album(&json, browse_id)
    }

    pub async fn artist(&self, browse_id: &str) -> Result<Artist> {
        let body = json!({ "browseId": browse_id });
        let json = self.post("browse", body).await?;
        parse_artist(&json, browse_id)
    }

    pub async fn playlist(&self, playlist_id: &str) -> Result<Playlist> {
        let browse_id = if playlist_id.starts_with("VL") {
            playlist_id.to_string()
        } else {
            format!("VL{playlist_id}")
        };
        let body = json!({ "browseId": browse_id });
        let json = self.post("browse", body).await?;
        self.full_playlist(&json, playlist_id).await
    }

    /// Parses a playlist page and appends every further page of tracks.
    async fn full_playlist(&self, json: &Value, id: &str) -> Result<Playlist> {
        let mut pl = parse_playlist(json, id)?;
        // Only follow the track shelf's token, not the "related" section's.
        let shelf = find_key(json, "musicPlaylistShelfRenderer")
            .or_else(|| find_key(json, "playlistVideoListRenderer"));
        let token = shelf.and_then(continuation_token);
        for page in self.browse_continuations(token).await {
            visit_contents(&page, &mut |item| {
                if let Some(t) = parse_flexible_track(item) {
                    pl.tracks.push(t);
                }
            });
        }
        let mut seen = std::collections::HashSet::new();
        pl.tracks.retain(|t| seen.insert(t.video_id.clone()));
        pl.track_count = Some(pl.tracks.len() as u32);
        Ok(pl)
    }

    /// Fetches the pages after `token`, following each page's own token.
    /// Failures end the walk early so the caller still gets what loaded.
    async fn browse_continuations(&self, mut token: Option<Continuation>) -> Vec<Value> {
        let mut pages = Vec::new();
        while let Some(c) = token.take() {
            if pages.len() >= MAX_CONTINUATION_PAGES {
                warn!("stopping after {MAX_CONTINUATION_PAGES} continuation pages");
                break;
            }
            let resp = match c {
                // Newer responses take the token in the body…
                Continuation::Command(t) => {
                    self.post("browse", json!({ "continuation": t })).await
                }
                // …older `nextContinuationData` ones as query parameters.
                Continuation::Next(t) => {
                    self.post_with_query(
                        "browse",
                        json!({}),
                        &[("ctoken", &t), ("continuation", &t), ("type", "next")],
                    )
                    .await
                }
            };
            match resp {
                Ok(page) => {
                    token = continuation_token(&page);
                    pages.push(page);
                }
                Err(e) => {
                    warn!("continuation page failed: {e:#}");
                    break;
                }
            }
        }
        pages
    }

    /// First page plus every continuation page of a library browse.
    async fn browse_all(&self, browse_id: &str, what: &str) -> Result<Vec<Value>> {
        let json = self.post("browse", json!({ "browseId": browse_id })).await?;
        self.require_signed_in(&json, what)?;
        let token = continuation_token(&json);
        let mut pages = vec![json];
        pages.extend(self.browse_continuations(token).await);
        Ok(pages)
    }

    pub async fn liked_songs(&self) -> Result<Vec<Track>> {
        let body = json!({ "browseId": "VLLM" });
        let json = self.post("browse", body).await?;
        self.require_signed_in(&json, "liked songs")?;
        let pl = self.full_playlist(&json, "LM").await?;
        Ok(pl.tracks)
    }

    pub async fn library_playlists(&self) -> Result<Vec<Playlist>> {
        let mut playlists = Vec::new();
        for page in self
            .browse_all("FEmusic_liked_playlists", "library playlists")
            .await?
        {
            playlists.extend(parse_library_playlists(&page));
        }
        let mut seen = std::collections::HashSet::new();
        playlists.retain(|p| seen.insert(p.id.clone()));
        // YouTube lists "Liked Music" with a generated icon or no artwork at all, so
        // always use the cover of the most recently liked song instead.
        let is_liked = |p: &Playlist| {
            p.id == "LM" || p.id == "VLLM" || p.title.eq_ignore_ascii_case("liked music")
        };
        if playlists.iter().any(is_liked) {
            if let Ok(resp) = self.post("browse", json!({ "browseId": "VLLM" })).await {
                if let Ok(pl) = parse_playlist(&resp, "LM") {
                    let thumbs = pl
                        .tracks
                        .iter()
                        .find_map(|t| t.art_url())
                        .map(|url| {
                            vec![Thumbnail {
                                url,
                                width: Some(544),
                                height: Some(544),
                            }]
                        })
                        .or_else(|| (!pl.thumbnails.is_empty()).then(|| pl.thumbnails.clone()));
                    if let Some(thumbs) = thumbs {
                        for p in playlists.iter_mut().filter(|p| is_liked(p)) {
                            p.thumbnails = thumbs.clone();
                        }
                    }
                }
            }
        }
        Ok(playlists)
    }

    pub async fn library_albums(&self) -> Result<Vec<Album>> {
        let pages = self.browse_all("FEmusic_liked_albums", "library albums").await?;
        Ok(pages.iter().flat_map(parse_library_albums).collect())
    }

    pub async fn library_artists(&self) -> Result<Vec<Artist>> {
        let pages = self
            .browse_all("FEmusic_library_corpus_track_artists", "library artists")
            .await?;
        Ok(pages.iter().flat_map(parse_library_artists).collect())
    }

    pub async fn history(&self) -> Result<Vec<Track>> {
        let body = json!({ "browseId": "FEmusic_history" });
        let json = self.post("browse", body).await?;
        self.require_signed_in(&json, "history")?;
        let tracks = parse_history(&json);
        dump_if_empty("history", tracks.is_empty(), &json);
        Ok(tracks)
    }

    fn require_signed_in(&self, json: &Value, what: &str) -> Result<()> {
        if Self::response_signed_out(json) {
            anyhow::bail!(
                "SESSION_EXPIRED: signed out while loading {what}. Press L to sign in again."
            );
        }
        Ok(())
    }

    pub async fn watch_playlist(&self, video_id: &str) -> Result<(Vec<Track>, Option<String>)> {
        let body = json!({
            "videoId": video_id,
            "playlistId": format!("RDAMVM{video_id}"),
            "isAudioOnly": true
        });
        let json = self.post("next", body).await?;
        Ok(parse_watch_playlist(&json))
    }

    pub async fn lyrics(&self, browse_id: &str) -> Result<Option<String>> {
        let body = json!({ "browseId": browse_id });
        let json = self.post("browse", body).await?;
        Ok(parse_lyrics_plain(&json))
    }

    pub async fn rate(&self, video_id: &str, like: bool) -> Result<()> {
        let endpoint = if like { "like/like" } else { "like/removelike" };
        let body = json!({ "target": { "videoId": video_id } });
        let _ = self.post(endpoint, body).await?;
        Ok(())
    }

    pub async fn add_to_playlist(&self, playlist_id: &str, video_ids: &[String]) -> Result<()> {
        let body = json!({
            "playlistId": playlist_id,
            "actions": video_ids.iter().map(|id| json!({
                "action": "ACTION_ADD_VIDEO",
                "addedVideoId": id
            })).collect::<Vec<_>>()
        });
        let _ = self.post("browse/edit_playlist", body).await?;
        Ok(())
    }

    pub async fn create_playlist(&self, title: &str) -> Result<Playlist> {
        let body = json!({
            "title": title,
            "privacyStatus": "PRIVATE"
        });
        let json = self.post("playlist/create", body).await?;
        let id = json
            .get("playlistId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        Ok(Playlist {
            id,
            title: title.into(),
            description: None,
            author: None,
            track_count: Some(0),
            thumbnails: vec![],
            tracks: vec![],
        })
    }

    pub async fn account(&self) -> Result<Option<Account>> {
        let body = json!({});
        let json = self.post("account/account_menu", body).await?;
        Ok(parse_account(&json))
    }
}

#[derive(Debug, Default)]
pub struct SearchBundle {
    pub tracks: Vec<Track>,
    pub albums: Vec<Album>,
    pub artists: Vec<Artist>,
    pub playlists: Vec<Playlist>,
}

fn search_params(filter: &str) -> Option<&'static str> {
    // Base64-ish protobuf params used by YT Music
    match filter {
        "songs" => Some("EgWKAQIIAWoKEAkQBRAKEAMQBA%3D%3D"),
        "albums" => Some("EgWKAQIYAWoKEAkQBRAKEAMQBA%3D%3D"),
        "artists" => Some("EgWKAQIgAWoKEAkQBRAKEAMQBA%3D%3D"),
        "playlists" => Some("EgeKAQQoADgBaggoQCRAHENIEEAoQAQ%3D%3D"),
        _ => None,
    }
}

// ---- parsers ----

pub(crate) fn parse_search(json: &Value) -> SearchBundle {
    let mut bundle = SearchBundle::default();
    visit_contents(json, &mut |item| {
        if let Some(t) = parse_flexible_track(item) {
            bundle.tracks.push(t);
        } else if let Some(a) = parse_two_row_album(item) {
            bundle.albums.push(a);
        } else if let Some(a) = parse_two_row_artist(item) {
            bundle.artists.push(a);
        } else if let Some(p) = parse_two_row_playlist(item) {
            bundle.playlists.push(p);
        }
    });
    bundle
}

fn parse_suggestions(json: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(arr) = json
        .pointer("/contents")
        .and_then(|v| v.as_array())
    {
        for c in arr {
            collect_suggestion_texts(c, &mut out);
        }
    }
    out
}

fn collect_suggestion_texts(v: &Value, out: &mut Vec<String>) {
    if let Some(runs) = v.pointer("/searchSuggestionRenderer/suggestion/runs") {
        if let Some(text) = runs_text(runs) {
            out.push(text);
        }
    }
    match v {
        Value::Object(m) => {
            for val in m.values() {
                collect_suggestion_texts(val, out);
            }
        }
        Value::Array(a) => {
            for val in a {
                collect_suggestion_texts(val, out);
            }
        }
        _ => {}
    }
}

fn parse_home(json: &Value) -> Vec<Shelf> {
    let mut shelves = Vec::new();
    let mut seen_titles = std::collections::HashSet::new();
    visit_carousels(json, &mut |title, items| {
        // Keep first shelf per title — duplicates from nested renderers cause the
        // home feed to look like it's reshuffling as selection moves.
        if seen_titles.insert(title.clone()) {
            shelves.push(Shelf { title, items });
        }
    });
    if shelves.is_empty() {
        // Fallback: dump tracks into a single shelf
        let mut tracks = Vec::new();
        visit_contents(json, &mut |item| {
            if let Some(t) = parse_flexible_track(item) {
                tracks.push(ShelfItem::Track(t));
            }
        });
        if !tracks.is_empty() {
            shelves.push(Shelf {
                title: "Home".into(),
                items: tracks,
            });
        }
    }
    shelves
}

fn parse_album(json: &Value, id: &str) -> Result<Album> {
    let header = find_key(json, "musicDetailHeaderRenderer")
        .or_else(|| find_key(json, "musicResponsiveHeaderRenderer"));
    let title = header
        .and_then(|h| runs_text(h.pointer("/title/runs").unwrap_or(&Value::Null)))
        .unwrap_or_else(|| "Album".into());
    let artists = header
        .map(parse_subtitle_artists)
        .unwrap_or_default();
    let year = header.and_then(|h| {
        runs_text(h.pointer("/subtitle/runs").unwrap_or(&Value::Null))
            .and_then(|s| s.split('•').last().map(|x| x.trim().to_string()))
    });
    let thumbnails = header
        .map(|h| parse_thumbnails(h.pointer("/thumbnail").unwrap_or(&Value::Null)))
        .unwrap_or_default();

    let mut tracks = Vec::new();
    visit_contents(json, &mut |item| {
        if let Some(mut t) = parse_flexible_track(item) {
            if t.album.is_none() {
                t.album = Some(AlbumRef {
                    id: Some(id.to_string()),
                    name: title.clone(),
                });
            }
            tracks.push(t);
        }
    });

    Ok(Album {
        id: id.to_string(),
        title,
        artists,
        year,
        thumbnails,
        tracks,
    })
}

fn parse_artist(json: &Value, id: &str) -> Result<Artist> {
    let header = find_key(json, "musicImmersiveHeaderRenderer")
        .or_else(|| find_key(json, "musicVisualHeaderRenderer"));
    let name = header
        .and_then(|h| runs_text(h.pointer("/title/runs").unwrap_or(&Value::Null)))
        .unwrap_or_else(|| "Artist".into());
    let thumbnails = header
        .map(|h| parse_thumbnails(h.pointer("/thumbnail").unwrap_or(&Value::Null)))
        .unwrap_or_default();
    let description = header.and_then(|h| {
        runs_text(h.pointer("/description/runs").unwrap_or(&Value::Null))
    });

    let mut top_songs = Vec::new();
    let mut albums = Vec::new();
    let mut singles = Vec::new();
    visit_carousels(json, &mut |title, items| {
        let lower = title.to_lowercase();
        for item in items {
            match item {
                ShelfItem::Track(t) if lower.contains("song") || lower.contains("top") => {
                    top_songs.push(t);
                }
                ShelfItem::Album(a) if lower.contains("single") => singles.push(a),
                ShelfItem::Album(a) => albums.push(a),
                ShelfItem::Track(t) => top_songs.push(t),
                _ => {}
            }
        }
    });
    if top_songs.is_empty() {
        visit_contents(json, &mut |item| {
            if let Some(t) = parse_flexible_track(item) {
                top_songs.push(t);
            }
        });
    }

    Ok(Artist {
        id: id.to_string(),
        name,
        thumbnails,
        top_songs,
        albums,
        singles,
        description,
    })
}

fn parse_playlist(json: &Value, id: &str) -> Result<Playlist> {
    let header = find_key(json, "musicDetailHeaderRenderer")
        .or_else(|| find_key(json, "musicResponsiveHeaderRenderer"));
    let title = header
        .and_then(|h| runs_text(h.pointer("/title/runs").unwrap_or(&Value::Null)))
        .unwrap_or_else(|| "Playlist".into());
    let thumbnails = header
        .map(|h| parse_thumbnails(h.pointer("/thumbnail").unwrap_or(&Value::Null)))
        .unwrap_or_default();
    let mut tracks = Vec::new();
    // Prefer the main playlist shelf so related/recommended rows aren't mixed in.
    let shelf = find_key(json, "musicPlaylistShelfRenderer")
        .or_else(|| find_key(json, "playlistVideoListRenderer"));
    let root = shelf.unwrap_or(json);
    visit_contents(root, &mut |item| {
        if let Some(t) = parse_flexible_track(item) {
            tracks.push(t);
        }
    });
    // Deduplicate while preserving order (YT sometimes repeats the playing row).
    let mut seen = std::collections::HashSet::new();
    tracks.retain(|t| seen.insert(t.video_id.clone()));
    let count = tracks.len() as u32;
    Ok(Playlist {
        id: id.trim_start_matches("VL").to_string(),
        title,
        description: None,
        author: None,
        track_count: Some(count),
        thumbnails,
        tracks,
    })
}

fn parse_library_playlists(json: &Value) -> Vec<Playlist> {
    let mut out = Vec::new();
    visit_contents(json, &mut |item| {
        if let Some(p) = parse_two_row_playlist(item).or_else(|| parse_responsive_playlist(item)) {
            out.push(p);
        }
    });
    out
}

fn parse_library_albums(json: &Value) -> Vec<Album> {
    let mut out = Vec::new();
    visit_contents(json, &mut |item| {
        if let Some(a) = parse_two_row_album(item) {
            out.push(a);
        }
    });
    out
}

fn parse_library_artists(json: &Value) -> Vec<Artist> {
    let mut out = Vec::new();
    visit_contents(json, &mut |item| {
        if let Some(a) = parse_two_row_artist(item) {
            out.push(a);
        }
    });
    out
}

fn parse_history(json: &Value) -> Vec<Track> {
    let mut out = Vec::new();
    visit_contents(json, &mut |item| {
        if let Some(t) = parse_flexible_track(item) {
            out.push(t);
        }
    });
    out
}

fn parse_watch_playlist(json: &Value) -> (Vec<Track>, Option<String>) {
    let mut tracks = Vec::new();
    if let Some(arr) = json
        .pointer("/contents/singleColumnMusicWatchNextResultsRenderer/tabbedRenderer/watchNextTabbedResultsRenderer/tabs/0/tabRenderer/content/musicQueueRenderer/content/playlistPanelRenderer/contents")
        .and_then(|v| v.as_array())
        .or_else(|| {
            json.pointer("/contents/singleColumnMusicWatchNextResultsRenderer/tabbedRenderer/watchNextTabbedResultsRenderer/tabs")
                .and_then(|_| None)
        })
    {
        for item in arr {
            if let Some(t) = parse_playlist_panel_track(item) {
                tracks.push(t);
            }
        }
    }
    // Broader fallback
    if tracks.is_empty() {
        visit_contents(json, &mut |item| {
            if let Some(t) = parse_flexible_track(item) {
                tracks.push(t);
            }
        });
    }

    let lyrics_id = json
        .pointer("/contents/singleColumnMusicWatchNextResultsRenderer/tabbedRenderer/watchNextTabbedResultsRenderer/tabs")
        .and_then(|t| t.as_array())
        .and_then(|tabs| {
            tabs.iter().find_map(|tab| {
                let title = tab.pointer("/tabRenderer/title").and_then(|v| v.as_str()).unwrap_or("");
                if title.eq_ignore_ascii_case("lyrics") || title.eq_ignore_ascii_case("lyrics") {
                    tab.pointer("/tabRenderer/endpoint/browseEndpoint/browseId")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                } else {
                    None
                }
            })
        });

    (tracks, lyrics_id)
}

fn parse_lyrics_plain(json: &Value) -> Option<String> {
    find_key(json, "messageRenderer")
        .and_then(|m| runs_text(m.pointer("/text/runs").unwrap_or(&Value::Null)))
        .or_else(|| {
            find_key(json, "musicDescriptionShelfRenderer").and_then(|m| {
                runs_text(m.pointer("/description/runs").unwrap_or(&Value::Null))
            })
        })
}

fn parse_account(json: &Value) -> Option<Account> {
    let name = find_text_by_key(json, "accountName")?;
    Some(Account {
        name,
        channel_id: None,
        thumbnails: vec![],
    })
}

// ---- item parsers ----

fn parse_flexible_track(item: &Value) -> Option<Track> {
    parse_responsive_track(item).or_else(|| parse_playlist_panel_track(item))
}

fn parse_responsive_track(item: &Value) -> Option<Track> {
    let r = item
        .get("musicResponsiveListItemRenderer")
        .or_else(|| item.pointer("/musicResponsiveListItemRenderer"))?;

    let video_id = r
        .pointer("/playlistItemData/videoId")
        .or_else(|| r.pointer("/flexColumns/0/musicResponsiveListItemFlexColumnRenderer/text/runs/0/navigationEndpoint/watchEndpoint/videoId"))
        .or_else(|| r.pointer("/overlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer/playNavigationEndpoint/watchEndpoint/videoId"))
        .and_then(|v| v.as_str())?
        .to_string();

    let title = r
        .pointer("/flexColumns/0/musicResponsiveListItemFlexColumnRenderer/text/runs")
        .and_then(runs_text)
        .unwrap_or_else(|| "Unknown".into());

    let artists = r
        .pointer("/flexColumns/1/musicResponsiveListItemFlexColumnRenderer/text/runs")
        .map(parse_artist_runs)
        .unwrap_or_default();

    let album = r
        .pointer("/flexColumns/2/musicResponsiveListItemFlexColumnRenderer/text/runs/0")
        .and_then(|run| {
            let name = run.get("text")?.as_str()?.to_string();
            // Duration often sits in flexColumns[2] when album is omitted — don't treat it as one.
            if parse_duration_label(&name).is_some() && !name.contains(' ') {
                return None;
            }
            let id = run
                .pointer("/navigationEndpoint/browseEndpoint/browseId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            Some(AlbumRef { id, name })
        });

    let duration = extract_track_duration(r);

    let thumbnails = parse_thumbnails(r.pointer("/thumbnail").unwrap_or(&Value::Null));
    let explicit = r.pointer("/badges").is_some();

    Some(Track {
        video_id,
        title,
        artists,
        album,
        duration,
        thumbnails,
        explicit,
        liked: None,
        from_radio: false,
    })
}

fn parse_playlist_panel_track(item: &Value) -> Option<Track> {
    let r = item.get("playlistPanelVideoRenderer")?;
    let video_id = r.get("videoId")?.as_str()?.to_string();
    let title = runs_text(r.pointer("/title/runs").unwrap_or(&Value::Null))
        .unwrap_or_else(|| "Unknown".into());
    let artists = r
        .pointer("/longBylineText/runs")
        .or_else(|| r.pointer("/shortBylineText/runs"))
        .map(parse_artist_runs)
        .unwrap_or_default();
    let duration = extract_track_duration(r);
    let thumbnails = parse_thumbnails(r.pointer("/thumbnail").unwrap_or(&Value::Null));
    Some(Track {
        video_id,
        title,
        artists,
        album: None,
        duration,
        thumbnails,
        explicit: false,
        liked: None,
        from_radio: false,
    })
}

fn parse_two_row_album(item: &Value) -> Option<Album> {
    let r = item.get("musicTwoRowItemRenderer")?;
    let browse = r.pointer("/navigationEndpoint/browseEndpoint")?;
    let page_type = browse
        .pointer("/browseEndpointContextSupportedConfigs/browseEndpointContextMusicConfig/pageType")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if page_type != "MUSIC_PAGE_TYPE_ALBUM" && !browse.get("browseId")?.as_str()?.starts_with("MPREb_")
    {
        // still allow MPREb
        if !browse.get("browseId")?.as_str()?.starts_with("MPRE") {
            return None;
        }
    }
    let id = browse.get("browseId")?.as_str()?.to_string();
    let title = runs_text(r.pointer("/title/runs").unwrap_or(&Value::Null))?;
    let artists = r
        .pointer("/subtitle/runs")
        .map(parse_artist_runs)
        .unwrap_or_default();
    let thumbnails = parse_two_row_thumbnails(r);
    Some(Album {
        id,
        title,
        artists,
        year: None,
        thumbnails,
        tracks: vec![],
    })
}

fn parse_two_row_artist(item: &Value) -> Option<Artist> {
    let r = item.get("musicTwoRowItemRenderer")?;
    let browse = r.pointer("/navigationEndpoint/browseEndpoint")?;
    let id = browse.get("browseId")?.as_str()?.to_string();
    if !id.starts_with("UC") {
        let page_type = browse
            .pointer("/browseEndpointContextSupportedConfigs/browseEndpointContextMusicConfig/pageType")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if page_type != "MUSIC_PAGE_TYPE_ARTIST" {
            return None;
        }
    }
    let name = runs_text(r.pointer("/title/runs").unwrap_or(&Value::Null))?;
    let thumbnails = parse_two_row_thumbnails(r);
    Some(Artist {
        id,
        name,
        thumbnails,
        top_songs: vec![],
        albums: vec![],
        singles: vec![],
        description: None,
    })
}

fn is_playlist_browse(browse: &Value) -> Option<String> {
    let id = browse.get("browseId")?.as_str()?.to_string();
    if id.starts_with("VL") || id.starts_with("PL") || id.starts_with("OL") {
        return Some(id);
    }
    let page_type = browse
        .pointer("/browseEndpointContextSupportedConfigs/browseEndpointContextMusicConfig/pageType")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    (page_type == "MUSIC_PAGE_TYPE_PLAYLIST").then_some(id)
}

fn parse_two_row_playlist(item: &Value) -> Option<Playlist> {
    let r = item.get("musicTwoRowItemRenderer")?;
    let browse = r.pointer("/navigationEndpoint/browseEndpoint")?;
    let id = is_playlist_browse(browse)?;
    let title = runs_text(r.pointer("/title/runs").unwrap_or(&Value::Null))?;
    let thumbnails = parse_two_row_thumbnails(r);
    Some(Playlist {
        id: id.trim_start_matches("VL").to_string(),
        title,
        description: None,
        author: None,
        track_count: None,
        thumbnails,
        tracks: vec![],
    })
}

/// Two-row cards usually nest art under `thumbnailRenderer`; some shelves use `thumbnail`.
fn parse_two_row_thumbnails(r: &Value) -> Vec<Thumbnail> {
    let from_renderer = parse_thumbnails(r.pointer("/thumbnailRenderer").unwrap_or(&Value::Null));
    if !from_renderer.is_empty() {
        return from_renderer;
    }
    parse_thumbnails(r.pointer("/thumbnail").unwrap_or(&Value::Null))
}

/// Library / search playlists rendered as a list row instead of a two-row card.
fn parse_responsive_playlist(item: &Value) -> Option<Playlist> {
    let r = item.get("musicResponsiveListItemRenderer")?;
    // Tracks also use this renderer — reject anything with a video id.
    if r.pointer("/playlistItemData/videoId").is_some()
        || r.pointer("/overlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer/playNavigationEndpoint/watchEndpoint/videoId")
            .is_some()
    {
        return None;
    }
    let browse = r
        .pointer("/navigationEndpoint/browseEndpoint")
        .or_else(|| {
            r.pointer(
                "/flexColumns/0/musicResponsiveListItemFlexColumnRenderer/text/runs/0/navigationEndpoint/browseEndpoint",
            )
        })?;
    let id = is_playlist_browse(browse)?;
    let title = r
        .pointer("/flexColumns/0/musicResponsiveListItemFlexColumnRenderer/text/runs")
        .and_then(runs_text)?;
    let author = r
        .pointer("/flexColumns/1/musicResponsiveListItemFlexColumnRenderer/text/runs")
        .and_then(runs_text);
    let thumbnails = parse_thumbnails(r.pointer("/thumbnail").unwrap_or(&Value::Null));
    Some(Playlist {
        id: id.trim_start_matches("VL").to_string(),
        title,
        description: None,
        author,
        track_count: None,
        thumbnails,
        tracks: vec![],
    })
}

fn parse_subtitle_artists(header: &Value) -> Vec<ArtistRef> {
    header
        .pointer("/subtitle/runs")
        .map(parse_artist_runs)
        .unwrap_or_default()
}

fn parse_artist_runs(runs: &Value) -> Vec<ArtistRef> {
    let Some(arr) = runs.as_array() else {
        return vec![];
    };
    arr.iter()
        .filter_map(|run| {
            let name = run.get("text")?.as_str()?.to_string();
            // separator runs such as ", " / " & " / " • " carry no artist
            if name.trim().chars().all(|c| c.is_whitespace() || "•&,/·".contains(c)) {
                return None;
            }
            // skip duration-like
            if parse_duration_label(&name).is_some() && !name.contains(' ') {
                return None;
            }
            let id = run
                .pointer("/navigationEndpoint/browseEndpoint/browseId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            Some(ArtistRef { id, name })
        })
        .collect()
}

fn parse_thumbnails(v: &Value) -> Vec<Thumbnail> {
    let mut out = Vec::new();
    let candidates = [
        v.pointer("/musicThumbnailRenderer/thumbnail/thumbnails"),
        v.pointer("/croppedSquareThumbnailRenderer/thumbnail/thumbnails"),
        v.pointer("/thumbnails"),
        v.as_array().map(|_| v),
    ];
    for c in candidates.into_iter().flatten() {
        if let Some(arr) = c.as_array() {
            for t in arr {
                if let Some(url) = t.get("url").and_then(|u| u.as_str()) {
                    out.push(Thumbnail {
                        url: url.to_string(),
                        width: t.get("width").and_then(|w| w.as_u64()).map(|w| w as u32),
                        height: t.get("height").and_then(|h| h.as_u64()).map(|h| h as u32),
                    });
                }
            }
        }
    }
    out
}

fn parse_duration_label(s: &str) -> Option<Duration> {
    let parts: Vec<&str> = s.trim().split(':').collect();
    if parts.len() == 2 {
        let m: u64 = parts[0].parse().ok()?;
        let sec: u64 = parts[1].parse().ok()?;
        if sec >= 60 {
            return None;
        }
        return Some(Duration::from_secs(m * 60 + sec));
    }
    if parts.len() == 3 {
        let h: u64 = parts[0].parse().ok()?;
        let m: u64 = parts[1].parse().ok()?;
        let sec: u64 = parts[2].parse().ok()?;
        if m >= 60 || sec >= 60 {
            return None;
        }
        return Some(Duration::from_secs(h * 3600 + m * 60 + sec));
    }
    None
}

/// Pull duration from the many shapes YT Music uses across search / library / queue.
fn extract_track_duration(r: &Value) -> Option<Duration> {
    const POINTERS: &[&str] = &[
        "/flexColumns/3/musicResponsiveListItemFlexColumnRenderer/text/runs/0/text",
        "/flexColumns/2/musicResponsiveListItemFlexColumnRenderer/text/runs/0/text",
        "/fixedColumns/0/musicResponsiveListItemFixedColumnRenderer/text/runs/0/text",
        "/fixedColumns/0/musicResponsiveListItemFixedColumnRenderer/text/simpleText",
        "/fixedColumns/0/musicResponsiveListItemFixedColumnRenderer/label",
        "/lengthText/runs/0/text",
        "/lengthText/simpleText",
        "/thumbnailOverlay/musicItemThumbnailOverlayRenderer/content/musicThumbnailBadgeRenderer/durationSeconds",
        "/lengthMs",
        "/lengthText",
    ];
    for ptr in POINTERS {
        if let Some(v) = r.pointer(ptr) {
            if let Some(n) = v.as_u64().or_else(|| v.as_f64().map(|f| f as u64)) {
                // lengthMs vs seconds — treat large values as milliseconds.
                if ptr.ends_with("lengthMs") || n > 86_400 {
                    return Some(Duration::from_millis(n));
                }
                if ptr.contains("durationSeconds") || ptr.contains("length") {
                    return Some(Duration::from_secs(n));
                }
            }
            if let Some(s) = v.as_str() {
                if let Some(d) = parse_duration_label(s) {
                    return Some(d);
                }
            }
        }
    }

    // Scan fixed + flex column runs for a bare mm:ss / h:mm:ss token.
    for key in ["fixedColumns", "flexColumns"] {
        let Some(cols) = r.get(key).and_then(|v| v.as_array()) else {
            continue;
        };
        for col in cols {
            let text = col
                .pointer("/musicResponsiveListItemFixedColumnRenderer/text")
                .or_else(|| col.pointer("/musicResponsiveListItemFlexColumnRenderer/text"))
                .unwrap_or(&Value::Null);
            if let Some(s) = text.get("simpleText").and_then(|v| v.as_str()) {
                if let Some(d) = parse_duration_label(s) {
                    return Some(d);
                }
            }
            if let Some(runs) = text.get("runs").and_then(|v| v.as_array()) {
                for run in runs {
                    if let Some(s) = run.get("text").and_then(|v| v.as_str()) {
                        let t = s.trim();
                        if !t.is_empty()
                            && !t.contains(' ')
                            && parse_duration_label(t).is_some()
                        {
                            return parse_duration_label(t);
                        }
                    }
                }
            }
            if let Some(s) = col
                .pointer("/musicResponsiveListItemFixedColumnRenderer/label")
                .and_then(|v| v.as_str())
            {
                if let Some(d) = parse_duration_label(s) {
                    return Some(d);
                }
            }
        }
    }
    None
}

fn runs_text(runs: &Value) -> Option<String> {
    let arr = runs.as_array()?;
    let s = arr
        .iter()
        .filter_map(|r| r.get("text")?.as_str())
        .collect::<String>();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn visit_contents(json: &Value, f: &mut dyn FnMut(&Value)) {
    match json {
        Value::Object(map) => {
            for (k, v) in map {
                if k == "musicResponsiveListItemRenderer"
                    || k == "musicTwoRowItemRenderer"
                    || k == "playlistPanelVideoRenderer"
                {
                    // wrap so parsers that expect {"music…": …} work
                    let wrapped = json!({ k: v });
                    f(&wrapped);
                }
                visit_contents(v, f);
            }
        }
        Value::Array(arr) => {
            for v in arr {
                visit_contents(v, f);
            }
        }
        _ => {}
    }
}

fn visit_carousels(json: &Value, f: &mut dyn FnMut(String, Vec<ShelfItem>)) {
    match json {
        Value::Object(map) => {
            if let Some(carousel) = map.get("musicCarouselShelfRenderer") {
                let title = carousel
                    .pointer("/header/musicCarouselShelfBasicHeaderRenderer/title/runs")
                    .and_then(runs_text)
                    .unwrap_or_else(|| "Shelf".into());
                let mut items = Vec::new();
                if let Some(contents) = carousel.get("contents").and_then(|c| c.as_array()) {
                    for item in contents {
                        if let Some(t) = parse_flexible_track(item) {
                            items.push(ShelfItem::Track(t));
                        } else if let Some(a) = parse_two_row_album(item) {
                            items.push(ShelfItem::Album(a));
                        } else if let Some(a) = parse_two_row_artist(item) {
                            items.push(ShelfItem::Artist(a));
                        } else if let Some(p) = parse_two_row_playlist(item) {
                            items.push(ShelfItem::Playlist(p));
                        }
                    }
                }
                if !items.is_empty() {
                    f(title, items);
                }
                // Don't walk into this carousel again (avoids duplicate/jittery shelves).
                for (k, v) in map {
                    if k != "musicCarouselShelfRenderer" {
                        visit_carousels(v, f);
                    }
                }
            } else {
                for v in map.values() {
                    visit_carousels(v, f);
                }
            }
        }
        Value::Array(arr) => {
            for v in arr {
                visit_carousels(v, f);
            }
        }
        _ => {}
    }
}

/// A "load more" token from a browse response.
#[derive(Debug, PartialEq)]
enum Continuation {
    /// `continuationItemRenderer.continuationEndpoint.continuationCommand.token`
    Command(String),
    /// `continuations[].nextContinuationData.continuation`
    Next(String),
}

fn continuation_token(v: &Value) -> Option<Continuation> {
    if let Some(t) = find_key(v, "continuationCommand")
        .and_then(|c| c.get("token"))
        .and_then(Value::as_str)
    {
        return Some(Continuation::Command(t.to_string()));
    }
    find_key(v, "nextContinuationData")
        .and_then(|c| c.get("continuation"))
        .and_then(Value::as_str)
        .map(|t| Continuation::Next(t.to_string()))
}

fn find_key<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(map) => {
            if let Some(val) = map.get(key) {
                return Some(val);
            }
            for val in map.values() {
                if let Some(found) = find_key(val, key) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(arr) => {
            for val in arr {
                if let Some(found) = find_key(val, key) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

fn find_text_by_key(v: &Value, key: &str) -> Option<String> {
    match v {
        Value::Object(map) => {
            if let Some(val) = map.get(key) {
                if let Some(s) = val.as_str() {
                    return Some(s.to_string());
                }
                if let Some(s) = runs_text(val.pointer("/runs").unwrap_or(&Value::Null)) {
                    return Some(s);
                }
            }
            for val in map.values() {
                if let Some(s) = find_text_by_key(val, key) {
                    return Some(s);
                }
            }
            None
        }
        Value::Array(arr) => {
            for val in arr {
                if let Some(s) = find_text_by_key(val, key) {
                    return Some(s);
                }
            }
            None
        }
        _ => None,
    }
}

#[allow(dead_code)]
fn _debug_keys(json: &Value) {
    debug!("keys visited");
    let _ = json;
}

#[cfg(test)]
mod fixture_tests {
    use super::*;

    #[test]
    fn parse_search_fixture() {
        let raw = include_str!("../../tests/fixtures/search_song.json");
        let json: Value = serde_json::from_str(raw).unwrap();
        let bundle = parse_search(&json);
        assert!(!bundle.tracks.is_empty(), "expected at least one track");
        assert_eq!(bundle.tracks[0].video_id, "abc123XYZ01");
        assert_eq!(bundle.tracks[0].title, "Blinding Lights");
        assert_eq!(
            bundle.tracks[0].duration,
            Some(Duration::from_secs(3 * 60 + 20))
        );
    }

    #[test]
    fn extract_duration_from_fixed_column_and_subtitle_runs() {
        let fixed = serde_json::json!({
            "fixedColumns": [{
                "musicResponsiveListItemFixedColumnRenderer": {
                    "text": { "simpleText": "4:12" }
                }
            }]
        });
        assert_eq!(
            extract_track_duration(&fixed),
            Some(Duration::from_secs(4 * 60 + 12))
        );

        let embedded = serde_json::json!({
            "flexColumns": [{
                "musicResponsiveListItemFlexColumnRenderer": {
                    "text": { "runs": [{ "text": "Title" }] }
                }
            }, {
                "musicResponsiveListItemFlexColumnRenderer": {
                    "text": { "runs": [
                        { "text": "Artist" },
                        { "text": " • " },
                        { "text": "Album" },
                        { "text": " • " },
                        { "text": "2:05" }
                    ] }
                }
            }]
        });
        assert_eq!(
            extract_track_duration(&embedded),
            Some(Duration::from_secs(2 * 60 + 5))
        );
    }

    #[test]
    fn parse_home_playlist_card_thumbnails() {
        let item = serde_json::json!({
            "musicTwoRowItemRenderer": {
                "title": { "runs": [{ "text": "Daily Mix" }] },
                "navigationEndpoint": {
                    "browseEndpoint": {
                        "browseId": "VLPL999",
                        "browseEndpointContextSupportedConfigs": {
                            "browseEndpointContextMusicConfig": {
                                "pageType": "MUSIC_PAGE_TYPE_PLAYLIST"
                            }
                        }
                    }
                },
                "thumbnailRenderer": {
                    "musicThumbnailRenderer": {
                        "thumbnail": {
                            "thumbnails": [{ "url": "https://lh3.googleusercontent.com/cover=w60", "width": 60 }]
                        }
                    }
                }
            }
        });
        let p = parse_two_row_playlist(&item).expect("playlist");
        assert_eq!(p.id, "PL999");
        assert_eq!(p.thumbnails.len(), 1);
        assert!(p.thumbnails[0].url.contains("googleusercontent"));

        let alt = serde_json::json!({
            "musicTwoRowItemRenderer": {
                "title": { "runs": [{ "text": "Chill" }] },
                "navigationEndpoint": {
                    "browseEndpoint": {
                        "browseId": "VLRD123",
                        "browseEndpointContextSupportedConfigs": {
                            "browseEndpointContextMusicConfig": {
                                "pageType": "MUSIC_PAGE_TYPE_PLAYLIST"
                            }
                        }
                    }
                },
                "thumbnail": {
                    "musicThumbnailRenderer": {
                        "thumbnail": {
                            "thumbnails": [{ "url": "https://i.ytimg.com/vi/abc/hqdefault.jpg", "width": 120 }]
                        }
                    }
                }
            }
        });
        let p = parse_two_row_playlist(&alt).expect("playlist alt thumb path");
        assert_eq!(p.thumbnails.len(), 1);
    }

    #[test]
    fn parse_library_playlists_from_list_rows() {
        let json = serde_json::json!({
            "contents": [{
                "musicResponsiveListItemRenderer": {
                    "navigationEndpoint": {
                        "browseEndpoint": {
                            "browseId": "VLPL123abc",
                            "browseEndpointContextSupportedConfigs": {
                                "browseEndpointContextMusicConfig": {
                                    "pageType": "MUSIC_PAGE_TYPE_PLAYLIST"
                                }
                            }
                        }
                    },
                    "flexColumns": [{
                        "musicResponsiveListItemFlexColumnRenderer": {
                            "text": { "runs": [{ "text": "Late Night Mix" }] }
                        }
                    }, {
                        "musicResponsiveListItemFlexColumnRenderer": {
                            "text": { "runs": [{ "text": "YouTube Music" }] }
                        }
                    }],
                    "thumbnail": {
                        "musicThumbnailRenderer": {
                            "thumbnail": { "thumbnails": [{ "url": "https://i.ytimg.com/x.jpg", "width": 60 }] }
                        }
                    }
                }
            }]
        });
        let playlists = parse_library_playlists(&json);
        assert_eq!(playlists.len(), 1);
        assert_eq!(playlists[0].id, "PL123abc");
        assert_eq!(playlists[0].title, "Late Night Mix");
        assert_eq!(playlists[0].author.as_deref(), Some("YouTube Music"));
    }
}

fn cookie_set(header: &str) -> std::collections::BTreeMap<String, String> {
    manual::cookie_map(header).into_iter().collect()
}

/// Saves the raw response of a signed-in list that parsed to nothing, so a changed YouTube
/// layout can be diagnosed instead of silently showing an empty list.
fn dump_if_empty(name: &str, empty: bool, json: &Value) {
    if !empty {
        return;
    }
    let dir = std::env::temp_dir().join("yt-tui-debug");
    let path = dir.join(format!("{name}.json"));
    let saved = std::fs::create_dir_all(&dir)
        .and_then(|_| std::fs::write(&path, serde_json::to_vec_pretty(json).unwrap_or_default()));
    match saved {
        Ok(()) => warn!("{name}: signed-in response parsed to 0 items; raw saved to {}", path.display()),
        Err(e) => warn!("{name}: parsed to 0 items (could not save raw response: {e})"),
    }
}
