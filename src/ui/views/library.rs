use crate::api::model::{Album, Artist, Playlist, Track};
use crate::ui::components::{empty_state, pills, row, section_header};
use crate::ui::glyphs::{glyph, Glyph};
use crate::ui::theme::Theme;
use crate::ui::widgets::track_list::{
    render_item_list, render_track_list, TrackListOpts, TrackListState,
};
use crate::art::Cover;
use crate::api::model::ShelfItem;
use crate::ui::cover::{CoverRenderer, CoverSlot};
use crate::ui::HitMap;
use ratatui::prelude::*;
use std::collections::HashMap;

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
    covers: &HashMap<String, Cover>,
    renderer: &CoverRenderer,
    graphics: bool,
    blocked: &[Rect],
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    hit: &mut HitMap,
) {
    if !data.signed_in {
        empty_state(
            buf,
            area,
            glyph(theme, Glyph::Library),
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
        1 => render_playlist_grid(
            data.playlists,
            list.selected,
            covers,
            renderer,
            graphics,
            blocked,
            body,
            buf,
            theme,
            hit,
        ),
        2 => {
            let items: Vec<_> = data
                .albums
                .iter()
                .map(|a| (a.title.clone(), artist_line(a)))
                .collect();
            render_item_list(
                &items,
                glyph(theme, Glyph::Album),
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
                glyph(theme, Glyph::Artist),
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

/// Playlist covers laid out left→right, top→bottom; the row holding the
/// selection is kept in view.
#[allow(clippy::too_many_arguments)]
fn render_playlist_grid(
    playlists: &[Playlist],
    selected: usize,
    covers: &HashMap<String, Cover>,
    renderer: &CoverRenderer,
    graphics: bool,
    blocked: &[Rect],
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    hit: &mut HitMap,
) {
    use crate::ui::views::home::{render_card, CARD_GAP};
    // Same sizing as the Home cards (the header sits above this body).
    let full_h = area.height + 2;
    let art_h: u16 = if full_h >= 34 {
        8
    } else if full_h >= 24 {
        6
    } else if full_h >= 16 {
        4
    } else {
        3
    };
    let card_w = renderer.width_for_rows(art_h).clamp(14, 22);
    let card_h = art_h + 2;
    let row_h = card_h + 1;
    let cols = ((area.width + CARD_GAP) / (card_w + CARD_GAP)).max(1) as usize;
    let rows_fit = ((area.height + 1) / row_h).max(1) as usize;
    hit.grid_cols = cols;

    let sel_row = selected / cols;
    let first_row = sel_row.saturating_sub(rows_fit - 1);
    for (ri, chunk) in playlists.chunks(cols).enumerate().skip(first_row).take(rows_fit) {
        let y = area.y + (ri - first_row) as u16 * row_h;
        let h = card_h.min((area.y + area.height).saturating_sub(y));
        if h < 2 {
            break;
        }
        for (ci, p) in chunk.iter().enumerate() {
            let idx = ri * cols + ci;
            let x = area.x + 1 + ci as u16 * (card_w + CARD_GAP);
            if x + card_w > area.x + area.width {
                break;
            }
            let card = Rect::new(x, y, card_w, h);
            let item = ShelfItem::Playlist(Playlist {
                tracks: Vec::new(),
                ..p.clone()
            });
            render_card(
                &item,
                covers,
                renderer,
                graphics,
                blocked,
                CoverSlot::Card,
                card,
                art_h,
                idx == selected,
                buf,
                theme,
            );
            hit.list_rows.push((card, idx));
        }
    }
}
