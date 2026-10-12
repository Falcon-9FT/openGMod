//! Sound files decoded into samples.
//!
//! WAV, MP3 and OGG are decoded by Symphonia. What Source adds to WAV is read here: a cue
//! point (the `cue ` chunk, else the first loop of a `smpl` chunk) marks where a sound loops
//! back to, and such a sound plays until it is stopped.

use std::io::{self, Cursor};

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// The most samples a sound may decode to: five minutes of stereo at 48 kHz.
pub const MAX_SAMPLES: usize = 5 * 60 * 48_000 * 2;

/// A decoded sound: mono or stereo samples at its own rate.
#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    /// Frames a second.
    pub rate: u32,
    /// 1 or 2; other layouts keep their first two channels.
    pub channels: u16,
    /// Interleaved, -1 to 1.
    pub samples: Vec<f32>,
    /// The frame a looping sound goes back to: the file's cue point.
    pub loop_start: Option<usize>,
}

impl Clip {
    /// Samples a channel has.
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels.max(1))
    }

    /// How long it plays once, in seconds.
    pub fn seconds(&self) -> f64 {
        self.frames() as f64 / f64::from(self.rate.max(1))
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// Decodes a sound file; `extension` (`wav`, `mp3`, `ogg`) helps tell what it is.
pub fn decode(data: &[u8], extension: &str) -> io::Result<Clip> {
    let source = MediaSourceStream::new(Box::new(Cursor::new(data.to_vec())), Default::default());
    let mut hint = Hint::new();
    hint.with_extension(extension);
    let mut format = symphonia::default::get_probe()
        .probe(&hint, source, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| invalid(format!("not a sound file: {e}")))?;
    let track = format.default_track(TrackType::Audio).ok_or_else(|| invalid("no sound in the file"))?;
    let params = track.codec_params.as_ref().and_then(|p| p.audio()).ok_or_else(|| invalid("no sound in the file"))?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| invalid(format!("an unknown kind of sound: {e}")))?;
    let track_id = track.id;
    let (mut rate, mut channels) = (0u32, 0usize);
    let mut samples = Vec::new();
    let mut packet_samples: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            // a file cut short plays what came before
            Err(Error::IoError(_)) => break,
            Err(e) => return Err(invalid(format!("the sound could not be read: {e}"))),
        };
        if packet.track_id != track_id {
            continue;
        }
        let buffer = match decoder.decode(&packet) {
            Ok(buffer) => buffer,
            Err(Error::DecodeError(_)) => continue,
            Err(e) => return Err(invalid(format!("the sound could not be decoded: {e}"))),
        };
        let spec = buffer.spec();
        if rate == 0 {
            rate = spec.rate();
            channels = spec.channels().count();
        }
        if channels == 0 || spec.channels().count() != channels {
            return Err(invalid("the channels change inside the sound"));
        }
        packet_samples.resize(buffer.samples_interleaved(), 0.0);
        buffer.copy_to_slice_interleaved(&mut packet_samples);
        let kept = channels.min(2);
        if samples.len() + packet_samples.len() / channels * kept > MAX_SAMPLES {
            return Err(invalid("the sound is too long"));
        }
        if kept == channels {
            samples.extend_from_slice(&packet_samples);
        } else {
            for frame in packet_samples.chunks(channels) {
                samples.extend_from_slice(&frame[..kept]);
            }
        }
    }
    if rate == 0 || channels == 0 {
        return Err(invalid("no sound in the file"));
    }
    let channels = channels.min(2) as u16;
    let frames = samples.len() / usize::from(channels);
    let loop_start = if extension.eq_ignore_ascii_case("wav") { wav_loop_start(data).filter(|&f| f < frames) } else { None };
    Ok(Clip { rate, channels, samples, loop_start })
}

/// The frame a WAV file loops back to: its first cue point, else the start of the first
/// loop of its sampler chunk.
pub fn wav_loop_start(data: &[u8]) -> Option<usize> {
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return None;
    }
    let u32_at = |at: usize| data.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let mut at = 12;
    let mut sampler = None;
    while at + 8 <= data.len() {
        let id = &data[at..at + 4];
        let len = u32_at(at + 4)? as usize;
        let body = at + 8;
        match id {
            // count, then 24 bytes a point: id, position, ...
            b"cue " if u32_at(body)? > 0 => return u32_at(body + 4 + 4).map(|p| p as usize),
            // ... 36 bytes of header, the number of loops at 28; a loop: id, type, start, end ...
            b"smpl" if u32_at(body + 28)? > 0 => sampler = u32_at(body + 36 + 8).map(|p| p as usize),
            _ => {}
        }
        // chunks are padded to an even length
        at = body + len + (len & 1);
    }
    sampler
}
