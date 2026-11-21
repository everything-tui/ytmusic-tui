use crate::api::model::{Lyrics, Track};
use crate::player::queue::RepeatMode;
use crate::player::PlaybackState;
use crate::ui::components::{fill, pad, truncate};
use crate::ui::cover::{CoverCtx, CoverSlot};
use crate::ui::theme::Theme;
use crate::ui::views::lyrics as lyrics_view;
use crate::ui::widgets::player_parts::{
    render_controls, render_progress, render_volume, spinner, ControlsState,
};
use crate::ui::widgets::visualizer_widget::render_visualizer;
use crate::ui::HitMap;
use crate::visualizer::Visualizer;
use ratatui::prelude::*;
use std::time::Duration;

pub struct FullscreenNowPlaying<'a> {
    pub track: Option<&'a Track>,
    pub state: PlaybackState,
    pub position: Duration,
    pub duration: Option<Duration>,
    pub volume: f32,
    pub muted: bool,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub liked: bool,
    pub lyrics: Option<&'a Lyrics>,
    pub cover: CoverCtx<'a>,
    pub visualizer: &'a Visualizer,
    pub theme: &'a Theme,
}

impl<'a> FullscreenNowPlaying<'a> {
    pub fn render(self, area: Rect, buf: &mut Buffer, hit: &mut HitMap) {
        let theme = self.theme;
        fill(buf, area, Style::default().bg(theme.bg));
        hit.player = Some(area);
        let inner = pad(area, 4, 1);
        if inner.height < 10 || inner.width < 30 {
            return;
        }

        let tall = inner.height >= 30;
        let viz_h = if tall { 5 } else { 3 };
        // Controls sit between the visualizer and the progress bar, big enough for image icons.
        let ctrl_h = if tall { 3 } else { 2 };
        let rows = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Length(viz_h),
            Constraint::Length(1),
            Constraint::Length(ctrl_h),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);
        let top = rows[0];

        let art_w = self
            .cover
            .width_for_rows(top.height)
            .min(top.width * 45 / 100);
        let art = self.cover.draw(
            CoverSlot::Fullscreen,
            Rect {
                width: art_w,
                ..top
            },
            buf,
            theme,
        );

        let info_x = art.x + art.width + 4;
        let info = Rect {
            x: info_x,
            y: top.y,
            width: (top.x + top.width).saturating_sub(info_x),
            height: top.height,
        };
        if info.width >= 12 {
            let w = info.width as usize;
            let label = match self.state {
                PlaybackState::Playing => "NOW PLAYING".to_string(),
                PlaybackState::Paused => "PAUSED".to_string(),
                PlaybackState::Loading | PlaybackState::Buffering => {
                    format!("{} LOADING", spinner())
                }
                PlaybackState::Stopped => "IDLE".to_string(),
            };
            buf.set_stringn(info.x, info.y, &label, w, theme.subheading());
            match self.track {
                Some(t) => {
                    buf.set_stringn(
                        info.x,
                        info.y + 2,
                        truncate(&t.title, w),
                        w,
                        theme.heading(),
                    );
                    buf.set_stringn(
                        info.x,
                        info.y + 3,
                        truncate(&t.artist_names(), w),
                        w,
                        theme.accent(),
                    );
                    if let Some(a) = &t.album {
                        buf.set_stringn(info.x, info.y + 4, truncate(&a.name, w), w, theme.dim());
                    }
                }
                None => {
                    buf.set_stringn(info.x, info.y + 2, "Nothing playing", w, theme.heading());
                    buf.set_stringn(info.x, info.y + 3, "Press / to search", w, theme.dim());
                }
            }
            if info.height > 8 {
                let lyr = Rect {
                    y: info.y + 6,
                    height: info.height - 6,
                    ..info
                };
                lyrics_view::render(self.lyrics, self.position, lyr, buf, theme);
            }
        }

        render_visualizer(self.visualizer, rows[2], buf, theme, true);

        let dur = self
            .duration
            .or(self.track.and_then(|t| t.duration))
            .unwrap_or_default();
        hit.progress = Some(render_progress(
            buf,
            rows[6],
            self.position,
            dur,
            self.state,
            theme,
        ));
        hit.duration = dur;

        let ctrl_row = rows[4];
        let c = ControlsState {
            state: self.state,
            shuffle: self.shuffle,
            repeat: self.repeat,
            liked: self.liked,
        };
        let buttons = render_controls(buf, ctrl_row, &c, theme, Some(&self.cover), ctrl_h);
        let row_end = ctrl_row.x + ctrl_row.width;
        let buttons_end = buttons
            .iter()
            .map(|(r, _)| r.x + r.width)
            .max()
            .unwrap_or(ctrl_row.x);
        hit.buttons.extend(buttons);
        // Volume lines up with the right end of the progress bar, centred on the controls,
        // and takes about a third of the row without running into the buttons.
        let free = row_end.saturating_sub(buttons_end + 4);
        let vol_w = (ctrl_row.width / 3).clamp(24, 56).min(free);
        if vol_w >= 16 {
            let vol = Rect {
                x: row_end - vol_w,
                y: ctrl_row.y + ctrl_row.height / 2,
                width: vol_w,
                height: 1,
            };
            hit.volume = Some(render_volume(buf, vol, self.volume, self.muted, theme));
        }
    }
}
