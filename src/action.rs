use crate::api::model::{Album, Artist, Lyrics, Playlist, Shelf, Track};
use crate::auth::AuthSession;
use std::time::Duration;

/// User / system intent mapped from keys, mouse, media keys, or internal triggers.
#[derive(Debug, Clone)]
pub enum Action {
    Quit,
    /// Ctrl+C — first press arms quit, second press within a short window exits.
    CtrlC,
    ConfirmQuit,
    CancelQuit,
    Tick,
    Resize(u16, u16),

    // Navigation
    Tab(usize),
    NextTab,
    PrevTab,
    Back,
    ToggleNowPlaying,
    Help,
    CloseOverlay,
    CommandPalette,
    ToggleMcp,
    Escape,
    /// Open the equalizer overlay.
    OpenEqualizer,
    /// Toggle EQ enabled/bypass.
    EqToggle,
    /// Reset the selected band to 0 dB.
    EqResetBand,
    /// Apply preset index (0 = Flat …).
    EqPreset(usize),
    /// Reset all bands to Flat.
    EqFlat,
    /// Cycle Adaptive EQ taste profile (Off → Balanced → Bass → Vocal → Bright).
    EqAdaptiveCycle,

    // Playback
    PlayPause,
    Play,
    Pause,
    Next,
    Previous,
    SeekForward(Duration),
    SeekBack(Duration),
    SeekTo(Duration),
    VolumeUp,
    VolumeDown,
    SetVolume(f32),
    Mute,
    ToggleShuffle,
    CycleRepeat,
    LikeCurrent,
    Select,
    /// Mouse: activate list row `index` (select + play/open).
    SelectAt(usize),
    /// Mouse: activate a Home shelf card.
    SelectHomeItem { shelf: usize, item: usize },

    // Lists
    MoveUp,
    MoveDown,
    MoveLeft,
    MoveRight,
    PageUp,
    PageDown,
    GoTop,
    GoBottom,
    AddToQueue,
    PlayNext,
    ContextMenu,
    RemoveFromQueue,
    ClearUpcoming,
    MoveQueueUp,
    MoveQueueDown,
    StartRadio,
    ShufflePlay,
    PlayAll,

    // Search
    StartSearch,
    /// Focus the search input without clearing the query (e.g. click the field).
    FocusSearch,
    SearchInput(char),
    SearchBackspace,
    SearchSubmit,
    CycleSearchFilter,
    SetSearchFilter(SearchFilter),

    // Auth
    BeginLogin,
    ContinueAsGuest,
    BeginManualCookie,
    /// Append a chunk of text (bracketed paste or coalesced key flood).
    PasteText(String),
    Logout,
    AccountMenu,

    // Library
    LibrarySection(usize),
    AddToPlaylist,
    CreatePlaylist(String),

    // Mouse / overlays
    Click { col: u16, row: u16 },
    Scroll { delta: i16 },
    RightClick { col: u16, row: u16 },
    ToastDismiss,

    // Lyrics
    SeekToLyricLine(usize),

    // Command palette
    PaletteInput(char),
    PaletteBackspace,
    PaletteSelect,

    // Internal from services
    ApplyEvent(AppEvent),
}

/// Commands the UI asks background services to perform.
#[derive(Debug, Clone)]
pub enum Command {
    ResolveAndPlay(Track),
    Prefetch(Track),
    PlayerPlay,
    PlayerPause,
    PlayerToggle,
    PlayerNext,
    PlayerPrevious,
    PlayerSeek(Duration),
    PlayerSetVolume(f32),
    PlayerStop,
    Search { query: String, filter: SearchFilter },
    SearchSuggestions(String),
    LoadHome { gen: u64 },
    LoadAlbum(String),
    LoadArtist(String),
    LoadPlaylist(String),
    LoadLibraryPlaylists,
    LoadLikedSongs,
    LoadLibraryAlbums,
    LoadLibraryArtists,
    LoadHistory,
    LoadRadio(String),
    LoadLyrics { video_id: String, browse_id: Option<String>, track: Track },
    Rate { video_id: String, liked: bool },
    AddToPlaylist { playlist_id: String, video_ids: Vec<String> },
    CreatePlaylist { title: String },
    FetchArt { url: String, video_id: String },
    /// UI → main: validate & persist a pasted cookie / cURL.
    ValidateCookies {
        cookies: String,
        visitor_id: Option<String>,
    },
    /// Main → API worker: install cookies + visitor / user-session ids.
    ApplyAuth(crate::auth::AuthSession),
    /// Main → API worker: drop cookies / auth meta (guest).
    ClearAuth,
    Logout,
    UpdateMediaControls,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchFilter {
    #[default]
    Songs,
    Albums,
    Artists,
    Playlists,
}

impl SearchFilter {
    pub fn next(self) -> Self {
        match self {
            Self::Songs => Self::Albums,
            Self::Albums => Self::Artists,
            Self::Artists => Self::Playlists,
            Self::Playlists => Self::Songs,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Songs => "Songs",
            Self::Albums => "Albums",
            Self::Artists => "Artists",
            Self::Playlists => "Playlists",
        }
    }
}

/// Events emitted by background services back to the UI.
#[derive(Debug, Clone)]
pub enum AppEvent {
    AuthState(AuthStateEvent),
    Account(Option<crate::api::model::Account>),

    SearchResults {
        query: String,
        filter: SearchFilter,
        tracks: Vec<Track>,
        albums: Vec<Album>,
        artists: Vec<Artist>,
        playlists: Vec<Playlist>,
    },
    SearchSuggestions(Vec<String>),
    Home { shelves: Vec<Shelf>, gen: u64 },
    AlbumLoaded(Album),
    ArtistLoaded(Artist),
    PlaylistLoaded(Playlist),
    LibraryPlaylists(Vec<Playlist>),
    LikedSongs(Vec<Track>),
    LibraryAlbums(Vec<Album>),
    LibraryArtists(Vec<Artist>),
    History(Vec<Track>),
    RadioTracks { seed: String, tracks: Vec<Track> },
    LyricsLoaded { video_id: String, lyrics: Lyrics },
    ArtLoaded {
        video_id: String,
        path: std::path::PathBuf,
        accent: Option<(u8, u8, u8)>,
        pixels: Option<std::sync::Arc<crate::art::ArtPixels>>,
        image: Option<std::sync::Arc<image::DynamicImage>>,
    },
    RateDone { video_id: String, liked: bool },
    PlaylistCreated(Playlist),
    AddedToPlaylist,
    QueueSnapshot { tracks: Vec<Track>, cursor: usize },
    Error(String),
    Info(String),
    Player(PlayerEvent),
}

#[derive(Debug, Clone)]
pub enum AuthStateEvent {
    Guest,
    WaitingForBrowser { browsers: Vec<String> },
    /// Live status while polling browsers (shown on onboarding screen).
    Progress(String),
    SignedIn(AuthSession),
    Failed(String),
    LoggedOut,
    /// Cookies are present but YouTube reports logged_in=0 — library will stay empty.
    SessionExpired(String),
}

#[derive(Debug, Clone)]
pub enum PlayerEvent {
    Loading { track: Track },
    Playing { track: Track },
    Paused,
    Resumed,
    Stopped,
    Position(Duration),
    Duration(Duration),
    Volume(f32),
    Buffering(bool),
    Ended,
    Error(String),
    DownloadProgress { video_id: String, pct: f32 },
    /// Codec/size of the stream just opened for `video_id`.
    Stream { video_id: String, info: crate::player::source::StreamInfo },
    /// What the resolver learned about the account's Music Premium status.
    Premium(crate::player::resolver::Premium),
}
