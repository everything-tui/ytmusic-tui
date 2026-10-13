use crate::api::model::Lyrics;
use crate::lyrics::current_line_index;
use crate::ui::components::{empty_state, truncate};
use crate::ui::theme::Theme;
use ratatui::prelude::*;
use ratatui::widgets::{Paragraph, Wrap};
use std::time::Duration;
use unicode_width::UnicodeWidthStr;

pub fn render(
    lyrics: Option<&Lyrics>,
    position: Duration,
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let Some(lyrics) = lyrics else {
        empty_state(buf, area, "✎", "No lyrics yet", "Lyrics show up here for the playing track", theme);
        return;
    };

    let body = Rect {
        height: area.height.saturating_sub(2),
        ..area
    };
    if let Some(ref synced) = lyrics.synced {
        let cur = current_line_index(synced, position).unwrap_or(0);
        let half = (body.height as usize / 2).max(1);
        let start = cur.saturating_sub(half);
        let mut y = body.y + half.saturating_sub(cur - start) as u16;
        for (i, line) in synced.iter().enumerate().skip(start) {
            if y >= body.y + body.height {
                break;
            }
            let style = match i.abs_diff(cur) {
                0 => theme.accent_bold(),
                1 => theme.normal(),
                2..=3 => theme.dim(),
                _ => theme.muted(),
            };
            let text = if line.text.trim().is_empty() {
                "♪".to_string()
            } else {
                truncate(&line.text, body.width as usize)
            };
            let x_off = body.width.saturating_sub(text.width() as u16) / 2;
            buf.set_string(body.x + x_off, y, &text, style);
            y += 1;
        }
    } else if let Some(ref plain) = lyrics.plain {
        Paragraph::new(plain.as_str())
            .style(theme.normal())
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .render(body, buf);
    } else {
        empty_state(buf, area, "✎", "Lyrics not available", "", theme);
        return;
    }

    if area.height > 2 {
        let src = format!("Lyrics · {}", lyrics.source);
        let src = truncate(&src, area.width as usize);
        let x = area.x + area.width.saturating_sub(src.width() as u16) / 2;
        buf.set_string(x, area.y + area.height - 1, &src, theme.muted());
    }
}
