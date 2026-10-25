use crate::action::*;
use crate::api::model::*;
use crate::auth::{self};
use crate::config::Config;
use crate::paths::Paths;
use crate::player::queue::RepeatMode;
use crate::player::PlaybackState;
use crate::ui::keymap::{self, InputContext};
use crate::ui::theme::Theme;
use crate::ui::widgets::toast::Toast;
use crate::ui::widgets::track_list::TrackListState;
use crate::visualizer::{Visualizer, VisualizerStyle};
use crate::media_controls::MediaControlsBridge;
use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyCode, KeyEventKind,
    KeyModifiers, MouseEventKind,
};
use futures::StreamExt;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Window in which a second Ctrl+C confirms quit.
const CTRL_C_QUIT_WINDOW: Duration = Duration::from_secs(2);
/// How long the highlight must rest on a track before it is prewarmed.
const PREWARM_DWELL: Duration = Duration::from_millis(400);
/// How long a library section is reused before it is re-synced with YouTube.
const LIBRARY_TTL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum Tab {
    Home = 0,
    Search = 1,
    Library = 2,
    Queue = 3,
    Lyrics = 4,
}

impl Tab {
    pub fn from_usize(i: usize) -> Self {
        match i {
            1 => Self::Search,
            2 => Self::Library,
            3 => Self::Queue,
            4 => Self::Lyrics,
            _ => Self::Home,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Overlay {
    None,
    Onboarding,
    Help { scroll: u16 },
    ConfirmQuit,
    ContextMenu { items: Vec<String>, selected: usize },
    CommandPalette { query: String, selected: usize },
    ManualCookie { input: String },
    AccountMenu,
    Equalizer { selected: usize },
}

#[derive(Debug, Clone)]
pub enum DetailView {
    Album(Album),
    Artist(Artist),
    Playlist(Playlist),
}

impl DetailView {
    pub fn cover_key(&self) -> String {
        let id = match self {
            Self::Album(a) => &a.id,
            Self::Artist(a) => &a.id,
            Self::Playlist(p) => &p.id,
        };
        format!("detail:{id}")
    }

    pub fn thumbnails(&self) -> &[crate::api::model::Thumbnail] {
        match self {
            Self::Album(a) => &a.thumbnails,
            Self::Artist(a) => &a.thumbnails,
            Self::Playlist(p) => &p.thumbnails,
        }
    }
}

pub struct App {
    pub paths: Arc<Paths>,
    pub config: Config,
    pub theme: Theme,
    pub tab: Tab,
    pub overlay: Overlay,
    pub fullscreen_np: bool,
    pub signed_in: bool,
    pub account_name: Option<String>,
    pub onboarding_status: String,

    pub playback_state: PlaybackState,
    pub position: Duration,
    pub duration: Option<Duration>,
    /// Codec/size of the opened stream, keyed by video id.
    pub stream_info: Option<(String, crate::player::source::StreamInfo)>,
    /// Music Premium status of the signed-in account, as the resolver learned it.
    pub premium: crate::player::resolver::Premium,
    /// Highlighted track and since when, for prewarming after a short dwell.
    prewarm_dwell: Option<(String, Instant)>,
    /// Last track sent for prewarming, so a dwell triggers it once.
    prewarm_sent: Option<String>,
    pub volume: f32,
    pub muted: bool,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub autoplay: bool,

    pub current: Option<Track>,
    pub queue: Vec<Track>,
    pub queue_cursor: usize,

    pub home_shelves: Vec<Shelf>,
    pub home_shelf_idx: usize,
    pub home_item_idx: usize,
    pub loading_home: bool,
    /// Ignore stale Home responses from overlapping LoadHome calls.
    home_fetch_gen: u64,
    pending_home_gen: Option<u64>,

    pub search_query: String,
    pub search_filter: SearchFilter,
    pub search_suggestions: Vec<String>,
    pub search_tracks: Vec<Track>,
    pub search_albums: Vec<Album>,
    pub search_artists: Vec<Artist>,
    pub search_playlists: Vec<Playlist>,
    pub suggestion_idx: usize,
    pub loading_search: bool,
    /// When true, keys type into the search field; Esc clears this.
    pub search_editing: bool,
    /// Keyboard focus is on the sidebar menu (↑/↓ switch tab, →/Enter/Esc leave).
    pub sidebar_focus: bool,
    pub term_w: u16,

    pub library_section: usize,
    pub liked_songs: Vec<Track>,
    pub library_playlists: Vec<Playlist>,
    pub library_albums: Vec<Album>,
    pub library_artists: Vec<Artist>,
    pub history: Vec<Track>,
    pub loading_library: bool,
    /// When each library section was last fetched; `None` = never, or invalidated by an edit.
    library_fetched: [Option<Instant>; 5],
    pub loading_detail: bool,

    pub lyrics: Option<Lyrics>,
    pub list_state: TrackListState,
    pub detail: Option<DetailView>,
    pub nav_stack: Vec<DetailView>,
    /// Last track id pushed to OS media controls (avoid spamming every tick).
    media_meta_video_id: Option<String>,

    pub toasts: Vec<Toast>,
    pub eq: crate::player::EqParams,
    /// Taste-profile adaptive EQ (`Off` = manual presets/sliders).
    pub eq_adaptive: crate::player::AdaptiveProfile,
    pub adaptive_eq: crate::player::AdaptiveEq,
    pub visualizer: Visualizer,
    pub sample_scratch: Vec<f32>,
    pub title_marquee: crate::ui::marquee::Marquee,
    /// Loaded artwork keyed by track `video_id` or `detail:<browse_id>`.
    pub covers: std::collections::HashMap<String, crate::art::Cover>,
    /// Track whose cover accent is currently applied to the theme.
    accent_for: Option<String>,
    pub cover_ui: crate::ui::cover::CoverRenderer,
    pub hit: std::cell::RefCell<crate::ui::HitMap>,
    radio_requested_for: Option<String>,

    pub should_quit: bool,
    pub first_run: bool,
    /// Set on first Ctrl+C; a second press within `CTRL_C_QUIT_WINDOW` exits.
    ctrl_c_armed_at: Option<Instant>,

    /// Where the control bridge reads player state from (None when disabled).
    pub control: Option<crate::control::SharedSnapshot>,
    control_handle: Option<crate::control::ControlHandle>,
    pub control_action_tx: Option<mpsc::UnboundedSender<Action>>,

    cmd_tx: mpsc::UnboundedSender<Command>,
    player_cmd_tx: mpsc::UnboundedSender<PlayerCommand>,
}

/// Internal player commands sent to the player task.
#[derive(Debug, Clone)]
pub enum PlayerCommand {
    PlayTrack(Track),
    PlayTracks { tracks: Vec<Track>, start: usize },
    Toggle,
    Play,
    Pause,
    Next,
    Previous,
    Seek(Duration),
    SeekRelative(i64),
    SetVolume(f32),
    VolumeDelta(f32),
    Mute,
    ToggleShuffle,
    CycleRepeat,
    AppendRadio(Vec<Track>),
    HandleEnded,
    AddToQueue(Track),
    PlayNext(Track),
    RemoveAt(usize),
    ClearUpcoming,
    MoveQueue { from: usize, to: usize },
    JumpTo(usize),
    RestoreSession {
        tracks: Vec<Track>,
        index: usize,
        position: Duration,
        shuffle: bool,
        repeat: RepeatMode,
    },
    Shutdown,
    SetEq(crate::player::EqParams),
    /// Sync Music session cookies into the stream resolver (yt-dlp auth path).
    SetCookies(Option<String>),
    /// Get a highlighted track ready to start instantly.
    Prewarm(Track),
}

impl App {
    pub fn new(
        paths: Arc<Paths>,
        config: Config,
        cmd_tx: mpsc::UnboundedSender<Command>,
        player_cmd_tx: mpsc::UnboundedSender<PlayerCommand>,
        signed_in: bool,
        account_name: Option<String>,
    ) -> Self {
        let theme = Theme::from_config(&config.ui);
        let first_run = !signed_in;
        let overlay = if first_run {
            Overlay::Onboarding
        } else {
            Overlay::None
        };
        Self {
            visualizer: Visualizer::new(VisualizerStyle::from_config(&config.ui.visualizer)),
            title_marquee: crate::ui::marquee::Marquee::new(""),
            covers: Default::default(),
            accent_for: None,
            cover_ui: Default::default(),
            hit: Default::default(),
            radio_requested_for: None,
            control: None,
            control_handle: None,
            control_action_tx: None,
            volume: config.playback.volume,
            autoplay: config.playback.autoplay,
            eq: crate::player::EqParams {
                enabled: config.playback.eq_enabled,
                gains_db: config.playback.eq_gains,
            }
            .clamped(),
            eq_adaptive: crate::player::AdaptiveProfile::from_config(
                &config.playback.eq_adaptive,
            ),
            adaptive_eq: crate::player::AdaptiveEq::default(),
            paths,
            config,
            theme,
            tab: Tab::Home,
            overlay,
            fullscreen_np: false,
            signed_in,
            account_name,
            onboarding_status: String::new(),
            playback_state: PlaybackState::Stopped,
            position: Duration::ZERO,
            duration: None,
            stream_info: None,
            premium: crate::player::resolver::Premium::UNKNOWN,
            prewarm_dwell: None,
            prewarm_sent: None,
            muted: false,
            shuffle: false,
            repeat: RepeatMode::Off,
            current: None,
            queue: Vec::new(),
            queue_cursor: 0,
            home_shelves: Vec::new(),
            home_shelf_idx: 0,
            home_item_idx: 0,
            loading_home: false,
            home_fetch_gen: 0,
            pending_home_gen: None,
            search_query: String::new(),
            search_filter: SearchFilter::Songs,
            search_suggestions: Vec::new(),
            search_tracks: Vec::new(),
            search_albums: Vec::new(),
            search_artists: Vec::new(),
            search_playlists: Vec::new(),
            suggestion_idx: 0,
            loading_search: false,
            search_editing: false,
            sidebar_focus: false,
            term_w: crossterm::terminal::size().map(|s| s.0).unwrap_or(120),
            library_section: 0,
            liked_songs: Vec::new(),
            library_playlists: Vec::new(),
            library_albums: Vec::new(),
            library_artists: Vec::new(),
            history: Vec::new(),
            loading_library: false,
            library_fetched: [None; 5],
            loading_detail: false,
            lyrics: None,
            list_state: TrackListState::default(),
            detail: None,
            nav_stack: Vec::new(),
            toasts: Vec::new(),
            sample_scratch: Vec::new(),
            should_quit: false,
            first_run,
            ctrl_c_armed_at: None,
            media_meta_video_id: None,
            cmd_tx,
            player_cmd_tx,
        }
    }

    pub fn current_track(&self) -> Option<&Track> {
        self.current.as_ref()
    }

    /// Stream info for the current track, if its audio has been opened.
    pub fn current_stream_info(&self) -> Option<&crate::player::source::StreamInfo> {
        let (id, info) = self.stream_info.as_ref()?;
        (self.current.as_ref()?.video_id == *id).then_some(info)
    }

    pub fn current_cover(&self) -> Option<&crate::art::Cover> {
        self.covers.get(&self.current.as_ref()?.video_id)
    }

    pub fn detail_cover_key(&self) -> Option<String> {
        self.detail.as_ref().map(DetailView::cover_key)
    }

    pub fn detail_cover(&self) -> Option<&crate::art::Cover> {
        self.covers.get(&self.detail_cover_key()?)
    }

    fn fetch_detail_cover(&self, cmds: &mut Vec<Command>) {
        let Some(detail) = &self.detail else { return };
        let key = detail.cover_key();
        if self.covers.contains_key(&key) {
            return;
        }
        if let Some(thumb) = detail.thumbnails().iter().max_by_key(|t| t.width.unwrap_or(0)) {
            cmds.push(Command::FetchArt {
                url: thumb.url.clone(),
                video_id: key,
            });
        }
    }

    /// Keeps the cover cache bounded while preserving the covers still on screen.
    fn prune_covers(&mut self) {
        const MAX_COVERS: usize = 96;
        if self.covers.len() <= MAX_COVERS {
            return;
        }
        let mut keep: Vec<String> = self.nav_stack.iter().map(DetailView::cover_key).collect();
        keep.extend(self.detail_cover_key());
        keep.extend(self.current.as_ref().map(|t| t.video_id.clone()));
        for shelf in &self.home_shelves {
            for item in &shelf.items {
                keep.push(item.cover_key().to_string());
            }
        }
        // Drop arbitrary extras first; never evict the active/detail/home covers.
        let extras: Vec<String> = self
            .covers
            .keys()
            .filter(|k| !keep.contains(k))
            .cloned()
            .collect();
        let overflow = self.covers.len().saturating_sub(MAX_COVERS);
        for key in extras.into_iter().take(overflow) {
            self.covers.remove(&key);
        }
    }

    /// Prefetch artwork for Home shelf cards (playlists, albums, tracks, artists).
    fn ensure_home_covers(&mut self, cmds: &mut Vec<Command>) {
        const MAX_FETCH: usize = 80;
        let requests: Vec<(String, String)> = self
            .home_shelves
            .iter()
            .flat_map(|s| s.items.iter())
            .filter_map(|item| {
                let key = item.cover_key().to_string();
                if self.covers.contains_key(&key) {
                    return None;
                }
                Some((key, item.art_url()?))
            })
            .take(MAX_FETCH)
            .collect();
        for (key, url) in requests {
            // Empty entry marks in-flight so navigation doesn't re-queue the same URL.
            self.covers
                .insert(key.clone(), crate::art::Cover::default());
            cmds.push(Command::FetchArt {
                url,
                video_id: key,
            });
        }
    }

    fn ensure_playlist_covers(&mut self, cmds: &mut Vec<Command>) {
        let requests: Vec<(String, String)> = self
            .library_playlists
            .iter()
            .filter(|p| !self.covers.contains_key(&p.id))
            .filter_map(|p| {
                let url = crate::api::model::ShelfItem::Playlist(crate::api::model::Playlist {
                    tracks: Vec::new(),
                    ..p.clone()
                })
                .art_url()?;
                Some((p.id.clone(), url))
            })
            .take(80)
            .collect();
        for (key, url) in requests {
            self.covers.insert(key.clone(), crate::art::Cover::default());
            cmds.push(Command::FetchArt { url, video_id: key });
        }
    }

    /// Library playlists are shown as a cover grid.
    fn in_playlist_grid(&self) -> bool {
        self.tab == Tab::Library
            && self.detail.is_none()
            && !self.fullscreen_np
            && self.library_section == 1
    }

    fn grid_cols(&self) -> usize {
        self.hit.borrow().grid_cols.max(1)
    }

    /// Adds a toast, refreshing an identical live one instead of stacking duplicates.
    fn push_toast(&mut self, toast: Toast) {
        self.toasts
            .retain(|t| !(t.message == toast.message && t.kind == toast.kind));
        self.toasts.push(toast);
        if self.toasts.len() > 8 {
            self.toasts.remove(0);
        }
    }

    pub fn queue_tracks(&self) -> Vec<&Track> {
        self.queue.iter().collect()
    }

    pub fn input_context(&self) -> InputContext {
        match &self.overlay {
            Overlay::Onboarding => InputContext::Onboarding,
            Overlay::Help { .. } => InputContext::Help,
            Overlay::ConfirmQuit => InputContext::ConfirmQuit,
            Overlay::CommandPalette { .. } => InputContext::CommandPalette,
            Overlay::ManualCookie { .. } => InputContext::ManualCookie,
            Overlay::Equalizer { .. } => InputContext::Equalizer,
            Overlay::ContextMenu { .. } => InputContext::Global,
            Overlay::AccountMenu => InputContext::AccountMenu,
            Overlay::None => {
                if self.tab == Tab::Search && self.detail.is_none() && self.search_editing {
                    InputContext::Search
                } else {
                    InputContext::List
                }
            }
        }
    }

    pub fn palette_actions(&self) -> Vec<(String, String)> {
        let all = [
            ("Play / Pause", "playpause"),
            ("Next track", "next"),
            ("Previous track", "prev"),
            ("Toggle shuffle", "shuffle"),
            ("Cycle repeat", "repeat"),
            ("Search", "search"),
            ("Home", "home"),
            ("Library", "library"),
            ("Queue", "queue"),
            ("Lyrics", "lyrics"),
            ("Now Playing", "np"),
            ("Sign in", "login"),
            ("Log out", "logout"),
            ("Help", "help"),
            ("Equalizer", "equalizer"),
            (
                if self.control.is_some() {
                    "Disable MCP"
                } else {
                    "Enable MCP"
                },
                "mcp",
            ),
            ("Quit", "quit"),
        ];
        let q = match &self.overlay {
            Overlay::CommandPalette { query, .. } => query.to_lowercase(),
            _ => String::new(),
        };
        let themes = crate::ui::theme::PRESETS.iter().map(|(name, _)| {
            let current = if *name == self.theme.name { "  ·  current" } else { "" };
            (
                format!("Theme: {}{current}", crate::ui::theme::display_name(name)),
                format!("theme:{name}"),
            )
        });
        all.into_iter()
            .map(|(n, i)| (n.to_string(), i.to_string()))
            .chain(themes)
            .filter(|(name, id)| {
                q.is_empty() || name.to_lowercase().contains(&q) || id.contains(&q)
            })
            .collect()
    }

    pub fn update(&mut self, action: Action) -> Vec<Command> {
        let mut cmds = Vec::new();
        // Always allow tick
        match action {
            Action::Tick => {
                self.sync_accent_to_cover();
                self.theme.tick_accent();
                self.toasts.retain(|t| !t.expired());
                if let Some(t) = self.current_track() {
                    self.title_marquee.set_text(t.title.clone());
                }
                self.title_marquee.tick();
                self.prewarm_highlighted();
                if let Some(at) = self.ctrl_c_armed_at {
                    if at.elapsed() > CTRL_C_QUIT_WINDOW {
                        self.ctrl_c_armed_at = None;
                    }
                }
                return cmds;
            }
            Action::ApplyEvent(ev) => {
                cmds.extend(self.apply_event(ev));
                return cmds;
            }
            Action::CtrlC => {
                self.handle_ctrl_c();
                return cmds;
            }
            _ => {}
        }

        // Any other key clears a pending Ctrl+C arm
        if !matches!(action, Action::Tick | Action::Resize(_, _)) {
            self.ctrl_c_armed_at = None;
        }

        // Overlay handling
        match &self.overlay {
            Overlay::Onboarding => {
                match action {
                    Action::BeginLogin => {
                        self.overlay = Overlay::ManualCookie {
                            input: String::new(),
                        };
                    }
                    Action::ContinueAsGuest => {
                        self.overlay = Overlay::None;
                        self.signed_in = false;
                        self.request_home(&mut cmds);
                    }
                    Action::BeginManualCookie => {
                        self.overlay = Overlay::ManualCookie {
                            input: String::new(),
                        };
                    }
                    Action::Quit => self.should_quit = true,
                    _ => {}
                }
                return cmds;
            }
            Overlay::Help { scroll } => {
                let mut scroll = *scroll;
                match action {
                    Action::CloseOverlay | Action::Escape | Action::Help => {
                        self.overlay = Overlay::None;
                    }
                    Action::MoveDown | Action::PageDown => scroll = scroll.saturating_add(3),
                    Action::MoveUp | Action::PageUp => scroll = scroll.saturating_sub(3),
                    _ => {}
                }
                if !matches!(self.overlay, Overlay::None) {
                    self.overlay = Overlay::Help { scroll };
                }
                return cmds;
            }
            Overlay::ConfirmQuit => {
                match action {
                    Action::ConfirmQuit => self.should_quit = true,
                    Action::CancelQuit | Action::Escape => self.overlay = Overlay::None,
                    _ => {}
                }
                return cmds;
            }
            Overlay::ManualCookie { .. } => {
                match action {
                    Action::CloseOverlay | Action::Escape => {
                        self.overlay = Overlay::None;
                    }
                    Action::SearchInput(c) => {
                        if let Overlay::ManualCookie { input } = &mut self.overlay {
                            input.push(c);
                        }
                    }
                    Action::PasteText(text) => {
                        if text.is_empty() {
                            if let Overlay::ManualCookie { input } = &mut self.overlay {
                                input.clear();
                            }
                        } else if let Overlay::ManualCookie { input } = &mut self.overlay {
                            // Cap absurd pastes (full page HTML etc.)
                            const MAX: usize = 512 * 1024;
                            let room = MAX.saturating_sub(input.len());
                            if room > 0 {
                                let take = text.chars().take(room).collect::<String>();
                                input.push_str(&take);
                            }
                        }
                    }
                    Action::SearchBackspace => {
                        if let Overlay::ManualCookie { input } = &mut self.overlay {
                            input.pop();
                        }
                    }
                    Action::SearchSubmit => {
                        let raw = if let Overlay::ManualCookie { input } = &self.overlay {
                            input.clone()
                        } else {
                            String::new()
                        };
                        self.push_toast(Toast::info("Validating cookie…"));
                        match auth::manual::parse_auth_input(&raw) {
                            Ok(parsed) => {
                                self.overlay = Overlay::None;
                                cmds.push(Command::ValidateCookies {
                                    cookies: parsed.cookies,
                                    visitor_id: parsed.visitor_id,
                                });
                            }
                            Err(e) => self.push_toast(Toast::error(e)),
                        }
                    }
                    _ => {}
                }
                return cmds;
            }
            Overlay::CommandPalette { query, selected } => {
                let mut query = query.clone();
                let mut selected = *selected;
                let actions = self.palette_actions();
                match action {
                    Action::CloseOverlay | Action::Escape => {
                        self.overlay = Overlay::None;
                        return cmds;
                    }
                    Action::PaletteInput(c) => {
                        query.push(c);
                        selected = 0;
                    }
                    Action::PasteText(text) => {
                        query.push_str(&single_line(&text, MAX_QUERY_CHARS));
                        selected = 0;
                    }
                    Action::PaletteBackspace => {
                        query.pop();
                        selected = 0;
                    }
                    Action::MoveDown => {
                        if selected + 1 < actions.len() {
                            selected += 1;
                        }
                    }
                    Action::MoveUp => selected = selected.saturating_sub(1),
                    Action::PaletteSelect => {
                        if let Some((_, id)) = actions.get(selected) {
                            self.overlay = Overlay::None;
                            cmds.extend(self.run_palette(id));
                            return cmds;
                        }
                    }
                    _ => {}
                }
                self.overlay = Overlay::CommandPalette { query, selected };
                return cmds;
            }
            Overlay::ContextMenu { items, selected } => {
                let items = items.clone();
                let mut selected = *selected;
                match action {
                    Action::CloseOverlay | Action::Escape => self.overlay = Overlay::None,
                    Action::MoveDown => {
                        if selected + 1 < items.len() {
                            selected += 1;
                        }
                    }
                    Action::MoveUp => selected = selected.saturating_sub(1),
                    Action::Select => {
                        let label = items.get(selected).cloned();
                        self.overlay = Overlay::None;
                        if let Some(label) = label {
                            cmds.extend(self.run_context_action(&label));
                        }
                        return cmds;
                    }
                    _ => {}
                }
                self.overlay = Overlay::ContextMenu { items, selected };
                return cmds;
            }
            Overlay::AccountMenu => {
                match action {
                    Action::CloseOverlay | Action::Escape => self.overlay = Overlay::None,
                    Action::BeginLogin | Action::Select if !self.signed_in => {
                        self.overlay = Overlay::ManualCookie {
                            input: String::new(),
                        };
                    }
                    Action::Logout | Action::Select if self.signed_in => {
                        cmds.push(Command::Logout);
                        self.signed_in = false;
                        self.account_name = None;
                        self.overlay = Overlay::None;
                        self.push_toast(Toast::info("Signed out"));
                    }
                    Action::BeginManualCookie if !self.signed_in => {
                        self.overlay = Overlay::ManualCookie {
                            input: String::new(),
                        };
                    }
                    _ => {}
                }
                return cmds;
            }
            Overlay::Equalizer { selected } => {
                let mut selected = *selected;
                let n = crate::player::BAND_COUNT;
                match action {
                    Action::CloseOverlay | Action::Escape => {
                        self.overlay = Overlay::None;
                        return cmds;
                    }
                    Action::MoveLeft => {
                        selected = selected.saturating_sub(1);
                    }
                    Action::MoveRight => {
                        selected = (selected + 1).min(n - 1);
                    }
                    Action::MoveUp => {
                        let mut p = self.eq;
                        p.gains_db[selected] =
                            (p.gains_db[selected] + 1.0).min(crate::player::GAIN_MAX_DB);
                        self.apply_eq_manual(p);
                    }
                    Action::MoveDown => {
                        let mut p = self.eq;
                        p.gains_db[selected] =
                            (p.gains_db[selected] - 1.0).max(crate::player::GAIN_MIN_DB);
                        self.apply_eq_manual(p);
                    }
                    Action::EqToggle => {
                        let mut p = self.eq;
                        p.enabled = !p.enabled;
                        self.apply_eq(p);
                    }
                    Action::EqResetBand => {
                        let mut p = self.eq;
                        p.gains_db[selected] = 0.0;
                        self.apply_eq_manual(p);
                    }
                    Action::EqFlat => {
                        let mut p = self.eq;
                        p.gains_db = [0.0; crate::player::BAND_COUNT];
                        self.apply_eq_manual(p);
                    }
                    Action::EqPreset(i) => {
                        if let Some(preset) = crate::player::PRESETS.get(i) {
                            let mut p = self.eq;
                            p.enabled = true;
                            p.gains_db = preset.gains_db;
                            self.apply_eq_manual(p);
                        }
                    }
                    Action::EqAdaptiveCycle => {
                        self.cycle_eq_adaptive();
                    }
                    _ => {}
                }
                self.overlay = Overlay::Equalizer { selected };
                return cmds;
            }
            Overlay::None => {}
        }

        if self.sidebar_focus {
            match action {
                Action::MoveUp | Action::MoveDown => {
                    let n = 5usize;
                    let i = self.tab as usize;
                    let next = if matches!(action, Action::MoveUp) {
                        (i + n - 1) % n
                    } else {
                        (i + 1) % n
                    };
                    self.tab = Tab::from_usize(next);
                    self.detail = None;
                    self.search_editing = false;
                    self.on_tab_enter(&mut cmds);
                    return cmds;
                }
                Action::MoveLeft => return cmds,
                Action::MoveRight | Action::Select | Action::Back | Action::Escape => {
                    self.sidebar_focus = false;
                    self.search_editing = self.tab == Tab::Search && self.detail.is_none();
                    return cmds;
                }
                _ => {}
            }
        }

        // Global actions
        match action {
            Action::Quit => {
                self.ctrl_c_armed_at = None;
                if !self.queue.is_empty() {
                    self.overlay = Overlay::ConfirmQuit;
                } else {
                    self.should_quit = true;
                }
            }
            Action::Help => self.overlay = Overlay::Help { scroll: 0 },
            Action::OpenEqualizer => {
                self.overlay = Overlay::Equalizer { selected: 0 };
            }
            Action::EqAdaptiveCycle => {
                self.cycle_eq_adaptive();
            }
            Action::ToggleMcp => self.toggle_control(),
            Action::CommandPalette => {
                self.overlay = Overlay::CommandPalette {
                    query: String::new(),
                    selected: 0,
                };
            }
            Action::Tab(i) => {
                self.sidebar_focus = false;
                self.tab = Tab::from_usize(i);
                self.detail = None;
                self.search_editing = self.tab == Tab::Search;
                self.on_tab_enter(&mut cmds);
            }
            Action::ToggleNowPlaying => self.fullscreen_np = !self.fullscreen_np,
            Action::PlayPause => {
                let _ = self.player_cmd_tx.send(PlayerCommand::Toggle);
            }
            Action::Play => {
                let _ = self.player_cmd_tx.send(PlayerCommand::Play);
            }
            Action::Pause => {
                let _ = self.player_cmd_tx.send(PlayerCommand::Pause);
            }
            Action::Next => {
                let _ = self.player_cmd_tx.send(PlayerCommand::Next);
            }
            Action::Previous => {
                let _ = self.player_cmd_tx.send(PlayerCommand::Previous);
            }
            Action::SeekForward(d) => {
                let _ = self
                    .player_cmd_tx
                    .send(PlayerCommand::SeekRelative(d.as_secs() as i64));
            }
            Action::SeekBack(d) => {
                let _ = self
                    .player_cmd_tx
                    .send(PlayerCommand::SeekRelative(-(d.as_secs() as i64)));
            }
            Action::SeekTo(d) => {
                let _ = self.player_cmd_tx.send(PlayerCommand::Seek(d));
            }
            Action::VolumeUp => {
                let _ = self.player_cmd_tx.send(PlayerCommand::VolumeDelta(0.05));
                self.volume = (self.volume + 0.05).min(1.0);
            }
            Action::VolumeDown => {
                let _ = self.player_cmd_tx.send(PlayerCommand::VolumeDelta(-0.05));
                self.volume = (self.volume - 0.05).max(0.0);
            }
            Action::SetVolume(v) => {
                let v = v.clamp(0.0, 1.0);
                let _ = self.player_cmd_tx.send(PlayerCommand::SetVolume(v));
                self.volume = v;
                self.muted = false;
            }
            Action::Mute => {
                let _ = self.player_cmd_tx.send(PlayerCommand::Mute);
                self.muted = !self.muted;
            }
            Action::ToggleShuffle => {
                let _ = self.player_cmd_tx.send(PlayerCommand::ToggleShuffle);
                self.shuffle = !self.shuffle;
            }
            Action::CycleRepeat => {
                let _ = self.player_cmd_tx.send(PlayerCommand::CycleRepeat);
                self.repeat = self.repeat.next();
            }
            Action::LikeCurrent => {
                if let Some(t) = self.current.clone() {
                    let liked = !t.liked.unwrap_or(false);
                    cmds.push(Command::Rate {
                        video_id: t.video_id,
                        liked,
                    });
                }
            }
            Action::AccountMenu => {
                self.overlay = Overlay::AccountMenu;
            }
            Action::StartSearch => {
                let already_here = self.tab == Tab::Search && self.detail.is_none();
                self.tab = Tab::Search;
                self.detail = None;
                self.search_editing = true;
                if !already_here {
                    self.search_query.clear();
                    self.clear_search_results();
                }
            }
            Action::FocusSearch => {
                self.tab = Tab::Search;
                self.detail = None;
                self.search_editing = true;
            }
            Action::SearchInput(c) => {
                self.search_editing = true;
                self.search_query.push(c);
                // Drop stale results so the next Enter runs a new search, not play.
                self.clear_search_results();
                self.debounce_search(&mut cmds);
            }
            // Bracketed paste (Cmd+V etc.) arrives as one event, not keystrokes.
            Action::PasteText(text) if self.tab == Tab::Search && self.detail.is_none() => {
                let room = MAX_QUERY_CHARS.saturating_sub(self.search_query.chars().count());
                let pasted = single_line(&text, room);
                if !pasted.is_empty() {
                    self.search_editing = true;
                    self.search_query.push_str(&pasted);
                    self.clear_search_results();
                    self.debounce_search(&mut cmds);
                }
            }
            Action::SearchBackspace => {
                self.search_query.pop();
                self.clear_search_results();
                self.debounce_search(&mut cmds);
            }
            Action::SearchSubmit => {
                // Search keymap binds Enter to SearchSubmit (not Select). Once results
                // are on screen, Enter should play/open the highlighted row — same as
                // a mouse click. Re-run the query only when the list is empty.
                if self.list_len() > 0 {
                    self.search_editing = false;
                    cmds.extend(self.activate_selection());
                } else if !self.search_query.is_empty() {
                    self.loading_search = true;
                    cmds.push(Command::Search {
                        query: self.search_query.clone(),
                        filter: self.search_filter,
                    });
                }
            }
            Action::CycleSearchFilter => {
                self.search_filter = self.search_filter.next();
                if !self.search_query.is_empty() {
                    self.loading_search = true;
                    cmds.push(Command::Search {
                        query: self.search_query.clone(),
                        filter: self.search_filter,
                    });
                }
            }
            Action::SetSearchFilter(f) => {
                if self.search_filter != f {
                    self.search_filter = f;
                    self.list_state = TrackListState::default();
                    if !self.search_query.is_empty() {
                        self.loading_search = true;
                        cmds.push(Command::Search {
                            query: self.search_query.clone(),
                            filter: self.search_filter,
                        });
                    }
                }
            }
            Action::Back => {
                if self.fullscreen_np {
                    self.fullscreen_np = false;
                } else if self.detail.is_some() {
                    self.detail = self.nav_stack.pop();
                    self.list_state = TrackListState::default();
                } else if self.tab == Tab::Search && self.search_editing {
                    self.search_editing = false;
                } else if self.tab == Tab::Search && !self.search_query.is_empty() {
                    self.search_query.clear();
                    self.clear_search_results();
                }
            }
            Action::Escape => {
                if self.fullscreen_np {
                    self.fullscreen_np = false;
                } else if self.tab == Tab::Search && self.search_editing {
                    // Leave the search box; results stay, list keys take over.
                    self.search_editing = false;
                }
            }
            Action::MoveUp => {
                if self.search_editing && self.list_len() > 0 {
                    self.search_editing = false;
                }
                self.move_sel(-1, &mut cmds);
            }
            Action::MoveDown => {
                if self.search_editing && self.list_len() > 0 {
                    self.search_editing = false;
                }
                self.move_sel(1, &mut cmds);
            }
            Action::MoveLeft | Action::MoveRight => {
                let right = matches!(action, Action::MoveRight);
                self.move_horizontal(right, &mut cmds);
            }
            Action::PageUp => self.list_state.page(-1, self.list_len(), 10),
            Action::PageDown => self.list_state.page(1, self.list_len(), 10),
            Action::GoTop => self.list_state.top(),
            Action::GoBottom => self.list_state.bottom(self.list_len()),
            Action::Select => cmds.extend(self.activate_selection()),
            Action::SelectAt(i) => {
                if self.tab == Tab::Search
                    && self.search_filter == SearchFilter::Songs
                    && self.search_tracks.is_empty()
                    && !self.search_suggestions.is_empty()
                {
                    self.suggestion_idx = i.min(self.search_suggestions.len() - 1);
                    cmds.extend(self.activate_selection());
                } else if self.list_len() > 0 {
                    self.list_state.selected = i.min(self.list_len() - 1);
                    cmds.extend(self.activate_selection());
                }
            }
            Action::SelectHomeItem { shelf, item } => {
                self.home_shelf_idx = shelf;
                self.home_item_idx = item;
                cmds.extend(self.activate_selection());
            }
            Action::AddToQueue => {
                if let Some(t) = self.selected_track() {
                    let _ = self.player_cmd_tx.send(PlayerCommand::AddToQueue(t));
                    self.push_toast(Toast::info("Added to queue"));
                }
            }
            Action::PlayNext => {
                if let Some(t) = self.selected_track() {
                    let _ = self.player_cmd_tx.send(PlayerCommand::PlayNext(t));
                    self.push_toast(Toast::info("Playing next"));
                }
            }
            Action::ContextMenu => {
                self.overlay = Overlay::ContextMenu {
                    items: vec![
                        "Play next".into(),
                        "Add to queue".into(),
                        "Start radio".into(),
                        "Go to album".into(),
                        "Go to artist".into(),
                        "Like".into(),
                        "Add to playlist…".into(),
                    ],
                    selected: 0,
                };
            }
            Action::RemoveFromQueue => {
                let _ = self
                    .player_cmd_tx
                    .send(PlayerCommand::RemoveAt(self.list_state.selected));
            }
            Action::ClearUpcoming => {
                let _ = self.player_cmd_tx.send(PlayerCommand::ClearUpcoming);
            }
            Action::MoveQueueUp => {
                let s = self.list_state.selected;
                if s > 0 {
                    let _ = self.player_cmd_tx.send(PlayerCommand::MoveQueue {
                        from: s,
                        to: s - 1,
                    });
                    self.list_state.selected = s - 1;
                }
            }
            Action::MoveQueueDown => {
                let s = self.list_state.selected;
                let _ = self.player_cmd_tx.send(PlayerCommand::MoveQueue {
                    from: s,
                    to: s + 1,
                });
                self.list_state.selected = s + 1;
            }
            Action::StartRadio => {
                if let Some(t) = self.selected_track().or_else(|| self.current.clone()) {
                    cmds.push(Command::LoadRadio(t.video_id));
                }
            }
            Action::PlayAll => {
                // Always start the open album/playlist from the top, not the
                // currently highlighted row.
                if self.detail.is_some() {
                    self.list_state.selected = 0;
                }
                cmds.extend(self.activate_selection());
            }
            Action::ShufflePlay => {
                cmds.extend(self.activate_selection());
            }
            Action::LibrarySection(i) => {
                // Sections live in the sidebar and stay clickable while a detail
                // page is open — dismiss it so the library list is actually shown.
                self.tab = Tab::Library;
                self.fullscreen_np = false;
                self.detail = None;
                self.nav_stack.clear();
                self.library_section = i;
                self.load_library_section(&mut cmds);
            }
            Action::BeginLogin => {
                self.overlay = Overlay::ManualCookie {
                    input: String::new(),
                };
            }
            Action::Logout => cmds.push(Command::Logout),
            Action::SeekToLyricLine(i) => {
                if let Some(ref lyrics) = self.lyrics {
                    if let Some(ref synced) = lyrics.synced {
                        if let Some(line) = synced.get(i) {
                            let _ = self.player_cmd_tx.send(PlayerCommand::Seek(line.time));
                        }
                    }
                }
            }
            Action::ToastDismiss => {
                self.toasts.clear();
            }
            Action::Resize(w, _) => self.term_w = w,
            _ => {}
        }
        cmds
    }

    fn handle_ctrl_c(&mut self) {
        let now = Instant::now();
        if let Some(at) = self.ctrl_c_armed_at {
            if now.duration_since(at) <= CTRL_C_QUIT_WINDOW {
                self.should_quit = true;
                self.ctrl_c_armed_at = None;
                return;
            }
        }
        self.ctrl_c_armed_at = Some(now);
        self.push_toast(Toast::info("Press Ctrl+C again to quit"));
    }

    fn run_palette(&mut self, id: &str) -> Vec<Command> {
        let mut cmds = Vec::new();
        match id {
            "playpause" => {
                let _ = self.player_cmd_tx.send(PlayerCommand::Toggle);
            }
            "next" => {
                let _ = self.player_cmd_tx.send(PlayerCommand::Next);
            }
            "prev" => {
                let _ = self.player_cmd_tx.send(PlayerCommand::Previous);
            }
            "shuffle" => {
                let _ = self.player_cmd_tx.send(PlayerCommand::ToggleShuffle);
                self.shuffle = !self.shuffle;
            }
            "repeat" => {
                let _ = self.player_cmd_tx.send(PlayerCommand::CycleRepeat);
                self.repeat = self.repeat.next();
            }
            "search" => {
                self.tab = Tab::Search;
                self.search_editing = true;
            }
            "home" => {
                self.tab = Tab::Home;
                self.on_tab_enter(&mut cmds);
            }
            "library" => {
                self.tab = Tab::Library;
                self.on_tab_enter(&mut cmds);
            }
            "queue" => self.tab = Tab::Queue,
            "lyrics" => self.tab = Tab::Lyrics,
            "np" => self.fullscreen_np = true,
            "login" => {
                self.overlay = Overlay::ManualCookie {
                    input: String::new(),
                };
            }
            "logout" => {
                if self.signed_in {
                    cmds.push(Command::Logout);
                    self.signed_in = false;
                    self.account_name = None;
                    self.push_toast(Toast::info("Signed out"));
                }
            }
            "help" => self.overlay = Overlay::Help { scroll: 0 },
            "equalizer" => self.overlay = Overlay::Equalizer { selected: 0 },
            "quit" => self.should_quit = true,
            "mcp" => self.toggle_control(),
            other => {
                if let Some(name) = other.strip_prefix("theme:") {
                    self.set_theme(name);
                }
            }
        }
        cmds
    }

    /// Switches the color theme live and remembers it in the config.
    pub fn set_theme(&mut self, name: &str) {
        let Some(name) = crate::ui::theme::preset_name(name) else {
            return;
        };
        self.config.ui.theme = name.into();
        let _ = self.config.save(&self.paths);
        self.theme = Theme::from_config(&self.config.ui);
        // Re-apply the cover accent on top of the new palette.
        self.accent_for = None;
        self.sync_accent_to_cover();
        self.push_toast(Toast::info(format!("Theme: {}", crate::ui::theme::display_name(name))));
    }

    pub fn toggle_control(&mut self) {
        let enable = self.control.is_none();
        match self.set_control_enabled(enable) {
            Ok(()) => {
                self.config.control.enabled = enable;
                let _ = self.config.save(&self.paths);
                self.push_toast(Toast::info(if enable {
                    "MCP bridge enabled"
                } else {
                    "MCP bridge disabled"
                }));
            }
            Err(e) => self.push_toast(Toast::error(format!("MCP: {e}"))),
        }
    }

    /// Start or stop the local control bridge without touching the saved config.
    pub fn set_control_enabled(&mut self, enable: bool) -> anyhow::Result<()> {
        if !enable {
            self.control = None;
            self.control_handle = None; // Drop stops the listener
            return Ok(());
        }
        if self.control.is_some() {
            return Ok(());
        }
        let tx = self
            .control_action_tx
            .clone()
            .ok_or_else(|| anyhow::anyhow!("control channel unavailable"))?;
        let (snapshot, handle) =
            crate::control::start(&self.paths, self.player_cmd_tx.clone(), tx)?;
        self.control = Some(snapshot);
        self.control_handle = Some(handle);
        Ok(())
    }

    fn apply_eq(&mut self, params: crate::player::EqParams) {
        self.eq = params.clamped();
        self.config.playback.eq_enabled = self.eq.enabled;
        self.config.playback.eq_gains = self.eq.gains_db;
        self.config.playback.eq_adaptive = self.eq_adaptive.as_config().to_string();
        let _ = self.config.save(&self.paths);
        let _ = self.player_cmd_tx.send(PlayerCommand::SetEq(self.eq));
    }

    /// Manual slider/preset edit — turns Adaptive off so modes don't fight.
    fn apply_eq_manual(&mut self, params: crate::player::EqParams) {
        if self.eq_adaptive != crate::player::AdaptiveProfile::Off {
            self.eq_adaptive = crate::player::AdaptiveProfile::Off;
            self.adaptive_eq.reset();
            self.push_toast(Toast::info("Adaptive EQ off (manual edit)"));
        }
        self.apply_eq(params);
    }

    fn cycle_eq_adaptive(&mut self) {
        self.eq_adaptive = self.eq_adaptive.next();
        self.adaptive_eq.reset();
        self.config.playback.eq_adaptive = self.eq_adaptive.as_config().to_string();
        let _ = self.config.save(&self.paths);
        if self.eq_adaptive == crate::player::AdaptiveProfile::Off {
            // Leave current gains as-is when leaving adaptive.
            self.push_toast(Toast::info("Adaptive EQ: Off"));
        } else {
            let mut p = self.eq;
            p.enabled = true;
            self.eq = p;
            self.config.playback.eq_enabled = true;
            let _ = self.config.save(&self.paths);
            self.push_toast(Toast::info(format!(
                "Adaptive EQ: {}",
                self.eq_adaptive.title()
            )));
        }
    }

    /// Analyze current PCM and nudge EQ toward the active Adaptive profile.
    pub fn tick_adaptive_eq(&mut self, samples: &[f32]) {
        if self.eq_adaptive == crate::player::AdaptiveProfile::Off {
            return;
        }
        if self.playback_state != PlaybackState::Playing {
            return;
        }
        if !self.eq.enabled {
            return;
        }
        let changed =
            self.adaptive_eq
                .update(samples, 44_100, self.eq_adaptive);
        if !changed {
            return;
        }
        let mut p = self.eq;
        p.gains_db = self.adaptive_eq.gains_db;
        // Apply live without writing config every 300ms (persist profile only).
        self.eq = p.clamped();
        let _ = self.player_cmd_tx.send(PlayerCommand::SetEq(self.eq));
    }

    fn run_context_action(&mut self, label: &str) -> Vec<Command> {
        let mut cmds = Vec::new();
        match label {
            "Play next" => {
                if let Some(t) = self.selected_track() {
                    let _ = self.player_cmd_tx.send(PlayerCommand::PlayNext(t));
                }
            }
            "Add to queue" => {
                if let Some(t) = self.selected_track() {
                    let _ = self.player_cmd_tx.send(PlayerCommand::AddToQueue(t));
                }
            }
            "Start radio" => {
                if let Some(t) = self.selected_track().or_else(|| self.current.clone()) {
                    cmds.push(Command::LoadRadio(t.video_id));
                }
            }
            "Go to album" => {
                if let Some(t) = self.selected_track() {
                    if let Some(ref a) = t.album {
                        if let Some(ref id) = a.id {
                            cmds.push(Command::LoadAlbum(id.clone()));
                        }
                    }
                }
            }
            "Go to artist" => {
                if let Some(t) = self.selected_track() {
                    if let Some(a) = t.artists.first() {
                        if let Some(ref id) = a.id {
                            cmds.push(Command::LoadArtist(id.clone()));
                        }
                    }
                }
            }
            "Like" => {
                if let Some(t) = self.selected_track() {
                    cmds.push(Command::Rate {
                        video_id: t.video_id,
                        liked: true,
                    });
                }
            }
            _ => {}
        }
        cmds
    }

    fn debounce_search(&mut self, cmds: &mut Vec<Command>) {
        if self.search_query.len() >= 2 {
            cmds.push(Command::SearchSuggestions(self.search_query.clone()));
        }
    }

    fn clear_search_results(&mut self) {
        self.search_tracks.clear();
        self.search_albums.clear();
        self.search_artists.clear();
        self.search_playlists.clear();
        self.list_state = TrackListState::default();
        self.suggestion_idx = 0;
    }

    fn on_tab_enter(&mut self, cmds: &mut Vec<Command>) {
        match self.tab {
            Tab::Home if self.home_shelves.is_empty() && self.pending_home_gen.is_none() => {
                self.request_home(cmds);
            }
            Tab::Library => self.load_library_section(cmds),
            Tab::Lyrics => {
                if let Some(ref t) = self.current {
                    cmds.push(Command::LoadLyrics {
                        video_id: t.video_id.clone(),
                        browse_id: None,
                        track: t.clone(),
                    });
                }
            }
            _ => {}
        }
    }

    fn load_library_section(&mut self, cmds: &mut Vec<Command>) {
        if !self.signed_in {
            return;
        }
        let section = self.library_section.min(4);
        self.list_state = TrackListState::default();
        // Reuse recent section data — re-fetching on every ←/→ made Library feel stuck.
        if self.library_fetched[section].is_some_and(|at| at.elapsed() < LIBRARY_TTL) {
            self.loading_library = false;
            return;
        }
        // Stale data stays on screen while it re-syncs; only an empty section shows loading.
        self.loading_library = self.library_len(section) == 0;
        match section {
            0 => cmds.push(Command::LoadLikedSongs),
            1 => cmds.push(Command::LoadLibraryPlaylists),
            2 => cmds.push(Command::LoadLibraryAlbums),
            3 => cmds.push(Command::LoadLibraryArtists),
            4 => cmds.push(Command::LoadHistory),
            _ => {}
        }
    }

    fn library_len(&self, section: usize) -> usize {
        match section {
            0 => self.liked_songs.len(),
            1 => self.library_playlists.len(),
            2 => self.library_albums.len(),
            3 => self.library_artists.len(),
            4 => self.history.len(),
            _ => 0,
        }
    }

    fn library_section_visible(&self, section: usize) -> bool {
        self.tab == Tab::Library && self.detail.is_none() && self.library_section.min(4) == section
    }

    /// Records a finished section fetch, keeping the selection in range of the new list.
    fn library_loaded(&mut self, section: usize) {
        self.library_fetched[section] = Some(Instant::now());
        if self.library_section.min(4) == section {
            self.loading_library = false;
        }
        if self.library_section_visible(section) {
            let len = self.library_len(section);
            self.list_state.selected = self.list_state.selected.min(len.saturating_sub(1));
            self.list_state.offset = self.list_state.offset.min(self.list_state.selected);
        }
    }

    /// Marks a section out of date after an edit, re-syncing it now if it is on screen.
    fn invalidate_library(&mut self, section: usize, cmds: &mut Vec<Command>) {
        self.library_fetched[section] = None;
        if self.library_section_visible(section) {
            self.load_library_section(cmds);
        }
    }

    fn request_home(&mut self, cmds: &mut Vec<Command>) {
        self.home_fetch_gen = self.home_fetch_gen.wrapping_add(1);
        let gen = self.home_fetch_gen;
        self.pending_home_gen = Some(gen);
        self.loading_home = true;
        cmds.push(Command::LoadHome { gen });
    }

    fn list_len(&self) -> usize {
        if let Some(ref d) = self.detail {
            return match d {
                DetailView::Album(a) => a.tracks.len(),
                DetailView::Artist(a) => a.top_songs.len(),
                DetailView::Playlist(p) => p.tracks.len(),
            };
        }
        match self.tab {
            Tab::Search => match self.search_filter {
                SearchFilter::Songs => self.search_tracks.len(),
                SearchFilter::Albums => self.search_albums.len(),
                SearchFilter::Artists => self.search_artists.len(),
                SearchFilter::Playlists => self.search_playlists.len(),
            },
            Tab::Library => match self.library_section {
                0 => self.liked_songs.len(),
                1 => self.library_playlists.len(),
                2 => self.library_albums.len(),
                3 => self.library_artists.len(),
                4 => self.history.len(),
                _ => 0,
            },
            Tab::Queue => self.queue.len(),
            Tab::Home => self
                .home_shelves
                .get(self.home_shelf_idx)
                .map(|s| s.items.len())
                .unwrap_or(0),
            Tab::Lyrics => 0,
        }
    }

    /// ←/→: move within Home shelves or switch Library sections; elsewhere seek ±5s.
    fn move_horizontal(&mut self, right: bool, cmds: &mut Vec<Command>) {
        let browsing = self.detail.is_none() && !self.fullscreen_np;
        match self.tab {
            Tab::Home if browsing && !self.home_shelves.is_empty() => {
                if right {
                    let len = self
                        .home_shelves
                        .get(self.home_shelf_idx)
                        .map_or(0, |s| s.items.len());
                    if self.home_item_idx + 1 < len {
                        self.home_item_idx += 1;
                    }
                } else {
                    if self.home_item_idx == 0 {
                        self.focus_sidebar();
                        return;
                    }
                    self.home_item_idx -= 1;
                }
                self.ensure_home_covers(cmds);
            }
            Tab::Library if browsing && self.library_section == 1 && self.list_len() > 0 => {
                let sel = self.list_state.selected;
                if right {
                    if sel + 1 < self.list_len() {
                        self.list_state.selected = sel + 1;
                    }
                } else if sel % self.grid_cols() == 0 {
                    self.focus_sidebar();
                } else {
                    self.list_state.selected = sel - 1;
                }
            }
            Tab::Library if browsing => {
                let n = crate::ui::views::library::SECTIONS.len();
                if !right && self.library_section == 0 {
                    self.focus_sidebar();
                    return;
                }
                let next = if right {
                    (self.library_section + 1) % n
                } else {
                    self.library_section - 1
                };
                self.library_section = next;
                self.list_state = TrackListState::default();
                self.load_library_section(cmds);
            }
            _ if !right => self.focus_sidebar(),
            _ => {
                let _ = self.player_cmd_tx.send(PlayerCommand::SeekRelative(5));
            }
        }
    }

    /// Move keyboard focus to the sidebar menu (no-op when it isn't visible).
    fn focus_sidebar(&mut self) {
        if crate::ui::widgets::sidebar::width_for(self.term_w) == 0 {
            let _ = self.player_cmd_tx.send(PlayerCommand::SeekRelative(-5));
            return;
        }
        self.sidebar_focus = true;
        self.search_editing = false;
    }

    fn move_sel(&mut self, delta: i32, cmds: &mut Vec<Command>) {
        if self.tab == Tab::Home && self.detail.is_none() {
            if delta < 0 {
                self.home_shelf_idx = self.home_shelf_idx.saturating_sub(1);
            } else if self.home_shelf_idx + 1 < self.home_shelves.len() {
                self.home_shelf_idx += 1;
            }
            self.home_item_idx = 0;
            self.ensure_home_covers(cmds);
            return;
        }
        let len = self.list_len();
        if self.in_playlist_grid() {
            let cols = self.grid_cols();
            let sel = self.list_state.selected;
            if delta < 0 {
                if sel >= cols {
                    self.list_state.selected = sel - cols;
                }
            } else if sel + cols < len {
                self.list_state.selected = sel + cols;
            } else if sel / cols < (len.saturating_sub(1)) / cols {
                // Last (short) row: land on its final card.
                self.list_state.selected = len - 1;
            }
            return;
        }
        if delta < 0 {
            self.list_state.select_prev();
        } else {
            self.list_state.select_next(len);
        }
    }

    fn selected_track(&self) -> Option<Track> {
        if let Some(ref d) = self.detail {
            let tracks = match d {
                DetailView::Album(a) => &a.tracks,
                DetailView::Artist(a) => &a.top_songs,
                DetailView::Playlist(p) => &p.tracks,
            };
            return tracks.get(self.list_state.selected).cloned();
        }
        match self.tab {
            Tab::Search if self.search_filter == SearchFilter::Songs => {
                self.search_tracks.get(self.list_state.selected).cloned()
            }
            Tab::Library if self.library_section == 0 => {
                self.liked_songs.get(self.list_state.selected).cloned()
            }
            Tab::Library if self.library_section == 4 => {
                self.history.get(self.list_state.selected).cloned()
            }
            Tab::Queue => self.queue.get(self.list_state.selected).cloned(),
            Tab::Home => {
                let shelf = self.home_shelves.get(self.home_shelf_idx)?;
                match shelf.items.get(self.home_item_idx)? {
                    ShelfItem::Track(t) => Some(t.clone()),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// After the highlight rests on a track for a moment, have the player get it
    /// ready; scrolling past tracks quickly fetches nothing.
    fn prewarm_highlighted(&mut self) {
        if !self.config.playback.prewarm || !matches!(self.overlay, Overlay::None) {
            return;
        }
        let Some(track) = self.selected_track() else {
            self.prewarm_dwell = None;
            return;
        };
        match &self.prewarm_dwell {
            Some((id, since)) if *id == track.video_id => {
                if since.elapsed() >= PREWARM_DWELL
                    && self.prewarm_sent.as_deref() != Some(id.as_str())
                {
                    self.prewarm_sent = Some(id.clone());
                    let _ = self.player_cmd_tx.send(PlayerCommand::Prewarm(track));
                }
            }
            _ => self.prewarm_dwell = Some((track.video_id, Instant::now())),
        }
    }

    /// Mark the UI as loading immediately so Enter feels instant while resolve/download run.
    fn begin_play_track(&mut self, track: Track) -> Vec<Command> {
        self.playback_state = PlaybackState::Loading;
        self.current = Some(track.clone());
        let mut cmds = Vec::new();
        self.ensure_track_art(&track, &mut cmds);
        let _ = self.player_cmd_tx.send(PlayerCommand::PlayTrack(track));
        cmds
    }

    fn begin_play_tracks(&mut self, tracks: Vec<Track>, start: usize) -> Vec<Command> {
        let mut cmds = Vec::new();
        if let Some(t) = tracks.get(start.min(tracks.len().saturating_sub(1))) {
            self.playback_state = PlaybackState::Loading;
            self.current = Some(t.clone());
            self.ensure_track_art(t, &mut cmds);
        }
        let _ = self.player_cmd_tx.send(PlayerCommand::PlayTracks { tracks, start });
        cmds
    }

    /// Applies the current track's cover color once, including for covers served from cache.
    fn sync_accent_to_cover(&mut self) {
        let Some(id) = self.current.as_ref().map(|t| t.video_id.clone()) else {
            return;
        };
        if self.accent_for.as_deref() == Some(id.as_str()) {
            return;
        }
        if let Some(cover) = self.covers.get(&id) {
            if let Some((r, g, b)) = cover.accent {
                self.theme.set_accent_rgb(r, g, b);
            }
            self.accent_for = Some(id);
        }
    }

    fn ensure_track_art(&mut self, track: &Track, cmds: &mut Vec<Command>) {
        if self.covers.contains_key(&track.video_id) {
            return;
        }
        if let Some(url) = track.art_url() {
            cmds.push(Command::FetchArt {
                url,
                video_id: track.video_id.clone(),
            });
        }
    }

    fn activate_selection(&mut self) -> Vec<Command> {
        let mut cmds = Vec::new();
        if let Some(ref d) = self.detail {
            let tracks = match d {
                DetailView::Album(a) => a.tracks.clone(),
                DetailView::Artist(a) => a.top_songs.clone(),
                DetailView::Playlist(p) => p.tracks.clone(),
            };
            if !tracks.is_empty() {
                let start = self.list_state.selected.min(tracks.len() - 1);
                cmds.extend(self.begin_play_tracks(tracks, start));
            }
            return cmds;
        }
        match self.tab {
            Tab::Search => match self.search_filter {
                SearchFilter::Songs => {
                    if self.search_tracks.is_empty() {
                        if let Some(s) = self.search_suggestions.get(self.suggestion_idx) {
                            self.search_query = s.clone();
                            self.loading_search = true;
                            cmds.push(Command::Search {
                                query: s.clone(),
                                filter: self.search_filter,
                            });
                        }
                    } else {
                        let start = self.list_state.selected;
                        cmds.extend(self.begin_play_tracks(self.search_tracks.clone(), start));
                    }
                }
                SearchFilter::Albums => {
                    if let Some(a) = self.search_albums.get(self.list_state.selected) {
                        cmds.push(Command::LoadAlbum(a.id.clone()));
                    }
                }
                SearchFilter::Artists => {
                    if let Some(a) = self.search_artists.get(self.list_state.selected) {
                        cmds.push(Command::LoadArtist(a.id.clone()));
                    }
                }
                SearchFilter::Playlists => {
                    if let Some(p) = self.search_playlists.get(self.list_state.selected) {
                        cmds.push(Command::LoadPlaylist(p.id.clone()));
                    }
                }
            },
            Tab::Home => {
                if let Some(shelf) = self.home_shelves.get(self.home_shelf_idx) {
                    if let Some(item) = shelf.items.get(self.home_item_idx) {
                        match item {
                            ShelfItem::Track(t) => {
                                cmds.extend(self.begin_play_track(t.clone()));
                            }
                            ShelfItem::Album(a) => {
                                self.loading_detail = true;
                                cmds.push(Command::LoadAlbum(a.id.clone()));
                            }
                            ShelfItem::Artist(a) => {
                                self.loading_detail = true;
                                cmds.push(Command::LoadArtist(a.id.clone()));
                            }
                            ShelfItem::Playlist(p) => {
                                let id = p.id.clone();
                                self.loading_detail = true;
                                self.push_toast(Toast::info("Loading playlist…"));
                                cmds.push(Command::LoadPlaylist(id));
                            }
                        }
                    }
                }
            }
            Tab::Library => match self.library_section {
                0 | 4 => {
                    let tracks = if self.library_section == 0 {
                        self.liked_songs.clone()
                    } else {
                        self.history.clone()
                    };
                    if !tracks.is_empty() {
                        let start = self.list_state.selected.min(tracks.len() - 1);
                        cmds.extend(self.begin_play_tracks(tracks, start));
                    }
                }
                1 => {
                    if let Some(p) = self.library_playlists.get(self.list_state.selected) {
                        let id = p.id.clone();
                        self.loading_detail = true;
                        self.push_toast(Toast::info("Loading playlist…"));
                        cmds.push(Command::LoadPlaylist(id));
                    }
                }
                2 => {
                    if let Some(a) = self.library_albums.get(self.list_state.selected) {
                        cmds.push(Command::LoadAlbum(a.id.clone()));
                    }
                }
                3 => {
                    if let Some(a) = self.library_artists.get(self.list_state.selected) {
                        cmds.push(Command::LoadArtist(a.id.clone()));
                    }
                }
                _ => {}
            },
            Tab::Queue => {
                if let Some(t) = self.queue.get(self.list_state.selected).cloned() {
                    self.playback_state = PlaybackState::Loading;
                    self.current = Some(t);
                }
                let _ = self
                    .player_cmd_tx
                    .send(PlayerCommand::JumpTo(self.list_state.selected));
            }
            Tab::Lyrics => {
                if let Some(ref lyrics) = self.lyrics {
                    if let Some(ref synced) = lyrics.synced {
                        if let Some(idx) = crate::lyrics::current_line_index(synced, self.position)
                        {
                            // seek to selected — use list_state as line index if we add that later
                            let _ = idx;
                        }
                    }
                }
            }
        }
        cmds
    }

    fn apply_event(&mut self, ev: AppEvent) -> Vec<Command> {
        let mut cmds = Vec::new();
        match ev {
            AppEvent::AuthState(AuthStateEvent::SignedIn(session)) => {
                self.signed_in = true;
                self.account_name = session.account_name.clone();
                self.overlay = Overlay::None;
                self.library_fetched = [None; 5];
                self.push_toast(Toast::success(format!(
                    "Signed in as {}",
                    session.account_name.as_deref().unwrap_or("user")
                )));
                cmds.push(Command::ApplyAuth(session));
                // Refresh once for the personalized feed; ignore overlapping guest fetches.
                self.request_home(&mut cmds);
            }
            AppEvent::AuthState(AuthStateEvent::WaitingForBrowser { .. }) => {
                self.onboarding_status =
                    "Waiting for you to log in at music.youtube.com…".into();
            }
            AppEvent::AuthState(AuthStateEvent::Progress(msg)) => {
                self.onboarding_status = msg;
            }
            AppEvent::AuthState(AuthStateEvent::Failed(e)) => {
                self.push_toast(Toast::error(e.clone()));
                self.onboarding_status = format!("{e}\n\n  [Enter] retry   [P] paste cookie   [G] guest");
            }
            AppEvent::AuthState(AuthStateEvent::Guest) => {
                self.signed_in = false;
            }
            AppEvent::AuthState(AuthStateEvent::LoggedOut) => {
                self.signed_in = false;
                self.account_name = None;
                self.library_fetched = [None; 5];
                self.liked_songs.clear();
                self.library_playlists.clear();
                self.library_albums.clear();
                self.library_artists.clear();
                self.history.clear();
                cmds.push(Command::ClearAuth);
            }
            AppEvent::AuthState(AuthStateEvent::SessionExpired(msg)) => {
                self.signed_in = false;
                self.loading_library = false;
                self.library_fetched = [None; 5];
                self.liked_songs.clear();
                self.library_playlists.clear();
                self.library_albums.clear();
                self.library_artists.clear();
                self.history.clear();
                self.push_toast(Toast::error(
                    "Session expired — press L to sign in again",
                ));
                self.onboarding_status = format!(
                    "{msg}\n\n  Your username was cached, but YouTube is treating this session as signed out.\n  \
                     Press L (or P) to paste a fresh cURL / cookie."
                );
                self.overlay = Overlay::Onboarding;
            }
            AppEvent::Account(Some(a)) => {
                self.account_name = Some(a.name);
                self.signed_in = true;
            }
            AppEvent::Account(None) => {}
            AppEvent::SearchResults {
                tracks,
                albums,
                artists,
                playlists,
                ..
            } => {
                self.loading_search = false;
                self.search_tracks = tracks;
                self.search_albums = albums;
                self.search_artists = artists;
                self.search_playlists = playlists;
                self.list_state = TrackListState::default();
                // Results are ready — leave the box so Enter plays (Esc does the same).
                self.search_editing = false;
            }
            AppEvent::SearchSuggestions(s) => self.search_suggestions = s,
            AppEvent::Home { shelves, gen } => {
                // Drop stale responses from overlapping LoadHome calls so the feed
                // doesn't keep swapping as older requests finish.
                if let Some(pending) = self.pending_home_gen {
                    if gen != pending {
                        return cmds;
                    }
                    self.pending_home_gen = None;
                }
                self.loading_home = false;
                if !shelves.is_empty() {
                    let shelf_len = shelves.len();
                    self.home_shelves = shelves;
                    self.home_shelf_idx = self.home_shelf_idx.min(shelf_len.saturating_sub(1));
                    let item_len = self
                        .home_shelves
                        .get(self.home_shelf_idx)
                        .map(|s| s.items.len())
                        .unwrap_or(0);
                    self.home_item_idx = self.home_item_idx.min(item_len.saturating_sub(1));
                    self.ensure_home_covers(&mut cmds);
                }
            }
            AppEvent::AlbumLoaded(a) => {
                self.loading_detail = false;
                if let Some(old) = self.detail.take() {
                    self.nav_stack.push(old);
                }
                self.detail = Some(DetailView::Album(a));
                self.list_state = TrackListState::default();
                self.fetch_detail_cover(&mut cmds);
            }
            AppEvent::ArtistLoaded(a) => {
                self.loading_detail = false;
                if let Some(old) = self.detail.take() {
                    self.nav_stack.push(old);
                }
                self.detail = Some(DetailView::Artist(a));
                self.list_state = TrackListState::default();
                self.fetch_detail_cover(&mut cmds);
            }
            AppEvent::PlaylistLoaded(p) => {
                self.loading_detail = false;
                if let Some(old) = self.detail.take() {
                    self.nav_stack.push(old);
                }
                self.detail = Some(DetailView::Playlist(p));
                self.list_state = TrackListState::default();
                self.fetch_detail_cover(&mut cmds);
            }
            AppEvent::LibraryPlaylists(p) => {
                self.library_playlists = p;
                self.library_loaded(1);
                self.ensure_playlist_covers(&mut cmds);
            }
            AppEvent::LikedSongs(t) => {
                self.liked_songs = t;
                self.library_loaded(0);
            }
            AppEvent::LibraryAlbums(a) => {
                self.library_albums = a;
                self.library_loaded(2);
            }
            AppEvent::LibraryArtists(a) => {
                self.library_artists = a;
                self.library_loaded(3);
            }
            AppEvent::History(t) => {
                self.history = t;
                self.library_loaded(4);
            }
            AppEvent::RadioTracks { tracks, .. } => {
                let _ = self
                    .player_cmd_tx
                    .send(PlayerCommand::AppendRadio(tracks));
            }
            AppEvent::LyricsLoaded { video_id, lyrics } => {
                if self.current.as_ref().map(|t| t.video_id.as_str()) == Some(video_id.as_str())
                    || self.lyrics.is_none()
                {
                    self.lyrics = Some(lyrics);
                }
            }
            AppEvent::ArtLoaded {
                video_id,
                accent,
                pixels,
                image,
                ..
            } => {
                // Always cache — a late reply after a quick skip still warms the
                // next play, and discarding here left the UI stuck on placeholders.
                self.covers
                    .insert(video_id, crate::art::Cover { pixels, image, accent });
                self.prune_covers();
            }
            AppEvent::RateDone { video_id, liked } => {
                if let Some(ref mut t) = self.current {
                    if t.video_id == video_id {
                        t.liked = Some(liked);
                    }
                }
                self.push_toast(Toast::info(if liked {
                    "Liked"
                } else {
                    "Removed like"
                }));
                self.invalidate_library(0, &mut cmds);
            }
            AppEvent::PlaylistCreated(_) => {
                self.push_toast(Toast::success("Playlist created"));
                self.invalidate_library(1, &mut cmds);
            }
            AppEvent::AddedToPlaylist => {
                self.push_toast(Toast::success("Added to playlist"));
                self.invalidate_library(1, &mut cmds);
            }
            AppEvent::Error(e) => {
                self.loading_library = false;
                self.loading_home = false;
                self.loading_search = false;
                self.loading_detail = false;
                self.pending_home_gen = None;
                self.push_toast(Toast::error(e));
            }
            AppEvent::Info(i) => self.push_toast(Toast::info(i)),
            AppEvent::QueueSnapshot { tracks, cursor } => {
                self.queue = tracks;
                self.queue_cursor = cursor;
                // Play-order snapshot: tracks[cursor] is what's playing. Only
                // update current when we're not mid-load with a newer selection.
                if !matches!(
                    self.playback_state,
                    PlaybackState::Loading | PlaybackState::Buffering
                ) {
                    if let Some(t) = self.queue.get(cursor).cloned() {
                        let changed = self.current.as_ref().map(|c| c.video_id.as_str())
                            != Some(t.video_id.as_str());
                        self.current = Some(t.clone());
                        // A restored session never goes through Loading, so
                        // fetch its cover and lyrics here.
                        if changed {
                            self.ensure_track_art(&t, &mut cmds);
                            cmds.push(Command::LoadLyrics {
                                video_id: t.video_id.clone(),
                                browse_id: None,
                                track: t,
                            });
                        }
                    }
                }
            }
            AppEvent::Player(pe) => {
                cmds.extend(self.apply_player_event(pe));
            }
        }
        cmds
    }

    fn apply_player_event(&mut self, pe: PlayerEvent) -> Vec<Command> {
        let mut cmds = Vec::new();
        match pe {
            PlayerEvent::Loading { track } => {
                self.playback_state = PlaybackState::Loading;
                self.current = Some(track.clone());
                // Fetch art + lyrics
                self.ensure_track_art(&track, &mut cmds);
                cmds.push(Command::LoadLyrics {
                    video_id: track.video_id.clone(),
                    browse_id: None,
                    track: track.clone(),
                });
            }
            PlayerEvent::Playing { track } => {
                self.playback_state = PlaybackState::Playing;
                self.current = Some(track.clone());
                self.duration = track.duration;
                self.ensure_track_art(&track, &mut cmds);
                // Autoplay radio nudge
                if self.autoplay
                    && self.queue.len().saturating_sub(self.queue_cursor + 1) <= 2
                    && self.radio_requested_for.as_deref() != Some(track.video_id.as_str())
                {
                    self.radio_requested_for = Some(track.video_id.clone());
                    cmds.push(Command::LoadRadio(track.video_id));
                }
            }
            PlayerEvent::Paused => self.playback_state = PlaybackState::Paused,
            PlayerEvent::Resumed => self.playback_state = PlaybackState::Playing,
            PlayerEvent::Stopped => {
                self.playback_state = PlaybackState::Stopped;
            }
            PlayerEvent::Position(p) => self.position = p,
            PlayerEvent::Duration(d) => self.duration = Some(d),
            PlayerEvent::Volume(v) => self.volume = v,
            PlayerEvent::Buffering(b) => {
                if b {
                    self.playback_state = PlaybackState::Buffering;
                }
            }
            PlayerEvent::Ended => {
                let _ = self.player_cmd_tx.send(PlayerCommand::HandleEnded);
            }
            PlayerEvent::Error(e) => {
                // #region agent log
                crate::debug_log::log(
                    "A",
                    "app.rs:PlayerEvent::Error",
                    "player error toast",
                    serde_json::json!({ "error": e }),
                );
                // #endregion
                // Only auto-skip on hard failures. Transient open/decode races are
                // retried inside the engine; if we still get here, advance the queue.
                self.push_toast(Toast::error(e));
                self.playback_state = PlaybackState::Stopped;
                let _ = self.player_cmd_tx.send(PlayerCommand::Next);
            }
            PlayerEvent::DownloadProgress { .. } => {}
            PlayerEvent::Stream { video_id, info } => {
                self.visualizer.set_sample_rate(info.sample_rate);
                self.stream_info = Some((video_id, info));
            }
            PlayerEvent::Premium(p) => self.premium = p,
        }
        cmds
    }

    pub fn sync_queue_snapshot(&mut self, tracks: Vec<Track>, cursor: usize) {
        self.queue = tracks;
        self.queue_cursor = cursor;
    }
}

fn contains(r: ratatui::layout::Rect, col: u16, row: u16) -> bool {
    col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height
}

fn mouse_click_action(hit: &crate::ui::HitMap, col: u16, row: u16) -> Option<Action> {
    if let Some((_, i)) = hit.tabs.iter().find(|(r, _)| contains(*r, col, row)) {
        return Some(Action::Tab(*i));
    }
    if let Some((_, a)) = hit.buttons.iter().find(|(r, _)| contains(*r, col, row)) {
        return Some(a.clone());
    }
    let ratio_in = |r: ratatui::layout::Rect| {
        ((col.saturating_sub(r.x)) as f64 / r.width.saturating_sub(1).max(1) as f64).clamp(0.0, 1.0)
    };
    if let Some(bar) = hit.progress {
        let padded = ratatui::layout::Rect {
            x: bar.x.saturating_sub(1),
            width: bar.width + 2,
            ..bar
        };
        if contains(padded, col, row) && !hit.duration.is_zero() {
            let pos = Duration::from_secs_f64(hit.duration.as_secs_f64() * ratio_in(bar));
            return Some(Action::SeekTo(pos));
        }
    }
    if let Some(vol) = hit.volume {
        if vol.width > 0 && contains(vol, col, row) {
            let v = (col - vol.x + 1) as f32 / vol.width as f32;
            return Some(Action::SetVolume(v));
        }
    }
    if let Some((_, i)) = hit.list_rows.iter().find(|(r, _)| contains(*r, col, row)) {
        return Some(Action::SelectAt(*i));
    }
    if let Some((_, s, i)) = hit.home_cards.iter().find(|(r, _, _)| contains(*r, col, row)) {
        return Some(Action::SelectHomeItem {
            shelf: *s,
            item: *i,
        });
    }
    // Clicks on chrome / empty space / search field must not restart playback.
    None
}

fn apply_cmds(app: &mut App, cmd_tx: &mpsc::UnboundedSender<Command>, action: Action) {
    let cmds = app.update(action);
    for c in cmds {
        let _ = cmd_tx.send(c);
    }
}

/// Longest search / palette query a paste can produce.
const MAX_QUERY_CHARS: usize = 200;

/// Flatten pasted text for a one-line field: line breaks, tabs and other
/// control characters become spaces, runs of whitespace collapse, and the
/// result is trimmed and capped at `max` characters.
fn single_line(text: &str, max: usize) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max)
        .collect()
}

fn handle_terminal_event(
    app: &mut App,
    cmd_tx: &mpsc::UnboundedSender<Command>,
    ev: Event,
    _terminal: &mut ratatui::DefaultTerminal,
) -> Result<()> {
    match ev {
        Event::Paste(text) => {
            apply_cmds(app, cmd_tx, Action::PasteText(text));
        }
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            let ctx = app.input_context();
            if let Some(action) = keymap::map_key(ctx, key) {
                apply_cmds(app, cmd_tx, action);
            }
        }
        Event::Mouse(me) => match me.kind {
            // Motion floods the event stream; ignore so we don't redraw every pixel.
            MouseEventKind::Moved | MouseEventKind::Drag(_) => {}
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = matches!(me.kind, MouseEventKind::ScrollUp);
                let in_player = app
                    .hit
                    .borrow()
                    .player
                    .is_some_and(|r| contains(r, me.column, me.row));
                let action = match (in_player, up) {
                    (true, true) => Action::VolumeUp,
                    (true, false) => Action::VolumeDown,
                    (false, true) => Action::MoveUp,
                    (false, false) => Action::MoveDown,
                };
                apply_cmds(app, cmd_tx, action);
            }
            MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                let action = mouse_click_action(&app.hit.borrow(), me.column, me.row);
                if let Some(action) = action {
                    apply_cmds(app, cmd_tx, action);
                }
            }
            MouseEventKind::Down(crossterm::event::MouseButton::Right) => {
                apply_cmds(app, cmd_tx, Action::ContextMenu);
            }
            _ => {}
        },
        Event::Resize(w, h) => {
            let _ = app.update(Action::Resize(w, h));
        }
        _ => {}
    }
    Ok(())
}

fn event_needs_redraw(ev: &Event) -> bool {
    match ev {
        Event::Mouse(me) => !matches!(
            me.kind,
            MouseEventKind::Moved | MouseEventKind::Drag(_)
        ),
        Event::FocusGained | Event::FocusLost => false,
        _ => true,
    }
}

fn app_event_needs_redraw(ev: &AppEvent) -> bool {
    // Download progress is throttled upstream and currently unused by the UI.
    !matches!(
        ev,
        AppEvent::Player(PlayerEvent::DownloadProgress { .. })
    )
}

/// Suck up a burst of pasted characters (or a bracketed paste) without redrawing
/// between each one — cURL pastes can be tens of thousands of key events.
async fn coalesce_paste_flood(
    app: &mut App,
    cmd_tx: &mpsc::UnboundedSender<Command>,
    events: &mut EventStream,
    terminal: &mut ratatui::DefaultTerminal,
) {
    let mut buf = String::new();
    let deadline = Instant::now() + Duration::from_millis(80);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(8), events.next()).await {
            Ok(Some(Ok(Event::Paste(text)))) => {
                buf.push_str(&text);
                // Bracketed paste usually arrives as one event; keep draining briefly
                // in case the terminal splits it.
            }
            Ok(Some(Ok(Event::Key(k))))
                if k.kind == KeyEventKind::Press
                    && matches!(k.code, KeyCode::Char(_))
                    && !k.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                if let KeyCode::Char(c) = k.code {
                    buf.push(c);
                }
                if buf.len() > 512 * 1024 {
                    break;
                }
            }
            Ok(Some(Ok(other))) => {
                if !buf.is_empty() {
                    apply_cmds(app, cmd_tx, Action::PasteText(std::mem::take(&mut buf)));
                }
                let _ = handle_terminal_event(app, cmd_tx, other, terminal);
                break;
            }
            Ok(Some(Err(_))) | Ok(None) | Err(_) => break,
        }
    }
    if !buf.is_empty() {
        apply_cmds(app, cmd_tx, Action::PasteText(buf));
    }
}

/// Resolves when a background-encoded cover is ready; pends forever without graphics.
async fn wait_cover_ready(notify: &Option<Arc<tokio::sync::Notify>>) {
    match notify {
        Some(n) => n.notified().await,
        None => std::future::pending().await,
    }
}

/// Next control-bridge action; pends forever when the bridge is disabled.
async fn recv_control(rx: &mut Option<mpsc::UnboundedReceiver<Action>>) -> Option<Action> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

pub async fn run_ui(
    mut app: App,
    mut event_rx: mpsc::UnboundedReceiver<AppEvent>,
    cmd_tx: mpsc::UnboundedSender<Command>,
    mut media: MediaControlsBridge,
    sample_provider: Arc<dyn Fn(&mut Vec<f32>) + Send + Sync>,
    mut control_rx: Option<mpsc::UnboundedReceiver<Action>>,
) -> Result<()> {
    let mut terminal = ratatui::init();
    crossterm::execute!(
        std::io::stdout(),
        crossterm::event::EnableMouseCapture,
        EnableBracketedPaste
    )?;
    app.cover_ui = crate::ui::cover::CoverRenderer::detect(&app.config.ui.art_protocol);
    let cover_ready = app.cover_ui.wake_handle();
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(33)); // ~30fps
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut idle_tick = tokio::time::interval(Duration::from_millis(250));
    idle_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_viz = std::time::Instant::now()
        .checked_sub(Duration::from_millis(50))
        .unwrap_or_else(std::time::Instant::now);

    // Initial home load if signed in / guest after onboarding skipped
    if !matches!(app.overlay, Overlay::Onboarding) {
        let mut cmds = Vec::new();
        app.request_home(&mut cmds);
        for c in cmds {
            let _ = cmd_tx.send(c);
        }
    }
    // Draw once before first event
    terminal.draw(|f| crate::ui::draw(f, &app))?;
    let mut dirty = false;
    let mut last_draw = Instant::now();
    let mut last_publish = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);

    /// Minimum spacing between frames; input arriving faster is coalesced into one draw.
    const FRAME: Duration = Duration::from_millis(16);

    let result = loop {
        let playing = app.playback_state == PlaybackState::Playing;
        // A throttled frame must still be drawn once the spacing has passed, not whenever
        // the next tick happens to arrive (up to 250 ms later while paused).
        let redraw_at = tokio::time::Instant::from_std(last_draw + FRAME);

        tokio::select! {
            _ = tokio::time::sleep_until(redraw_at), if dirty => {}
            _ = wait_cover_ready(&cover_ready) => {
                dirty = true;
            }
            _ = tick.tick(), if playing => {
                for a in media.drain_actions() {
                    let cmds = app.update(a);
                    for c in cmds { let _ = cmd_tx.send(c); }
                }
                let cmds = app.update(Action::Tick);
                for c in cmds { let _ = cmd_tx.send(c); }
                if let Some(ref t) = app.current {
                    if app.media_meta_video_id.as_deref() != Some(t.video_id.as_str()) {
                        app.media_meta_video_id = Some(t.video_id.clone());
                        media.set_metadata(
                            t,
                            t.best_thumbnail().map(|th| th.upscaled_url()).as_deref(),
                        );
                    }
                    media.set_playing(app.position);
                }
                dirty = true;
            }
            _ = idle_tick.tick(), if !playing => {
                for a in media.drain_actions() {
                    let cmds = app.update(a);
                    for c in cmds { let _ = cmd_tx.send(c); }
                }
                let cmds = app.update(Action::Tick);
                for c in cmds { let _ = cmd_tx.send(c); }
                if let Some(ref t) = app.current {
                    media.set_metadata(t, None);
                    media.set_paused(app.position);
                }
                dirty = true;
            }
            maybe_ev = events.next() => {
                match maybe_ev {
                    Some(Ok(ev)) => {
                        let needs = event_needs_redraw(&ev);
                        let in_cookie = matches!(app.overlay, Overlay::ManualCookie { .. });
                        let looks_like_paste = match &ev {
                            Event::Paste(_) => true,
                            Event::Key(k)
                                if k.kind == KeyEventKind::Press
                                    && matches!(k.code, KeyCode::Char(_))
                                    && !k.modifiers.contains(KeyModifiers::CONTROL) =>
                            {
                                true
                            }
                            _ => false,
                        };
                        handle_terminal_event(&mut app, &cmd_tx, ev, &mut terminal)?;
                        if in_cookie && looks_like_paste {
                            coalesce_paste_flood(
                                &mut app,
                                &cmd_tx,
                                &mut events,
                                &mut terminal,
                            )
                            .await;
                            dirty = true;
                        } else if needs {
                            dirty = true;
                        }
                    }
                    Some(Err(e)) => {
                        tracing::error!("event error: {e}");
                    }
                    None => break Ok(()),
                }
            }
            Some(a) = recv_control(&mut control_rx) => {
                let cmds = app.update(a);
                for c in cmds { let _ = cmd_tx.send(c); }
                dirty = true;
            }
            ev = event_rx.recv() => {
                match ev {
                    Some(ev) => {
                        let needs = app_event_needs_redraw(&ev);
                        let cmds = app.update(Action::ApplyEvent(ev));
                        for c in cmds { let _ = cmd_tx.send(c); }
                        if needs {
                            dirty = true;
                        }
                    }
                    None => break Ok(()),
                }
            }
        }

        if let Some(shared) = &app.control {
            if last_publish.elapsed() >= Duration::from_millis(200) {
                *shared.lock() = crate::control::Snapshot::from_app(&app);
                last_publish = Instant::now();
            }
        }

        if !dirty || last_draw.elapsed() < FRAME {
            if app.should_quit {
                break Ok(());
            }
            continue;
        }

        // Feed the visualizer every frame path, not only on the playing tick.
        // Crossterm / API events can starve `tick` in `select!`, which left both
        // the mini bar and fullscreen NP stuck flat.
        let playing_now = app.playback_state == PlaybackState::Playing;
        if last_viz.elapsed() >= Duration::from_millis(33) {
            last_viz = Instant::now();
            app.visualizer.set_band_count(48);
            if playing_now {
                sample_provider(&mut app.sample_scratch);
                app.visualizer.update(&app.sample_scratch, true);
                let pcm = app.sample_scratch.clone();
                app.tick_adaptive_eq(&pcm);
            } else {
                app.visualizer.update(&[], false);
            }
        }

        terminal.draw(|f| crate::ui::draw(f, &app))?;
        last_draw = Instant::now();
        dirty = false;

        if app.should_quit {
            break Ok(());
        }
    };

    // Persist session
    let session = crate::state::SessionState {
        queue: app.queue.clone(),
        index: app.queue_cursor,
        position_ms: app.position.as_millis() as u64,
        volume: app.volume,
        shuffle: app.shuffle,
        repeat: app.repeat,
        last_tab: app.tab as usize,
        autoplay: app.autoplay,
    };
    let _ = session.save(&app.paths);
    let _ = app.config.save(&app.paths);

    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableMouseCapture,
        DisableBracketedPaste
    );
    ratatui::restore();
    result
}

#[cfg(test)]
mod paste_tests {
    use super::single_line;

    #[test]
    fn paste_flattens_to_one_line() {
        assert_eq!(single_line("  Bausa – Was du Liebe nennst\n", 200), "Bausa – Was du Liebe nennst");
        assert_eq!(single_line("Post Malone\r\n\tFuck", 200), "Post Malone Fuck");
        assert_eq!(single_line("a\u{1b}[31mb", 200), "a [31mb");
        assert_eq!(single_line("Träumerei über alles", 8), "Träumere");
        assert_eq!(single_line(" \n\t ", 200), "");
    }
}

#[cfg(test)]
mod prewarm_tests {
    use super::*;
    use crate::api::model::{ArtistRef, Shelf, ShelfItem};

    fn app(prewarm: bool) -> (App, mpsc::UnboundedReceiver<PlayerCommand>) {
        let root = std::env::temp_dir().join(format!("yt-tui-prewarm-test-{}", std::process::id()));
        let paths = Arc::new(Paths {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
        });
        let mut config = Config::default();
        config.playback.prewarm = prewarm;
        let (cmd_tx, _) = mpsc::unbounded_channel();
        let (player_tx, player_rx) = mpsc::unbounded_channel();
        let mut app = App::new(paths, config, cmd_tx, player_tx, true, None);
        app.overlay = Overlay::None;
        app.tab = Tab::Home;
        app.home_shelves = vec![Shelf {
            title: "Quick picks".into(),
            items: (0..3)
                .map(|i| {
                    ShelfItem::Track(Track {
                        video_id: format!("v{i}"),
                        title: format!("Song {i}"),
                        artists: vec![ArtistRef { id: None, name: "A".into() }],
                        album: None,
                        duration: None,
                        thumbnails: vec![],
                        explicit: false,
                        liked: None,
                        from_radio: false,
                    })
                })
                .collect(),
        }];
        (app, player_rx)
    }

    fn prewarmed(rx: &mut mpsc::UnboundedReceiver<PlayerCommand>) -> Vec<String> {
        std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|c| match c {
                PlayerCommand::Prewarm(t) => Some(t.video_id),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn prewarms_highlight_after_dwell_once() {
        let (mut app, mut rx) = app(true);
        app.update(Action::Tick);
        assert!(prewarmed(&mut rx).is_empty(), "not before the dwell");
        // Scrolling on resets the dwell.
        app.home_item_idx = 1;
        app.prewarm_dwell = Some(("v1".into(), Instant::now() - PREWARM_DWELL));
        app.update(Action::Tick);
        app.update(Action::Tick);
        assert_eq!(prewarmed(&mut rx), ["v1"]);
        app.home_item_idx = 2;
        app.update(Action::Tick);
        assert!(prewarmed(&mut rx).is_empty());
    }

    #[test]
    fn prewarm_can_be_turned_off() {
        let (mut app, mut rx) = app(false);
        app.prewarm_dwell = Some(("v0".into(), Instant::now() - PREWARM_DWELL));
        app.update(Action::Tick);
        assert!(prewarmed(&mut rx).is_empty());
    }
}
