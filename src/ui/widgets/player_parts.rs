use crate::action::Action;
use crate::api::model::format_duration;
use crate::art::ArtPixels;
use crate::player::queue::RepeatMode;
use crate::player::PlaybackState;
use crate::ui::cover::CoverCtx;
use crate::ui::icons::Icon;
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

/// `0:42  ████████████▒▒▒▒▒▒▒▒  3:20` — block-fill progress bar; returns the bar rect for click-to-seek.
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
    let filled = ((bar.width as f64 * ratio).round() as u16).min(bar.width);
    let loading = matches!(state, PlaybackState::Loading | PlaybackState::Buffering);
    for i in 0..bar.width {
        let (sym, color) = if loading {
            ("▒", theme.text_muted)
        } else if i < filled {
            ("█", theme.accent)
        } else {
            ("▒", theme.bg_selected)
        };
        buf.set_string(bar.x + i, bar.y, sym, Style::default().fg(color));
    }
    bar
}

pub struct ControlsState {
    pub state: PlaybackState,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub liked: bool,
}

/// Flat controls, no backgrounds: `⇄ ⏮ ▶ ⏭ ↻ ♥`. Drawn as scalable images when the terminal
/// supports graphics, as plain glyphs otherwise. Returns clickable rects.
pub fn render_controls(
    buf: &mut Buffer,
    area: Rect,
    c: &ControlsState,
    theme: &Theme,
    cover: Option<&CoverCtx>,
) -> Vec<(Rect, Action)> {
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }
    let active = Style::default().fg(theme.accent).add_modifier(Modifier::BOLD);
    let inactive = Style::default().fg(theme.text_muted);
    let normal = Style::default().fg(theme.text);
    let loading = matches!(c.state, PlaybackState::Loading | PlaybackState::Buffering);
    let spin = spinner();

    // (icon, glyph, style, action)
    let buttons: Vec<(Icon, &str, Style, Action)> = vec![
        (Icon::Shuffle, "⇄", if c.shuffle { active } else { inactive }, Action::ToggleShuffle),
        (Icon::Prev, "⏮", normal, Action::Previous),
        (
            if c.state == PlaybackState::Playing { Icon::Pause } else { Icon::Play },
            match c.state {
                PlaybackState::Playing => "⏸",
                _ if loading => spin,
                _ => "▶",
            },
            active,
            Action::PlayPause,
        ),
        (Icon::Next, "⏭", normal, Action::Next),
        (
            if c.repeat == RepeatMode::One { Icon::RepeatOne } else { Icon::Repeat },
            if c.repeat == RepeatMode::One { "↻¹" } else { "↻" },
            if c.repeat == RepeatMode::Off { inactive } else { active },
            Action::CycleRepeat,
        ),
        (
            if c.liked { Icon::Heart } else { Icon::HeartOutline },
            if c.liked { "♥" } else { "♡" },
            if c.liked { active } else { inactive },
            Action::LikeCurrent,
        ),
    ];

    let renderer = cover
        .filter(|ctx| {
            ctx.graphics
                && ctx.renderer.icons_available()
                && !ctx.blocked.iter().any(|b| b.intersects(area))
        })
        .map(|ctx| ctx.renderer)
        .filter(|_| area.height >= 2);

    let rows = area.height.min(2);
    let icon_w = renderer.map(|r| r.width_for_rows(rows)).unwrap_or(0);
    let width_of = |glyph: &str| if renderer.is_some() { icon_w } else { glyph.width() as u16 };
    let gap_for = |cut: u16, play: bool| -> u16 {
        let base: u16 = if renderer.is_some() { if play { 4 } else { 3 } } else if play { 3 } else { 2 };
        base.saturating_sub(cut).max(1)
    };
    let total_at = |cut: u16| -> u16 {
        buttons
            .iter()
            .map(|(_, g, _, a)| width_of(g) + gap_for(cut, matches!(a, Action::PlayPause)))
            .sum::<u16>()
            .saturating_sub(gap_for(cut, false))
    };
    let cut = (0u16..=3).find(|&d| total_at(d) <= area.width).unwrap_or(3);

    let mut x = area.x + area.width.saturating_sub(total_at(cut)) / 2;
    let top = area.y + (area.height - rows) / 2;
    let mid = area.y + (area.height - 1) / 2;
    let bg = rgb_of(theme.bg_player, (24, 24, 24));
    let mut hits = Vec::new();
    for (icon, glyph, style, action) in buttons {
        let is_play = matches!(action, Action::PlayPause);
        let w = width_of(glyph);
        let half = gap_for(cut, is_play) / 2;
        if x + w > area.x + area.width {
            break;
        }
        let rect = Rect {
            x: x.saturating_sub(half).max(area.x),
            y: area.y,
            width: w + 2 * half,
            height: area.height,
        };
        let drawn = renderer.is_some_and(|r| {
            let fg = rgb_of(style.fg.unwrap_or(theme.text), (230, 230, 230));
            r.draw_icon(icon, fg, bg, Rect { x, y: top, width: w, height: rows }, buf)
        });
        if !drawn {
            buf.set_string(x, mid, glyph, style);
        }
        hits.push((rect, action));
        x += w + gap_for(cut, is_play);
    }
    hits
}

fn rgb_of(c: Color, fallback: (u8, u8, u8)) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Indexed(n) if n >= 232 => {
            let v = 8 + 10 * (n - 232);
            (v, v, v)
        }
        Color::Indexed(n) if n >= 16 => {
            let n = n - 16;
            let lv = |i: u8| if i == 0 { 0 } else { 55 + 40 * i };
            (lv(n / 36), lv((n / 6) % 6), lv(n % 6))
        }
        _ => fallback,
    }
}

/// `♫ ██████████░░░░ 65%` — icon + block level stretched across `area` + percentage.
/// Returns the block bar rect for click-to-set volume.
pub fn render_volume(buf: &mut Buffer, area: Rect, volume: f32, muted: bool, theme: &Theme) -> Rect {
    if area.width < 6 || area.height == 0 {
        return Rect::default();
    }
    let silent = muted || volume <= 0.001;
    let pct = if muted {
        " mut".to_string()
    } else {
        format!("{:>4}", format!("{:.0}%", volume * 100.0))
    };
    // icon(1) + space(1) + blocks + space(1) + pct; blocks stretch to fill `area`.
    let segs = area.width.saturating_sub(3 + pct.width() as u16).max(3);
    let x0 = area.x;

    let icon_color = if silent { theme.text_muted } else { theme.accent };
    buf.set_string(x0, area.y, "♫", Style::default().fg(icon_color));

    let bar = Rect { x: x0 + 2, y: area.y, width: segs, height: 1 };
    let filled = if silent {
        0
    } else {
        (volume.clamp(0.0, 1.0) * segs as f32).round() as u16
    };
    for i in 0..segs {
        let (sym, color) = if i < filled { ("█", theme.text) } else { ("░", theme.text_muted) };
        buf.set_string(bar.x + i, bar.y, sym, Style::default().fg(color));
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
