use crate::action::Action;
use crate::api::model::format_duration;
use crate::art::ArtPixels;
use crate::player::queue::RepeatMode;
use crate::player::PlaybackState;
use crate::ui::theme::Theme;
use ratatui::prelude::*;
use std::time::Duration;
use unicode_width::UnicodeWidthStr;

/// Largest square (in pixels) that fits `area`, given half-block cells are 1×2 pixels.
pub fn art_rect(area: Rect) -> Rect {
    let rows = area.height.min(area.width / 2);
    let cols = rows * 2;
    Rect {
        x: area.x + (area.width - cols) / 2,
        y: area.y + (area.height - rows) / 2,
        width: cols,
        height: rows,
    }
}

pub fn render_art(buf: &mut Buffer, area: Rect, pixels: Option<&ArtPixels>, theme: &Theme) {
    let r = art_rect(area);
    if r.width == 0 || r.height == 0 {
        return;
    }
    let Some(px) = pixels else {
        render_art_placeholder(buf, r, theme);
        return;
    };
    let w = r.width as f32;
    let h = (r.height * 2) as f32;
    for cy in 0..r.height {
        for cx in 0..r.width {
            let x0 = cx as f32 / w;
            let x1 = (cx + 1) as f32 / w;
            let ty0 = (cy * 2) as f32 / h;
            let ty1 = (cy * 2 + 1) as f32 / h;
            let by1 = (cy * 2 + 2) as f32 / h;
            let (tr, tg, tb) = px.sample(x0, ty0, x1, ty1);
            let (br, bg, bb) = px.sample(x0, ty1, x1, by1);
            if let Some(cell) = buf.cell_mut((r.x + cx, r.y + cy)) {
                cell.set_symbol("▀")
                    .set_fg(theme.color(tr, tg, tb))
                    .set_bg(theme.color(br, bg, bb));
            }
        }
    }
}

pub fn render_art_placeholder(buf: &mut Buffer, r: Rect, theme: &Theme) {
    crate::ui::components::fill(buf, r, Style::default().bg(theme.bg_selected));
    let note = "♫";
    let y = r.y + r.height / 2;
    let x = r.x + r.width / 2;
    buf.set_string(x, y, note, Style::default().fg(theme.text_muted).bg(theme.bg_selected));
}

/// `0:42 ━━━━━━━━●━━━━━━━━━ 3:20` — returns the rect of the bar itself for click-to-seek.
pub fn render_progress(
    buf: &mut Buffer,
    area: Rect,
    position: Duration,
    duration: Duration,
    state: PlaybackState,
    theme: &Theme,
) -> Rect {
    if area.width < 12 || area.height == 0 {
        return Rect::default();
    }
    let left = format_duration(position);
    let right = if duration.is_zero() {
        "--:--".to_string()
    } else {
        format_duration(duration)
    };
    let lw = left.width() as u16;
    let rw = right.width() as u16;
    buf.set_string(area.x, area.y, &left, theme.dim());
    buf.set_string(area.x + area.width - rw, area.y, &right, theme.dim());

    let bar = Rect {
        x: area.x + lw + 2,
        y: area.y,
        width: area.width.saturating_sub(lw + rw + 4),
        height: 1,
    };
    if bar.width == 0 {
        return bar;
    }
    let ratio = if duration.as_secs_f64() > 0.0 {
        (position.as_secs_f64() / duration.as_secs_f64()).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let knob = ((bar.width - 1) as f64 * ratio).round() as u16;
    let loading = matches!(state, PlaybackState::Loading | PlaybackState::Buffering);
    for i in 0..bar.width {
        let (sym, style) = if i < knob {
            ("━", Style::default().fg(theme.accent))
        } else if i == knob && !loading {
            ("●", Style::default().fg(theme.text))
        } else {
            ("━", Style::default().fg(theme.bg_selected))
        };
        buf.set_string(bar.x + i, bar.y, sym, style);
    }
    bar
}

pub struct ControlsState {
    pub state: PlaybackState,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub liked: bool,
}

/// Centered `⇄   ⏮   ( ⏸ )   ⏭   ↻   ♥` row. Returns clickable rects.
pub fn render_controls(
    buf: &mut Buffer,
    area: Rect,
    c: &ControlsState,
    theme: &Theme,
) -> Vec<(Rect, Action)> {
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }
    let on = theme.accent_bold();
    let off = theme.dim();
    let normal = theme.title();
    let play = match c.state {
        PlaybackState::Playing => " ⏸ ",
        PlaybackState::Loading | PlaybackState::Buffering => " … ",
        _ => " ▶ ",
    };
    let repeat = match c.repeat {
        RepeatMode::One => "↻¹",
        _ => "↻",
    };
    let buttons: Vec<(&str, Style, Action)> = vec![
        ("⇄", if c.shuffle { on } else { off }, Action::ToggleShuffle),
        ("⏮", normal, Action::Previous),
        (play, theme.on_accent(), Action::PlayPause),
        ("⏭", normal, Action::Next),
        (
            repeat,
            if c.repeat == RepeatMode::Off { off } else { on },
            Action::CycleRepeat,
        ),
        (
            if c.liked { "♥" } else { "♡" },
            if c.liked { on } else { off },
            Action::LikeCurrent,
        ),
    ];
    let gap: u16 = if area.width >= 44 { 4 } else { 2 };
    let total: u16 = buttons.iter().map(|(s, _, _)| s.width() as u16).sum::<u16>()
        + gap * (buttons.len() as u16 - 1);
    let mut x = area.x + area.width.saturating_sub(total) / 2;
    let mut hits = Vec::new();
    for (sym, style, action) in buttons {
        let w = sym.width() as u16;
        if x + w > area.x + area.width {
            break;
        }
        buf.set_string(x, area.y, sym, style);
        hits.push((
            Rect {
                x: x.saturating_sub(1),
                y: area.y,
                width: w + 2,
                height: 1,
            },
            action,
        ));
        x += w + gap;
    }
    hits
}

/// `🔊 ━━━━━━━━──── 60%`, right-aligned in `area` — returns the bar rect for click-to-set.
pub fn render_volume(buf: &mut Buffer, area: Rect, volume: f32, muted: bool, theme: &Theme) -> Rect {
    if area.width < 8 || area.height == 0 {
        return Rect::default();
    }
    let icon = if muted || volume <= 0.001 {
        "🔇"
    } else if volume < 0.5 {
        "🔉"
    } else {
        "🔊"
    };
    let pct = if muted {
        "mute".to_string()
    } else {
        format!("{:>4}", format!("{:.0}%", volume * 100.0))
    };
    let iw = icon.width() as u16;
    let segs = area.width.saturating_sub(iw + 1 + pct.width() as u16 + 1).min(14);
    let total = iw + 1 + segs + 1 + pct.width() as u16;
    let x0 = area.x + area.width.saturating_sub(total);
    buf.set_string(x0, area.y, icon, theme.dim());
    let bar = Rect {
        x: x0 + iw + 1,
        y: area.y,
        width: segs,
        height: 1,
    };
    let filled = if muted {
        0
    } else {
        (volume.clamp(0.0, 1.0) * segs as f32).round() as u16
    };
    for i in 0..segs {
        let color = if i < filled { theme.text } else { theme.bg_selected };
        buf.set_string(bar.x + i, bar.y, "━", Style::default().fg(color));
    }
    buf.set_string(bar.x + segs + 1, area.y, &pct, theme.muted());
    bar
}

/// Braille spinner frame derived from wall-clock time.
pub fn spinner() -> &'static str {
    const FRAMES: [&str; 8] = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    FRAMES[(ms / 120) as usize % FRAMES.len()]
}
