use std::time::{Duration, Instant};

/// Horizontally scrolling text for long titles.
#[derive(Debug, Clone)]
pub struct Marquee {
    text: String,
    offset: usize,
    last_tick: Instant,
    paused_until: Instant,
}

impl Marquee {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            offset: 0,
            last_tick: Instant::now(),
            paused_until: Instant::now() + Duration::from_millis(800),
        }
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        let t = text.into();
        if t != self.text {
            self.text = t;
            self.offset = 0;
            self.paused_until = Instant::now() + Duration::from_millis(800);
        }
    }

    pub fn tick(&mut self) {
        if Instant::now() < self.paused_until {
            return;
        }
        if self.last_tick.elapsed() < Duration::from_millis(200) {
            return;
        }
        self.last_tick = Instant::now();
        let chars: Vec<char> = self.text.chars().collect();
        if chars.is_empty() {
            return;
        }
        self.offset = (self.offset + 1) % (chars.len() + 4);
        if self.offset == 0 {
            self.paused_until = Instant::now() + Duration::from_millis(1000);
        }
    }

    pub fn render(&self, width: usize) -> String {
        if width == 0 {
            return String::new();
        }
        let chars: Vec<char> = self.text.chars().collect();
        if chars.len() <= width {
            return self.text.clone();
        }
        let padded: Vec<char> = chars
            .iter()
            .copied()
            .chain([' ', ' ', ' ', ' '])
            .chain(chars.iter().copied())
            .collect();
        let start = self.offset.min(padded.len().saturating_sub(1));
        padded.iter().cycle().skip(start).take(width).collect()
    }
}
