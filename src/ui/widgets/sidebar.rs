use crate::action::Action;
use crate::api::model::Track;
use crate::ui::components::{fill, truncate};
use crate::ui::cover::{CoverCtx, CoverSlot};
use crate::ui::glyphs::{glyph, Glyph};
use crate::ui::theme::Theme;
use crate::ui::views::library::SECTIONS;
use crate::ui::widgets::visualizer_widget::render_visualizer;
use crate::ui::HitMap;
use crate::visualizer::Visualizer;
use ratatui::prelude::*;
use unicode_width::UnicodeWidthStr;

pub const NAV: [(Glyph, &str); 5] = [
    (Glyph::Home, "Home"),
    (Glyph::Search, "Search"),
    (Glyph::Library, "Library"),
    (Glyph::Queue, "Queue"),
    (Glyph::Lyrics, "Lyrics"),
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
    /// Keyboard focus is on the menu: the active pill is drawn in full accent.
    pub focused: bool,
    pub library_section: Option<usize>,
    pub account: Option<&'a str>,
    pub signed_in: bool,
    /// Music Premium, once known.
    pub premium: Option<bool>,
    pub mcp_enabled: bool,
    pub track: Option<&'a Track>,
    pub cover: CoverCtx<'a>,
    pub visualizer: &'a Visualizer,
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
        buf.set_string(area.x + 1, area.y + 1, " ▶ ", theme.on_accent());
        for (i, (g, _)) in NAV.iter().enumerate() {
            let y = area.y + 3 + i as u16 * 2;
            if y >= area.y + area.height {
                break;
            }
            let r = Rect::new(area.x, y, area.width, 1);
            let active = self.active_tab == Some(i);
            let style = if active {
                let pill = self.pill_bg();
                fill(buf, Rect::new(area.x + 1, y, 3, 1), Style::default().bg(pill));
                theme.accent_bold().bg(pill)
            } else {
                theme.muted().patch(bg)
            };
            // Without icon glyphs the rail shows the tab's number key instead.
            let key = (i + 1).to_string();
            let mark = match glyph(theme, *g) {
                "" => key.as_str(),
                icon => icon,
            };
            buf.set_string(r.x + 2, y, mark, style);
            hit.tabs.push((r, i));
        }
        let help_y = area.y + area.height - 1;
        buf.set_string(area.x + 2, help_y, "?", theme.faint().patch(bg));
    }

    /// Background of the active nav pill: a soft accent tint, stronger with keyboard focus.
    fn pill_bg(&self) -> Color {
        let theme = self.theme;
        // Accents read much louder on light backgrounds, so tint them half as strongly.
        let strength = if self.focused { 0.28 } else { 0.15 };
        theme.tint(theme.bg_hover, if theme.light { strength / 2.0 } else { strength })
    }

    fn render_full(&self, area: Rect, buf: &mut Buffer, hit: &mut HitMap) {
        let theme = self.theme;
        let bg = Style::default().bg(theme.bg_sidebar);
        // Text column; the pill extends one cell either side of it.
        let x = area.x + 3;
        let w = area.width.saturating_sub(6);
        let bottom = area.y + area.height;
        let key_x = x + w - 1;
        let icons = theme.nerd_icons;
        let label_x = if icons { x + 3 } else { x };
        let label_w = key_x.saturating_sub(label_x + 1) as usize;

        // Brand: a small accent "app icon" chip next to the name.
        buf.set_line(
            area.x + 2,
            area.y + 1,
            &Line::from(vec![
                Span::styled(" ▶ ", theme.on_accent()),
                Span::styled(" yt-tui", theme.heading()),
                Span::styled(" music", theme.muted()),
            ])
            .patch_style(bg),
            area.width.saturating_sub(3),
        );

        // Nav in two groups; the active entry is a tinted pill with an accent edge.
        const GROUPS: [(&str, std::ops::Range<usize>); 2] =
            [("Discover", 0..3), ("Playing", 3..5)];
        let mut y = area.y + 4;
        'groups: for (title, range) in GROUPS {
            if y + 1 >= bottom {
                break;
            }
            buf.set_string(x, y, title, theme.muted().patch(bg));
            y += 1;
            for i in range {
                if y >= bottom {
                    break 'groups;
                }
                let (g, label) = NAV[i];
                let icon = glyph(theme, g);
                let r = Rect::new(area.x, y, area.width, 1);
                let active = self.active_tab == Some(i);
                let key = format!("{}", i + 1);
                if active {
                    let pill_bg = self.pill_bg();
                    let st = Style::default().bg(pill_bg);
                    fill(buf, Rect::new(area.x + 1, y, area.width.saturating_sub(2), 1), st);
                    buf.set_string(area.x + 1, y, "▎", theme.accent().patch(st));
                    buf.set_string(x, y, icon, theme.accent_bold().patch(st));
                    buf.set_stringn(label_x, y, label, label_w, theme.heading().patch(st));
                    buf.set_string(key_x, y, &key, theme.dim().patch(st));
                } else {
                    buf.set_string(x, y, icon, theme.muted().patch(bg));
                    buf.set_stringn(label_x, y, label, label_w, theme.dim().patch(bg));
                    buf.set_string(key_x, y, &key, theme.faint().patch(bg));
                }
                hit.tabs.push((r, i));
                y += 1;

                if i == 2 {
                    if let Some(sec) = self.library_section {
                        for (si, name) in SECTIONS.iter().enumerate() {
                            if y >= bottom {
                                break 'groups;
                            }
                            let r = Rect::new(area.x, y, area.width, 1);
                            let sel = si == sec;
                            // Tree guide under the Library icon; the selected branch lights up.
                            let (guide, gs, ls) = if sel {
                                ("┃", theme.accent(), theme.normal())
                            } else {
                                ("│", theme.faint(), theme.muted())
                            };
                            buf.set_string(x, y, guide, gs.patch(bg));
                            let sub_x = if icons { x + 3 } else { x + 2 };
                            buf.set_stringn(
                                sub_x,
                                y,
                                name,
                                (x + w).saturating_sub(sub_x) as usize,
                                ls.patch(bg),
                            );
                            hit.buttons.push((r, Action::LibrarySection(si)));
                            y += 1;
                        }
                    }
                }
            }
            y += 1;
        }
        let nav_end = y;

        // Footer: account, MCP status and help hint, separated by space rather than a rule.
        let account_y = bottom.saturating_sub(4);
        let footer_top = account_y.saturating_sub(1);
        if account_y >= nav_end {
            let avatar = theme.accent_bold().bg(theme.tint(theme.bg_hover, 0.28));
            let (initial, chip, name, name_style) = match self.account {
                Some(n) => (
                    initial_of(n),
                    avatar,
                    n.to_string(),
                    theme.normal(),
                ),
                None if self.signed_in => (
                    '●',
                    avatar,
                    "Signed in".into(),
                    theme.normal(),
                ),
                None => (
                    '?',
                    Style::default().fg(theme.dim).bg(theme.bg_selected),
                    "Guest · L to sign in".into(),
                    theme.dim(),
                ),
            };
            buf.set_string(x - 1, account_y, format!(" {initial} "), chip);
            let room = w.saturating_sub(3) as usize;
            buf.set_stringn(
                x + 3,
                account_y,
                truncate(&name, room),
                room,
                name_style.patch(bg),
            );
            // Tier as a subtitle under the name, in the spacer row above the MCP status.
            let tier = match self.premium.filter(|_| self.signed_in) {
                Some(true) => Some(("Premium", theme.accent())),
                Some(false) => Some(("Free", theme.faint())),
                None => None,
            };
            if let Some((tier, style)) = tier {
                buf.set_stringn(x + 3, account_y + 1, tier, room, style.patch(bg));
            }
            hit.buttons.push((Rect::new(area.x, account_y, area.width, 1), Action::AccountMenu));
        }
        let status_y = bottom.saturating_sub(2);
        if status_y >= nav_end {
            let (dot, dot_style, label) = if self.mcp_enabled {
                ("●", Style::default().fg(theme.success), "MCP on")
            } else {
                ("○", theme.faint(), "MCP off")
            };
            buf.set_line(
                x,
                status_y,
                &Line::from(vec![
                    Span::styled(dot, dot_style),
                    Span::styled(format!(" {label}"), theme.muted()),
                ])
                .patch_style(bg),
                w,
            );
            let mcp_w = (label.width() as u16 + 2).min(w);
            hit.buttons.push((Rect::new(area.x, status_y, x - area.x + mcp_w, 1), Action::ToggleMcp));
            // "? help" when it fits beside the status, a lone "?" otherwise.
            let help: &[(&str, Style)] = if w >= mcp_w + 8 {
                &[("?", theme.dim()), (" help", theme.faint())]
            } else {
                &[("?", theme.faint())]
            };
            let hw: u16 = help.iter().map(|(t, _)| t.width() as u16).sum();
            if w >= mcp_w + hw + 2 {
                let spans: Vec<Span> = help.iter().map(|(t, st)| Span::styled(*t, *st)).collect();
                buf.set_line(x + w - hw, status_y, &Line::from(spans).patch_style(bg), hw);
            }
        }

        // Cover panel between nav and footer, only when it fits comfortably.
        let Some(track) = self.track else { return };
        let side = w;
        let cover_rows = self.cover.renderer.fit(Rect::new(0, 0, side, 200)).height;
        // cover, gap, title, artist, gap before footer
        let need = cover_rows + 4;
        if footer_top < nav_end + 2 || footer_top - nav_end - 1 < need {
            return;
        }
        let top = footer_top - need;
        // Visualizer sits directly above the cover when there is room for it.
        const VIZ_H: u16 = 3;
        let room = footer_top - nav_end - 1;
        if room >= need + VIZ_H + 1 {
            let viz = Rect::new(x, top - VIZ_H - 1, side, VIZ_H);
            render_visualizer(self.visualizer, viz, buf, theme, true);
        } else if room >= need + 2 {
            let viz = Rect::new(x, top - 2, side, 1);
            render_visualizer(self.visualizer, viz, buf, theme, false);
        }
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

/// Uppercase first letter or digit of an account name, for the avatar chip.
fn initial_of(name: &str) -> char {
    name.chars()
        .find(|c| c.is_alphanumeric())
        .and_then(|c| c.to_uppercase().next())
        .unwrap_or('●')
}
