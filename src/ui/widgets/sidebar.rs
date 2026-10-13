use crate::action::Action;
use crate::api::model::Track;
use crate::ui::components::{fill, truncate};
use crate::ui::cover::{CoverCtx, CoverSlot};
use crate::ui::theme::Theme;
use crate::ui::views::library::SECTIONS;
use crate::ui::HitMap;
use ratatui::prelude::*;
use unicode_width::UnicodeWidthStr;

pub const NAV: [(&str, &str); 5] = [
    ("⌂", "Home"),
    ("⌕", "Search"),
    ("♫", "Library"),
    ("≡", "Queue"),
    ("✎", "Lyrics"),
];

/// Full sidebar at or above this width; an icon rail below it.
pub const FULL_MIN_TERM_W: u16 = 80;
pub const RAIL_MIN_TERM_W: u16 = 50;
pub const RAIL_W: u16 = 5;

pub fn width_for(term_w: u16) -> u16 {
    if term_w >= FULL_MIN_TERM_W {
        (term_w / 6).clamp(22, 28)
    } else if term_w >= RAIL_MIN_TERM_W {
        RAIL_W
    } else {
        0
    }
}

pub struct Sidebar<'a> {
    /// `None` while a detail page or fullscreen view hides the tab highlight.
    pub active_tab: Option<usize>,
    pub library_section: Option<usize>,
    pub account: Option<&'a str>,
    pub signed_in: bool,
    pub track: Option<&'a Track>,
    pub cover: CoverCtx<'a>,
    pub theme: &'a Theme,
}

impl<'a> Sidebar<'a> {
    pub fn render(self, area: Rect, buf: &mut Buffer, hit: &mut HitMap) {
        let theme = self.theme;
        fill(buf, area, Style::default().bg(theme.bg_sidebar));
        hit.inert.push(area);
        if area.width == 0 || area.height < 8 {
            return;
        }
        if area.width <= RAIL_W {
            self.render_rail(area, buf, hit);
        } else {
            self.render_full(area, buf, hit);
        }
    }

    fn render_rail(&self, area: Rect, buf: &mut Buffer, hit: &mut HitMap) {
        let theme = self.theme;
        let bg = Style::default().bg(theme.bg_sidebar);
        buf.set_string(area.x + 2, area.y + 1, "▶", theme.accent_bold().patch(bg));
        for (i, (icon, _)) in NAV.iter().enumerate() {
            let y = area.y + 3 + i as u16 * 2;
            if y >= area.y + area.height {
                break;
            }
            let r = Rect::new(area.x, y, area.width, 1);
            let active = self.active_tab == Some(i);
            if active {
                fill(buf, r, Style::default().bg(theme.bg_selected));
                buf.set_string(r.x, y, "▎", theme.accent().bg(theme.bg_selected));
            }
            let style = if active {
                theme.heading().bg(theme.bg_selected)
            } else {
                theme.dim().patch(bg)
            };
            buf.set_string(r.x + 2, y, icon, style);
            hit.tabs.push((r, i));
        }
        let help_y = area.y + area.height - 1;
        buf.set_string(area.x + 2, help_y, "?", theme.muted().patch(bg));
    }

    fn render_full(&self, area: Rect, buf: &mut Buffer, hit: &mut HitMap) {
        let theme = self.theme;
        let bg = Style::default().bg(theme.bg_sidebar);
        let x = area.x + 2;
        let w = area.width.saturating_sub(3);
        let bottom = area.y + area.height;

        buf.set_line(
            x,
            area.y + 1,
            &Line::from(vec![
                Span::styled("▶ ", theme.accent_bold()),
                Span::styled("yt-tui", theme.heading()),
            ])
            .patch_style(bg),
            w,
        );

        let mut y = area.y + 3;
        for (i, (icon, label)) in NAV.iter().enumerate() {
            if y >= bottom {
                break;
            }
            let r = Rect::new(area.x, y, area.width, 1);
            let active = self.active_tab == Some(i);
            let row_bg = if active {
                fill(buf, r, Style::default().bg(theme.bg_selected));
                buf.set_string(r.x, y, "▎", theme.accent().bg(theme.bg_selected));
                Style::default().bg(theme.bg_selected)
            } else {
                bg
            };
            let style = if active { theme.heading() } else { theme.dim() }.patch(row_bg);
            buf.set_string(x, y, icon, if active { theme.accent_bold().patch(row_bg) } else { style });
            buf.set_stringn(x + 3, y, label, w.saturating_sub(6) as usize, style);
            let key = format!("{}", i + 1);
            buf.set_string(area.x + area.width - 2, y, &key, theme.muted().patch(row_bg));
            hit.tabs.push((r, i));
            y += 1;

            if i == 2 {
                if let Some(sec) = self.library_section {
                    for (si, name) in SECTIONS.iter().enumerate() {
                        if y >= bottom {
                            break;
                        }
                        let r = Rect::new(area.x, y, area.width, 1);
                        let style = if si == sec {
                            theme.accent_bold()
                        } else {
                            theme.dim()
                        }
                        .patch(bg);
                        let mark = if si == sec { "•" } else { " " };
                        buf.set_stringn(
                            x + 3,
                            y,
                            format!("{mark} {name}"),
                            w.saturating_sub(4) as usize,
                            style,
                        );
                        hit.buttons.push((r, Action::LibrarySection(si)));
                        y += 1;
                    }
                }
            }
        }
        let nav_end = y;

        // Footer: account + help hint.
        let mut footer: Vec<(String, Style, Option<Action>)> = Vec::new();
        footer.push(("? help · ^P commands".into(), theme.muted(), None));
        let (account, account_style) = match self.account {
            Some(n) => (format!("● {n}"), Style::default().fg(theme.success)),
            None if self.signed_in => ("● Signed in".into(), Style::default().fg(theme.success)),
            None => ("○ Guest · L to sign in".into(), theme.dim()),
        };
        footer.push((account, account_style, Some(Action::AccountMenu)));
        let footer_h = footer.len() as u16;
        let footer_top = bottom.saturating_sub(footer_h + 1);
        for (i, (text, style, action)) in footer.iter().rev().enumerate() {
            let fy = bottom - 2 - i as u16;
            if fy < nav_end {
                break;
            }
            buf.set_stringn(x, fy, truncate(text, w as usize), w as usize, style.patch(bg));
            if let Some(a) = action {
                hit.buttons.push((Rect::new(area.x, fy, area.width, 1), a.clone()));
            }
        }

        // Cover panel between nav and footer, only when it fits comfortably.
        let Some(track) = self.track else { return };
        let side = area.width.saturating_sub(4);
        let cover_rows = self.cover.renderer.fit(Rect::new(0, 0, side, 200)).height;
        // cover, gap, title, artist, gap before footer
        let need = cover_rows + 4;
        if footer_top < nav_end + 2 || footer_top - nav_end - 1 < need {
            return;
        }
        let top = footer_top - need;
        let cover_area = Rect::new(x, top, side, cover_rows);
        let r = self.cover.draw(CoverSlot::Sidebar, cover_area, buf, theme);
        hit.buttons.push((r, Action::ToggleNowPlaying));
        let ty = r.y + r.height + 1;
        let title = truncate(&track.title, w as usize);
        buf.set_string(x, ty, &title, theme.heading().patch(bg));
        let artist = truncate(&track.artist_names(), w as usize);
        if artist.width() > 0 {
            buf.set_string(x, ty + 1, &artist, theme.dim().patch(bg));
        }
    }
}
