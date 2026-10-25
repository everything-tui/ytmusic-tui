use anyhow::{Context, Result};
use parking_lot::{Condvar, Mutex, MutexGuard};
use ringbuf::traits::{Producer, Split};
use ringbuf::{HeapProd, HeapRb};
use rodio::Source;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug)]
struct GrowingState {
    len: u64,
    expected: Option<u64>,
    complete: bool,
    failed: Option<String>,
    path: PathBuf,
    /// Kept open for the writer so rename of `.part` → final cannot cause ENOENT mid-append.
    writer: Option<File>,
    /// Bumped when a download task is superseded so stale writers stop cleanly.
    generation: u64,
}

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

/// How often blocked readers wake up to check their [`CancelToken`].
const CANCEL_POLL: Duration = Duration::from_millis(50);

/// Set when the playback that owns a reader is superseded, so reads blocked on
/// a slow or stalled download give up instead of hanging the audio thread.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Block until `ready` holds (downloads always notify on progress/finish),
/// bailing out once `cancel` fires.
fn wait_state<'a>(
    inner: &'a (Mutex<GrowingState>, Condvar),
    cancel: &CancelToken,
    ready: impl Fn(&GrowingState) -> bool,
) -> std::io::Result<MutexGuard<'a, GrowingState>> {
    let (lock, cv) = inner;
    let mut g = lock.lock();
    while !ready(&g) {
        if cancel.is_cancelled() {
            return Err(std::io::Error::other("playback cancelled"));
        }
        cv.wait_for(&mut g, CANCEL_POLL);
    }
    Ok(g)
}

/// A file that grows as a background download writes to it.
/// Readers block (via Condvar) until the requested bytes are available.
#[derive(Clone)]
pub struct GrowingFile {
    inner: Arc<(Mutex<GrowingState>, Condvar)>,
}

impl GrowingFile {
    pub fn create(path: &Path) -> Result<Arc<Self>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Truncate/create, then reopen for append so the writer holds a stable fd.
        File::create(path)?;
        let writer = OpenOptions::new().append(true).open(path)?;
        let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
        Ok(Arc::new(Self {
            inner: Arc::new((
                Mutex::new(GrowingState {
                    len: 0,
                    expected: None,
                    complete: false,
                    failed: None,
                    path: path.to_path_buf(),
                    writer: Some(writer),
                    generation,
                }),
                Condvar::new(),
            )),
        }))
    }

    pub fn open_complete(path: &Path) -> Result<Arc<Self>> {
        let meta = std::fs::metadata(path)?;
        Ok(Arc::new(Self {
            inner: Arc::new((
                Mutex::new(GrowingState {
                    len: meta.len(),
                    expected: Some(meta.len()),
                    complete: true,
                    failed: None,
                    path: path.to_path_buf(),
                    writer: None,
                    generation: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed),
                }),
                Condvar::new(),
            )),
        }))
    }

    pub fn path(&self) -> PathBuf {
        self.inner.0.lock().path.clone()
    }

    pub fn generation(&self) -> u64 {
        self.inner.0.lock().generation
    }

    /// True if this handle was replaced by a newer download attempt.
    pub fn is_stale_generation(&self, gen: u64) -> bool {
        self.inner.0.lock().generation != gen
    }

    /// After the `.part` file is renamed into the cache, keep the logical path in sync
    /// so later reopen attempts don't hit ENOENT on the old name.
    pub fn set_path(&self, path: PathBuf) {
        let mut g = self.inner.0.lock();
        // Drop the writer fd — it still refers to the inode after rename, but we
        // should not append further once the file is finalized.
        g.writer = None;
        g.path = path;
    }

    /// Point this handle at a finished cache file (e.g. after a twin download won the rename).
    pub fn adopt_complete_file(&self, path: &Path) -> Result<()> {
        let meta = std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
        let mut g = self.inner.0.lock();
        g.writer = None;
        g.path = path.to_path_buf();
        g.len = meta.len();
        g.expected = Some(meta.len());
        g.complete = true;
        g.failed = None;
        self.inner.1.notify_all();
        Ok(())
    }

    pub fn append(&self, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let mut guard = self.inner.0.lock();
        if guard.complete && guard.failed.is_none() && guard.writer.is_none() {
            // Already finalized (e.g. twin finished) — ignore late writes.
            return Ok(());
        }
        if guard.writer.is_none() {
            // Re-open if needed (should be rare).
            let w = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&guard.path)
                .with_context(|| format!("reopen append {}", guard.path.display()))?;
            guard.writer = Some(w);
        }
        {
            let writer = guard.writer.as_mut().unwrap();
            writer.write_all(data)?;
            let _ = writer.flush();
        }
        guard.len += data.len() as u64;
        self.inner.1.notify_all();
        Ok(())
    }

    pub fn set_expected_len(&self, len: u64) {
        self.inner.0.lock().expected = Some(len);
    }

    pub fn mark_complete(&self) {
        let mut g = self.inner.0.lock();
        g.complete = true;
        g.expected = Some(g.len);
        g.writer = None;
        self.inner.1.notify_all();
    }

    pub fn set_failed(&self, err: String) {
        let mut g = self.inner.0.lock();
        // Don't poison a handle that already adopted a finished file.
        if g.complete && g.failed.is_none() && g.len > 0 && g.writer.is_none() {
            return;
        }
        g.failed = Some(err);
        g.complete = true;
        g.writer = None;
        self.inner.1.notify_all();
    }

    pub fn is_failed(&self) -> bool {
        self.inner.0.lock().failed.is_some()
    }

    pub fn is_complete(&self) -> bool {
        self.inner.0.lock().complete
    }

    pub fn len(&self) -> u64 {
        self.inner.0.lock().len
    }

    pub fn wait_bytes(&self, min: u64) -> Result<()> {
        self.wait_bytes_cancellable(min, &CancelToken::default())
    }

    pub fn wait_bytes_cancellable(&self, min: u64, cancel: &CancelToken) -> Result<()> {
        let g = wait_state(&self.inner, cancel, |g| g.len >= min || g.complete)?;
        if let Some(ref e) = g.failed {
            anyhow::bail!("download failed: {e}");
        }
        if g.len == 0 {
            anyhow::bail!("empty audio file");
        }
        Ok(())
    }

    pub fn reader(&self, cancel: CancelToken) -> GrowingFileReader {
        GrowingFileReader {
            inner: self.inner.clone(),
            cancel,
            probing: Arc::new(AtomicBool::new(false)),
            pos: 0,
            file: None,
            opened_path: None,
        }
    }
}

pub struct GrowingFileReader {
    inner: Arc<(Mutex<GrowingState>, Condvar)>,
    cancel: CancelToken,
    /// While set, an unfinished download reports itself unseekable (see `is_seekable`).
    probing: Arc<AtomicBool>,
    pos: u64,
    file: Option<File>,
    opened_path: Option<PathBuf>,
}

impl GrowingFileReader {
    fn is_fully_buffered(&self) -> bool {
        let g = self.inner.0.lock();
        g.complete && g.failed.is_none()
    }

    fn ensure_file(&mut self) -> std::io::Result<&mut File> {
        // Retry briefly: rename `.part` → final can race the first open.
        let mut last_err = None;
        for _ in 0..40 {
            let path = self.inner.0.lock().path.clone();
            let need_reopen = match &self.opened_path {
                Some(p) => p != &path || self.file.is_none(),
                None => true,
            };
            if !need_reopen {
                break;
            }
            match File::open(&path) {
                Ok(f) => {
                    self.file = Some(f);
                    self.opened_path = Some(path);
                    last_err = None;
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    last_err = Some(e);
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => return Err(e),
            }
        }
        self.file.as_mut().ok_or_else(|| {
            last_err.unwrap_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "audio file missing")
            })
        })
    }

    fn wait_for(&self, need: u64) -> std::io::Result<()> {
        let guard = wait_state(&self.inner, &self.cancel, |g| g.len >= need || g.complete)?;
        if let Some(ref err) = guard.failed {
            return Err(std::io::Error::other(err.clone()));
        }
        Ok(())
    }
}

impl Read for GrowingFileReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let need = self.pos + 1;
        self.wait_for(need)?;
        let available = self.inner.0.lock().len;
        if self.pos >= available {
            return Ok(0);
        }
        let pos = self.pos;
        let to_read = ((available - pos) as usize).min(buf.len());
        self.ensure_file()?;
        let file = self.file.as_mut().unwrap();
        file.seek(SeekFrom::Start(pos))?;
        let n = file.read(&mut buf[..to_read])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for GrowingFileReader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let new_pos = match pos {
            SeekFrom::Start(p) => p,
            SeekFrom::End(p) => {
                // Never invent an end offset from `expected` alone — Symphonia/MP4
                // will try to read there and rodio panics on the resulting SeekError.
                let guard =
                    wait_state(&self.inner, &self.cancel, |g| g.complete || g.failed.is_some())?;
                if let Some(ref err) = guard.failed {
                    return Err(std::io::Error::other(err.clone()));
                }
                let len = guard.len;
                if p < 0 {
                    len.saturating_sub((-p) as u64)
                } else {
                    len + p as u64
                }
            }
            SeekFrom::Current(p) => {
                if p < 0 {
                    self.pos.saturating_sub((-p) as u64)
                } else {
                    self.pos + p as u64
                }
            }
        };
        self.wait_for(new_pos.saturating_add(1).max(1))?;
        self.pos = new_pos;
        Ok(self.pos)
    }
}

/// Forwards samples unchanged and pushes a mono downmix into a ring buffer for the visualizer.
pub struct TapSource<S> {
    inner: S,
    channels: u16,
    prod: HeapProd<f32>,
    frame: Vec<f32>,
}

impl<S> TapSource<S>
where
    S: Source<Item = f32>,
{
    pub fn new(inner: S, prod: HeapProd<f32>) -> Self {
        let channels = inner.channels();
        Self {
            inner,
            channels,
            prod,
            frame: Vec::with_capacity(channels as usize),
        }
    }
}

impl<S> Iterator for TapSource<S>
where
    S: Source<Item = f32>,
{
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        let sample = self.inner.next()?;
        self.frame.push(sample);
        if self.frame.len() >= self.channels as usize {
            let mono = if self.channels == 1 {
                self.frame[0]
            } else {
                self.frame.iter().sum::<f32>() / self.channels as f32
            };
            let _ = self.prod.try_push(mono);
            self.frame.clear();
        }
        Some(sample)
    }
}

impl<S> Source for TapSource<S>
where
    S: Source<Item = f32>,
{
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

pub fn make_visualizer_ring(capacity: usize) -> (HeapProd<f32>, ringbuf::HeapCons<f32>) {
    let rb = HeapRb::<f32>::new(capacity);
    rb.split()
}

impl symphonia::core::io::MediaSource for GrowingFileReader {
    fn is_seekable(&self) -> bool {
        let g = self.inner.0.lock();
        // A seekable probe scans every top-level atom, i.e. waits for the whole
        // download — minutes when googlevideo throttles to ~realtime. Probe in
        // streaming mode instead; packets still seek once probing is done.
        if self.probing.load(Ordering::SeqCst) && !g.complete {
            return false;
        }
        // YouTube DASH/fMP4 carries a segment index with absolute packet offsets.
        // Symphonia needs a seekable MediaSource (with known byte_len) to jump to
        // those offsets while the file is still growing — otherwise demux dies with
        // "packet out-of-bounds for a non-seekable stream" after a few seconds.
        // Seek/Read on GrowingFileReader already block until the bytes exist.
        // Never claim seekable without a length (rodio/Symphonia panic on that).
        g.failed.is_none() && (g.complete || g.expected.is_some_and(|n| n > 0))
    }

    fn byte_len(&self) -> Option<u64> {
        let g = self.inner.0.lock();
        if g.failed.is_some() {
            return None;
        }
        if g.complete {
            Some(g.len)
        } else {
            g.expected.filter(|&n| n > 0)
        }
    }
}

/// Bytes of slack past a seek target's estimated offset before the seek is
/// attempted (~4s of 128 kbps AAC), so it rarely blocks on the download.
const SEEK_MARGIN_BYTES: u64 = 64 * 1024;

#[derive(Default)]
struct StreamControlInner {
    seek: Mutex<Option<Duration>>,
    position_ms: AtomicU64,
}

/// Shared between the engine and a playing [`SymphoniaSource`]: seek requests
/// go in (latest wins), the playback position comes out. Seeks run on the audio
/// thread, so the engine never blocks the way `Sink::try_seek` does.
#[derive(Clone, Default)]
pub struct StreamControl(Arc<StreamControlInner>);

impl StreamControl {
    pub fn seek(&self, pos: Duration) {
        *self.0.seek.lock() = Some(pos);
        self.set_position(pos);
    }

    pub fn position(&self) -> Duration {
        Duration::from_millis(self.0.position_ms.load(Ordering::Relaxed))
    }

    fn set_position(&self, pos: Duration) {
        self.0.position_ms.store(pos.as_millis() as u64, Ordering::Relaxed);
    }

    fn take_seek(&self) -> Option<Duration> {
        self.0.seek.lock().take()
    }
}

/// What is actually being decoded, for the quality badge.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamInfo {
    pub codec: &'static str,
    pub sample_rate: u32,
    /// Full file size, once known — with the duration it gives the average bitrate.
    pub bytes: Option<u64>,
}

/// Bitrates YouTube serves AAC at (itags 139/140/141). Opus is VBR, so its
/// average is shown as measured.
const NOMINAL_KBPS: [u32; 3] = [48, 128, 256];

/// Lowest average Opus bitrate counted as the Premium tier (itag 774 averages
/// ~230–280 kbps; the free itag 251 stays under ~170).
const PREMIUM_OPUS_KBPS: u32 = 200;

/// Bitrate in AAC terms, for comparing renditions across codecs: Opus matches
/// AAC's quality at roughly 70% of the bitrate.
pub fn perceived_kbps(codec: &str, kbps: u32) -> u32 {
    if codec == "Opus" {
        kbps * 10 / 7
    } else {
        kbps
    }
}

impl StreamInfo {
    /// Average kbps over `duration`; for AAC snapped to a nominal rate when it's
    /// close (container overhead adds a few percent).
    pub fn kbps(&self, duration: Duration) -> Option<u32> {
        let secs = duration.as_secs_f64();
        if secs < 1.0 {
            return None;
        }
        let kbps = self.bytes? as f64 * 8.0 / secs / 1000.0;
        // Larger means a muxed video file (itag 18); the figure would be meaningless.
        if !(8.0..=400.0).contains(&kbps) {
            return None;
        }
        let nominal = (self.codec == "AAC")
            .then(|| NOMINAL_KBPS.into_iter().find(|&n| (kbps / n as f64 - 1.0).abs() <= 0.1))
            .flatten();
        Some(nominal.unwrap_or(kbps.round() as u32))
    }

    /// `AAC 256 kbps · 44.1 kHz`, dropping parts that are unknown or don't fit
    /// (down to just `256 kbps`).
    pub fn label(&self, duration: Option<Duration>, max_width: usize) -> String {
        let rate = self.kbps(duration.unwrap_or_default());
        let base = match rate {
            Some(k) => format!("{} {k} kbps", self.codec),
            None => self.codec.to_string(),
        };
        let khz = self.sample_rate as f64 / 1000.0;
        let full = format!("{base} · {} kHz", (khz * 10.0).round() / 10.0);
        let short = rate.map(|k| format!("{k} kbps")).unwrap_or_default();
        [full, base, short]
            .into_iter()
            .find(|l| l.chars().count() <= max_width)
            .unwrap_or_default()
    }

    /// The Premium tier: AAC 256 kbps or Opus at a comparable rate.
    pub fn is_high(&self, duration: Option<Duration>) -> bool {
        let min = if self.codec == "Opus" { PREMIUM_OPUS_KBPS } else { 256 };
        self.kbps(duration.unwrap_or_default()).is_some_and(|k| k >= min)
    }
}

fn codec_name(codec: symphonia::core::codecs::CodecType) -> &'static str {
    use symphonia::core::codecs::*;
    match codec {
        CODEC_TYPE_AAC => "AAC",
        CODEC_TYPE_OPUS => "Opus",
        CODEC_TYPE_VORBIS => "Vorbis",
        CODEC_TYPE_MP3 => "MP3",
        CODEC_TYPE_FLAC => "FLAC",
        CODEC_TYPE_ALAC => "ALAC",
        _ => "Audio",
    }
}

/// Symphonia-backed audio source. Avoids `rodio::Decoder`, which wraps readers in a
/// MediaSource that is always seekable but never reports `byte_len` — that combination
/// panics inside rodio on YouTube's DASH/fMP4 streams.
pub struct SymphoniaSource {
    format: Box<dyn symphonia::core::formats::FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    track_id: u32,
    sample_buf: Option<symphonia::core::audio::SampleBuffer<f32>>,
    spec: symphonia::core::audio::SignalSpec,
    cursor: usize,
    total_duration: Option<Duration>,
    exhausted: bool,
    file: Arc<(Mutex<GrowingState>, Condvar)>,
    time_base: Option<symphonia::core::units::TimeBase>,
    ctl: StreamControl,
    /// Track length used to map a seek target to a byte offset.
    duration_hint: Option<Duration>,
    /// Seek waiting for its bytes to download; silence plays meanwhile.
    pending_seek: Option<Duration>,
    /// Samples left before the pending seek is checked again.
    recheck_in: u32,
    codec: &'static str,
}

impl SymphoniaSource {
    pub fn open(reader: GrowingFileReader, extension: &str) -> Result<Self> {
        use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
        use symphonia::core::formats::FormatOptions;
        use symphonia::core::io::MediaSourceStream;
        use symphonia::core::meta::MetadataOptions;
        use symphonia::core::probe::Hint;

        // Gapless probe may touch the end of the file; only enable it once the
        // download is finished. Progressive DASH still advertises seekability via
        // MediaSource when `expected` length is known (see GrowingFileReader).
        let gapless = reader.is_fully_buffered();
        let file = reader.inner.clone();
        let probing = reader.probing.clone();
        probing.store(true, Ordering::SeqCst);
        let mss = MediaSourceStream::new(Box::new(reader), Default::default());
        let mut hint = Hint::new();
        hint.with_extension(extension);
        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                mss,
                &FormatOptions {
                    enable_gapless: gapless,
                    ..Default::default()
                },
                &MetadataOptions::default(),
            )
            .context("probing audio format");
        probing.store(false, Ordering::SeqCst);
        let probed = probed?;

        let track = probed
            .format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .cloned()
            .context("no supported audio track")?;
        let track_id = track.id;
        let decoder = crate::player::opus::codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .context("creating audio decoder")?;
        let total_duration = track
            .codec_params
            .time_base
            .zip(track.codec_params.n_frames)
            .map(|(base, frames)| {
                let t = base.calc_time(frames);
                Duration::new(t.seconds, (t.frac * 1e9) as u32)
            });

        // Placeholder spec until the first packet is decoded.
        let spec = symphonia::core::audio::SignalSpec {
            rate: track.codec_params.sample_rate.unwrap_or(44100),
            channels: track
                .codec_params
                .channels
                .unwrap_or(
                    symphonia::core::audio::Channels::FRONT_LEFT
                        | symphonia::core::audio::Channels::FRONT_RIGHT,
                ),
        };

        let mut source = Self {
            format: probed.format,
            decoder,
            track_id,
            sample_buf: None,
            spec,
            cursor: 0,
            total_duration,
            exhausted: false,
            file,
            time_base: track.codec_params.time_base,
            ctl: StreamControl::default(),
            duration_hint: total_duration,
            pending_seek: None,
            recheck_in: 0,
            codec: codec_name(track.codec_params.codec),
        };
        // Prime the first buffer so channels()/sample_rate() are accurate for rodio.
        if source.fill_buf().is_err() {
            // Empty/short files still construct; iterator will yield nothing.
            source.exhausted = true;
        }
        Ok(source)
    }

    fn fill_buf(&mut self) -> Result<()> {
        use symphonia::core::errors::Error as SymError;
        self.cursor = 0;
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    self.exhausted = true;
                    self.sample_buf = None;
                    return Ok(());
                }
                Err(SymError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(e) => return Err(anyhow::anyhow!("demux: {e}")),
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(audio) => {
                    self.spec = *audio.spec();
                    let frames = symphonia::core::units::Duration::from(audio.capacity() as u64);
                    let mut buf =
                        symphonia::core::audio::SampleBuffer::<f32>::new(frames, self.spec);
                    buf.copy_interleaved_ref(audio);
                    self.sample_buf = Some(buf);
                    if let Some(tb) = self.time_base {
                        let t = tb.calc_time(packet.ts());
                        self.ctl
                            .set_position(Duration::new(t.seconds, (t.frac * 1e9) as u32));
                    }
                    return Ok(());
                }
                Err(SymError::DecodeError(_)) => continue,
                Err(e) => return Err(anyhow::anyhow!("decode: {e}")),
            }
        }
    }
}

impl SymphoniaSource {
    pub fn stream_info(&self) -> StreamInfo {
        StreamInfo {
            codec: self.codec,
            sample_rate: self.spec.rate,
            bytes: self.file.0.lock().expected.filter(|&n| n > 0),
        }
    }

    pub fn control(&self) -> StreamControl {
        self.ctl.clone()
    }

    pub fn set_duration_hint(&mut self, duration: Option<Duration>) {
        if duration.is_some() {
            self.duration_hint = duration;
        }
    }

    /// Whether the bytes for `target` are (estimated to be) downloaded. Audio is
    /// near-CBR, so the offset scales with time; unknown sizes just try.
    fn seek_ready(&self, target: Duration) -> bool {
        let g = self.file.0.lock();
        if g.complete {
            return true;
        }
        let (Some(total), Some(dur)) = (g.expected, self.duration_hint) else {
            return true;
        };
        if dur.is_zero() {
            return true;
        }
        let frac = (target.as_secs_f64() / dur.as_secs_f64()).clamp(0.0, 1.0);
        let need = ((total as f64 * frac) as u64 + SEEK_MARGIN_BYTES).min(total);
        g.len >= need
    }

    fn apply_seek(&mut self, pos: Duration) {
        use symphonia::core::formats::{SeekMode, SeekTo};
        use symphonia::core::units::Time;

        let time = Time {
            seconds: pos.as_secs(),
            frac: f64::from(pos.subsec_nanos()) / 1_000_000_000.0,
        };
        let res = self.format.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time,
                track_id: Some(self.track_id),
            },
        );
        if let Err(e) = res {
            tracing::debug!("symphonia seek failed: {e}");
            return;
        }
        self.decoder.reset();
        self.exhausted = false;
        self.ctl.set_position(pos);
        if let Err(e) = self.fill_buf() {
            tracing::warn!("audio stream ended after seek: {e}");
            self.exhausted = true;
        }
    }
}

impl Iterator for SymphoniaSource {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(target) = self.pending_seek {
            if self.recheck_in > 0 {
                self.recheck_in -= 1;
                return Some(0.0);
            }
            if let Some(newer) = self.ctl.take_seek() {
                self.pending_seek = Some(newer);
                return self.next();
            }
            if !self.seek_ready(target) {
                // ~10ms at 48kHz stereo between checks.
                self.recheck_in = 1024;
                return Some(0.0);
            }
            self.pending_seek = None;
            self.apply_seek(target);
        }
        if self.exhausted {
            return None;
        }
        loop {
            if let Some(buf) = self.sample_buf.as_ref() {
                let samples = buf.samples();
                if self.cursor < samples.len() {
                    let s = samples[self.cursor];
                    self.cursor += 1;
                    return Some(s);
                }
            }
            // Packet boundary: pick up seeks posted by the engine.
            if let Some(target) = self.ctl.take_seek() {
                self.pending_seek = Some(target);
                self.sample_buf = None;
                self.cursor = 0;
                self.recheck_in = 0;
                return self.next();
            }
            match self.fill_buf() {
                Ok(()) if self.exhausted => return None,
                Ok(()) => continue,
                Err(e) => {
                    tracing::warn!("audio stream ended early: {e}");
                    self.exhausted = true;
                    return None;
                }
            }
        }
    }
}

impl Source for SymphoniaSource {
    fn current_frame_len(&self) -> Option<usize> {
        self.sample_buf
            .as_ref()
            .map(|b| b.samples().len().saturating_sub(self.cursor))
    }
    fn channels(&self) -> u16 {
        self.spec.channels.count() as u16
    }
    fn sample_rate(&self) -> u32 {
        self.spec.rate
    }
    fn total_duration(&self) -> Option<Duration> {
        self.total_duration
    }
    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        self.ctl.seek(pos);
        Ok(())
    }
}

/// Bytes needed before probing — enough for ftyp+moov/sidx on typical YouTube AAC.
pub const START_BUFFER_BYTES: u64 = 96 * 1024;

pub fn open_decoder(gf: Arc<GrowingFile>, cancel: &CancelToken) -> Result<SymphoniaSource> {
    gf.wait_bytes_cancellable(START_BUFFER_BYTES, cancel)?;
    // Symphonia detects the container (MP4 or WebM) from the bytes; the hint is
    // unused, and the second attempt is a plain retry with fresh state.
    match SymphoniaSource::open(gf.reader(cancel.clone()), "m4a") {
        Ok(src) => Ok(src),
        Err(e) => SymphoniaSource::open(gf.reader(cancel.clone()), "mp4")
            .with_context(|| format!("opening audio stream (first attempt: {e})")),
    }
}

/// Like `open_decoder`, but if the first probe fails while more data is still arriving,
/// wait for a larger buffer and try once more (avoids false "can't open" toasts).
pub fn open_decoder_resilient(gf: Arc<GrowingFile>, cancel: &CancelToken) -> Result<SymphoniaSource> {
    match open_decoder(gf.clone(), cancel) {
        Ok(src) => Ok(src),
        Err(first) => {
            if gf.is_failed() || cancel.is_cancelled() {
                return Err(first);
            }
            // Wait for more data (or completion), then retry with a fresh reader.
            let _ = gf.wait_bytes_cancellable(512 * 1024, cancel);
            if gf.is_failed() || cancel.is_cancelled() {
                return Err(first);
            }
            open_decoder(gf, cancel).map_err(|e| first.context(format!("retry also failed: {e}")))
        }
    }
}

#[cfg(test)]
mod decode_tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn stream_info_label() {
        let info = |bytes| StreamInfo { codec: "AAC", sample_rate: 44100, bytes };
        let d = Some(Duration::from_secs(211));
        // CT_J9G6LHfQ: itag 141 at 6,761,864 bytes over 3:31.
        let premium = info(Some(6_761_864));
        assert_eq!(premium.label(d, 40), "AAC 256 kbps · 44.1 kHz");
        assert_eq!(premium.label(d, 14), "AAC 256 kbps");
        assert_eq!(premium.label(d, 10), "256 kbps");
        assert!(premium.is_high(d));
        // itag 140 with a little container overhead.
        let standard = info(Some(211 * 131 * 1000 / 8));
        assert_eq!(standard.label(d, 40), "AAC 128 kbps · 44.1 kHz");
        assert!(!standard.is_high(d));
        // Off-tier rates show as measured; unknown size or duration drops the rate.
        assert_eq!(info(Some(211 * 96 * 1000 / 8)).label(d, 12), "AAC 96 kbps");
        assert_eq!(info(None).label(d, 40), "AAC · 44.1 kHz");
        assert_eq!(premium.label(None, 40), "AAC · 44.1 kHz");

        // Opus is VBR: shown as measured, never snapped; Premium from ~200 kbps.
        let opus = |kbps: u64| StreamInfo { codec: "Opus", sample_rate: 48000, bytes: Some(211 * kbps * 1000 / 8) };
        assert_eq!(opus(137).label(d, 40), "Opus 137 kbps · 48 kHz");
        assert!(!opus(150).is_high(d));
        assert!(opus(245).is_high(d));
        assert!(perceived_kbps("Opus", 150) > perceived_kbps("AAC", 128));
        assert!(perceived_kbps("Opus", 245) > perceived_kbps("AAC", 256));
    }

    fn sample_path() -> PathBuf {
        // Prefer a larger DASH m4a so progressive demux exercises the sidx path.
        let dir = PathBuf::from("/Users/francoisegner/Library/Caches/com.yt-tui.yt-tui/audio");
        let mut best: Option<(u64, PathBuf)> = None;
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("m4a") {
                    continue;
                }
                if p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(".part"))
                {
                    continue;
                }
                let len = e.metadata().map(|m| m.len()).unwrap_or(0);
                if len > best.as_ref().map(|(l, _)| *l).unwrap_or(0) {
                    best = Some((len, p));
                }
            }
        }
        best.map(|(_, p)| p).unwrap_or_else(|| dir.join("3QY9AzTHoio.m4a"))
    }

    #[test]
    fn cancel_unblocks_reader_on_stalled_download() {
        let dir = std::env::temp_dir().join("yt-tui-cancel-read");
        let gf = GrowingFile::create(&dir.join("stalled.m4a")).unwrap();
        gf.append(&[0u8; 16]).unwrap();
        let cancel = CancelToken::default();
        let mut reader = gf.reader(cancel.clone());
        let handle = std::thread::spawn(move || {
            let mut buf = [0u8; 64];
            let first = reader.read(&mut buf).unwrap();
            // Nothing more ever arrives — this read used to block forever.
            (first, reader.read(&mut buf).is_err())
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        cancel.cancel();
        let (first, cancelled) = handle.join().unwrap();
        assert_eq!(first, 16);
        assert!(cancelled);
    }

    #[test]
    fn open_does_not_wait_for_full_download() {
        let path = sample_path();
        if !path.exists() {
            return;
        }
        let data = std::fs::read(&path).unwrap();
        let dir = std::env::temp_dir().join("yt-tui-throttled-open");
        let gf = GrowingFile::create(&dir.join("throttled.m4a")).unwrap();
        // A download stuck after the first ~200 KB (throttled CDN).
        gf.append(&data[..data.len().min(200 * 1024)]).unwrap();
        gf.set_expected_len(data.len() as u64);

        let cancel = CancelToken::default();
        let (tx, rx) = std::sync::mpsc::channel();
        let gf2 = gf.clone();
        let cancel2 = cancel.clone();
        std::thread::spawn(move || {
            let n = open_decoder(gf2, &cancel2).map(|mut src| src.by_ref().take(4_000).count());
            let _ = tx.send(n.map_err(|e| e.to_string()));
        });
        let res = rx.recv_timeout(std::time::Duration::from_secs(5));
        cancel.cancel();
        let n = res.expect("open blocked waiting for the whole file").unwrap();
        assert!(n > 0);
    }

    #[test]
    fn seek_past_download_plays_silence_then_applies() {
        let path = sample_path();
        if !path.exists() {
            return;
        }
        let data = std::fs::read(&path).unwrap();
        let dir = std::env::temp_dir().join("yt-tui-seek-ahead");
        let gf = GrowingFile::create(&dir.join("seek.m4a")).unwrap();
        let head = data.len().min(200 * 1024);
        gf.append(&data[..head]).unwrap();
        gf.set_expected_len(data.len() as u64);

        let mut src = open_decoder(gf.clone(), &CancelToken::default()).unwrap();
        let dur = src.total_duration().unwrap_or(Duration::from_secs(180));
        src.set_duration_hint(Some(dur));
        let target = dur.mul_f64(0.8);
        let ctl = src.control();
        ctl.seek(target);

        // Target isn't downloaded: must not block, only silence.
        let silent = src.by_ref().take(4_096).all(|s| s == 0.0);
        assert!(silent);
        assert_eq!(ctl.position().as_secs(), target.as_secs());

        gf.append(&data[head..]).unwrap();
        gf.mark_complete();
        let n = src.by_ref().take(20_000).filter(|s| *s != 0.0).count();
        assert!(n > 0, "no audio after seek applied");
        let pos = ctl.position();
        assert!(
            pos + Duration::from_secs(2) >= target && pos <= target + Duration::from_secs(2),
            "position {pos:?} not near {target:?}"
        );
    }

    #[test]
    fn decode_complete_cached_dash_m4a() {
        let path = sample_path();
        if !path.exists() {
            return;
        }
        let gf = GrowingFile::open_complete(&path).unwrap();
        let mut src = open_decoder(gf, &CancelToken::default()).unwrap();
        assert!(src.channels() >= 1);
        assert!(src.sample_rate() >= 8_000);
        assert!(src.by_ref().take(4_000).count() > 0);
    }

    #[test]
    fn decode_partial_download_without_panic() {
        let path = sample_path();
        if !path.exists() {
            return;
        }
        let data = std::fs::read(&path).unwrap();
        let dir = std::env::temp_dir().join("yt-tui-partial-decode");
        let _ = std::fs::create_dir_all(&dir);
        let part = dir.join("partial.m4a");
        let gf = GrowingFile::create(&part).unwrap();
        // Small prefix reproduces the DASH sidx / non-seekable out-of-bounds bug.
        let first = data.len().min(96 * 1024);
        gf.append(&data[..first]).unwrap();
        gf.set_expected_len(data.len() as u64);

        let gf2 = gf.clone();
        let rest = data[first..].to_vec();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(80));
            for chunk in rest.chunks(64 * 1024) {
                gf2.append(chunk).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            gf2.mark_complete();
        });

        let mut src = open_decoder(gf, &CancelToken::default()).expect("partial open");
        // Drain well past the initial buffer — this used to die with
        // "packet out-of-bounds for a non-seekable stream".
        let n = src.by_ref().take(480_000).count();
        assert!(n > 200_000, "expected sustained progressive decode, got {n}");
    }
}
