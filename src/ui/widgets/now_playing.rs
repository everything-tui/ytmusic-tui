use crate::api::model::Track;
use crate::player::queue::RepeatMode;
use crate::player::PlaybackState;
use crate::ui::components::{fill, truncate};
use crate::ui::cover::{CoverCtx, CoverSlot};
use crate::ui::marquee::Marquee;
use crate::ui::theme::Theme;
use crate::ui::widgets::player_parts::{
    render_controls, render_progress, render_volume, spinner, ControlsState,
};
use crate::ui::widgets::visualizer_widget::render_visualizer;
use crate::ui::HitMap;
use crate::visualizer::Visualizer;
use ratatui::prelude::*;
use std::time::Duration;
use unicode_width::UnicodeWidthStr;

/// Height the bar wants for a given terminal height.
pub fn preferred_height(term_height: u16) -> u16 {
    if term_height >= 24 {
        5
    } else {
        4
    }
}

pub struct NowPlayingBar<'a> {
    pub track: Option<&'a Track>,
    pub state: PlaybackState,
    pub position: Duration,
    pub duration: Option<Duration>,
    pub volume: f32,
    pub muted: bool,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub liked: bool,
    pub cover: CoverCtx<'a>,
    pub visualizer: &'a Visualizer,
    pub marquee: &'a Marquee,
    pub theme: &'a Theme,
}

impl<'a> NowPlayingBar<'a> {
    pub fn render(self, area: Rect, buf: &mut Buffer, hit: &mut HitMap) {
        let theme = self.theme;
        fill(buf, area, Style::default().bg(theme.bg_player));
        hit.player = Some(area);
        if area.height < 2 || area.width < 30 {
            return;
        }

        // Three content rows, vertically centered.
        let rows_h = area.height.min(3);
        let content = Rect {
            x: area.x + 1,
            y: area.y + (area.height - rows_h) / 2,
            width: area.width - 2,
            height: rows_h,
        };
        let w = content.width;
        let right_w = if w >= 70 { (w * 28 / 100).clamp(20, 40) } else { 0 };
        let left_w = (w * 30 / 100).clamp(16, 48);
        let cols = Layout::horizontal([
            Constraint::Length(left_w),
            Constraint::Fill(1),
            Constraint::Length(right_w),
        ])
        .spacing(2)
        .split(content);

        self.render_track_info(cols[0], buf);

        // Center: controls, then progress.
        let center = cols[1];
        let dur = self
            .duration
            .or(self.track.and_then(|t| t.duration))
            .unwrap_or_default();
        let c = ControlsState {
            state: self.state,
            shuffle: self.shuffle,
            repeat: self.repeat,
            liked: self.liked,
        };
        hit.buttons
            .extend(render_controls(buf, Rect { height: 1, ..center }, &c, theme));
        let prog_w = center.width.min(72);
        let prog_y = if center.height >= 3 { center.y + 2 } else { center.y + 1 };
        if prog_y < center.y + center.height {
            let prog = Rect {
                x: center.x + (center.width - prog_w) / 2,
                y: prog_y,
                width: prog_w,
                height: 1,
            };
            hit.progress = Some(render_progress(buf, prog, self.position, dur, self.state, theme));
            hit.duration = dur;
        }

        // Right: visualizer above volume.
        if right_w > 0 {
            let right = cols[2];
            let vol_row = Rect {
                y: right.y + right.height - 1,
                height: 1,
                ..right
            };
            if right.height >= 2 {
                let viz = Rect {
                    height: right.height - 1,
                    ..right
                };
                render_visualizer(self.visualizer, viz, buf, theme, viz.height > 1);
            }
            hit.volume = Some(render_volume(buf, vol_row, self.volume, self.muted, theme));
        }
    }

    fn render_track_info(&self, area: Rect, buf: &mut Buffer) {
        let theme = self.theme;
        let art_w = self.cover.width_for_rows(area.height);
        let show_art = area.width >= art_w + 12;
        let text_x = if show_art {
            self.cover.draw(
                CoverSlot::Player,
                Rect {
                    width: art_w,
                    ..area
                },
                buf,
                theme,
            );
            area.x + art_w + 2
        } else {
            area.x
        };
        let text_w = (area.x + area.width).saturating_sub(text_x) as usize;
        if text_w < 4 {
            return;
        }
        let line_y = |i: u16| area.y + i.min(area.height.saturating_sub(1));
        let top = 0;

        let Some(track) = self.track else {
            buf.set_stringn(text_x, line_y(top), "Nothing playing", text_w, theme.dim());
            if area.height >= 2 {
                buf.set_stringn(text_x, line_y(top + 1), "Press / to search", text_w, theme.muted());
            }
            return;
        };

        let loading = matches!(self.state, PlaybackState::Loading | PlaybackState::Buffering);
        let lead = if loading { format!("{} ", spinner()) } else { String::new() };
        let title_w = text_w.saturating_sub(lead.width());
        let title = self.marquee.render(title_w);
        let title = if title.is_empty() {
            truncate(&track.title, title_w)
        } else {
            title
        };
        buf.set_line(
            text_x,
            line_y(top),
            &Line::from(vec![
                Span::styled(lead, theme.accent()),
                Span::styled(title, theme.heading()),
            ]),
            text_w as u16,
        );
        if area.height >= 2 {
            buf.set_stringn(
                text_x,
                line_y(top + 1),
                truncate(&track.artist_names(), text_w),
                text_w,
                theme.dim(),
            );
        }
        if area.height >= 3 {
            if let Some(album) = &track.album {
                buf.set_stringn(
                    text_x,
                    line_y(top + 2),
                    truncate(&album.name, text_w),
                    text_w,
                    theme.muted(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::model::{AlbumRef, ArtistRef};
    use crate::art::{ArtPixels, Cover};
    use crate::ui::cover::CoverRenderer;
    use crate::visualizer::VisualizerStyle;
    use std::sync::Arc;

    fn dump(buf: &Buffer) -> String {
        let a = buf.area;
        let mut out = String::new();
        for y in 0..a.height {
            let mut skip = 0;
            for x in 0..a.width {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let s = buf[(x, y)].symbol();
                skip = unicode_width::UnicodeWidthStr::width(s).saturating_sub(1);
                out.push_str(s);
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn renders_player_bar() {
        let track = Track {
            video_id: "x".into(),
            title: "Blinding Lights".into(),
            artists: vec![ArtistRef { id: None, name: "The Weeknd".into() }],
            album: Some(AlbumRef { id: None, name: "After Hours".into() }),
            duration: Some(Duration::from_secs(200)),
            thumbnails: vec![],
            explicit: false,
            liked: Some(true),
            from_radio: false,
        };
        let theme = Theme::new("#ff0033", false);
        let viz = Visualizer::new(VisualizerStyle::from_config("bars"));
        let marquee = Marquee::new("Blinding Lights");
        let px = ArtPixels {
            size: ArtPixels::SIZE,
            rgb: (0..ArtPixels::SIZE * ArtPixels::SIZE * 3).map(|i| (i % 251) as u8).collect(),
        };
        let cover_data = Cover {
            pixels: Some(Arc::new(px)),
            image: None,
        };
        let renderer = CoverRenderer::halfblocks();
        let cover = CoverCtx {
            renderer: &renderer,
            key: Some("x"),
            cover: Some(&cover_data),
            graphics: true,
            blocked: &[],
        };

        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        let mut hit = HitMap::default();
        crate::ui::views::now_playing::FullscreenNowPlaying {
            track: Some(&track),
            state: PlaybackState::Paused,
            position: Duration::from_secs(73),
            duration: None,
            volume: 0.6,
            muted: false,
            shuffle: false,
            repeat: RepeatMode::One,
            liked: false,
            lyrics: None,
            cover,
            visualizer: &viz,
            theme: &theme,
        }
        .render(area, &mut buf, &mut hit);
        let out = dump(&buf);
        println!("{out}");
        assert_eq!(hit.buttons.len(), 6);
        assert!(out.contains("Blinding Lights"));

        for (w, h) in [(120u16, 5u16), (80, 4), (60, 4)] {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            let mut hit = HitMap::default();
            NowPlayingBar {
                track: Some(&track),
                state: PlaybackState::Playing,
                position: Duration::from_secs(73),
                duration: None,
                volume: 0.6,
                muted: false,
                shuffle: true,
                repeat: RepeatMode::All,
                liked: true,
                cover,
                visualizer: &viz,
                marquee: &marquee,
                theme: &theme,
            }
            .render(area, &mut buf, &mut hit);
            let out = dump(&buf);
            println!("{out}");
            assert!(hit.progress.is_some(), "progress missing at {w}x{h}");
            assert_eq!(hit.buttons.len(), 6, "controls missing at {w}x{h}");
            assert!(out.contains("Blinding"), "title missing at {w}x{h}");
            for g in ['│', '╭', '╮', '╰', '╯'] {
                assert!(!out.contains(g), "border glyph {g} at {w}x{h}");
            }
        }
    }
}
