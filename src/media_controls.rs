use crate::action::Action;
use crate::api::model::Track;
use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{info, warn};

pub struct MediaControlsBridge {
    controls: Option<MediaControls>,
    pending: Arc<Mutex<Vec<Action>>>,
}

impl MediaControlsBridge {
    pub fn new() -> Self {
        let pending = Arc::new(Mutex::new(Vec::new()));
        let controls = match init_controls(pending.clone()) {
            Ok(c) => {
                info!("media controls initialized");
                Some(c)
            }
            Err(e) => {
                warn!("media controls unavailable: {e}");
                None
            }
        };
        Self { controls, pending }
    }

    pub fn drain_actions(&self) -> Vec<Action> {
        self.pending
            .lock()
            .map(|mut v| std::mem::take(&mut *v))
            .unwrap_or_default()
    }

    pub fn set_metadata(&mut self, track: &Track, cover_url: Option<&str>) {
        let Some(ref mut c) = self.controls else {
            return;
        };
        let artist = track.artist_names();
        let album = track.album.as_ref().map(|a| a.name.as_str());
        let duration = track.duration;
        let _ = c.set_metadata(MediaMetadata {
            title: Some(&track.title),
            artist: Some(&artist),
            album,
            cover_url,
            duration,
        });
    }

    pub fn set_playing(&mut self, progress: Duration) {
        if let Some(ref mut c) = self.controls {
            let _ = c.set_playback(MediaPlayback::Playing {
                progress: Some(MediaPosition(progress)),
            });
        }
    }

    pub fn set_paused(&mut self, progress: Duration) {
        if let Some(ref mut c) = self.controls {
            let _ = c.set_playback(MediaPlayback::Paused {
                progress: Some(MediaPosition(progress)),
            });
        }
    }

    pub fn set_stopped(&mut self) {
        if let Some(ref mut c) = self.controls {
            let _ = c.set_playback(MediaPlayback::Stopped);
        }
    }
}

fn init_controls(pending: Arc<Mutex<Vec<Action>>>) -> Result<MediaControls, souvlaki::Error> {
    let config = PlatformConfig {
        dbus_name: "yt_tui",
        display_name: "yt-tui",
        hwnd: None,
    };
    let mut controls = MediaControls::new(config)?;
    controls.attach(move |event| {
        let action = match event {
            MediaControlEvent::Toggle => Some(Action::PlayPause),
            MediaControlEvent::Play => Some(Action::Play),
            MediaControlEvent::Pause => Some(Action::Pause),
            MediaControlEvent::Next => Some(Action::Next),
            MediaControlEvent::Previous => Some(Action::Previous),
            MediaControlEvent::Stop => Some(Action::Pause),
            MediaControlEvent::SetPosition(pos) => Some(Action::SeekTo(pos.0)),
            MediaControlEvent::Seek(dir) => match dir {
                souvlaki::SeekDirection::Forward => {
                    Some(Action::SeekForward(Duration::from_secs(5)))
                }
                souvlaki::SeekDirection::Backward => {
                    Some(Action::SeekBack(Duration::from_secs(5)))
                }
            },
            MediaControlEvent::SeekBy(dir, dur) => match dir {
                souvlaki::SeekDirection::Forward => Some(Action::SeekForward(dur)),
                souvlaki::SeekDirection::Backward => Some(Action::SeekBack(dur)),
            },
            _ => None,
        };
        if let Some(a) = action {
            if let Ok(mut q) = pending.lock() {
                q.push(a);
            }
        }
    })?;
    Ok(controls)
}

/// Pump the macOS CFRunLoop so souvlaki media key events are delivered.
/// Call from the main thread in a loop until `shutdown` is set.
#[cfg(target_os = "macos")]
pub fn pump_run_loop(shutdown: Arc<std::sync::atomic::AtomicBool>) {
    use core_foundation::runloop::{
        kCFRunLoopDefaultMode, CFRunLoopGetCurrent, CFRunLoopRunInMode,
    };
    use std::sync::atomic::Ordering;
    while !shutdown.load(Ordering::Relaxed) {
        unsafe {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.05, 0);
        }
        let _ = CFRunLoopGetCurrent;
    }
}

#[cfg(not(target_os = "macos"))]
pub fn pump_run_loop(shutdown: Arc<std::sync::atomic::AtomicBool>) {
    use std::sync::atomic::Ordering;
    while !shutdown.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(50));
    }
}
