use crate::action::PlayerEvent;
use crate::api::model::Track;
use crate::player::eq::{EqHandle, EqParams, EqSource};
use crate::player::source::{CancelToken, GrowingFile, SymphoniaSource};
use crossbeam_channel::{Receiver, Sender};
use rodio::{OutputStream, Sink, Source};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{error, info};

pub enum EngineCmd {
    /// Decoder is opened by the caller: probing waits on the download, and the
    /// engine thread must never block on network I/O.
    Load {
        track: Track,
        file: Arc<GrowingFile>,
        source: SymphoniaSource,
        start: Duration,
        volume: f32,
        cancel: CancelToken,
    },
    Play,
    Pause,
    Toggle,
    Seek(Duration),
    SetVolume(f32),
    SetEq(EqParams),
    Stop,
    /// Like `Stop`, but silent: a new track is already loading.
    Unload,
    Shutdown,
}

/// Shared ring of recent mono PCM for the visualizer.
/// Audio thread appends; UI thread snapshots (does not clear) so FFT always has history.
#[derive(Clone, Default)]
pub struct SampleBuffer {
    inner: Arc<Mutex<Vec<f32>>>,
}

impl SampleBuffer {
    const CAP: usize = 4096;

    pub fn push_many(&self, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }
        if let Ok(mut buf) = self.inner.lock() {
            buf.extend_from_slice(samples);
            if buf.len() > Self::CAP {
                let drain = buf.len() - Self::CAP;
                buf.drain(..drain);
            }
        }
    }

    /// Copy the current ring into `out` without clearing the ring.
    pub fn snapshot(&self, out: &mut Vec<f32>) {
        out.clear();
        if let Ok(buf) = self.inner.lock() {
            out.extend_from_slice(&buf);
        }
    }

    /// Drain the ring (tests / legacy). Prefer [`snapshot`] for the UI.
    pub fn take(&self, out: &mut Vec<f32>) {
        out.clear();
        if let Ok(mut buf) = self.inner.lock() {
            out.append(&mut buf);
        }
    }
}

pub struct EngineHandle {
    pub cmd_tx: Sender<EngineCmd>,
    pub samples: SampleBuffer,
    pub eq: EqHandle,
}

impl EngineHandle {
    pub fn spawn(event_tx: Sender<PlayerEvent>) -> Self {
        Self::spawn_with_samples(event_tx, SampleBuffer::default(), EqHandle::default())
    }

    /// Share `samples` with the UI so the visualizer can read PCM without
    /// waiting on player-command handling.
    pub fn spawn_with_samples(
        event_tx: Sender<PlayerEvent>,
        samples: SampleBuffer,
        eq: EqHandle,
    ) -> Self {
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let samples_thread = samples.clone();
        let eq_thread = eq.clone();

        std::thread::Builder::new()
            .name("yt-tui-audio".into())
            .spawn(move || {
                if let Err(e) = run_engine(cmd_rx, event_tx, samples_thread, eq_thread) {
                    error!("audio engine exited: {e}");
                }
            })
            .expect("spawn audio thread");

        Self { cmd_tx, samples, eq }
    }

    pub fn send(&self, cmd: EngineCmd) {
        let _ = self.cmd_tx.send(cmd);
    }

    pub fn take_samples(&self, out: &mut Vec<f32>) {
        self.samples.take(out);
    }
}

fn run_engine(
    cmd_rx: Receiver<EngineCmd>,
    event_tx: Sender<PlayerEvent>,
    samples: SampleBuffer,
    eq: EqHandle,
) -> anyhow::Result<()> {
    let (_stream, handle) = OutputStream::try_default()?;
    let sink = Sink::try_new(&handle)?;
    sink.pause();

    let mut current_track: Option<Track> = None;
    let mut current_file: Option<Arc<GrowingFile>> = None;
    let mut current_cancel: Option<CancelToken> = None;
    let sink_pos = Arc::new(Mutex::new(Duration::ZERO));
    let playing = Arc::new(Mutex::new(false));

    let event_tx2 = event_tx.clone();
    let sink_pos2 = sink_pos.clone();
    let playing2 = playing.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(250));
        if *playing2.lock().unwrap() {
            let pos = *sink_pos2.lock().unwrap();
            let _ = event_tx2.send(PlayerEvent::Position(pos));
        }
    });

    let mut last_empty_check = std::time::Instant::now();

    loop {
        let cmd = cmd_rx.recv_timeout(Duration::from_millis(50));
        match cmd {
            Ok(EngineCmd::Load {
                track,
                file,
                source,
                start,
                volume,
                cancel,
            }) => {
                // Superseded while in flight — a newer Load/Unload follows.
                if cancel.is_cancelled() {
                    continue;
                }
                halt(&sink, &mut current_cancel);
                current_file = Some(file);
                current_cancel = Some(cancel);
                let decoded_dur = source.total_duration();
                let eqd = EqSource::new(source, eq.clone());
                let tapped = DirectTap::new(eqd, samples.clone());
                sink.append(tapped);
                sink.set_volume(volume * volume);
                if start > Duration::ZERO {
                    let _ = sink.try_seek(start);
                }
                sink.play();
                *playing.lock().unwrap() = true;
                let mut track = track;
                if track.duration.is_none() {
                    track.duration = decoded_dur;
                }
                current_track = Some(track.clone());
                let _ = event_tx.send(PlayerEvent::Playing {
                    track: track.clone(),
                });
                if let Some(d) = track.duration.or(decoded_dur) {
                    let _ = event_tx.send(PlayerEvent::Duration(d));
                }
            }
            Ok(EngineCmd::Play) => {
                sink.play();
                *playing.lock().unwrap() = true;
                let _ = event_tx.send(PlayerEvent::Resumed);
            }
            Ok(EngineCmd::Pause) => {
                sink.pause();
                *playing.lock().unwrap() = false;
                let _ = event_tx.send(PlayerEvent::Paused);
            }
            Ok(EngineCmd::Toggle) => {
                if sink.is_paused() {
                    sink.play();
                    *playing.lock().unwrap() = true;
                    let _ = event_tx.send(PlayerEvent::Resumed);
                } else {
                    sink.pause();
                    *playing.lock().unwrap() = false;
                    let _ = event_tx.send(PlayerEvent::Paused);
                }
            }
            Ok(EngineCmd::Seek(pos)) => {
                if let Err(e) = sink.try_seek(pos) {
                    info!("seek failed: {e}");
                }
                *sink_pos.lock().unwrap() = pos;
                let _ = event_tx.send(PlayerEvent::Position(pos));
            }
            Ok(EngineCmd::SetVolume(v)) => {
                sink.set_volume(v * v);
                let _ = event_tx.send(PlayerEvent::Volume(v));
            }
            Ok(EngineCmd::SetEq(params)) => {
                eq.set(params);
            }
            Ok(cmd @ (EngineCmd::Stop | EngineCmd::Unload)) => {
                halt(&sink, &mut current_cancel);
                *playing.lock().unwrap() = false;
                current_track = None;
                current_file = None;
                if matches!(cmd, EngineCmd::Stop) {
                    let _ = event_tx.send(PlayerEvent::Stopped);
                }
            }
            Ok(EngineCmd::Shutdown) => {
                if let Some(c) = current_cancel.take() {
                    c.cancel();
                }
                sink.stop();
                break;
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }

        let pos = sink.get_pos();
        *sink_pos.lock().unwrap() = pos;

        if last_empty_check.elapsed() > Duration::from_millis(200) {
            last_empty_check = std::time::Instant::now();
            // A cancelled stream ends on purpose — not a natural track end.
            let cancelled = current_cancel.as_ref().is_some_and(|c| c.is_cancelled());
            if *playing.lock().unwrap() && sink.empty() && current_track.is_some() && !cancelled {
                *playing.lock().unwrap() = false;
                let title = current_track
                    .as_ref()
                    .map(|t| t.title.clone())
                    .unwrap_or_default();
                current_track = None;
                // Mid-download CDN/auth failure used to look like a clean end →
                // playlist auto-advanced after ~few seconds of audio.
                let failed = current_file
                    .as_ref()
                    .map(|f| f.is_failed())
                    .unwrap_or(false);
                current_file = None;
                if failed {
                    let _ = event_tx.send(PlayerEvent::Error(format!(
                        "Audio stream ended early for {title}"
                    )));
                } else {
                    let _ = event_tx.send(PlayerEvent::Ended);
                }
            }
        }
    }
    Ok(())
}

/// Drop the current stream. The cancel must come first: `Sink::clear` waits for
/// the audio callback to release the source, and that callback may be blocked
/// reading a download that has stalled.
fn halt(sink: &Sink, current_cancel: &mut Option<CancelToken>) {
    if let Some(c) = current_cancel.take() {
        c.cancel();
    }
    sink.stop();
    sink.clear();
}

struct DirectTap<S> {
    inner: S,
    channels: u16,
    samples: SampleBuffer,
    frame: Vec<f32>,
    batch: Vec<f32>,
}

impl<S: rodio::Source<Item = f32>> DirectTap<S> {
    fn new(inner: S, samples: SampleBuffer) -> Self {
        let channels = inner.channels().max(1);
        Self {
            inner,
            channels,
            samples,
            frame: Vec::with_capacity(channels as usize),
            batch: Vec::with_capacity(64),
        }
    }
}

impl<S: rodio::Source<Item = f32>> Iterator for DirectTap<S> {
    type Item = f32;
    fn next(&mut self) -> Option<Self::Item> {
        let sample = self.inner.next()?;
        // Channel count can change after the first decoded packet.
        let ch = self.inner.channels().max(1);
        if ch != self.channels {
            self.channels = ch;
            self.frame.clear();
        }
        self.frame.push(sample);
        if self.frame.len() >= self.channels as usize {
            let mono = if self.channels == 1 {
                self.frame[0]
            } else {
                self.frame.iter().sum::<f32>() / self.channels as f32
            };
            self.batch.push(mono);
            // Push often so the UI sees fresh energy even if ticks are sparse.
            if self.batch.len() >= 32 {
                self.samples.push_many(&self.batch);
                self.batch.clear();
            }
            self.frame.clear();
        }
        Some(sample)
    }
}

impl<S: rodio::Source<Item = f32>> rodio::Source for DirectTap<S> {
    fn current_frame_len(&self) -> Option<usize> {
        self.inner.current_frame_len()
    }
    fn channels(&self) -> u16 {
        self.inner.channels()
    }
    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }
    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        self.inner.try_seek(pos)
    }
}

#[cfg(test)]
mod viz_feed_tests {
    use super::*;
    use crate::player::source::open_decoder;
    use crate::visualizer::{Visualizer, VisualizerStyle};
    use std::path::PathBuf;

    #[test]
    fn direct_tap_feeds_visualizer() {
        let path = PathBuf::from(
            "/Users/francoisegner/Library/Caches/com.yt-tui.yt-tui/audio/3QY9AzTHoio.m4a",
        );
        if !path.exists() {
            return;
        }
        let gf = GrowingFile::open_complete(&path).unwrap();
        let source = open_decoder(gf, &CancelToken::default()).unwrap();
        let buf = SampleBuffer::default();
        let mut tapped = DirectTap::new(source, buf.clone());
        let ch = tapped.channels() as usize;
        // Pull ~200ms of audio at typical rates
        let n = tapped.by_ref().take(44100 / 5 * ch.max(1)).count();
        assert!(n > 1000, "expected samples from decoder, got {n}");
        let mut out = Vec::new();
        buf.snapshot(&mut out);
        assert!(!out.is_empty(), "DirectTap wrote no mono samples");
        let peak = out.iter().copied().map(f32::abs).fold(0.0f32, f32::max);
        eprintln!("mono samples={}, peak={peak}, ch={ch}", out.len());
        let mut viz = Visualizer::new(VisualizerStyle::Bars);
        viz.set_band_count(48);
        viz.update(&out, true);
        let levels = viz.band_levels();
        let max_band = levels.iter().copied().fold(0.0f32, f32::max);
        let min_band = levels.iter().copied().fold(1.0f32, f32::min);
        let sum: f32 = levels.iter().sum();
        eprintln!(
            "max_band={max_band} min_band={min_band} sum={sum} line={}",
            viz.render_line(40)
        );
        assert!(max_band > 0.05, "visualizer bands flat: max={max_band}");
        // AGC should leave some shape, not a solid wall of 1.0.
        assert!(min_band < 0.95, "visualizer saturated: min={min_band}");
    }
}
