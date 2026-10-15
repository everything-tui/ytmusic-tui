use crate::api::model::{Shelf, ShelfItem};
use crate::art::Cover;
use crate::ui::components::{empty_state, fill, section_header, truncate};
use crate::ui::cover::{CoverRenderer, CoverSlot};
use crate::ui::theme::Theme;
use crate::ui::HitMap;
use ratatui::prelude::*;
use std::collections::HashMap;
use unicode_width::UnicodeWidthStr;

pub(crate) const CARD_GAP: u16 = 2;

pub fn render(
    shelves: &[Shelf],
    shelf_idx: usize,
    item_idx: usize,
    loading: bool,
    covers: &HashMap<String, Cover>,
    renderer: &CoverRenderer,
    graphics: bool,
    blocked: &[Rect],
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    hit: &mut HitMap,
) {
    if loading && shelves.is_empty() {
        empty_state(buf, area, "", "Loading your home feed…", "", theme);
        return;
    }
    if shelves.is_empty() {
        empty_state(
            buf,
            area,
            "⌂",
            "Nothing here yet",
            "Press / to search · sign in with L for personalized picks",
            theme,
        );
        return;
    }

    // Taller art so half-blocks (and kitty/iTerm overlays) have enough resolution.
    let art_h: u16 = if area.height >= 34 {
        8
    } else if area.height >= 24 {
        6
    } else if area.height >= 16 {
        4
    } else {
        3
    };
    let card_w = renderer.width_for_rows(art_h).clamp(14, 22);
    let card_h = art_h + 2;
    let shelf_h = 2 + card_h + 2;

    let per_page = (area.height / shelf_h).max(1) as usize;
    let first = shelf_idx.saturating_sub(per_page - 1).min(shelf_idx);

    let cards_fit = ((area.width + CARD_GAP) / (card_w + CARD_GAP)).max(1) as usize;
    let mut y = area.y;
    let mut slot_i: u8 = 0;
    for (si, shelf) in shelves.iter().enumerate().skip(first) {
        if y + 3 > area.y + area.height {
            break;
        }
        let active_shelf = si == shelf_idx;
        let meta = if active_shelf && shelf.items.len() > cards_fit {
            format!("{}/{}  ←→", item_idx + 1, shelf.items.len())
        } else {
            String::new()
        };
        section_header(
            buf,
            Rect { y, height: 1, ..area },
            &shelf.title,
            &meta,
            theme,
        );
        y += 2;

        let start = if active_shelf {
            item_idx.saturating_sub(cards_fit - 1)
        } else {
            0
        };
        let mut x = area.x;
        for (ii, item) in shelf.items.iter().enumerate().skip(start) {
            if x + card_w > area.x + area.width {
                break;
            }
            let h = card_h.min((area.y + area.height).saturating_sub(y));
            if h < 2 {
                break;
            }
            let selected = active_shelf && ii == item_idx;
            let card = Rect::new(x, y, card_w, h);
            render_card(
                item,
                covers,
                renderer,
                graphics,
                blocked,
                CoverSlot::Home(slot_i),
                card,
                art_h,
                selected,
                buf,
                theme,
            );
            slot_i = slot_i.saturating_add(1);
            hit.home_cards.push((card, si, ii));
            x += card_w + CARD_GAP;
        }
        y += card_h + 2;
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_card(
    item: &ShelfItem,
    covers: &HashMap<String, Cover>,
    renderer: &CoverRenderer,
    graphics: bool,
    blocked: &[Rect],
    slot: CoverSlot,
    r: Rect,
    art_h: u16,
    selected: bool,
    buf: &mut Buffer,
    theme: &Theme,
) {
    let (title, sub, glyph) = item_labels(item);
    if selected {
        fill(
            buf,
            Rect {
                x: r.x.saturating_sub(1),
                width: r.width + 2,
                ..r
            },
            Style::default().bg(theme.bg_selected),
        );
    }
    let art = Rect {
        height: art_h.min(r.height.saturating_sub(2)),
        ..r
    };
    if art.height > 0 {
        let key = item.cover_key();
        let cover = covers.get(key);
        let has_pixels = cover.and_then(|c| c.pixels.as_ref()).is_some();
        if has_pixels {
            let fitted = renderer.fit(art);
            let gfx = graphics && !blocked.iter().any(|b| b.intersects(fitted));
            renderer.render(slot, Some(key), cover, art, buf, theme, gfx);
        } else {
            let tint = tint_for(&title, theme);
            fill(buf, art, Style::default().bg(tint));
            let gx = art.x + art.width.saturating_sub(glyph.width() as u16) / 2;
            let gy = art.y + art.height / 2;
            buf.set_string(gx, gy, glyph, Style::default().fg(theme.dim).bg(tint));
        }
    }
    let ty = art.y + art.height;
    let bg = if selected {
        Style::default().bg(theme.bg_selected)
    } else {
        Style::default()
    };
    if ty < r.y + r.height {
        let style = if selected { theme.heading() } else { theme.normal() };
        buf.set_stringn(r.x, ty, truncate(&title, r.width as usize), r.width as usize, style.patch(bg));
    }
    if ty + 1 < r.y + r.height {
        buf.set_stringn(r.x, ty + 1, truncate(&sub, r.width as usize), r.width as usize, theme.dim().patch(bg));
    }
}

/// Deterministic dark tint per item so cards are distinguishable without artwork.
fn tint_for(s: &str, theme: &Theme) -> Color {
    let h = s.bytes().fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619));
    let hue = (h % 360) as f32 / 360.0;
    let (r, g, b) = hsl(hue, 0.35, 0.22);
    theme.color(r, g, b)
}

fn hsl(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let f = |mut t: f32| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v * 255.0).round() as u8
    };
    (f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0))
}

fn item_labels(item: &ShelfItem) -> (String, String, &'static str) {
    match item {
        ShelfItem::Track(t) => (t.title.clone(), t.artist_names(), "♫"),
        ShelfItem::Album(a) => (
            a.title.clone(),
            a.artists
                .iter()
                .map(|x| x.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            "◉",
        ),
        ShelfItem::Playlist(p) => (p.title.clone(), p.author.clone().unwrap_or_default(), "≡"),
        ShelfItem::Artist(a) => (a.name.clone(), "Artist".into(), "☺"),
    }
}
