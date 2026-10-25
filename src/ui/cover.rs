use crate::art::Cover;
use crate::ui::icons::{self, Icon};
use crate::ui::theme::Theme;
use crate::ui::widgets::player_parts::render_art;
use image::DynamicImage;
use ratatui::prelude::*;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::{FilterType, Resize, StatefulImage};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

/// Where a cover is drawn. Encoded images are cached per (slot, cover), since each slot
/// has its own size; all cards (Home shelves, Library grid) share one size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CoverSlot {
    Sidebar,
    Player,
    Fullscreen,
    Detail,
    Card,
}

/// Encoded covers kept around so scrolling back or switching tabs shows them instantly.
const CACHE_CAP: usize = 40;
const ENCODE_WORKERS: usize = 2;

type CacheKey = (CoverSlot, String);

enum Entry {
    /// A worker is resizing/encoding; half-blocks show until it is back.
    Pending,
    Ready(StatefulProtocol),
}

struct Cached {
    entry: Entry,
    used: u64,
}

enum Job {
    /// Build the protocol from scratch: hashing and compositing the source is itself costly.
    Create { key: CacheKey, image: Arc<DynamicImage>, area: Rect },
    Resize { key: CacheKey, proto: StatefulProtocol, area: Rect },
}

/// Background resize+encode so drawing never blocks on image work.
struct Encoder {
    jobs: mpsc::Sender<Job>,
    done: Mutex<mpsc::Receiver<(CacheKey, StatefulProtocol)>>,
    wake: Arc<Notify>,
}

fn resize_mode() -> Resize {
    Resize::Scale(Some(FilterType::CatmullRom))
}

impl Encoder {
    fn spawn(picker: Picker) -> Self {
        let (jobs, job_rx) = mpsc::channel::<Job>();
        let (done_tx, done) = mpsc::channel();
        let job_rx = Arc::new(Mutex::new(job_rx));
        let wake = Arc::new(Notify::new());
        for i in 0..ENCODE_WORKERS {
            let job_rx = job_rx.clone();
            let done_tx = done_tx.clone();
            let wake = wake.clone();
            let _ = std::thread::Builder::new()
                .name(format!("cover-encode-{i}"))
                .spawn(move || loop {
                    let job = match job_rx.lock() {
                        Ok(rx) => rx.recv(),
                        Err(_) => return,
                    };
                    let Ok(job) = job else { return };
                    let (key, mut proto, area) = match job {
                        Job::Create { key, image, area } => {
                            let image = shrink_for(&picker, key.0, &image, area);
                            (key, picker.new_resize_protocol(image), area)
                        }
                        Job::Resize { key, proto, area } => (key, proto, area),
                    };
                    let bg = proto.background_color();
                    proto.resize_encode(&resize_mode(), bg, area);
                    if done_tx.send((key, proto)).is_err() {
                        return;
                    }
                    wake.notify_one();
                });
        }
        Self {
            jobs,
            done: Mutex::new(done),
            wake,
        }
    }
}

/// Cards never change size with the window, so their source can be shrunk to what is shown,
/// which keeps the cache small. Other slots keep full resolution for later resizes.
fn shrink_for(picker: &Picker, slot: CoverSlot, image: &DynamicImage, area: Rect) -> DynamicImage {
    let (fw, fh) = picker.font_size();
    let px = (area.width as u32 * fw as u32).max(area.height as u32 * fh as u32);
    if slot == CoverSlot::Card && px > 0 && image.width() > px {
        image.resize(px, px, image::imageops::FilterType::Triangle)
    } else {
        image.clone()
    }
}

pub struct CoverRenderer {
    /// `None` means half-blocks only.
    picker: Option<Picker>,
    disabled: bool,
    encoder: Option<Encoder>,
    cache: RefCell<HashMap<CacheKey, Cached>>,
    uses: Cell<u64>,
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
            encoder: None,
            cache: RefCell::new(HashMap::new()),
            uses: Cell::new(0),
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
            encoder: picker.map(Encoder::spawn),
            ..Self::halfblocks()
        }
    }

    /// Fires whenever a background-encoded cover is ready to be drawn.
    pub fn wake_handle(&self) -> Option<Arc<Notify>> {
        self.encoder.as_ref().map(|e| e.wake.clone())
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
        let key = format!("{icon:?}-{fg:?}-{bg:?}");
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
        // a graphics-only path left covers blank. They also stand in while encoding.
        render_art(buf, r, cover.and_then(|c| c.pixels.as_deref()), theme);
        if let (true, Some(encoder), Some(key), Some(image)) = (
            graphics_ok,
            self.encoder.as_ref(),
            key,
            cover.and_then(|c| c.image.as_ref()),
        ) {
            self.collect_encoded(encoder);
            let use_no = self.uses.get() + 1;
            self.uses.set(use_no);
            let mut cache = self.cache.borrow_mut();
            let ck = (slot, key.to_string());
            let cached = cache.entry(ck.clone()).or_insert_with(|| {
                let _ = encoder.jobs.send(Job::Create {
                    key: ck.clone(),
                    image: image.clone(),
                    area: r,
                });
                Cached { entry: Entry::Pending, used: use_no }
            });
            cached.used = use_no;
            if let Entry::Ready(proto) = &mut cached.entry {
                match proto.needs_resize(&resize_mode(), r) {
                    // Window or layout changed size: re-encode off-thread, half-blocks meanwhile.
                    Some(target) => {
                        if let Entry::Ready(proto) =
                            std::mem::replace(&mut cached.entry, Entry::Pending)
                        {
                            let _ = encoder.jobs.send(Job::Resize { key: ck, proto, area: target });
                        }
                    }
                    None => proto.render(r, buf),
                }
            }
            evict(&mut cache);
        }
        r
    }

    /// Moves finished encodes into the cache.
    fn collect_encoded(&self, encoder: &Encoder) {
        let Ok(done) = encoder.done.lock() else { return };
        let mut cache = self.cache.borrow_mut();
        while let Ok((key, proto)) = done.try_recv() {
            let used = cache.get(&key).map_or(0, |c| c.used);
            cache.insert(key, Cached { entry: Entry::Ready(proto), used });
        }
    }
}

/// Drops the least recently drawn finished covers beyond the cap.
fn evict(cache: &mut HashMap<CacheKey, Cached>) {
    while cache.len() > CACHE_CAP {
        let oldest = cache
            .iter()
            .filter(|(_, c)| matches!(c.entry, Entry::Ready(_)))
            .min_by_key(|(_, c)| c.used)
            .map(|(k, _)| k.clone());
        match oldest {
            Some(k) => {
                cache.remove(&k);
            }
            None => break,
        }
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
