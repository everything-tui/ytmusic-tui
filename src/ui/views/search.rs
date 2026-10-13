use crate::action::{Action, SearchFilter};
use crate::api::model::{Album, Artist, Playlist, Track};
use crate::ui::components::{empty_state, fill, pills, row, section_header, truncate};
use crate::ui::theme::Theme;
use crate::ui::views::library::artist_line;
use crate::ui::widgets::track_list::{
    render_item_list, render_track_list, TrackListOpts, TrackListState,
};
use crate::ui::HitMap;
use ratatui::prelude::*;
use unicode_width::UnicodeWidthStr;

pub struct SearchData<'a> {
    pub query: &'a str,
    pub filter: SearchFilter,
    pub suggestions: &'a [String],
    pub tracks: &'a [Track],
    pub albums: &'a [Album],
    pub artists: &'a [Artist],
    pub playlists: &'a [Playlist],
    pub loading: bool,
    /// The input currently has keyboard focus.
    pub editing: bool,
}

const FILTERS: [SearchFilter; 4] = [
    SearchFilter::Songs,
    SearchFilter::Albums,
    SearchFilter::Artists,
    SearchFilter::Playlists,
];

pub fn render(
    data: &SearchData<'_>,
    list: &TrackListState,
    cursor: usize,
    playing_id: Option<&str>,
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    hit: &mut HitMap,
) {
    if area.height < 6 {
        return;
    }

    // Input: a tinted 3-row field — click to focus, never starts playback.
    let field = Rect {
        height: 3,
        width: area.width.min(72),
        ..area
    };
    hit.buttons.push((field, Action::FocusSearch));
    fill(buf, field, Style::default().bg(theme.bg_selected));
    let bg = Style::default().bg(theme.bg_selected);
    let tx = field.x + 2;
    let ty = field.y + 1;
    let max = field.width.saturating_sub(6) as usize;
    buf.set_string(tx, ty, "⌕", theme.dim().patch(bg));
    if data.query.is_empty() {
        let px = if data.editing {
            buf.set_string(tx + 3, ty, "█", theme.accent().patch(bg));
            tx + 5
        } else {
            tx + 3
        };
        buf.set_stringn(px, ty, "What do you want to play?", max.saturating_sub(2), theme.muted().patch(bg));
    } else {
        // Keep the tail of long queries visible.
        let q: String = if data.query.width() + 1 > max {
            let chars: Vec<char> = data.query.chars().collect();
            let mut out = String::new();
            for ch in chars.iter().rev() {
                if out.width() + 2 > max {
                    break;
                }
                out.insert(0, *ch);
            }
            out
        } else {
            data.query.to_string()
        };
        buf.set_string(tx + 3, ty, &q, theme.normal().patch(bg));
        if data.editing {
            buf.set_string(tx + 3 + q.width() as u16, ty, "█", theme.accent().patch(bg));
        }
    }
    if field.width >= 50 {
        let hint = if data.editing {
            "tab filter · esc done"
        } else {
            "/ edit · enter play"
        };
        buf.set_string(
            field.x + field.width - hint.width() as u16 - 2,
            ty,
            hint,
            theme.muted().patch(bg),
        );
    }

    let labels: Vec<&str> = FILTERS.iter().map(|f| f.label()).collect();
    let active = FILTERS.iter().position(|f| *f == data.filter);
    let filter_rects = pills(buf, row(area, 4), &labels, active, theme);
    for (i, r) in filter_rects.into_iter().enumerate() {
        hit.buttons
            .push((r, Action::SetSearchFilter(FILTERS[i])));
    }

    let body = Rect {
        y: area.y + 6,
        height: area.height - 6,
        ..area
    };

    if data.loading {
        empty_state(buf, body, "", "Searching…", "", theme);
        return;
    }

    let count = match data.filter {
        SearchFilter::Songs => data.tracks.len(),
        SearchFilter::Albums => data.albums.len(),
        SearchFilter::Artists => data.artists.len(),
        SearchFilter::Playlists => data.playlists.len(),
    };

    if count == 0 {
        if data.filter == SearchFilter::Songs
            && !data.query.is_empty()
            && !data.suggestions.is_empty()
        {
            section_header(buf, row(body, 0), "Suggestions", "enter to search", theme);
            for (i, s) in data.suggestions.iter().enumerate() {
                let y = body.y + 2 + i as u16;
                if y >= body.y + body.height {
                    break;
                }
                let r = Rect { y, height: 1, ..body };
                hit.list_rows.push((r, i));
                if i == cursor {
                    fill(buf, r, theme.row_selected());
                }
                let style = if i == cursor { theme.normal() } else { theme.dim() };
                buf.set_stringn(
                    r.x + 1,
                    y,
                    format!("⌕  {}", truncate(s, r.width.saturating_sub(5) as usize)),
                    r.width as usize,
                    style.patch(if i == cursor { theme.row_selected() } else { Style::default() }),
                );
            }
        } else if data.query.is_empty() {
            empty_state(buf, body, "⌕", "Search YouTube Music", "Songs, albums, artists and playlists", theme);
        } else {
            empty_state(buf, body, "", "No results", "Try a different spelling or filter", theme);
        }
        return;
    }

    let meta = if count == 1 { "1 result".to_string() } else { format!("{count} results") };
    section_header(buf, row(body, 0), data.filter.label(), &meta, theme);
    let list_area = Rect {
        y: body.y + 2,
        height: body.height.saturating_sub(2),
        ..body
    };
    match data.filter {
        SearchFilter::Songs => render_track_list(
            data.tracks,
            list,
            TrackListOpts {
                playing_id,
                ..Default::default()
            },
            list_area,
            buf,
            theme,
            Some(&mut hit.list_rows),
        ),
        SearchFilter::Albums => {
            let items: Vec<_> = data
                .albums
                .iter()
                .map(|a| (a.title.clone(), artist_line(a)))
                .collect();
            render_item_list(
                &items,
                "◉",
                list.selected,
                list_area,
                buf,
                theme,
                Some(&mut hit.list_rows),
            );
        }
        SearchFilter::Artists => {
            let items: Vec<_> = data
                .artists
                .iter()
                .map(|a| (a.name.clone(), "Artist".to_string()))
                .collect();
            render_item_list(
                &items,
                "☺",
                list.selected,
                list_area,
                buf,
                theme,
                Some(&mut hit.list_rows),
            );
        }
        SearchFilter::Playlists => {
            let items: Vec<_> = data
                .playlists
                .iter()
                .map(|p| (p.title.clone(), p.author.clone().unwrap_or_default()))
                .collect();
            render_item_list(
                &items,
                "≡",
                list.selected,
                list_area,
                buf,
                theme,
                Some(&mut hit.list_rows),
            );
        }
    }
}
