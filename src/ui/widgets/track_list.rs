use crate::api::model::Track;
use crate::ui::components::{scroll_offset, scrollbar, truncate};
use crate::ui::theme::Theme;
use ratatui::prelude::*;
use ratatui::widgets::{Cell, Row, Table, TableState};
use std::borrow::Borrow;

#[derive(Debug, Clone, Default)]
pub struct TrackListState {
    pub selected: usize,
    pub offset: usize,
    pub filter: String,
}

impl TrackListState {
    pub fn select_next(&mut self, len: usize) {
        if len == 0 {
            return;
        }
        self.selected = (self.selected + 1).min(len - 1);
    }
    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
    pub fn page(&mut self, delta: i32, len: usize, page: usize) {
        if len == 0 {
            return;
        }
        let s = self.selected as i32 + delta * page as i32;
        self.selected = s.clamp(0, len as i32 - 1) as usize;
    }
    pub fn top(&mut self) {
        self.selected = 0;
    }
    pub fn bottom(&mut self, len: usize) {
        if len > 0 {
            self.selected = len - 1;
        }
    }
}

/// Optional knobs for [`render_track_list`].
#[derive(Debug, Clone, Copy, Default)]
pub struct TrackListOpts<'a> {
    pub playing_id: Option<&'a str>,
    /// Rows before this index are drawn muted (already-played queue entries).
    pub dim_before: Option<usize>,
    /// Hide the cursor highlight (list isn't focused).
    pub no_selection: bool,
}

const RULE: &str = "─";

/// Header row, a thin rule, then one row per track with a full-width cursor highlight.
/// When `hits` is provided, appends `(row_rect, track_index)` for mouse targeting.
pub fn render_track_list<T: Borrow<Track>>(
    tracks: &[T],
    state: &TrackListState,
    opts: TrackListOpts<'_>,
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    hits: Option<&mut Vec<(Rect, usize)>>,
) {
    if area.height < 3 || area.width < 16 {
        return;
    }
    let w = area.width;
    let show_artist = w >= 56;
    let show_album = w >= 100;
    // Always reserve a time column once the table is usable — durations are a
    // primary cue and were easy to lose when width hovered near the old cutoff.
    let show_time = w >= 28;

    let visible = (area.height - 2) as usize;
    let needs_bar = tracks.len() > visible;
    let table_area = Rect {
        width: if needs_bar { w - 1 } else { w },
        ..area
    };

    let mut widths = vec![Constraint::Length(4), Constraint::Fill(4)];
    let mut header = vec![Cell::from(Line::from("#").right_aligned()), Cell::from("Title")];
    if show_artist {
        widths.push(Constraint::Fill(3));
        header.push(Cell::from("Artist"));
    }
    if show_album {
        widths.push(Constraint::Fill(3));
        header.push(Cell::from("Album"));
    }
    widths.push(Constraint::Length(1));
    header.push(Cell::from(""));
    if show_time {
        // Fits `h:mm:ss` / `hh:mm:ss` (e.g. 1:05:30, 12:00:00). Width 6
        // right-aligned clipped the hour, leaving a leading ":mm:ss".
        widths.push(Constraint::Length(8));
        header.push(Cell::from(Line::from("Time").right_aligned()));
    }

    let title_w = estimate_fill_width(table_area.width, &widths, 4);
    let rows: Vec<Row> = tracks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let t: &Track = t.borrow();
            let is_playing = opts.playing_id == Some(t.video_id.as_str());
            let played = opts.dim_before.is_some_and(|d| i < d);
            let base = if is_playing {
                theme.row_playing()
            } else if played {
                theme.muted()
            } else {
                theme.normal()
            };
            let sub = if played { theme.muted() } else { theme.dim() };

            let num = if is_playing {
                Line::from(Span::styled("▶", theme.playing())).right_aligned()
            } else {
                Line::from(Span::styled(format!("{}", i + 1), theme.muted())).right_aligned()
            };

            let mut title_spans = Vec::new();
            let mut budget = title_w;
            if t.explicit {
                budget = budget.saturating_sub(3);
            }
            if t.from_radio {
                budget = budget.saturating_sub(2);
            }
            let title_style = if is_playing { theme.playing() } else { base };
            if !show_artist {
                let artist = t.artist_names();
                let tt = truncate(&t.title, budget);
                let rest = budget.saturating_sub(unicode_width::UnicodeWidthStr::width(tt.as_str()) + 3);
                title_spans.push(Span::styled(tt, title_style));
                if rest > 2 && !artist.is_empty() {
                    title_spans.push(Span::styled(format!(" · {}", truncate(&artist, rest)), sub));
                }
            } else {
                title_spans.push(Span::styled(truncate(&t.title, budget), title_style));
            }
            if t.explicit {
                title_spans.push(Span::styled(" E ", Style::default().fg(theme.bg).bg(theme.dim)));
            }
            if t.from_radio {
                title_spans.push(Span::styled(" ⟳", theme.muted()));
            }

            let mut cells = vec![Cell::from(num), Cell::from(Line::from(title_spans))];
            if show_artist {
                cells.push(Cell::from(Span::styled(t.artist_names(), sub)));
            }
            if show_album {
                let album = t.album.as_ref().map(|a| a.name.as_str()).unwrap_or("");
                cells.push(Cell::from(Span::styled(album.to_string(), sub)));
            }
            let heart = match t.liked {
                Some(true) => Span::styled("♥", theme.accent()),
                _ => Span::raw(""),
            };
            cells.push(Cell::from(heart));
            if show_time {
                cells.push(Cell::from(
                    Line::from(Span::styled(t.duration_label(), sub)).right_aligned(),
                ));
            }
            Row::new(cells).style(base)
        })
        .collect();

    let offset = scroll_offset(state.selected, visible, tracks.len());
    let mut ts = TableState::new().with_offset(offset);
    if !opts.no_selection && !tracks.is_empty() {
        ts = ts.with_selected(Some(state.selected.min(tracks.len() - 1)));
    }
    let table = Table::new(rows, widths)
        .header(Row::new(header).style(theme.muted()).bottom_margin(1))
        .column_spacing(2)
        .row_highlight_style(theme.row_selected());
    ratatui::widgets::StatefulWidget::render(table, table_area, buf, &mut ts);

    let rule_y = area.y + 1;
    for x in area.x..area.x + area.width {
        buf.set_string(x, rule_y, RULE, theme.rule());
    }
    if needs_bar {
        let bar = Rect {
            y: area.y + 2,
            height: area.height - 2,
            ..area
        };
        scrollbar(buf, bar, tracks.len(), offset, visible, theme);
    }
    if let Some(hits) = hits {
        for i in 0..visible {
            let idx = offset + i;
            if idx >= tracks.len() {
                break;
            }
            let y = area.y + 2 + i as u16;
            if y >= area.y + area.height {
                break;
            }
            hits.push((
                Rect {
                    x: table_area.x,
                    y,
                    width: table_area.width,
                    height: 1,
                },
                idx,
            ));
        }
    }
}

/// Rough width of a `Fill(weight)` column, for pre-truncating text that is built from spans.
fn estimate_fill_width(total: u16, widths: &[Constraint], weight: u16) -> usize {
    let spacing = 2 * widths.len().saturating_sub(1) as u16;
    let fixed: u16 = widths
        .iter()
        .map(|c| match c {
            Constraint::Length(n) => *n,
            _ => 0,
        })
        .sum();
    let weights: u16 = widths
        .iter()
        .map(|c| match c {
            Constraint::Fill(n) => *n,
            _ => 0,
        })
        .sum();
    let free = total.saturating_sub(fixed + spacing);
    (free as u32 * weight as u32 / weights.max(1) as u32) as usize
}

/// A list of named things (albums, artists, playlists): icon, title, muted subtitle.
pub fn render_item_list(
    items: &[(String, String)],
    icon: &str,
    selected: usize,
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    hits: Option<&mut Vec<(Rect, usize)>>,
) {
    if area.height == 0 || area.width < 8 || items.is_empty() {
        return;
    }
    let visible = area.height as usize;
    let needs_bar = items.len() > visible;
    let list_area = Rect {
        width: if needs_bar { area.width - 1 } else { area.width },
        ..area
    };
    let show_sub = list_area.width >= 40;
    let mut widths = vec![Constraint::Length(2), Constraint::Fill(3)];
    if show_sub {
        widths.push(Constraint::Fill(2));
    }
    let rows: Vec<Row> = items
        .iter()
        .map(|(title, sub)| {
            let mut cells = vec![
                Cell::from(Span::styled(icon.to_string(), theme.muted())),
                Cell::from(Span::styled(title.clone(), theme.normal())),
            ];
            if show_sub {
                cells.push(Cell::from(Span::styled(sub.clone(), theme.dim())));
            }
            Row::new(cells)
        })
        .collect();
    let offset = scroll_offset(selected, visible, items.len());
    let mut ts = TableState::new()
        .with_offset(offset)
        .with_selected(Some(selected.min(items.len() - 1)));
    let table = Table::new(rows, widths)
        .column_spacing(2)
        .row_highlight_style(theme.row_selected());
    ratatui::widgets::StatefulWidget::render(table, list_area, buf, &mut ts);
    if needs_bar {
        scrollbar(buf, area, items.len(), offset, visible, theme);
    }
    if let Some(hits) = hits {
        for i in 0..visible {
            let idx = offset + i;
            if idx >= items.len() {
                break;
            }
            let y = area.y + i as u16;
            if y >= area.y + area.height {
                break;
            }
            hits.push((
                Rect {
                    x: list_area.x,
                    y,
                    width: list_area.width,
                    height: 1,
                },
                idx,
            ));
        }
    }
}
