use crate::config::{parse_hex_color, ColorOverrides, UiConfig};
use ratatui::style::{Color, Modifier, Style};

type Rgb = (u8, u8, u8);

/// Raw colors of a theme; presets are plain data so they are easy to add and override.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub accent: Rgb,
    pub text: Rgb,
    pub dim: Rgb,
    pub muted: Rgb,
    pub faint: Rgb,
    pub error: Rgb,
    pub success: Rgb,
    pub border: Rgb,
    pub bg: Rgb,
    pub bg_sidebar: Rgb,
    pub bg_player: Rgb,
    pub bg_hover: Rgb,
    pub bg_selected: Rgb,
    pub bg_modal: Rgb,
}

const MIDNIGHT: Palette = Palette {
    accent: (255, 0, 51),
    text: (236, 236, 241),
    dim: (158, 158, 170),
    muted: (108, 108, 122),
    faint: (68, 68, 80),
    error: (255, 99, 105),
    success: (74, 222, 128),
    border: (42, 42, 52),
    bg: (18, 18, 23),
    // Sidebar and player share one chrome tone that frames the content.
    bg_sidebar: (11, 11, 15),
    bg_player: (11, 11, 15),
    bg_hover: (28, 28, 36),
    bg_selected: (38, 38, 48),
    bg_modal: (28, 28, 36),
};

/// Built-in themes, in the order the command palette lists them.
pub const PRESETS: &[(&str, Palette)] = &[
    ("midnight", MIDNIGHT),
    (
        "oled",
        Palette {
            accent: (255, 0, 51),
            text: (240, 240, 240),
            dim: (160, 160, 160),
            muted: (110, 110, 110),
            faint: (62, 62, 62),
            error: (255, 99, 105),
            success: (74, 222, 128),
            border: (34, 34, 34),
            bg: (10, 10, 10),
            bg_sidebar: (0, 0, 0),
            bg_player: (0, 0, 0),
            bg_hover: (22, 22, 22),
            bg_selected: (32, 32, 32),
            bg_modal: (22, 22, 22),
        },
    ),
    (
        "catppuccin",
        Palette {
            accent: (203, 166, 247),
            text: (205, 214, 244),
            dim: (166, 173, 200),
            muted: (108, 112, 134),
            faint: (88, 91, 112),
            error: (243, 139, 168),
            success: (166, 227, 161),
            border: (49, 50, 68),
            bg: (30, 30, 46),
            bg_sidebar: (17, 17, 27),
            bg_player: (17, 17, 27),
            bg_hover: (40, 41, 59),
            bg_selected: (69, 71, 90),
            bg_modal: (49, 50, 68),
        },
    ),
    (
        "tokyo-night",
        Palette {
            accent: (122, 162, 247),
            text: (192, 202, 245),
            dim: (169, 177, 214),
            muted: (86, 95, 137),
            faint: (59, 66, 97),
            error: (247, 118, 142),
            success: (158, 206, 106),
            border: (41, 46, 66),
            bg: (26, 27, 38),
            bg_sidebar: (22, 22, 30),
            bg_player: (22, 22, 30),
            bg_hover: (36, 40, 59),
            bg_selected: (52, 59, 88),
            bg_modal: (36, 40, 59),
        },
    ),
    (
        "nord",
        Palette {
            accent: (136, 192, 208),
            text: (236, 239, 244),
            dim: (180, 188, 204),
            muted: (123, 136, 161),
            faint: (76, 86, 106),
            error: (191, 97, 106),
            success: (163, 190, 140),
            border: (59, 66, 82),
            bg: (46, 52, 64),
            bg_sidebar: (36, 41, 51),
            bg_player: (36, 41, 51),
            bg_hover: (59, 66, 82),
            bg_selected: (67, 76, 94),
            bg_modal: (59, 66, 82),
        },
    ),
    (
        "gruvbox",
        Palette {
            accent: (254, 128, 25),
            text: (235, 219, 178),
            dim: (189, 174, 147),
            muted: (146, 131, 116),
            faint: (102, 92, 84),
            error: (251, 73, 52),
            success: (184, 187, 38),
            border: (60, 56, 54),
            bg: (40, 40, 40),
            bg_sidebar: (29, 32, 33),
            bg_player: (29, 32, 33),
            bg_hover: (50, 48, 47),
            bg_selected: (80, 73, 69),
            bg_modal: (50, 48, 47),
        },
    ),
    (
        "rose-pine",
        Palette {
            accent: (235, 188, 186),
            text: (224, 222, 244),
            dim: (144, 140, 170),
            muted: (110, 106, 134),
            faint: (64, 61, 82),
            error: (235, 111, 146),
            success: (156, 207, 216),
            border: (38, 35, 58),
            bg: (25, 23, 36),
            bg_sidebar: (19, 17, 28),
            bg_player: (19, 17, 28),
            bg_hover: (31, 29, 46),
            bg_selected: (45, 42, 66),
            bg_modal: (31, 29, 46),
        },
    ),
    (
        "dracula",
        Palette {
            accent: (189, 147, 249),
            text: (248, 248, 242),
            dim: (190, 192, 210),
            muted: (98, 114, 164),
            faint: (68, 71, 90),
            error: (255, 85, 85),
            success: (80, 250, 123),
            border: (68, 71, 90),
            bg: (40, 42, 54),
            bg_sidebar: (33, 34, 44),
            bg_player: (33, 34, 44),
            bg_hover: (52, 55, 70),
            bg_selected: (68, 71, 90),
            bg_modal: (52, 55, 70),
        },
    ),
    (
        "light",
        Palette {
            accent: (224, 0, 43),
            text: (28, 28, 34),
            dim: (88, 88, 100),
            muted: (130, 130, 144),
            faint: (190, 190, 200),
            error: (210, 40, 50),
            success: (22, 150, 80),
            border: (226, 226, 232),
            bg: (250, 250, 252),
            bg_sidebar: (240, 240, 244),
            bg_player: (240, 240, 244),
            bg_hover: (232, 232, 238),
            bg_selected: (222, 222, 230),
            bg_modal: (255, 255, 255),
        },
    ),
];

/// "tokyo-night" → "Tokyo Night", for menus and toasts.
pub fn display_name(name: &str) -> String {
    name.split('-')
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect::<String>()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Canonical preset name for a user-supplied one (case, `-`/`_`/space insensitive).
pub fn preset_name(name: &str) -> Option<&'static str> {
    let norm = |s: &str| s.to_lowercase().replace(['-', '_', ' '], "");
    let want = norm(name);
    let want = match want.as_str() {
        "catppuccinmocha" | "mocha" => "catppuccin".to_string(),
        "tokyonight" => "tokyonight".to_string(),
        "rosepine" => "rosepine".to_string(),
        "black" | "amoled" => "oled".to_string(),
        "default" | "dark" | "dynamic" | "static" | "" => "midnight".to_string(),
        _ => want,
    };
    PRESETS.iter().map(|(n, _)| *n).find(|n| norm(n) == want)
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub name: &'static str,
    pub accent: Color,
    pub accent_target: Color,
    pub accent_from: Color,
    pub accent_lerp: f32,
    pub text: Color,
    pub dim: Color,
    pub text_muted: Color,
    /// Barely-there text for secondary hints (key numbers, tree guides).
    pub faint: Color,
    pub error: Color,
    pub success: Color,
    pub border: Color,
    pub bg: Color,
    pub bg_sidebar: Color,
    pub bg_player: Color,
    pub bg_hover: Color,
    pub bg_selected: Color,
    pub bg_modal: Color,
    pub dynamic: bool,
    pub truecolor: bool,
    /// Light backgrounds: dynamic accents are darkened to stay readable.
    pub light: bool,
    /// Draw Nerd Font pictograms instead of plain text.
    pub nerd_icons: bool,
}

impl Theme {
    /// Theme described by the `[ui]` config: preset, accent override and per-color overrides.
    pub fn from_config(ui: &UiConfig) -> Self {
        let name = preset_name(&ui.theme).unwrap_or("midnight");
        let mut p = palette(name);
        if let Some(c) = parse_hex_color(&ui.accent) {
            p.accent = c;
        }
        apply_overrides(&mut p, &ui.colors);
        let mut theme = Self::from_palette(name, &p, ui.dynamic_accent);
        theme.nerd_icons = ui.icons.eq_ignore_ascii_case("nerd");
        theme
    }

    /// A built-in preset as-is (unknown names fall back to midnight).
    pub fn preset(name: &str, dynamic: bool) -> Self {
        let name = preset_name(name).unwrap_or("midnight");
        Self::from_palette(name, &palette(name), dynamic)
    }

    fn from_palette(name: &'static str, p: &Palette, dynamic: bool) -> Self {
        let truecolor = detect_truecolor();
        let c = |(r, g, b): Rgb| rgb(r, g, b, truecolor);
        let accent = c(p.accent);
        Self {
            name,
            accent,
            accent_target: accent,
            accent_from: accent,
            accent_lerp: 1.0,
            text: c(p.text),
            dim: c(p.dim),
            text_muted: c(p.muted),
            faint: c(p.faint),
            error: c(p.error),
            success: c(p.success),
            border: c(p.border),
            bg: c(p.bg),
            bg_sidebar: c(p.bg_sidebar),
            bg_player: c(p.bg_player),
            bg_hover: c(p.bg_hover),
            bg_selected: c(p.bg_selected),
            bg_modal: c(p.bg_modal),
            dynamic,
            truecolor,
            light: luminance(p.bg) > 0.5,
            nerd_icons: false,
        }
    }

    pub fn set_accent_rgb(&mut self, r: u8, g: u8, b: u8) {
        if !self.dynamic {
            return;
        }
        // Cover accents are tuned for dark UIs; deepen them on light backgrounds.
        let (r, g, b) = if self.light {
            let d = |v: u8| (v as f32 * 0.68).round() as u8;
            (d(r), d(g), d(b))
        } else {
            (r, g, b)
        };
        self.accent_from = self.accent;
        self.accent_target = rgb(r, g, b, self.truecolor);
        self.accent_lerp = 0.0;
    }

    /// Advance accent animation (~300ms at 30fps → ~0.1 per frame)
    pub fn tick_accent(&mut self) {
        if self.accent_lerp >= 1.0 {
            return;
        }
        self.accent_lerp = (self.accent_lerp + 0.1).min(1.0);
        self.accent = lerp_color(self.accent_from, self.accent_target, self.accent_lerp);
    }

    pub fn title(&self) -> Style {
        Style::default()
            .fg(self.text)
            .add_modifier(Modifier::BOLD)
    }

    pub fn normal(&self) -> Style {
        Style::default().fg(self.text)
    }

    pub fn dim(&self) -> Style {
        Style::default().fg(self.dim)
    }

    pub fn accent(&self) -> Style {
        Style::default().fg(self.accent)
    }

    pub fn accent_bold(&self) -> Style {
        Style::default()
            .fg(self.accent)
            .add_modifier(Modifier::BOLD)
    }

    pub fn muted(&self) -> Style {
        Style::default().fg(self.text_muted)
    }

    pub fn heading(&self) -> Style {
        Style::default()
            .fg(self.text)
            .add_modifier(Modifier::BOLD)
    }

    pub fn subheading(&self) -> Style {
        Style::default()
            .fg(self.dim)
            .add_modifier(Modifier::BOLD)
    }

    /// Full-row highlight for the cursor row; only sets the background.
    pub fn row_selected(&self) -> Style {
        Style::default().bg(self.tint(self.bg_selected, 0.12))
    }

    pub fn row_playing(&self) -> Style {
        Style::default().fg(self.accent)
    }

    pub fn playing(&self) -> Style {
        Style::default()
            .fg(self.accent)
            .add_modifier(Modifier::BOLD)
    }

    pub fn pill(&self, active: bool) -> Style {
        if active {
            Style::default()
                .fg(self.bg)
                .bg(self.text)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(self.text).bg(self.bg_selected)
        }
    }

    pub fn rule(&self) -> Style {
        Style::default().fg(self.border)
    }

    pub fn faint(&self) -> Style {
        Style::default().fg(self.faint)
    }

    /// `base` blended toward the accent by `amount` (0..1), for soft highlights.
    pub fn tint(&self, base: Color, amount: f32) -> Color {
        let (r1, g1, b1) = rgb_of(base, (24, 24, 30));
        let (r2, g2, b2) = rgb_of(self.accent, (255, 0, 51));
        let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * amount).round() as u8;
        rgb(l(r1, r2), l(g1, g2), l(b1, b2), self.truecolor)
    }

    pub fn color(&self, r: u8, g: u8, b: u8) -> Color {
        rgb(r, g, b, self.truecolor)
    }

    /// Text drawn on top of an accent-colored background: dark on bright accents, white otherwise.
    pub fn on_accent(&self) -> Style {
        let fg = if luminance(rgb_of(self.accent, (255, 0, 51))) > 0.55 {
            rgb(18, 18, 22, self.truecolor)
        } else {
            rgb(255, 255, 255, self.truecolor)
        };
        Style::default()
            .fg(fg)
            .bg(self.accent)
            .add_modifier(Modifier::BOLD)
    }
}

fn palette(name: &str) -> Palette {
    PRESETS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, p)| *p)
        .unwrap_or(MIDNIGHT)
}

fn apply_overrides(p: &mut Palette, o: &ColorOverrides) {
    let slots: [(&Option<String>, &mut Rgb); 13] = [
        (&o.text, &mut p.text),
        (&o.dim, &mut p.dim),
        (&o.muted, &mut p.muted),
        (&o.faint, &mut p.faint),
        (&o.error, &mut p.error),
        (&o.success, &mut p.success),
        (&o.border, &mut p.border),
        (&o.bg, &mut p.bg),
        (&o.bg_sidebar, &mut p.bg_sidebar),
        (&o.bg_player, &mut p.bg_player),
        (&o.bg_hover, &mut p.bg_hover),
        (&o.bg_selected, &mut p.bg_selected),
        (&o.bg_modal, &mut p.bg_modal),
    ];
    for (hex, slot) in slots {
        if let Some(c) = hex.as_deref().and_then(parse_hex_color) {
            *slot = c;
        }
    }
}

/// Perceived brightness in 0..1 (Rec. 709 weights, no gamma: good enough for picking text color).
fn luminance((r, g, b): Rgb) -> f32 {
    (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0
}

fn detect_truecolor() -> bool {
    if let Ok(v) = std::env::var("COLORTERM") {
        if v == "truecolor" || v == "24bit" {
            return true;
        }
    }
    matches!(
        std::env::var("TERM_PROGRAM").as_deref(),
        Ok("iTerm.app" | "WezTerm" | "vscode" | "ghostty" | "Hyper" | "WarpTerminal" | "Tabby")
    ) || std::env::var("TERM")
        .map(|t| t.contains("kitty") || t.contains("alacritty") || t.contains("direct"))
        .unwrap_or(false)
}

fn rgb(r: u8, g: u8, b: u8, truecolor: bool) -> Color {
    if truecolor {
        Color::Rgb(r, g, b)
    } else {
        Color::Indexed(rgb_to_256(r, g, b))
    }
}

fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let nearest = |v: u8| {
        LEVELS
            .iter()
            .enumerate()
            .min_by_key(|(_, l)| (**l as i16 - v as i16).abs())
            .map(|(i, _)| i as u8)
            .unwrap_or(0)
    };
    let (ri, gi, bi) = (nearest(r), nearest(g), nearest(b));
    let cube = 16 + 36 * ri + 6 * gi + bi;
    let cube_rgb = (LEVELS[ri as usize], LEVELS[gi as usize], LEVELS[bi as usize]);

    let avg = (r as u16 + g as u16 + b as u16) / 3;
    let gray_i = if avg < 8 { 0 } else { ((avg - 8) / 10).min(23) as u8 };
    let gray_v = 8 + gray_i * 10;

    let dist = |(a, b2, c): (u8, u8, u8)| {
        let d = |x: u8, y: u8| (x as i32 - y as i32).pow(2);
        d(a, r) + d(b2, g) + d(c, b)
    };
    if dist((gray_v, gray_v, gray_v)) < dist(cube_rgb) {
        232 + gray_i
    } else {
        cube
    }
}

/// Best-effort RGB of a theme color, inverting the 256-color mapping.
pub fn rgb_of(c: Color, fallback: (u8, u8, u8)) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Indexed(n) if n >= 232 => {
            let v = 8 + 10 * (n - 232);
            (v, v, v)
        }
        Color::Indexed(n) if n >= 16 => {
            let n = n - 16;
            let lv = |i: u8| if i == 0 { 0 } else { 55 + 40 * i };
            (lv(n / 36), lv((n / 6) % 6), lv(n % 6))
        }
        _ => fallback,
    }
}

fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    match (a, b) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
            Color::Rgb(l(r1, r2), l(g1, g2), l(b1, b2))
        }
        _ => b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_names_resolve_loosely() {
        assert_eq!(preset_name("Tokyo Night"), Some("tokyo-night"));
        assert_eq!(preset_name("rose_pine"), Some("rose-pine"));
        assert_eq!(preset_name("dynamic"), Some("midnight"), "legacy value");
        assert_eq!(preset_name("nope"), None);
        for (name, _) in PRESETS {
            assert_eq!(preset_name(name), Some(*name));
        }
    }

    #[test]
    fn config_overrides_apply_on_top_of_preset() {
        let mut ui = UiConfig::default();
        ui.theme = "nord".into();
        ui.accent = "#00ff00".into();
        ui.colors.bg = Some("#101010".into());
        let t = Theme::from_config(&ui);
        assert_eq!(t.name, "nord");
        if t.truecolor {
            assert_eq!(t.accent, Color::Rgb(0, 255, 0));
            assert_eq!(t.bg, Color::Rgb(16, 16, 16));
            assert_eq!(t.text, Color::Rgb(236, 239, 244));
        }
        assert!(Theme::preset("light", true).light);
        assert!(!Theme::preset("midnight", true).light);
    }
}
