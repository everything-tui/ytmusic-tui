use crate::art::Cover;
use crate::ui::icons::{self, Icon};
use crate::ui::theme::Theme;
use crate::ui::widgets::player_parts::render_art;
use ratatui::prelude::*;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::{FilterType, Resize, StatefulImage};
use std::cell::RefCell;
use std::collections::HashMap;

/// Each place a cover is drawn keeps its own encoded protocol so moving between
/// sizes doesn't thrash a single cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CoverSlot {
    Sidebar,
    Player,
    Fullscreen,
    Detail,
    /// Visible Home shelf card; index is stable for the current frame only.
    Home(u8),
}

struct SlotState {
    key: String,
    proto: StatefulProtocol,
}

pub struct CoverRenderer {
    /// `None` means half-blocks only.
    picker: Option<Picker>,
    disabled: bool,
    slots: RefCell<HashMap<CoverSlot, SlotState>>,
    icons: RefCell<HashMap<String, StatefulProtocol>>,
}

impl Default for CoverRenderer {
    fn default() -> Self {
        Self::halfblocks()
    }
}

impl CoverRenderer {
    pub fn halfblocks() -> Self {
        Self {
            picker: None,
            disabled: false,
            slots: RefCell::new(HashMap::new()),
            icons: RefCell::new(HashMap::new()),
        }
    }

    /// Picks a graphics protocol according to `ui.art_protocol`.
    ///
    /// Must run after entering the alternate screen and before reading terminal
    /// events: `auto` queries the terminal over stdio.
    pub fn detect(setting: &str) -> Self {
        let forced = match setting.to_ascii_lowercase().as_str() {
            "off" | "none" => {
                return Self {
                    disabled: true,
                    ..Self::halfblocks()
                }
            }
            "halfblocks" | "halfblock" | "blocks" => return Self::halfblocks(),
            "kitty" => Some(ProtocolType::Kitty),
            "iterm" | "iterm2" => Some(ProtocolType::Iterm2),
            "sixel" => Some(ProtocolType::Sixel),
            _ => None,
        };
        let picker = match Picker::from_query_stdio() {
            Ok(mut p) => {
                if let Some(proto) = forced {
                    p.set_protocol_type(proto);
                }
                Some(p)
            }
            Err(e) => {
                tracing::debug!("terminal graphics query failed: {e}");
                forced.map(|proto| {
                    let mut p = Picker::from_fontsize((8, 16));
                    p.set_protocol_type(proto);
                    p
                })
            }
        };
        let picker = picker.filter(|p| p.protocol_type() != ProtocolType::Halfblocks);
        tracing::info!(
            "cover art protocol: {:?}",
            picker.map(|p| p.protocol_type())
        );
        Self {
            picker,
            disabled: false,
            slots: RefCell::new(HashMap::new()),
            icons: RefCell::new(HashMap::new()),
        }
    }

    /// True when scalable control icons can be drawn as real images.
    pub fn icons_available(&self) -> bool {
        !self.disabled && self.picker.is_some()
    }

    /// Draws a control icon as an image into the square fitted inside `area`.
    pub fn draw_icon(
        &self,
        icon: Icon,
        fg: (u8, u8, u8),
        bg: (u8, u8, u8),
        area: Rect,
        buf: &mut Buffer,
    ) -> bool {
        let Some(picker) = self.picker.as_ref().filter(|_| !self.disabled) else {
            return false;
        };
        let r = self.fit(area);
        if r.width == 0 || r.height == 0 {
            return false;
        }
        let key = format!("{icon:?}-{}-{}-{}", fg.0, fg.1, fg.2);
        let mut map = self.icons.borrow_mut();
        if map.len() > 96 {
            map.clear();
        }
        let proto = map
            .entry(key)
            .or_insert_with(|| picker.new_resize_protocol(icons::render(icon, fg, bg)));
        StatefulImage::default()
            .resize(Resize::Scale(Some(FilterType::CatmullRom)))
            .render(r, buf, proto);
        true
    }

    /// Terminal cell size in pixels (or a 1:2 approximation for half-blocks).
    fn cell(&self) -> (u16, u16) {
        self.picker
            .map(|p| p.font_size())
            .filter(|(w, h)| *w > 0 && *h > 0)
            .unwrap_or((1, 2))
    }

    /// Largest visually-square rect inside `area`, centered.
    pub fn fit(&self, area: Rect) -> Rect {
        let (fw, fh) = self.cell();
        let rows_by_width = (area.width as u32 * fw as u32 / fh as u32) as u16;
        let rows = area.height.min(rows_by_width);
        let cols = ((rows as u32 * fh as u32 + fw as u32 / 2) / fw as u32) as u16;
        let cols = cols.min(area.width);
        Rect {
            x: area.x + (area.width - cols) / 2,
            y: area.y + (area.height - rows) / 2,
            width: cols,
            height: rows,
        }
    }

    /// Width in columns a square cover of `rows` rows occupies.
    pub fn width_for_rows(&self, rows: u16) -> u16 {
        let (fw, fh) = self.cell();
        ((rows as u32 * fh as u32 + fw as u32 / 2) / fw as u32) as u16
    }

    /// Draws `cover` (keyed by `key`) into the square fitted inside `area`.
    /// `graphics_ok` must be false whenever something may be drawn on top of the
    /// cover, since image protocols paint above the text layer.
    /// Returns the rect actually covered.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &self,
        slot: CoverSlot,
        key: Option<&str>,
        cover: Option<&Cover>,
        area: Rect,
        buf: &mut Buffer,
        theme: &Theme,
        graphics_ok: bool,
    ) -> Rect {
        let r = self.fit(area);
        if r.width == 0 || r.height == 0 {
            return r;
        }
        if self.disabled {
            render_art(buf, r, None, theme);
            return r;
        }
        // Always paint half-blocks first. Kitty/iTerm protocols are detected in
        // many hosts (including Cursor) that don't actually composite images, so
        // a graphics-only path left covers blank.
        render_art(buf, r, cover.and_then(|c| c.pixels.as_deref()), theme);
        if let (true, Some(picker), Some(key), Some(image)) = (
            graphics_ok,
            self.picker.as_ref(),
            key,
            cover.and_then(|c| c.image.as_ref()),
        ) {
            let mut slots = self.slots.borrow_mut();
            let stale = slots.get(&slot).is_none_or(|s| s.key != key);
            if stale {
                slots.insert(
                    slot,
                    SlotState {
                        key: key.to_string(),
                        proto: picker.new_resize_protocol((**image).clone()),
                    },
                );
            }
            if let Some(state) = slots.get_mut(&slot) {
                StatefulImage::default()
                    .resize(Resize::Scale(Some(FilterType::CatmullRom)))
                    .render(r, buf, &mut state.proto);
            }
        }
        r
    }
}

/// Everything a widget needs to draw one particular cover.
#[derive(Clone, Copy)]
pub struct CoverCtx<'a> {
    pub renderer: &'a CoverRenderer,
    pub key: Option<&'a str>,
    pub cover: Option<&'a Cover>,
    /// False while an overlay is open.
    pub graphics: bool,
    /// Rects drawn after the cover (toasts); graphics would hide them.
    pub blocked: &'a [Rect],
}

impl<'a> CoverCtx<'a> {
    pub fn draw(&self, slot: CoverSlot, area: Rect, buf: &mut Buffer, theme: &Theme) -> Rect {
        let fitted = self.renderer.fit(area);
        let graphics = self.graphics && !self.blocked.iter().any(|b| b.intersects(fitted));
        self.renderer
            .render(slot, self.key, self.cover, area, buf, theme, graphics)
    }

    pub fn width_for_rows(&self, rows: u16) -> u16 {
        self.renderer.width_for_rows(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halfblock_fit_is_twice_as_wide_as_tall() {
        let c = CoverRenderer::halfblocks();
        let r = c.fit(Rect::new(0, 0, 40, 10));
        assert_eq!((r.width, r.height), (20, 10));
        let r = c.fit(Rect::new(0, 0, 10, 30));
        assert_eq!((r.width, r.height), (10, 5));
        assert_eq!(c.width_for_rows(4), 8);
    }
}
