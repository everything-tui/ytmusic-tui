use crate::api::model::{Album, Artist, Playlist, Track};
use crate::ui::components::{empty_state, pills, row, section_header};
use crate::ui::theme::Theme;
use crate::ui::widgets::track_list::{
    render_item_list, render_track_list, TrackListOpts, TrackListState,
};
use crate::ui::HitMap;
use ratatui::prelude::*;

pub const SECTIONS: &[&str] = &[
    "Liked songs",
    "Playlists",
    "Albums",
    "Artists",
    "History",
];

pub struct LibraryData<'a> {
    pub signed_in: bool,
    pub section: usize,
    pub liked: &'a [Track],
    pub playlists: &'a [Playlist],
    pub albums: &'a [Album],
    pub artists: &'a [Artist],
    pub history: &'a [Track],
    pub loading: bool,
}

/// `section_pills` draws the section switcher inline when the sidebar can't show it.
pub fn render(
    data: &LibraryData<'_>,
    list: &TrackListState,
    playing_id: Option<&str>,
    section_pills: bool,
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    hit: &mut HitMap,
) {
    if !data.signed_in {
        empty_state(
            buf,
            area,
            "♫",
            "Sign in to see your library",
            "Press L to sign in — liked songs, playlists, albums and history appear here",
            theme,
        );
        return;
    }

    let section = data.section.min(SECTIONS.len() - 1);
    let count = match section {
        0 => data.liked.len(),
        1 => data.playlists.len(),
        2 => data.albums.len(),
        3 => data.artists.len(),
        _ => data.history.len(),
    };
    let meta = if data.loading {
        String::new()
    } else if count == 1 {
        "1 item".into()
    } else {
        format!("{count} items")
    };
    section_header(buf, row(area, 0), SECTIONS[section], &meta, theme);

    let mut y = 2;
    if section_pills {
        let rects = pills(buf, row(area, y), SECTIONS, Some(section), theme);
        for (i, r) in rects.into_iter().enumerate() {
            hit.buttons.push((r, crate::action::Action::LibrarySection(i)));
        }
        y += 2;
    }
    let body = Rect {
        y: area.y + y,
        height: area.height.saturating_sub(y),
        ..area
    };

    if data.loading {
        empty_state(buf, body, "", "Loading…", "", theme);
        return;
    }
    if count == 0 {
        empty_state(
            buf,
            body,
            "",
            "Nothing here yet",
            "If this stays empty, press L to refresh your login",
            theme,
        );
        return;
    }

    let opts = TrackListOpts {
        playing_id,
        ..Default::default()
    };
    match section {
        0 => render_track_list(
            data.liked,
            list,
            opts,
            body,
            buf,
            theme,
            Some(&mut hit.list_rows),
        ),
        1 => {
            let items: Vec<_> = data
                .playlists
                .iter()
                .map(|p| {
                    let sub = match (p.author.as_deref(), p.track_count) {
                        (Some(a), Some(c)) => format!("{a} · {c} tracks"),
                        (Some(a), None) => a.to_string(),
                        (None, Some(c)) => format!("{c} tracks"),
                        _ => String::new(),
                    };
                    (p.title.clone(), sub)
                })
                .collect();
            render_item_list(
                &items,
                "≡",
                list.selected,
                body,
                buf,
                theme,
                Some(&mut hit.list_rows),
            );
        }
        2 => {
            let items: Vec<_> = data
                .albums
                .iter()
                .map(|a| (a.title.clone(), artist_line(a)))
                .collect();
            render_item_list(
                &items,
                "◉",
                list.selected,
                body,
                buf,
                theme,
                Some(&mut hit.list_rows),
            );
        }
        3 => {
            let items: Vec<_> = data
                .artists
                .iter()
                .map(|a| (a.name.clone(), "Artist".to_string()))
                .collect();
            render_item_list(
                &items,
                "☺",
                list.selected,
                body,
                buf,
                theme,
                Some(&mut hit.list_rows),
            );
        }
        _ => render_track_list(
            data.history,
            list,
            opts,
            body,
            buf,
            theme,
            Some(&mut hit.list_rows),
        ),
    }
}

pub fn artist_line(a: &Album) -> String {
    let artists = a
        .artists
        .iter()
        .map(|x| x.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    match &a.year {
        Some(y) if !artists.is_empty() => format!("{artists} · {y}"),
        Some(y) => y.clone(),
        None => artists,
    }
}
