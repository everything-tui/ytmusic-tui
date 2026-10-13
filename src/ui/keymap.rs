use crate::action::Action;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputContext {
    Global,
    Search,
    ManualCookie,
    CommandPalette,
    Onboarding,
    Help,
    ConfirmQuit,
    List,
    Equalizer,
    AccountMenu,
}

pub fn map_key(ctx: InputContext, key: KeyEvent) -> Option<Action> {
    // Ctrl+C always available (double-press to quit), even inside overlays/inputs
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
    {
        return Some(Action::CtrlC);
    }

    // Overlay contexts first
    match ctx {
        InputContext::Help => {
            return match key.code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => Some(Action::CloseOverlay),
                _ => None,
            };
        }
        InputContext::ConfirmQuit => {
            return match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => Some(Action::ConfirmQuit),
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => Some(Action::CancelQuit),
                // Second Ctrl+C is handled above; also allow plain 'c' as confirm while prompted
                KeyCode::Char('c') | KeyCode::Char('C') => Some(Action::ConfirmQuit),
                _ => None,
            };
        }
        InputContext::Onboarding => {
            return match key.code {
                KeyCode::Enter => Some(Action::BeginLogin),
                KeyCode::Char('g') | KeyCode::Char('G') => Some(Action::ContinueAsGuest),
                KeyCode::Char('p') | KeyCode::Char('P') => Some(Action::BeginManualCookie),
                KeyCode::Char('q') => Some(Action::Quit),
                _ => None,
            };
        }
        InputContext::Search => {
            return map_search(key);
        }
        InputContext::ManualCookie => {
            return match key.code {
                KeyCode::Esc => Some(Action::CloseOverlay),
                // Plain Enter or Ctrl+Enter submits
                KeyCode::Enter => Some(Action::SearchSubmit),
                KeyCode::Backspace => Some(Action::SearchBackspace),
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    // Clear input
                    Some(Action::PasteText(String::new()))
                }
                KeyCode::Char(c) => Some(Action::SearchInput(c)),
                _ => None,
            };
        }
        InputContext::CommandPalette => {
            return match key.code {
                KeyCode::Esc => Some(Action::CloseOverlay),
                KeyCode::Enter => Some(Action::PaletteSelect),
                KeyCode::Backspace => Some(Action::PaletteBackspace),
                KeyCode::Up | KeyCode::Char('k') => Some(Action::MoveUp),
                KeyCode::Down | KeyCode::Char('j') => Some(Action::MoveDown),
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    Some(Action::PaletteInput(c))
                }
                _ => None,
            };
        }
        InputContext::Equalizer => {
            return match key.code {
                KeyCode::Esc | KeyCode::Char('e') | KeyCode::Char('E') | KeyCode::Char('q') => {
                    Some(Action::CloseOverlay)
                }
                KeyCode::Left | KeyCode::Char('h') => Some(Action::MoveLeft),
                KeyCode::Right | KeyCode::Char('l') => Some(Action::MoveRight),
                KeyCode::Up | KeyCode::Char('k') => Some(Action::MoveUp),
                KeyCode::Down | KeyCode::Char('j') => Some(Action::MoveDown),
                KeyCode::Char(' ') => Some(Action::EqToggle),
                KeyCode::Char('0') => Some(Action::EqResetBand),
                KeyCode::Char('r') | KeyCode::Char('R') => Some(Action::EqFlat),
                KeyCode::Char('a') | KeyCode::Char('A') => Some(Action::EqAdaptiveCycle),
                KeyCode::Char('1') => Some(Action::EqPreset(0)),
                KeyCode::Char('2') => Some(Action::EqPreset(1)),
                KeyCode::Char('3') => Some(Action::EqPreset(2)),
                KeyCode::Char('4') => Some(Action::EqPreset(3)),
                KeyCode::Char('5') => Some(Action::EqPreset(4)),
                _ => None,
            };
        }
        InputContext::AccountMenu => {
            return match key.code {
                KeyCode::Esc | KeyCode::Char('q') => Some(Action::CloseOverlay),
                KeyCode::Enter => Some(Action::Select),
                KeyCode::Char('L') | KeyCode::Char('l') => Some(Action::Logout),
                KeyCode::Char('P') | KeyCode::Char('p') => Some(Action::BeginManualCookie),
                _ => None,
            };
        }
        InputContext::Global | InputContext::List => {}
    }

    // Global shortcuts
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('p') | KeyCode::Char('P') => Some(Action::CommandPalette),
            _ => None,
        };
    }

    match key.code {
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Char(' ') => Some(Action::PlayPause),
        KeyCode::Char('n') => Some(Action::Next),
        KeyCode::Char('p') => Some(Action::Previous),
        KeyCode::Right if key.modifiers.contains(KeyModifiers::SHIFT) => {
            Some(Action::SeekForward(Duration::from_secs(30)))
        }
        KeyCode::Left if key.modifiers.contains(KeyModifiers::SHIFT) => {
            Some(Action::SeekBack(Duration::from_secs(30)))
        }
        KeyCode::Right => Some(Action::MoveRight),
        KeyCode::Left => Some(Action::MoveLeft),
        KeyCode::Char('.') | KeyCode::Char('>') => {
            Some(Action::SeekForward(Duration::from_secs(5)))
        }
        KeyCode::Char(',') | KeyCode::Char('<') => Some(Action::SeekBack(Duration::from_secs(5))),
        KeyCode::Char('+') | KeyCode::Char('=') => Some(Action::VolumeUp),
        KeyCode::Char('-') | KeyCode::Char('_') => Some(Action::VolumeDown),
        KeyCode::Char('M') => Some(Action::Mute),
        KeyCode::Char('s') => Some(Action::ToggleShuffle),
        KeyCode::Char('r') => Some(Action::CycleRepeat),
        KeyCode::Char('l') => Some(Action::LikeCurrent),
        KeyCode::Char('L') => Some(Action::AccountMenu),
        KeyCode::Char('1') => Some(Action::Tab(0)),
        KeyCode::Char('2') => Some(Action::Tab(1)),
        KeyCode::Char('3') => Some(Action::Tab(2)),
        KeyCode::Char('4') => Some(Action::Tab(3)),
        KeyCode::Char('5') => Some(Action::Tab(4)),
        KeyCode::Tab => Some(Action::ToggleNowPlaying),
        KeyCode::Char('/') => Some(Action::StartSearch),
        KeyCode::Char('?') => Some(Action::Help),
        KeyCode::Esc | KeyCode::Backspace => Some(Action::Back),
        KeyCode::Enter => Some(Action::Select),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::MoveUp),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::MoveDown),
        KeyCode::Char('h') => Some(Action::MoveLeft),
        // Note: 'l' is LikeCurrent globally; use ←/→ for horizontal nav
        KeyCode::PageUp => Some(Action::PageUp),
        KeyCode::PageDown => Some(Action::PageDown),
        KeyCode::Char('g') => Some(Action::GoTop),
        KeyCode::Char('G') => Some(Action::GoBottom),
        KeyCode::Char('a') => Some(Action::AddToQueue),
        KeyCode::Char('A') => Some(Action::PlayNext),
        KeyCode::Char('m') => Some(Action::ContextMenu),
        KeyCode::Char('d') => Some(Action::RemoveFromQueue),
        KeyCode::Char('c') => Some(Action::ClearUpcoming),
        KeyCode::Char('J') => Some(Action::MoveQueueDown),
        KeyCode::Char('K') => Some(Action::MoveQueueUp),
        KeyCode::Char('f') => Some(Action::ToggleNowPlaying),
        KeyCode::Char('e') => Some(Action::OpenEqualizer),
        _ => None,
    }
}

fn map_search(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Esc => Some(Action::Escape),
        KeyCode::Enter => Some(Action::SearchSubmit),
        KeyCode::Backspace => Some(Action::SearchBackspace),
        KeyCode::Tab => Some(Action::CycleSearchFilter),
        KeyCode::Down | KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(Action::MoveDown)
        }
        KeyCode::Up | KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(Action::MoveUp)
        }
        KeyCode::Down => Some(Action::MoveDown),
        KeyCode::Up => Some(Action::MoveUp),
        KeyCode::Char(c) => Some(Action::SearchInput(c)),
        _ => None,
    }
}

pub fn help_text() -> &'static str {
    r#"yt-tui — keyboard reference

Playback
  space           Play / Pause
  n / p           Next / Previous
  , / .           Seek ±5s   (Shift+← / Shift+→: ±30s)
  ← / →           Navigate (Home items, Library sections); seek elsewhere
  + / -           Volume
  M               Mute
  s               Shuffle
  r               Cycle repeat (off → all → one)
  e               Equalizer (10-band; a = Adaptive taste; 1–5 presets)
  l               Like current track

Navigation
  1–5             Home / Search / Library / Queue / Lyrics
  Tab / f         Full-screen Now Playing
  /               Search
  ↑↓←→ / hjkl     Move
  g / G           Top / Bottom
  Enter           Play / Open
  Esc / Backspace Back
  Ctrl+P          Command palette
  ?               This help
  q               Quit (confirms if queue is playing)
  Ctrl+C twice    Quit immediately

Lists
  a               Add to queue
  A               Play next
  m               Context menu
  d               Remove from queue (Queue tab)
  J / K           Reorder queue

Account
  L               Account menu
  L (in menu)     Log out (when signed in)
  Enter (menu)    Sign in / Log out
  P (menu)        Paste cookie"#
}
