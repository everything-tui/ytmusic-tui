use crate::ui::theme::Theme;
use ratatui::prelude::*;
use ratatui::widgets::Clear;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Error,
    Success,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub message: String,
    pub kind: ToastKind,
    pub created: Instant,
}

impl Toast {
    pub fn info(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            kind: ToastKind::Info,
            created: Instant::now(),
        }
    }
    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            kind: ToastKind::Error,
            created: Instant::now(),
        }
    }
    pub fn success(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            kind: ToastKind::Success,
            created: Instant::now(),
        }
    }

    pub fn expired(&self) -> bool {
        let ttl = match self.kind {
            ToastKind::Error => 6,
            _ => 3,
        };
        self.created.elapsed() > Duration::from_secs(ttl)
    }
}

const MAX_VISIBLE: usize = 3;
const MAX_WIDTH: u16 = 52;

struct Placed<'a> {
    rect: Rect,
    toast: &'a Toast,
    lines: Vec<String>,
}

/// Stacks the newest toasts upward from the bottom-right corner of `area`.
fn place(toasts: &[Toast], area: Rect) -> Vec<Placed<'_>> {
    let mut out = Vec::new();
    if area.width < 24 || area.height < 6 {
        return out;
    }
    let mut bottom = area.y + area.height.saturating_sub(1);
    for t in toasts.iter().rev().filter(|t| !t.expired()).take(MAX_VISIBLE) {
        let max_w = MAX_WIDTH.min(area.width.saturating_sub(4));
        let text_w = max_w.saturating_sub(8) as usize;
        let lines = wrap_lines(&t.message, text_w, 2);
        let content_w = lines.iter().map(|l| l.width()).max().unwrap_or(0) as u16;
        let width = (content_w + 8).clamp(24, max_w);
        let height = lines.len() as u16 + 2;
        if bottom < area.y + height {
            break;
        }
        let rect = Rect {
            x: area.x + area.width.saturating_sub(width + 2),
            y: bottom - height,
            width,
            height,
        };
        bottom = rect.y.saturating_sub(1);
        out.push(Placed { rect, toast: t, lines });
    }
    out
}

/// Where toasts will be drawn, so covers underneath can avoid graphics protocols.
pub fn toast_rects(toasts: &[Toast], area: Rect) -> Vec<Rect> {
    place(toasts, area).into_iter().map(|p| p.rect).collect()
}

pub fn render_toasts(toasts: &[Toast], area: Rect, buf: &mut Buffer, theme: &Theme) {
    for p in place(toasts, area) {
        let color = match p.toast.kind {
            ToastKind::Error => theme.error,
            ToastKind::Success => theme.success,
            ToastKind::Info => theme.accent,
        };
        let icon = match p.toast.kind {
            ToastKind::Error => "✕",
            ToastKind::Success => "✓",
            ToastKind::Info => "●",
        };
        let r = p.rect;
        Clear.render(r, buf);
        let bg = Style::default().bg(theme.bg_modal);
        crate::ui::components::fill(buf, r, bg);
        for y in r.y..r.y + r.height {
            buf.set_string(r.x, y, "▌", Style::default().fg(color).bg(theme.bg_modal));
        }
        for (i, l) in p.lines.iter().enumerate() {
            let y = r.y + 1 + i as u16;
            if i == 0 {
                buf.set_string(r.x + 2, y, icon, Style::default().fg(color).add_modifier(Modifier::BOLD).bg(theme.bg_modal));
            }
            buf.set_stringn(r.x + 5, y, l, r.width.saturating_sub(6) as usize, theme.normal().patch(bg));
        }
    }
}

fn wrap_lines(msg: &str, width: usize, max_lines: usize) -> Vec<String> {
    let width = width.max(8);
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in msg.split_whitespace() {
        let candidate_w = cur.width() + if cur.is_empty() { 0 } else { 1 } + word.width();
        if candidate_w > width && !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
    }
    for l in lines.iter_mut() {
        if l.width() > width {
            let mut s: String = String::new();
            for ch in l.chars() {
                if s.width() + 2 > width {
                    break;
                }
                s.push(ch);
            }
            s.push('…');
            *l = s;
        }
    }
    if msg.split_whitespace().count() > 0
        && lines.len() == max_lines
        && lines.join(" ").len() < msg.split_whitespace().collect::<Vec<_>>().join(" ").len()
    {
        if let Some(last) = lines.last_mut() {
            if !last.ends_with('…') {
                last.push('…');
            }
        }
    }
    lines
}
