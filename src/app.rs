use crate::action::*;
use crate::api::model::*;
use crate::auth::{self, AuthService, AuthSession};
use crate::config::Config;
use crate::paths::Paths;
use crate::player::queue::RepeatMode;
use crate::player::{PlaybackState, PlayerService};
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
use tracing::info;

/// Window in which a second Ctrl+C confirms quit.
const CTRL_C_QUIT_WINDOW: Duration = Duration::from_secs(2);

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

    pub library_section: usize,
    pub liked_songs: Vec<Track>,
    pub library_playlists: Vec<Playlist>,
    pub library_albums: Vec<Album>,
    pub library_artists: Vec<Artist>,
    pub history: Vec<Track>,
    pub loading_library: bool,
    /// Which library sections already have a successful fetch this session.
    library_fetched: [bool; 5],
    pub loading_detail: bool,

    pub lyrics: Option<Lyrics>,
    pub list_state: TrackListState,
    pub detail: Option<DetailView>,
    pub nav_stack: Vec<DetailView>,
    /// Last track id pushed to OS media controls (avoid spamming every tick).
    media_meta_video_id: Option<String>,

    pub toasts: Vec<Toast>,
    pub deps_status: String,
    pub visualizer: Visualizer,
    pub sample_scratch: Vec<f32>,
    pub title_marquee: crate::ui::marquee::Marquee,
    /// Loaded artwork keyed by track `video_id` or `detail:<browse_id>`.
    pub covers: std::collections::HashMap<String, crate::art::Cover>,
    pub cover_ui: crate::ui::cover::CoverRenderer,
    pub hit: std::cell::RefCell<crate::ui::HitMap>,
    radio_requested_for: Option<String>,

    pub should_quit: bool,
    pub first_run: bool,
    /// Set on first Ctrl+C; a second press within `CTRL_C_QUIT_WINDOW` exits.
    ctrl_c_armed_at: Option<Instant>,

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
    RefreshCookies,
    SetDeps { ytdlp: std::path::PathBuf, js: Option<String> },
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
        let theme = Theme::new(
            &config.ui.accent,
            config.ui.theme == "dynamic",
        );
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
            cover_ui: Default::default(),
            hit: Default::default(),
            radio_requested_for: None,
            volume: config.playback.volume,
            autoplay: config.playback.autoplay,
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
            library_section: 0,
            liked_songs: Vec::new(),
            library_playlists: Vec::new(),
            library_albums: Vec::new(),
            library_artists: Vec::new(),
            history: Vec::new(),
            loading_library: false,
            library_fetched: [false; 5],
            loading_detail: false,
            lyrics: None,
            list_state: TrackListState::default(),
            detail: None,
            nav_stack: Vec::new(),
            toasts: Vec::new(),
            deps_status: String::new(),
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
        const MAX_COVERS: usize = 48;
        if self.covers.len() <= MAX_COVERS {
            return;
        }
        let mut keep: Vec<String> = self.nav_stack.iter().map(DetailView::cover_key).collect();
        keep.extend(self.detail_cover_key());
        keep.extend(self.current.as_ref().map(|t| t.video_id.clone()));
        // Drop arbitrary extras first; never evict the active/detail covers.
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
            Overlay::ContextMenu { .. } | Overlay::AccountMenu => InputContext::Global,
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
            ("Help", "help"),
            ("Quit", "quit"),
        ];
        let q = match &self.overlay {
            Overlay::CommandPalette { query, .. } => query.to_lowercase(),
            _ => String::new(),
        };
        all.into_iter()
            .filter(|(name, id)| {
                q.is_empty() || name.to_lowercase().contains(&q) || id.contains(&q)
            })
            .map(|(n, i)| (n.to_string(), i.to_string()))
            .collect()
    }

    pub fn update(&mut self, action: Action) -> Vec<Command> {
        let mut cmds = Vec::new();
        // Always allow tick
        match action {
            Action::Tick => {
                self.theme.tick_accent();
                self.toasts.retain(|t| !t.expired());
                if let Some(t) = self.current_track() {
                    self.title_marquee.set_text(t.title.clone());
                }
                self.title_marquee.tick();
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
                        self.onboarding_status =
                            "Opening browser… log in, then return here.".into();
                        #[cfg(target_os = "macos")]
                        {
                            self.onboarding_status.push_str(
                                "\n  (macOS may ask to unlock Chrome Safe Storage — click Always Allow)",
                            );
                        }
                        self.overlay = Overlay::Onboarding;
                        cmds.push(Command::BeginBrowserLogin);
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
                    Action::CloseOverlay | Action::Escape => self.overlay = Overlay::None,
                    Action::PaletteInput(c) => {
                        query.push(c);
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
                    Action::BeginLogin | Action::Select => {
                        self.overlay = Overlay::None;
                        cmds.push(Command::BeginBrowserLogin);
                    }
                    Action::Logout | Action::LikeCurrent => {
                        // L while in account menu = logout if signed in
                        if self.signed_in {
                            cmds.push(Command::Logout);
                            self.signed_in = false;
                            self.account_name = None;
                            self.overlay = Overlay::None;
                            self.push_toast(Toast::info("Signed out"));
                        }
                    }
                    Action::BeginManualCookie => {
                        self.overlay = Overlay::ManualCookie {
                            input: String::new(),
                        };
                    }
                    _ => {}
                }
                return cmds;
            }
            Overlay::None => {}
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
            Action::CommandPalette => {
                self.overlay = Overlay::CommandPalette {
                    query: String::new(),
                    selected: 0,
                };
            }
            Action::Tab(i) => {
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
                self.move_sel(-1);
            }
            Action::MoveDown => {
                if self.search_editing && self.list_len() > 0 {
                    self.search_editing = false;
                }
                self.move_sel(1);
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
                // #region agent log
                crate::debug_log::log(
                    "A",
                    "app.rs:LibrarySection",
                    "library section selected",
                    serde_json::json!({
                        "section": i,
                        "signed_in": self.signed_in,
                        "had_detail": self.detail.is_some(),
                        "prev_tab": self.tab as usize,
                    }),
                );
                // #endregion
                self.tab = Tab::Library;
                self.fullscreen_np = false;
                self.detail = None;
                self.nav_stack.clear();
                self.library_section = i;
                self.load_library_section(&mut cmds);
            }
            Action::BeginLogin => cmds.push(Command::BeginBrowserLogin),
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
            Action::Resize(_, _) => {}
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
            "login" => cmds.push(Command::BeginBrowserLogin),
            "help" => self.overlay = Overlay::Help { scroll: 0 },
            "quit" => self.should_quit = true,
            _ => {}
        }
        cmds
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
        // #region agent log
        crate::debug_log::log(
            "E",
            "app.rs:load_library_section",
            "load_library_section entry",
            serde_json::json!({
                "signed_in": self.signed_in,
                "section": self.library_section,
            }),
        );
        // #endregion
        if !self.signed_in {
            return;
        }
        let section = self.library_section.min(4);
        // Reuse cached section data — re-fetching on every ←/→ made Library feel stuck.
        if self.library_fetched[section] {
            self.loading_library = false;
            self.list_state = TrackListState::default();
            return;
        }
        self.loading_library = true;
        self.list_state = TrackListState::default();
        match section {
            0 => cmds.push(Command::LoadLikedSongs),
            1 => cmds.push(Command::LoadLibraryPlaylists),
            2 => cmds.push(Command::LoadLibraryAlbums),
            3 => cmds.push(Command::LoadLibraryArtists),
            4 => cmds.push(Command::LoadHistory),
            _ => {}
        }
        // #region agent log
        crate::debug_log::log(
            "A",
            "app.rs:load_library_section",
            "pushed library load command",
            serde_json::json!({
                "section": self.library_section,
                "cmd_count": cmds.len(),
                "loading_library": self.loading_library,
            }),
        );
        // #endregion
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
                    self.home_item_idx = self.home_item_idx.saturating_sub(1);
                }
            }
            Tab::Library if browsing => {
                let n = crate::ui::views::library::SECTIONS.len();
                let next = if right {
                    (self.library_section + 1) % n
                } else {
                    (self.library_section + n - 1) % n
                };
                self.library_section = next;
                self.list_state = TrackListState::default();
                self.load_library_section(cmds);
            }
            _ => {
                let _ = self
                    .player_cmd_tx
                    .send(PlayerCommand::SeekRelative(if right { 5 } else { -5 }));
            }
        }
    }

    fn move_sel(&mut self, delta: i32) {
        if self.tab == Tab::Home && self.detail.is_none() {
            if delta < 0 {
                self.home_shelf_idx = self.home_shelf_idx.saturating_sub(1);
            } else if self.home_shelf_idx + 1 < self.home_shelves.len() {
                self.home_shelf_idx += 1;
            }
            self.home_item_idx = 0;
            return;
        }
        if self.tab == Tab::Library && self.detail.is_none() {
            // If at left edge conceptually, change section with h/l — for up/down move list
        }
        let len = self.list_len();
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
            AppEvent::DepsReady { ytdlp, js_runtime } => {
                self.deps_status = format!("yt-dlp {ytdlp} · {js_runtime}");
            }
            AppEvent::DepsStatus(s) => self.deps_status = s,
            AppEvent::DepsError(e) => self.push_toast(Toast::error(e)),
            AppEvent::AuthState(AuthStateEvent::SignedIn(session)) => {
                self.signed_in = true;
                self.account_name = session.account_name.clone();
                self.overlay = Overlay::None;
                self.library_fetched = [false; 5];
                self.push_toast(Toast::success(format!(
                    "Signed in as {}",
                    session.account_name.as_deref().unwrap_or("user")
                )));
                let _ = self.player_cmd_tx.send(PlayerCommand::RefreshCookies);
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
                self.library_fetched = [false; 5];
                self.liked_songs.clear();
                self.library_playlists.clear();
                self.library_albums.clear();
                self.library_artists.clear();
                self.history.clear();
            }
            AppEvent::AuthState(AuthStateEvent::SessionExpired(msg)) => {
                self.signed_in = false;
                self.loading_library = false;
                self.library_fetched = [false; 5];
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
                     Press L to refresh login (or P to paste a fresh cookie)."
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
                // #region agent log
                crate::debug_log::log(
                    "D",
                    "app.rs:LibraryPlaylists",
                    "received LibraryPlaylists event",
                    serde_json::json!({
                        "count": p.len(),
                        "tab": self.tab as usize,
                        "section": self.library_section,
                        "detail_open": self.detail.is_some(),
                        "titles": p.iter().take(5).map(|x| &x.title).collect::<Vec<_>>(),
                    }),
                );
                // #endregion
                self.loading_library = false;
                self.library_fetched[1] = true;
                self.library_playlists = p;
            }
            AppEvent::LikedSongs(t) => {
                self.loading_library = false;
                self.library_fetched[0] = true;
                self.liked_songs = t;
            }
            AppEvent::LibraryAlbums(a) => {
                self.loading_library = false;
                self.library_fetched[2] = true;
                self.library_albums = a;
            }
            AppEvent::LibraryArtists(a) => {
                self.loading_library = false;
                self.library_fetched[3] = true;
                self.library_artists = a;
            }
            AppEvent::History(t) => {
                self.loading_library = false;
                self.library_fetched[4] = true;
                self.history = t;
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
                let is_current =
                    self.current.as_ref().map(|t| t.video_id.as_str()) == Some(video_id.as_str());
                if is_current {
                    if let Some((r, g, b)) = accent {
                        self.theme.set_accent_rgb(r, g, b);
                    }
                }
                // Always cache — a late reply after a quick skip still warms the
                // next play, and discarding here left the UI stuck on placeholders.
                self.covers
                    .insert(video_id, crate::art::Cover { pixels, image });
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
            }
            AppEvent::PlaylistCreated(_) => self.push_toast(Toast::success("Playlist created")),
            AppEvent::AddedToPlaylist => self.push_toast(Toast::success("Added to playlist")),
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
                    if let Some(t) = self.queue.get(cursor) {
                        self.current = Some(t.clone());
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
                // Only auto-skip on hard failures. Transient open/decode races are
                // retried inside the engine; if we still get here, advance the queue.
                self.push_toast(Toast::error(e));
                self.playback_state = PlaybackState::Stopped;
                let _ = self.player_cmd_tx.send(PlayerCommand::Next);
            }
            PlayerEvent::DownloadProgress { .. } => {}
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

pub async fn run_ui(
    mut app: App,
    mut event_rx: mpsc::UnboundedReceiver<AppEvent>,
    cmd_tx: mpsc::UnboundedSender<Command>,
    mut media: MediaControlsBridge,
    sample_provider: Arc<dyn Fn(&mut Vec<f32>) + Send + Sync>,
) -> Result<()> {
    let mut terminal = ratatui::init();
    crossterm::execute!(
        std::io::stdout(),
        crossterm::event::EnableMouseCapture,
        EnableBracketedPaste
    )?;
    app.cover_ui = crate::ui::cover::CoverRenderer::detect(&app.config.ui.art_protocol);
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
    let _ = cmd_tx.send(Command::EnsureDeps);

    // Draw once before first event
    terminal.draw(|f| crate::ui::draw(f, &app))?;

    let result = loop {
        let playing = app.playback_state == PlaybackState::Playing;

        tokio::select! {
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
            }
            maybe_ev = events.next() => {
                match maybe_ev {
                    Some(Ok(ev)) => {
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
                        }
                    }
                    Some(Err(e)) => {
                        tracing::error!("event error: {e}");
                    }
                    None => break Ok(()),
                }
            }
            ev = event_rx.recv() => {
                match ev {
                    Some(ev) => {
                        let cmds = app.update(Action::ApplyEvent(ev));
                        for c in cmds { let _ = cmd_tx.send(c); }
                    }
                    None => break Ok(()),
                }
            }
        }

        // Feed the visualizer every frame path, not only on the playing tick.
        // Crossterm / API events can starve `tick` in `select!`, which left both
        // the mini bar and fullscreen NP stuck flat.
        let playing_now = app.playback_state == PlaybackState::Playing;
        if last_viz.elapsed() >= Duration::from_millis(33) {
            last_viz = std::time::Instant::now();
            app.visualizer.set_band_count(48);
            if playing_now {
                sample_provider(&mut app.sample_scratch);
                app.visualizer.update(&app.sample_scratch, true);
            } else {
                app.visualizer.update(&[], false);
            }
        }

        terminal.draw(|f| crate::ui::draw(f, &app))?;

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
