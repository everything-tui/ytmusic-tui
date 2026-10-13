use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone)]
pub struct Theme {
    pub accent: Color,
    pub accent_target: Color,
    pub accent_from: Color,
    pub accent_lerp: f32,
    pub text: Color,
    pub dim: Color,
    pub text_muted: Color,
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
}

impl Theme {
    pub fn new(accent_hex: &str, dynamic: bool) -> Self {
        let truecolor = detect_truecolor();
        let (r, g, b) = crate::config::parse_hex_color(accent_hex).unwrap_or((255, 0, 51));
        let accent = rgb(r, g, b, truecolor);
        Self {
            accent,
            accent_target: accent,
            accent_from: accent,
            accent_lerp: 1.0,
            text: rgb(238, 238, 238, truecolor),
            dim: rgb(150, 150, 150, truecolor),
            text_muted: rgb(110, 110, 110, truecolor),
            error: rgb(255, 92, 92, truecolor),
            success: rgb(80, 200, 120, truecolor),
            border: rgb(52, 52, 52, truecolor),
            bg: rgb(18, 18, 18, truecolor),
            bg_sidebar: rgb(10, 10, 10, truecolor),
            bg_player: rgb(24, 24, 24, truecolor),
            bg_hover: rgb(32, 32, 32, truecolor),
            bg_selected: rgb(44, 44, 44, truecolor),
            bg_modal: rgb(36, 36, 36, truecolor),
            dynamic,
            truecolor,
        }
    }

    pub fn set_accent_rgb(&mut self, r: u8, g: u8, b: u8) {
        if !self.dynamic {
            return;
        }
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
        Style::default().bg(self.bg_selected)
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
                .fg(rgb(12, 12, 12, self.truecolor))
                .bg(self.text)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(self.text).bg(self.bg_selected)
        }
    }

    pub fn rule(&self) -> Style {
        Style::default().fg(self.border)
    }

    pub fn color(&self, r: u8, g: u8, b: u8) -> Color {
        rgb(r, g, b, self.truecolor)
    }

    /// Dark text drawn on top of an accent-colored background.
    pub fn on_accent(&self) -> Style {
        Style::default()
            .fg(rgb(18, 18, 18, self.truecolor))
            .bg(self.accent)
            .add_modifier(Modifier::BOLD)
    }
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

fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    match (a, b) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
            Color::Rgb(l(r1, r2), l(g1, g2), l(b1, b2))
        }
        _ => b,
    }
}
