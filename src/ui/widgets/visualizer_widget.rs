use crate::ui::theme::Theme;
use crate::visualizer::Visualizer;
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

/// Block glyphs that fill a cell from the bottom up (index 0 = empty).
const BLOCKS: &[char] = &[' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

pub fn render_visualizer(
    viz: &Visualizer,
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    full_height: bool,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    if full_height && area.height > 1 {
        render_bars(viz, area, buf, theme);
    } else {
        let line = viz.render_line(area.width as usize);
        Paragraph::new(line)
            .style(theme.accent())
            .render(area, buf);
    }
}

/// Solid column bars: lower rows are full █ whenever the level reaches them;
/// only the tip row may be a partial block. (The old threshold math left gaps.)
fn render_bars(viz: &Visualizer, area: Rect, buf: &mut Buffer, theme: &Theme) {
    let levels = viz.band_levels();
    let h = area.height as f32;
    for row in 0..area.height {
        // row 0 = top of the widget
        let row_from_bottom = (area.height - 1 - row) as f32;
        let low = row_from_bottom / h;
        let high = (row_from_bottom + 1.0) / h;
        let style = if row < area.height / 3 {
            theme.accent()
        } else {
            theme.accent_bold()
        };
        for col in 0..area.width as usize {
            let bi = col * levels.len() / area.width as usize;
            let v = levels.get(bi).copied().unwrap_or(0.0).clamp(0.0, 1.0);
            let ch = if v >= high {
                '█'
            } else if v <= low {
                // Quiet baseline on the bottom row only.
                if row + 1 == area.height {
                    '▁'
                } else {
                    ' '
                }
            } else {
                let frac = ((v - low) / (high - low)).clamp(0.0, 1.0);
                let idx = (frac * 8.0).ceil().clamp(1.0, 8.0) as usize;
                BLOCKS[idx]
            };
            let fg = if ch == '▁' && v <= low {
                theme.border
            } else {
                style.fg.unwrap_or(theme.accent)
            };
            if let Some(cell) = buf.cell_mut((area.x + col as u16, area.y + row)) {
                cell.set_symbol(&ch.to_string()).set_fg(fg);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tall_bar_has_no_floating_gap() {
        let h = 4u16;
        let v = 0.55f32;
        let mut col = Vec::new();
        for row in 0..h {
            let row_from_bottom = (h - 1 - row) as f32;
            let low = row_from_bottom / h as f32;
            let high = (row_from_bottom + 1.0) / h as f32;
            let ch = if v >= high {
                '█'
            } else if v <= low {
                ' '
            } else {
                let frac = (v - low) / (high - low);
                let idx = (frac * 8.0).ceil().clamp(1.0, 8.0) as usize;
                BLOCKS[idx]
            };
            col.push(ch);
        }
        // Top → bottom: empty, tip, then solid fills below — never a gap.
        assert_eq!(col[0], ' ', "top above the tip should be empty");
        assert_ne!(col[1], ' ', "tip row should draw");
        assert_eq!(col[2], '█', "rows below the tip must be solid");
        assert_eq!(col[3], '█', "baseline row must be solid");
    }
}
