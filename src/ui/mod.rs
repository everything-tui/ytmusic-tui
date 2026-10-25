pub mod components;
pub mod cover;
pub mod glyphs;
pub mod icons;
pub mod keymap;
pub mod marquee;
pub mod theme;
pub mod views;
pub mod widgets;

use crate::action::Action;
use crate::app::{App, Overlay, Tab};
use crate::ui::components::{fill, modal, pad, pill, row, truncate};
use crate::ui::cover::CoverCtx;
use crate::ui::theme::Theme;
use crate::ui::views::detail::DetailKind;
use crate::ui::widgets::equalizer::EqualizerOverlay;
use crate::ui::widgets::help::HelpOverlay;
use crate::ui::widgets::now_playing::{preferred_height, NowPlayingBar};
use crate::ui::widgets::sidebar::{self, Sidebar};
use crate::ui::widgets::toast::{render_toasts, toast_rects};
use ratatui::prelude::*;
use std::time::Duration;

/// Clickable regions recorded during the last draw, used for mouse hit-testing.
#[derive(Debug, Default, Clone)]
pub struct HitMap {
    pub tabs: Vec<(Rect, usize)>,
    pub buttons: Vec<(Rect, Action)>,
    pub progress: Option<Rect>,
    pub duration: Duration,
    pub volume: Option<Rect>,
    pub player: Option<Rect>,
    /// Visible list rows → absolute index in the current list.
    pub list_rows: Vec<(Rect, usize)>,
    /// Home shelf cards → (shelf_idx, item_idx).
    pub home_cards: Vec<(Rect, usize, usize)>,
    /// Columns in the Library playlist cover grid (0 when not shown).
    pub grid_cols: usize,
    /// Regions where a click that hits nothing else should do nothing.
    pub inert: Vec<Rect>,
}

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let theme = &app.theme;
    let mut hit = HitMap::default();
    let buf = frame.buffer_mut();
    fill(buf, area, Style::default().bg(theme.bg).fg(theme.text));

    if matches!(app.overlay, Overlay::Onboarding) {
        views::onboarding::render(area, buf, theme, &app.onboarding_status);
        render_toasts(&app.toasts, area, buf, theme);
        *app.hit.borrow_mut() = hit;
        return;
    }

    let player_h = if app.fullscreen_np {
        0
    } else {
        preferred_height(area.height)
    };
    let [body, player] =
        Layout::vertical([Constraint::Min(5), Constraint::Length(player_h)]).areas(area);
    let side_w = if app.fullscreen_np {
        0
    } else {
        sidebar::width_for(area.width)
    };
    let [side, main] =
        Layout::horizontal([Constraint::Length(side_w), Constraint::Min(10)]).areas(body);

    let toast_area = if app.fullscreen_np { body } else { main };
    let blocked = toast_rects(&app.toasts, toast_area);
    let graphics = matches!(app.overlay, Overlay::None);
    let current_key = app.current_track().map(|t| t.video_id.as_str());
    let current_cover = CoverCtx {
        renderer: &app.cover_ui,
        key: current_key,
        cover: app.current_cover(),
        graphics,
        blocked: &blocked,
    };
    let liked = app.current_track().and_then(|t| t.liked).unwrap_or(false);

    if app.fullscreen_np {
        views::now_playing::FullscreenNowPlaying {
            track: app.current_track(),
            state: app.playback_state,
            position: app.position,
            duration: app.duration,
            volume: app.volume,
            muted: app.muted,
            shuffle: app.shuffle,
            repeat: app.repeat,
            liked,
            lyrics: app.lyrics.as_ref(),
            cover: current_cover,
            visualizer: &app.visualizer,
            theme,
        }
        .render(body, buf, &mut hit);
    } else {
        if side_w > 0 {
            let in_tab = app.detail.is_none();
            Sidebar {
                active_tab: in_tab.then_some(app.tab as usize),
                focused: app.sidebar_focus,
                library_section: (app.tab == Tab::Library && app.signed_in)
                    .then_some(app.library_section),
                account: app.account_name.as_deref(),
                signed_in: app.signed_in,
                premium: app.premium.known(),
                mcp_enabled: app.control.is_some(),
                track: app.current_track(),
                cover: current_cover,
                visualizer: &app.visualizer,
                theme,
            }
            .render(side, buf, &mut hit);
        }
        let detail_key = app.detail_cover_key();
        let detail_cover = CoverCtx {
            renderer: &app.cover_ui,
            key: detail_key.as_deref(),
            cover: app.detail_cover(),
            graphics,
            blocked: &blocked,
        };
        let full_sidebar = side_w > sidebar::RAIL_W;
        draw_main(
            pad(main, 3, 1),
            buf,
            app,
            theme,
            detail_cover,
            graphics,
            &blocked,
            full_sidebar,
            &mut hit,
        );
        NowPlayingBar {
            track: app.current_track(),
            state: app.playback_state,
            position: app.position,
            duration: app.duration,
            stream: app.current_stream_info(),
            volume: app.volume,
            muted: app.muted,
            shuffle: app.shuffle,
            repeat: app.repeat,
            liked,
            cover: current_cover,
            marquee: &app.title_marquee,
            theme,
        }
        .render(player, buf, &mut hit);
    }

    match &app.overlay {
        Overlay::Help { scroll } => {
            HelpOverlay {
                theme,
                scroll: *scroll,
            }
            .render(area, buf);
        }
        Overlay::ConfirmQuit => draw_confirm_quit(area, buf, theme),
        Overlay::ContextMenu { items, selected } => {
            draw_context_menu(area, buf, theme, items, *selected);
        }
        Overlay::CommandPalette { query, selected } => {
            draw_command_palette(area, buf, theme, query, *selected, &app.palette_actions());
        }
        Overlay::ManualCookie { input } => draw_manual_cookie(area, buf, theme, input),
        Overlay::AccountMenu => draw_account_menu(area, buf, theme, app),
        Overlay::Equalizer { selected } => {
            EqualizerOverlay {
                theme,
                params: app.eq,
                selected: *selected,
                adaptive: app.eq_adaptive,
            }
            .render(area, buf);
        }
        Overlay::None | Overlay::Onboarding => {}
    }

    render_toasts(&app.toasts, toast_area, buf, theme);
    *app.hit.borrow_mut() = hit;
}

fn draw_main(
    area: Rect,
    buf: &mut Buffer,
    app: &App,
    theme: &Theme,
    detail_cover: CoverCtx<'_>,
    graphics: bool,
    blocked: &[Rect],
    full_sidebar: bool,
    hit: &mut HitMap,
) {
    let playing_id = app.current_track().map(|t| t.video_id.as_str());
    if let Some(ref detail) = app.detail {
        let kind = match detail {
            crate::app::DetailView::Album(a) => DetailKind::Album(a),
            crate::app::DetailView::Artist(a) => DetailKind::Artist(a),
            crate::app::DetailView::Playlist(p) => DetailKind::Playlist(p),
        };
        views::detail::render(
            kind,
            &app.list_state,
            playing_id,
            detail_cover,
            area,
            buf,
            theme,
            hit,
        );
        return;
    }

    match app.tab {
        Tab::Home => {
            views::home::render(
                &app.home_shelves,
                app.home_shelf_idx,
                app.home_item_idx,
                app.loading_home,
                &app.covers,
                &app.cover_ui,
                graphics,
                blocked,
                area,
                buf,
                theme,
                hit,
            );
        }
        Tab::Search => {
            let editing = app.input_context() == keymap::InputContext::Search;
            views::search::render(
                &views::search::SearchData {
                    query: &app.search_query,
                    filter: app.search_filter,
                    suggestions: &app.search_suggestions,
                    tracks: &app.search_tracks,
                    albums: &app.search_albums,
                    artists: &app.search_artists,
                    playlists: &app.search_playlists,
                    loading: app.loading_search,
                    editing,
                },
                &app.list_state,
                app.suggestion_idx,
                playing_id,
                area,
                buf,
                theme,
                hit,
            );
        }
        Tab::Library => {
            views::library::render(
                &views::library::LibraryData {
                    signed_in: app.signed_in,
                    section: app.library_section,
                    liked: &app.liked_songs,
                    playlists: &app.library_playlists,
                    albums: &app.library_albums,
                    artists: &app.library_artists,
                    history: &app.history,
                    loading: app.loading_library,
                },
                &app.list_state,
                playing_id,
                !full_sidebar,
                &app.covers,
                &app.cover_ui,
                graphics,
                blocked,
                area,
                buf,
                theme,
                hit,
            );
        }
        Tab::Queue => {
            let tracks = app.queue_tracks();
            views::queue::render(
                &tracks,
                app.queue_cursor,
                app.list_state.selected,
                area,
                buf,
                theme,
                hit,
            );
        }
        Tab::Lyrics => {
            views::lyrics::render(app.lyrics.as_ref(), app.position, area, buf, theme);
        }
    }
}

fn draw_confirm_quit(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let inner = modal(buf, area, 44, 8, "Quit yt-tui?", "", theme);
    let bg = Style::default().bg(theme.bg_modal);
    buf.set_stringn(inner.x, inner.y, "Playback will stop.", inner.width as usize, theme.dim().patch(bg));
    let y = inner.y + 2;
    let r = pill(buf, inner.x, y, "y  Quit", true, theme);
    pill(buf, r.x + r.width + 2, y, "n  Cancel", false, theme);
}

fn draw_context_menu(area: Rect, buf: &mut Buffer, theme: &Theme, items: &[String], selected: usize) {
    let height = (items.len() as u16 + 5).min(area.height.saturating_sub(4));
    let width = 40.min(area.width.saturating_sub(4));
    let inner = modal(buf, area, width, height, "Actions", "esc", theme);
    let bg = Style::default().bg(theme.bg_modal);
    for (i, item) in items.iter().enumerate() {
        if i as u16 >= inner.height {
            break;
        }
        let r = Rect {
            x: inner.x.saturating_sub(1),
            width: inner.width + 2,
            ..row(inner, i as u16)
        };
        let style = if i == selected {
            fill(buf, r, theme.row_selected());
            theme.heading().patch(theme.row_selected())
        } else {
            theme.normal().patch(bg)
        };
        buf.set_stringn(inner.x, r.y, item, inner.width as usize, style);
    }
}

fn draw_command_palette(
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    query: &str,
    selected: usize,
    actions: &[(String, String)],
) {
    let height = 18.min(area.height.saturating_sub(4));
    let width = 64.min(area.width.saturating_sub(4));
    let inner = modal(buf, area, width, height, "Command palette", "↑↓ · enter · esc", theme);
    if inner.height < 3 {
        return;
    }
    let input = Rect {
        x: inner.x.saturating_sub(1),
        width: inner.width + 2,
        ..row(inner, 0)
    };
    let field = Style::default().bg(theme.bg_selected);
    fill(buf, input, field);
    let q = truncate(query, inner.width.saturating_sub(6) as usize);
    buf.set_line(
        inner.x,
        input.y,
        &Line::from(vec![
            Span::styled("›  ", theme.accent_bold()),
            Span::styled(q, theme.normal()),
            Span::styled("█", theme.accent()),
        ])
        .patch_style(field),
        inner.width,
    );

    let bg = Style::default().bg(theme.bg_modal);
    let list = Rect {
        y: inner.y + 2,
        height: inner.height - 2,
        ..inner
    };
    if actions.is_empty() {
        buf.set_string(list.x, list.y, "No matching commands", theme.muted().patch(bg));
        return;
    }
    let visible = list.height as usize;
    let offset = components::scroll_offset(selected, visible, actions.len());
    for (i, (name, _)) in actions.iter().enumerate().skip(offset).take(visible) {
        let r = Rect {
            x: list.x.saturating_sub(1),
            width: list.width + 2,
            ..row(list, (i - offset) as u16)
        };
        let style = if i == selected {
            fill(buf, r, theme.row_selected());
            theme.heading().patch(theme.row_selected())
        } else {
            theme.normal().patch(bg)
        };
        buf.set_stringn(list.x, r.y, name, list.width as usize, style);
    }
}

fn draw_manual_cookie(area: Rect, buf: &mut Buffer, theme: &Theme, input: &str) {
    let inner = modal(
        buf,
        area,
        76.min(area.width.saturating_sub(2)),
        17,
        "Paste a cookie",
        "enter submit · esc cancel · ^U clear",
        theme,
    );
    let bg = Style::default().bg(theme.bg_modal);

    // Never render a multi‑KB cURL into the terminal — that freezes the UI.
    let preview = cookie_input_preview(input);
    let nbytes = input.len();
    let (status, status_style) = if nbytes == 0 {
        ("Waiting for paste…".to_string(), theme.muted())
    } else if input.contains("curl ") || input.contains("Cookie:") || input.contains("cookie:") {
        (
            format!("✓ {nbytes} bytes captured — looks like a cookie/cURL paste"),
            Style::default().fg(theme.success),
        )
    } else {
        (format!("{nbytes} bytes captured"), theme.dim())
    };

    let steps = [
        "Paste a Cookie header or \"Copy as cURL\" from DevTools.",
        "",
        "1. Open a PRIVATE window, go to music.youtube.com and sign in",
        "2. DevTools → Network → any request → Copy as cURL",
        "3. Paste here (Cmd+V), press Enter, then close the private window",
        "yt-tui then keeps this session alive itself — you only do this once.",
    ];
    for (i, s) in steps.iter().enumerate() {
        let style = if i == 0 { theme.normal() } else { theme.dim() };
        buf.set_stringn(inner.x, inner.y + i as u16, s, inner.width as usize, style.patch(bg));
    }
    let y = inner.y + steps.len() as u16 + 1;
    buf.set_stringn(inner.x, y, &status, inner.width as usize, status_style.patch(bg));
    let field = Rect {
        x: inner.x.saturating_sub(1),
        width: inner.width + 2,
        ..row(inner, y - inner.y + 2)
    };
    if field.height > 0 {
        fill(buf, field, Style::default().bg(theme.bg_selected));
        buf.set_stringn(
            inner.x,
            field.y,
            &preview,
            inner.width as usize,
            theme.dim().bg(theme.bg_selected),
        );
    }
}

fn cookie_input_preview(input: &str) -> String {
    if input.is_empty() {
        return "█".into();
    }
    // Prefer showing the cookie fragment if we can find it
    let snippet = if let Some(idx) = input.to_ascii_lowercase().find("cookie:") {
        let rest = &input[idx..];
        rest.chars().take(80).collect::<String>()
    } else {
        input.chars().take(80).collect::<String>()
    };
    let truncated = if input.chars().count() > 80 {
        format!("{snippet}…")
    } else {
        snippet
    };
    // Single-line preview
    truncated.replace('\n', " ").replace('\r', "")
}

fn draw_account_menu(area: Rect, buf: &mut Buffer, theme: &Theme, app: &App) {
    let inner = modal(buf, area, 46, 10, "Account", "esc", theme);
    let bg = Style::default().bg(theme.bg_modal);
    let mut opts: Vec<(&str, &str)> = Vec::new();
    let mut opts_y = inner.y + 2;
    if app.signed_in {
        let name = app.account_name.as_deref().unwrap_or("user");
        buf.set_line(
            inner.x,
            inner.y,
            &Line::from(vec![
                Span::styled("● ", Style::default().fg(theme.success)),
                Span::styled("Signed in as ", theme.dim()),
                Span::styled(name.to_string(), theme.heading()),
            ])
            .patch_style(bg),
            inner.width,
        );
        let (status, style) = match app.premium.known() {
            Some(true) => ("Music Premium · 256 kbps audio", theme.accent()),
            Some(false) => ("Free account · up to 128 kbps audio", theme.dim()),
            None => ("Music Premium: checking…", theme.muted()),
        };
        buf.set_string(inner.x + 2, inner.y + 1, status, style.patch(bg));
        opts_y += 1;
        opts.push(("L", "Log out"));
    } else {
        buf.set_string(inner.x, inner.y, "○ Not signed in", theme.dim().patch(bg));
        opts.push(("Enter", "Sign in (paste cURL once)"));
        opts.push(("P", "Paste cookie"));
    }
    for (i, (key, label)) in opts.iter().enumerate() {
        let y = opts_y + i as u16 * 2;
        if y >= inner.y + inner.height {
            break;
        }
        pill(buf, inner.x, y, &format!("{key:^5}"), i == 0, theme);
        buf.set_stringn(inner.x + 9, y, label, inner.width.saturating_sub(9) as usize, theme.normal().patch(bg));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::model::{AlbumRef, ArtistRef, Shelf, ShelfItem, Track};
    use crate::app::DetailView;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::sync::Arc;

    fn track(i: usize) -> Track {
        Track {
            video_id: format!("v{i}"),
            title: format!("Song number {i}"),
            artists: vec![ArtistRef { id: None, name: format!("Artist {i}") }],
            album: Some(AlbumRef { id: None, name: format!("Album {i}") }),
            duration: Some(Duration::from_secs(180 + i as u64)),
            thumbnails: vec![],
            explicit: i.is_multiple_of(3),
            liked: Some(i.is_multiple_of(2)),
            from_radio: false,
        }
    }

    fn test_app() -> App {
        let root = std::env::temp_dir().join(format!("yt-tui-ui-test-{}", std::process::id()));
        let paths = Arc::new(crate::paths::Paths {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
        });
        let (cmd_tx, _) = tokio::sync::mpsc::unbounded_channel();
        let (player_tx, _) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            paths,
            crate::config::Config::default(),
            cmd_tx,
            player_tx,
            true,
            Some("Tester".into()),
        );
        app.current = Some(track(1));
        app.queue = (0..30).map(track).collect();
        app.queue_cursor = 1;
        app.home_shelves = vec![Shelf {
            title: "Quick picks".into(),
            items: (0..8).map(|i| ShelfItem::Track(track(i))).collect(),
        }];
        app.liked_songs = (0..40).map(track).collect();
        app
    }

    fn render(app: &App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..h {
            let mut skip = 0;
            for x in 0..w {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let s = buf[(x, y)].symbol();
                skip = UnicodeWidthStr::width(s).saturating_sub(1);
                out.push_str(s);
            }
            out.push('\n');
        }
        out
    }

    fn assert_borderless(out: &str, ctx: &str) {
        for g in ['╭', '╮', '╰', '╯', '┌', '┐', '└', '┘'] {
            assert!(!out.contains(g), "border glyph {g} in {ctx}:\n{out}");
        }
    }

    use unicode_width::UnicodeWidthStr;

    #[test]
    fn shows_premium_status() {
        use crate::player::resolver::Premium;
        let mut app = test_app();
        // Unknown yet: no tier claimed in the sidebar.
        let out = render(&app, 120, 36);
        assert!(out.contains("Tester") && !out.contains("Premium") && !out.contains("Free"), "{out}");

        for (premium, sidebar, menu) in [
            (Premium::Yes, "Premium", "Music Premium · 256 kbps audio"),
            (Premium::No, "Free", "Free account · up to 128 kbps audio"),
        ] {
            app.premium = premium;
            app.overlay = Overlay::None;
            let out = render(&app, 120, 36);
            assert!(out.contains(sidebar), "{out}");
            app.overlay = Overlay::AccountMenu;
            let out = render(&app, 120, 36);
            assert!(out.contains(menu), "{out}");
        }
    }

    #[test]
    fn frame_layouts_are_borderless_and_responsive() {
        let mut app = test_app();
        for tab in [Tab::Home, Tab::Search, Tab::Library, Tab::Queue, Tab::Lyrics] {
            app.tab = tab;
            for (w, h) in [(140u16, 40u16), (90, 30), (60, 20)] {
                let out = render(&app, w, h);
                let ctx = format!("{tab:?} at {w}x{h}");
                println!("== {ctx}\n{out}");
                assert_borderless(&out, &ctx);
                let hit = app.hit.borrow();
                assert_eq!(hit.tabs.len(), 5, "sidebar nav missing in {ctx}");
                assert_eq!(hit.buttons.iter().filter(|(_, a)| matches!(a, Action::PlayPause)).count(), 1);
                // Full sidebar (with labels) only from 80 columns.
                assert_eq!(out.contains("yt-tui"), w >= 80, "{ctx}");
            }
        }
        // Narrower than the rail threshold hides the sidebar entirely.
        app.tab = Tab::Home;
        let _ = render(&app, 46, 20);
        assert!(app.hit.borrow().tabs.is_empty());
    }

    #[test]
    fn track_list_shows_duration_column() {
        let mut app = test_app();
        app.tab = Tab::Library;
        app.library_section = 0;
        let out = render(&app, 140, 40);
        println!("library liked:\n{out}");
        assert!(out.contains("Time"), "Time header missing:\n{out}");
        assert!(
            out.contains("3:00") || out.contains("3:01") || out.contains("3:02"),
            "duration values missing:\n{out}"
        );
    }

    #[test]
    fn library_sections_nest_under_sidebar() {
        let mut app = test_app();
        app.tab = Tab::Library;
        let out = render(&app, 140, 40);
        assert!(out.contains("Liked songs"));
        let hit = app.hit.borrow();
        let sections = hit
            .buttons
            .iter()
            .filter(|(_, a)| matches!(a, Action::LibrarySection(_)))
            .count();
        assert_eq!(sections, views::library::SECTIONS.len());
    }

    #[test]
    fn library_section_action_dismisses_detail() {
        let mut app = test_app();
        app.tab = Tab::Library;
        app.detail = Some(DetailView::Playlist(crate::api::model::Playlist {
            id: "PL1".into(),
            title: "Open playlist".into(),
            description: None,
            author: None,
            track_count: Some(1),
            thumbnails: vec![],
            tracks: vec![track(0)],
        }));
        app.nav_stack.push(DetailView::Album(crate::api::model::Album {
            id: "alb".into(),
            title: "Previous".into(),
            artists: vec![],
            year: None,
            thumbnails: vec![],
            tracks: vec![],
        }));
        let cmds = app.update(Action::LibrarySection(1));
        assert!(app.detail.is_none(), "detail should close so the list is visible");
        assert!(app.nav_stack.is_empty());
        assert_eq!(app.tab, Tab::Library);
        assert_eq!(app.library_section, 1);
        assert!(app.loading_library);
        assert!(cmds.iter().any(|c| matches!(c, crate::action::Command::LoadLibraryPlaylists)));
    }

    #[test]
    fn page_keys_jump_ten_songs_in_a_playlist() {
        let mut app = test_app();
        app.tab = Tab::Library;
        app.detail = Some(DetailView::Playlist(crate::api::model::Playlist {
            id: "PL1".into(),
            title: "Playlist".into(),
            description: None,
            author: None,
            track_count: Some(25),
            thumbnails: vec![],
            tracks: (0..25).map(track).collect(),
        }));
        app.update(Action::PageDown);
        assert_eq!(app.list_state.selected, 10);
        app.update(Action::PageDown);
        assert_eq!(app.list_state.selected, 20);
        app.update(Action::PageDown);
        assert_eq!(app.list_state.selected, 24, "clamped to the last song");
        app.update(Action::PageUp);
        assert_eq!(app.list_state.selected, 14);
        let out = render(&app, 120, 36);
        assert!(out.contains("Song number 14"), "selection stays on screen:\n{out}");
    }

    #[test]
    fn detail_and_overlays_render() {
        let mut app = test_app();
        app.detail = Some(DetailView::Album(crate::api::model::Album {
            id: "alb".into(),
            title: "Great Album".into(),
            artists: vec![ArtistRef { id: None, name: "Band".into() }],
            year: Some("2020".into()),
            thumbnails: vec![],
            tracks: (0..12).map(track).collect(),
        }));
        let out = render(&app, 120, 36);
        assert!(out.contains("ALBUM") && out.contains("Great Album"), "{out}");
        assert!(app
            .hit
            .borrow()
            .buttons
            .iter()
            .any(|(_, a)| matches!(a, Action::PlayAll)));

        for overlay in [
            Overlay::Help { scroll: 0 },
            Overlay::ConfirmQuit,
            Overlay::AccountMenu,
            Overlay::CommandPalette { query: "pl".into(), selected: 0 },
            Overlay::ManualCookie { input: "Cookie: a=b".into() },
            Overlay::Equalizer { selected: 0 },
        ] {
            app.overlay = overlay;
            let out = render(&app, 120, 36);
            assert_borderless(&out, "overlay");
        }
        app.overlay = Overlay::Onboarding;
        let out = render(&app, 100, 30);
        assert!(out.contains("Sign in (paste cURL once)"));
    }
}
