//! Opus decoding for Symphonia, backed by the pure-Rust `rusty-opus` crate
//! (bit-exact with libopus on the RFC 8251 test vectors).
//!
//! YouTube serves its best non-Premium audio (itag 251, ~150 kbps) and a Premium
//! rendition (itag 774, ~256 kbps) as Opus in WebM. Symphonia demuxes WebM but has
//! no Opus decoder, so this one is registered alongside the built-in codecs.

use std::sync::OnceLock;
use symphonia::core::audio::{AsAudioBufferRef, AudioBuffer, AudioBufferRef, Channels, Signal, SignalSpec};
use symphonia::core::codecs::{
    CodecDescriptor, CodecParameters, CodecRegistry, Decoder, DecoderOptions, FinalizeResult,
    CODEC_TYPE_OPUS,
};
use symphonia::core::errors::{decode_error, unsupported_error, Result};
use symphonia::core::formats::Packet;
use symphonia::core::support_codec;

/// Opus always decodes at 48 kHz.
const RATE: u32 = 48_000;
/// Longest Opus packet: 120 ms at 48 kHz.
const MAX_FRAMES: usize = 5_760;

/// All built-in codecs plus Opus.
pub fn codecs() -> &'static CodecRegistry {
    static REGISTRY: OnceLock<CodecRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut registry = CodecRegistry::new();
        symphonia::default::register_enabled_codecs(&mut registry);
        registry.register_all::<OpusDecoder>();
        registry
    })
}

pub struct OpusDecoder {
    params: CodecParameters,
    decoder: rusty_opus::OpusDecoder,
    channels: usize,
    /// Encoder delay (OpusHead pre-skip) still to drop from the stream start.
    skip_left: usize,
    pcm: Vec<f32>,
    buf: AudioBuffer<f32>,
}

/// `(channels, pre_skip)` from an `OpusHead` (RFC 7845 §5.1).
fn parse_head(head: &[u8]) -> Option<(usize, usize)> {
    if head.len() < 19 || &head[..8] != b"OpusHead" {
        return None;
    }
    let channels = head[9] as usize;
    let pre_skip = u16::from_le_bytes([head[10], head[11]]) as usize;
    // Family 0 is plain mono/stereo; others need the multistream decoder.
    let family = head[18];
    (family == 0 && (1..=2).contains(&channels)).then_some((channels, pre_skip))
}

impl Decoder for OpusDecoder {
    fn try_new(params: &CodecParameters, _options: &DecoderOptions) -> Result<Self> {
        let (channels, pre_skip) = match params.extra_data.as_deref() {
            Some(head) => match parse_head(head) {
                Some(parsed) => parsed,
                None => return unsupported_error("opus: unsupported OpusHead"),
            },
            // No header: assume stereo without delay.
            None => (params.channels.map_or(2, |c| c.count()), 0),
        };
        let decoder = match rusty_opus::OpusDecoder::new(RATE as i32, channels) {
            Ok(d) => d,
            Err(_) => return unsupported_error("opus: cannot create decoder"),
        };
        let layout = if channels == 1 {
            Channels::FRONT_LEFT
        } else {
            Channels::FRONT_LEFT | Channels::FRONT_RIGHT
        };
        let mut params = params.clone();
        params.with_sample_rate(RATE).with_channels(layout);
        Ok(Self {
            params,
            decoder,
            channels,
            skip_left: pre_skip,
            pcm: vec![0.0; MAX_FRAMES * channels],
            buf: AudioBuffer::new(MAX_FRAMES as u64, SignalSpec::new(RATE, layout)),
        })
    }

    fn supported_codecs() -> &'static [CodecDescriptor] {
        &[support_codec!(CODEC_TYPE_OPUS, "opus", "Opus")]
    }

    fn reset(&mut self) {
        // No reset in rusty-opus; a fresh decoder is cheap. Seeks land mid-stream,
        // so the stream-start pre-skip is not dropped again.
        if let Ok(fresh) = rusty_opus::OpusDecoder::new(RATE as i32, self.channels) {
            self.decoder = fresh;
        }
    }

    fn codec_params(&self) -> &CodecParameters {
        &self.params
    }

    fn decode(&mut self, packet: &Packet) -> Result<AudioBufferRef<'_>> {
        self.buf.clear();
        let frames = match self.decoder.decode(&packet.data, MAX_FRAMES, &mut self.pcm) {
            Ok(n) => n,
            Err(_) => return decode_error("opus: invalid packet"),
        };
        let skip = self.skip_left.min(frames);
        self.skip_left -= skip;
        let keep = frames - skip;
        self.buf.render_reserved(Some(keep));
        for ch in 0..self.channels {
            let plane = self.buf.chan_mut(ch);
            for (i, sample) in plane.iter_mut().enumerate() {
                *sample = self.pcm[(skip + i) * self.channels + ch];
            }
        }
        Ok(self.buf.as_audio_buffer_ref())
    }

    fn finalize(&mut self) -> FinalizeResult {
        FinalizeResult::default()
    }

    fn last_decoded(&self) -> AudioBufferRef<'_> {
        self.buf.as_audio_buffer_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(channels: u8, pre_skip: u16, family: u8) -> Vec<u8> {
        let mut h = b"OpusHead".to_vec();
        h.push(1);
        h.push(channels);
        h.extend_from_slice(&pre_skip.to_le_bytes());
        h.extend_from_slice(&48_000u32.to_le_bytes());
        h.extend_from_slice(&0i16.to_le_bytes());
        h.push(family);
        h
    }

    #[test]
    fn parses_opus_head() {
        assert_eq!(parse_head(&head(2, 312, 0)), Some((2, 312)));
        assert_eq!(parse_head(&head(1, 0, 0)), Some((1, 0)));
        // Surround needs the multistream decoder.
        assert_eq!(parse_head(&head(6, 312, 1)), None);
        assert_eq!(parse_head(b"OpusTags"), None);
    }

    #[test]
    fn decodes_through_symphonia_and_drops_pre_skip() {
        use symphonia::core::codecs::DecoderOptions;
        // One second of a 440 Hz stereo tone, encoded in 20 ms packets.
        const FRAME: usize = 960;
        let mut enc = rusty_opus::OpusEncoder::new(RATE as i32, 2, rusty_opus::Application::Audio).unwrap();
        let mut params = CodecParameters::new();
        params.for_codec(CODEC_TYPE_OPUS).with_extra_data(head(2, 312, 0).into_boxed_slice());
        let mut dec = codecs().make(&params, &DecoderOptions::default()).unwrap();
        assert_eq!(dec.codec_params().sample_rate, Some(RATE));

        let mut frames = 0;
        let mut peak = 0f32;
        for n in 0..50 {
            let pcm: Vec<f32> = (0..FRAME * 2)
                .map(|i| {
                    let t = (n * FRAME + i / 2) as f32 / RATE as f32;
                    0.5 * (2.0 * std::f32::consts::PI * 440.0 * t).sin()
                })
                .collect();
            let mut out = [0u8; 1500];
            let len = enc.encode(&pcm, FRAME, &mut out).unwrap();
            let packet = Packet::new_from_slice(0, (n * FRAME) as u64, FRAME as u64, &out[..len]);
            let buf = dec.decode(&packet).unwrap();
            frames += buf.frames();
            if let AudioBufferRef::F32(b) = buf {
                peak = b.chan(0).iter().chain(b.chan(1)).fold(peak, |m, s| m.max(s.abs()));
            }
        }
        assert_eq!(frames, 50 * FRAME - 312, "pre-skip dropped once");
        assert!(peak > 0.3 && peak < 0.7, "tone survives the round trip: peak {peak}");

        // After a seek the decoder restarts but never re-drops the pre-skip.
        dec.reset();
        let mut out = [0u8; 1500];
        let len = enc.encode(&vec![0.0; FRAME * 2], FRAME, &mut out).unwrap();
        let buf = dec.decode(&Packet::new_from_slice(0, 0, FRAME as u64, &out[..len])).unwrap();
        assert_eq!(buf.frames(), FRAME);
    }

    #[test]
    fn registry_knows_opus() {
        assert!(codecs().get_codec(CODEC_TYPE_OPUS).is_some());
        assert!(codecs().get_codec(symphonia::core::codecs::CODEC_TYPE_AAC).is_some());
    }
}
