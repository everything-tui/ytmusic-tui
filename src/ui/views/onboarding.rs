use crate::ui::components::{centered, fill, pill};
use crate::ui::theme::Theme;
use ratatui::prelude::*;
use ratatui::widgets::{Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

pub fn render(area: Rect, buf: &mut Buffer, theme: &Theme, status: &str) {
    fill(buf, area, Style::default().bg(theme.bg));
    let width = 56.min(area.width.saturating_sub(4));
    let height = 18.min(area.height);
    let r = centered(area, width, height);
    if r.width < 20 || r.height < 10 {
        return;
    }
    let center = |text: &str| r.x + r.width.saturating_sub(text.width() as u16) / 2;

    let brand = "▶  yt-tui";
    buf.set_string(center(brand), r.y, brand, theme.accent_bold());
    let pitch = "YouTube Music, right in your terminal";
    buf.set_string(center(pitch), r.y + 2, pitch, theme.dim());

    let options = [
        ("Enter", "Sign in with your browser"),
        ("G", "Continue as guest"),
        ("P", "Paste a cookie manually"),
    ];
    let row_w = 7 + 2 + options.iter().map(|(_, l)| l.width()).max().unwrap_or(0) as u16;
    let ox = r.x + r.width.saturating_sub(row_w) / 2;
    for (i, (key, label)) in options.iter().enumerate() {
        let y = r.y + 5 + i as u16 * 2;
        let key_label = format!("{key:^5}");
        pill(buf, ox, y, &key_label, i == 0, theme);
        let style = if i == 0 { theme.heading() } else { theme.normal() };
        buf.set_string(ox + 9, y, label, style);
    }

    if !status.is_empty() {
        let s = Rect {
            y: r.y + 12,
            height: r.height.saturating_sub(12),
            ..r
        };
        Paragraph::new(status.trim())
            .style(theme.dim())
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .render(s, buf);
    }
}
