//! Pictograms for navigation and item kinds. Plain text by default (an empty string, so
//! callers simply skip the icon); Nerd Font icons when `ui.icons = "nerd"`.
use crate::ui::theme::Theme;

#[derive(Debug, Clone, Copy)]
pub enum Glyph {
    Home,
    Search,
    Library,
    Queue,
    Lyrics,
    Song,
    Album,
    Artist,
    Playlist,
    Volume,
    VolumeOff,
}

/// Font Awesome 4 code points, present in every Nerd Font patch.
fn nerd(g: Glyph) -> &'static str {
    match g {
        Glyph::Home => "\u{f015}",
        Glyph::Search => "\u{f002}",
        Glyph::Library | Glyph::Song => "\u{f001}",
        Glyph::Queue => "\u{f0cb}",
        Glyph::Lyrics => "\u{f130}",
        Glyph::Album => "\u{f192}",
        Glyph::Artist => "\u{f007}",
        Glyph::Playlist => "\u{f03a}",
        Glyph::Volume => "\u{f028}",
        Glyph::VolumeOff => "\u{f026}",
    }
}

pub fn glyph(theme: &Theme, g: Glyph) -> &'static str {
    if theme.nerd_icons {
        nerd(g)
    } else {
        ""
    }
}
