use crate::api::model::Track;
use crate::ui::components::{empty_state, row, section_header, truncate};
use crate::ui::glyphs::{glyph, Glyph};
use crate::ui::theme::Theme;
use crate::ui::widgets::track_list::{render_track_list, TrackListOpts, TrackListState};
use crate::ui::HitMap;
use ratatui::prelude::*;

pub fn render(
    tracks: &[&Track],
    cursor: usize,
    selected: usize,
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    hit: &mut HitMap,
) {
    if tracks.is_empty() {
        empty_state(
            buf,
            area,
            glyph(theme, Glyph::Queue),
            "Your queue is empty",
            "Search for a song and press Enter, or press a to add",
            theme,
        );
        return;
    }

    let mut y = 0;
    if let Some(cur) = tracks.get(cursor) {
        section_header(buf, row(area, 0), "Now playing", "", theme);
        let line = Line::from(vec![
            Span::styled("▶  ", theme.playing()),
            Span::styled(truncate(&cur.title, area.width as usize / 2), theme.playing()),
            Span::styled(format!("  ·  {}", cur.artist_names()), theme.dim()),
        ]);
        buf.set_line(area.x, area.y + 2, &line, area.width);
        y = 5;
    }

    let upcoming = tracks.len().saturating_sub(cursor + 1);
    let meta = match upcoming {
        0 => "nothing up next".to_string(),
        1 => "1 track up next".to_string(),
        n => format!("{n} tracks up next"),
    };
    section_header(buf, row(area, y), "Queue", &meta, theme);
    let list_area = Rect {
        y: area.y + y + 2,
        height: area.height.saturating_sub(y + 2),
        ..area
    };
    let state = TrackListState {
        selected,
        ..Default::default()
    };
    render_track_list(
        tracks,
        &state,
        TrackListOpts {
            playing_id: tracks.get(cursor).map(|t| t.video_id.as_str()),
            dim_before: Some(cursor),
            no_selection: false,
        },
        list_area,
        buf,
        theme,
        Some(&mut hit.list_rows),
    );
}
