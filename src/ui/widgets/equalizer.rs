use crate::player::eq::{BAND_COUNT, BAND_FREQS_HZ, BAND_LABELS, GAIN_MAX_DB, PRESETS};
use crate::player::EqParams;
use crate::ui::components::{modal, truncate};
use crate::ui::theme::Theme;
use ratatui::prelude::*;
use unicode_width::UnicodeWidthStr;

pub struct EqualizerOverlay<'a> {
    pub theme: &'a Theme,
    pub params: EqParams,
    pub selected: usize,
}

impl<'a> Widget for EqualizerOverlay<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let theme = self.theme;
        // Wide enough for 10 slim columns + preset row.
        let width = (area.width.saturating_mul(85) / 100).clamp(56, 88);
        let height = (area.height.saturating_mul(70) / 100).clamp(16, 22);
        let status = if self.params.enabled { "on" } else { "off" };
        let title = format!("equalizer ({status})");
        let inner = modal(
            buf,
            area,
            width,
            height,
            &title,
            "esc close · space toggle",
            theme,
        );
        if inner.width < 24 || inner.height < 8 {
            return;
        }

        let bg = Style::default().bg(theme.bg_modal);
        let selected = self.selected.min(BAND_COUNT - 1);

        // Preset row
        let mut preset_x = inner.x;
        let preset_y = inner.y;
        for (i, p) in PRESETS.iter().enumerate() {
            let label = format!("[{}]{}", i + 1, p.name);
            let active = self.params.gains_db == p.gains_db;
            let style = if active {
                theme.accent_bold().patch(bg)
            } else {
                theme.dim().patch(bg)
            };
            let w = label.width() as u16;
            if preset_x + w >= inner.x + inner.width {
                break;
            }
            buf.set_string(preset_x, preset_y, &label, style);
            preset_x += w + 2;
        }

        // Slider area
        let slider_top = inner.y + 2;
        let slider_bottom = inner.y + inner.height.saturating_sub(3);
        let slider_h = slider_bottom.saturating_sub(slider_top).max(1);
        let band_w = (inner.width / BAND_COUNT as u16).max(4);
        let zero_row = slider_top + (slider_h / 2);

        for bi in 0..BAND_COUNT {
            let x = inner.x + bi as u16 * band_w;
            let col_w = band_w.saturating_sub(1).max(2);
            let gain = self.params.gains_db[bi];
            let is_sel = bi == selected;

            let zero_ch = if is_sel { "─" } else { "·" };
            buf.set_stringn(
                x,
                zero_row,
                zero_ch.repeat(col_w as usize),
                col_w as usize,
                theme.muted().patch(bg),
            );

            let gain_norm = ((gain / GAIN_MAX_DB).clamp(-1.0, 1.0) + 1.0) / 2.0;
            let tip_row =
                slider_bottom.saturating_sub((gain_norm * (slider_h as f32 - 1.0)) as u16);
            let (from, to) = if tip_row <= zero_row {
                (tip_row, zero_row)
            } else {
                (zero_row, tip_row)
            };
            let bar_style = if is_sel {
                theme.accent().patch(bg)
            } else {
                Style::default().fg(theme.text).patch(bg)
            };
            for y in from..=to {
                if y == zero_row {
                    continue;
                }
                let ch = if is_sel { "█" } else { "▓" };
                buf.set_stringn(x + col_w / 2, y, ch, 1, bar_style);
            }
            buf.set_stringn(
                x + col_w / 2,
                tip_row,
                if is_sel { "●" } else { "○" },
                1,
                bar_style,
            );

            let label_y = slider_bottom.saturating_add(1).min(inner.y + inner.height - 1);
            let label = BAND_LABELS[bi];
            let db = format!("{gain:+.0}");
            let style = if is_sel {
                theme.accent_bold().patch(bg)
            } else {
                theme.dim().patch(bg)
            };
            buf.set_stringn(
                x,
                label_y,
                truncate(label, col_w as usize),
                col_w as usize,
                style,
            );
            if label_y + 1 < inner.y + inner.height {
                // Show dB; freq is already the column label.
                let _ = BAND_FREQS_HZ[bi];
                buf.set_stringn(
                    x,
                    label_y + 1,
                    truncate(&db, col_w as usize),
                    col_w as usize,
                    theme.muted().patch(bg),
                );
            }
        }

        let hint_y = inner.y + inner.height.saturating_sub(1);
        let hints = "←/→ band  ↑/↓ ±1dB  0 reset  r flat  1 Flat 2 Bass 3 Elec 4 Vocal 5 Rock";
        buf.set_stringn(
            inner.x,
            hint_y,
            truncate(hints, inner.width as usize),
            inner.width as usize,
            theme.muted().patch(bg),
        );
    }
}
