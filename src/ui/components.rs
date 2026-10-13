use crate::ui::theme::Theme;
use ratatui::prelude::*;
use ratatui::widgets::Clear;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Paints every cell of `rect` with `style` (typically a background).
pub fn fill(buf: &mut Buffer, rect: Rect, style: Style) {
    let rect = rect.intersection(buf.area);
    for y in rect.y..rect.y + rect.height {
        for x in rect.x..rect.x + rect.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.reset();
                cell.set_style(style);
            }
        }
    }
}

pub fn pad(rect: Rect, x: u16, y: u16) -> Rect {
    Rect {
        x: rect.x + x.min(rect.width / 2),
        y: rect.y + y.min(rect.height / 2),
        width: rect.width.saturating_sub(x * 2),
        height: rect.height.saturating_sub(y * 2),
    }
}

pub fn row(rect: Rect, i: u16) -> Rect {
    Rect {
        y: rect.y + i,
        height: if i < rect.height { 1 } else { 0 },
        ..rect
    }
}

/// Width-aware truncation that ends in `…` when the text doesn't fit.
pub fn truncate(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut w = 0;
    let mut out = String::new();
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if w + cw + 1 > max {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

/// Bold title on the left, muted metadata right-aligned on the same row.
pub fn section_header(buf: &mut Buffer, rect: Rect, title: &str, meta: &str, theme: &Theme) {
    if rect.height == 0 || rect.width == 0 {
        return;
    }
    let meta_w = meta.width() as u16;
    let title_w = rect.width.saturating_sub(if meta.is_empty() { 0 } else { meta_w + 2 });
    buf.set_stringn(rect.x, rect.y, truncate(title, title_w as usize), title_w as usize, theme.heading());
    if !meta.is_empty() && meta_w + 2 < rect.width {
        buf.set_string(rect.x + rect.width - meta_w, rect.y, meta, theme.muted());
    }
}

/// ` label ` drawn as a chip. Returns its rect for mouse hit-testing.
pub fn pill(buf: &mut Buffer, x: u16, y: u16, label: &str, active: bool, theme: &Theme) -> Rect {
    let text = format!(" {label} ");
    let w = text.width() as u16;
    buf.set_string(x, y, &text, theme.pill(active));
    Rect { x, y, width: w, height: 1 }
}

/// Draws a row of pills separated by one space. Returns each pill's rect.
pub fn pills(
    buf: &mut Buffer,
    rect: Rect,
    labels: &[&str],
    active: Option<usize>,
    theme: &Theme,
) -> Vec<Rect> {
    let mut x = rect.x;
    let mut out = Vec::new();
    for (i, l) in labels.iter().enumerate() {
        let w = l.width() as u16 + 2;
        if x + w > rect.x + rect.width {
            break;
        }
        out.push(pill(buf, x, rect.y, l, active == Some(i), theme));
        x += w + 1;
    }
    out
}

pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

/// Clears a centered panel, paints it with the modal background and draws
/// `title` (and a muted right-aligned `hint`) on its first row.
/// Returns the padded content rect below the title.
pub fn modal(
    buf: &mut Buffer,
    area: Rect,
    width: u16,
    height: u16,
    title: &str,
    hint: &str,
    theme: &Theme,
) -> Rect {
    let popup = centered(area, width, height);
    Clear.render(popup, buf);
    fill(buf, popup, Style::default().bg(theme.bg_modal));
    let inner = pad(popup, 2, 1);
    if !title.is_empty() && inner.height > 0 {
        section_header(buf, row(inner, 0), title, hint, theme);
        return Rect {
            y: inner.y + 2,
            height: inner.height.saturating_sub(2),
            ..inner
        };
    }
    inner
}

/// Vertically centered icon, title and hint for loading or empty screens.
pub fn empty_state(buf: &mut Buffer, rect: Rect, icon: &str, title: &str, hint: &str, theme: &Theme) {
    if rect.height == 0 || rect.width == 0 {
        return;
    }
    let lines: Vec<(&str, Style)> = [(icon, theme.accent_bold()), (title, theme.heading()), (hint, theme.dim())]
        .into_iter()
        .filter(|(s, _)| !s.is_empty())
        .collect();
    let h = lines.len() as u16 + 1;
    let mut y = rect.y + rect.height.saturating_sub(h) / 2;
    for (i, (text, style)) in lines.iter().enumerate() {
        if y >= rect.y + rect.height {
            break;
        }
        for (j, part) in text.lines().enumerate() {
            let t = truncate(part, rect.width as usize);
            let x = rect.x + rect.width.saturating_sub(t.width() as u16) / 2;
            buf.set_string(x, y, &t, *style);
            if j + 1 < text.lines().count() {
                y += 1;
            }
        }
        y += if i == 0 { 2 } else { 1 };
    }
}

/// One-column scrollbar along the right edge of `rect`.
pub fn scrollbar(buf: &mut Buffer, rect: Rect, total: usize, offset: usize, visible: usize, theme: &Theme) {
    if total <= visible || rect.height == 0 || rect.width == 0 {
        return;
    }
    let x = rect.x + rect.width - 1;
    let h = rect.height as usize;
    let thumb = (h * visible / total).max(1);
    let max_off = total - visible;
    let start = (h - thumb) * offset.min(max_off) / max_off.max(1);
    for i in 0..h {
        let (sym, color) = if i >= start && i < start + thumb {
            ("┃", theme.dim)
        } else {
            ("│", theme.bg_hover)
        };
        buf.set_string(x, rect.y + i as u16, sym, Style::default().fg(color));
    }
}

/// Scroll offset that keeps `selected` within a window of `visible` rows.
pub fn scroll_offset(selected: usize, visible: usize, total: usize) -> usize {
    if visible == 0 || total <= visible {
        return 0;
    }
    let margin = (visible / 4).min(3);
    let max_off = total - visible;
    if selected + margin + 1 > visible {
        (selected + margin + 1 - visible).min(max_off)
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_respects_width() {
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(truncate("hello world", 6), "hello…");
        assert_eq!(truncate("日本語テキスト", 5).width(), 5);
    }

    #[test]
    fn scroll_keeps_selection_visible() {
        for sel in 0..50 {
            let off = scroll_offset(sel, 10, 50);
            assert!(sel >= off && sel < off + 10);
        }
        assert_eq!(scroll_offset(3, 10, 5), 0);
    }
}
