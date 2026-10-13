use crate::ui::components::{modal, truncate};
use crate::ui::keymap::help_text;
use crate::ui::theme::Theme;
use ratatui::prelude::*;

pub struct HelpOverlay<'a> {
    pub theme: &'a Theme,
    pub scroll: u16,
}

enum HelpLine {
    Heading(String),
    Entry(String, String),
    Blank,
}

/// Splits `help_text()` into headed groups of `key   description` rows.
fn groups() -> Vec<Vec<HelpLine>> {
    help_text()
        .split("\n\n")
        .skip(1)
        .map(|block| {
            let mut lines = block.lines();
            let mut out = vec![HelpLine::Heading(lines.next().unwrap_or("").trim().to_string())];
            for l in lines {
                let l = l.trim();
                let (k, d) = match l.find("  ") {
                    Some(i) => (l[..i].trim(), l[i..].trim()),
                    None => (l, ""),
                };
                out.push(HelpLine::Entry(k.to_string(), d.to_string()));
            }
            out.push(HelpLine::Blank);
            out
        })
        .collect()
}

impl<'a> Widget for HelpOverlay<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let theme = self.theme;
        let width = (area.width.saturating_mul(85) / 100).min(110);
        let height = area.height.saturating_mul(85) / 100;
        let inner = modal(buf, area, width, height, "Keyboard shortcuts", "esc to close", theme);
        if inner.height == 0 {
            return;
        }

        let two_cols = inner.width >= 76;
        let groups = groups();
        let cols: Vec<Vec<HelpLine>> = if two_cols {
            let total: usize = groups.iter().map(Vec::len).sum();
            let mut left = Vec::new();
            let mut right = Vec::new();
            for g in groups {
                if left.len() < total / 2 {
                    left.extend(g);
                } else {
                    right.extend(g);
                }
            }
            vec![left, right]
        } else {
            vec![groups.into_iter().flatten().collect()]
        };

        let col_w = if two_cols { (inner.width - 4) / 2 } else { inner.width };
        let bg = Style::default().bg(theme.bg_modal);
        for (ci, lines) in cols.iter().enumerate() {
            let x = inner.x + ci as u16 * (col_w + 4);
            for (i, line) in lines.iter().skip(self.scroll as usize).enumerate() {
                if i as u16 >= inner.height {
                    break;
                }
                let y = inner.y + i as u16;
                match line {
                    HelpLine::Heading(h) => {
                        buf.set_stringn(x, y, h.to_uppercase(), col_w as usize, theme.subheading().patch(bg));
                    }
                    HelpLine::Entry(k, d) => {
                        let kw = 16.min(col_w / 2);
                        buf.set_stringn(x, y, truncate(k, kw as usize), kw as usize, theme.accent().patch(bg));
                        buf.set_stringn(
                            x + kw + 1,
                            y,
                            truncate(d, col_w.saturating_sub(kw + 1) as usize),
                            col_w.saturating_sub(kw + 1) as usize,
                            theme.normal().patch(bg),
                        );
                    }
                    HelpLine::Blank => {}
                }
            }
        }
    }
}
