//! The mixer: the sounds playing, mixed into one stereo stream.
//!
//! A sound in the world is as loud as Source makes it: its sound level says at what distance
//! it falls off - a sound of `L` decibels is at full volume within `36 × 10^((L - 60) / 20)`
//! units and then falls as one over the distance, a level of 0 is heard everywhere - and it
//! comes from the side it is on, seen from where the listener faces. A sound with no place
//! plays as it was recorded, in both ears.
//!
//! Sounds are given a source (an entity) and a channel: a new sound on a channel of a source
//! cuts off the one there, but for [`CHAN_AUTO`] and [`CHAN_STATIC`]; the sounds of a source
//! move with it ([`Mixer::move_source`]).

use std::sync::Arc;

use crate::clip::Clip;
use crate::script::{CHAN_AUTO, CHAN_STATIC};

/// The level of sound at the reference distance, and that distance, as Source has them
/// (`snd_refdb`, `snd_refdist`).
const REF_DB: f32 = 60.0;
const REF_DISTANCE: f32 = 36.0;
/// Above this gain a sound's loudness is compressed, so that it does not clip up close.
const COMPRESS_FROM: f32 = 0.36;
/// The most voices that play at once; the oldest goes for a new one.
pub const MAX_VOICES: usize = 128;

/// A sound to play.
#[derive(Debug, Clone)]
pub struct Sound {
    pub clip: Arc<Clip>,
    /// 1 is the sound as recorded.
    pub volume: f32,
    /// 1 is the recorded pitch (and speed).
    pub pitch: f32,
    /// In decibels: how far it carries; 0 is everywhere.
    pub level: f32,
    /// Where it is in the world, or nowhere (a sound of the interface).
    pub position: Option<[f32; 3]>,
    /// Plays until stopped, going back to the clip's loop start (or its start).
    pub looping: bool,
    /// Who makes it (an entity), 0 for nobody.
    pub source: u64,
    pub channel: i32,
    /// What it was asked for by, to stop it by the same name.
    pub name: String,
    /// Seconds into the clip it starts at.
    pub start: f64,
}

/// Where the sounds are heard from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Listener {
    pub position: [f32; 3],
    /// The direction to the listener's right.
    pub right: [f32; 3],
}

impl Default for Listener {
    fn default() -> Listener {
        Listener { position: [0.0; 3], right: [0.0, -1.0, 0.0] }
    }
}

#[derive(Debug, Clone)]
struct Voice {
    id: u64,
    sound: Sound,
    /// The frame it plays, with a fraction.
    cursor: f64,
}

/// The sounds that play.
#[derive(Debug, Clone)]
pub struct Mixer {
    rate: u32,
    voices: Vec<Voice>,
    next_id: u64,
    pub listener: Listener,
    /// Over every sound.
    pub master: f32,
}

/// The level in decibels of a sound heard whole up to `distance` units and fading as one over
/// the distance beyond (as BASS fades a 3D sound past its least distance).
pub fn level_for_distance(distance: f32) -> f32 {
    REF_DB - 20.0 * (REF_DISTANCE / distance.max(1.0)).log10()
}

/// How loud a sound of `level` decibels is at `distance` units, as Source works it out.
pub fn distance_gain(level: f32, distance: f32) -> f32 {
    if level <= 0.0 {
        return 1.0;
    }
    let multiplier = 10f32.powf((REF_DB - level) / 20.0) / REF_DISTANCE;
    let relative = distance * multiplier;
    let gain = if relative > 0.1 { 1.0 / relative } else { 10.0 };
    if gain <= COMPRESS_FROM {
        return gain;
    }
    // closer than that, it grows ever slower towards full volume
    let headroom = 1.0 - COMPRESS_FROM;
    COMPRESS_FROM + headroom * (1.0 - (-(gain - COMPRESS_FROM) / headroom).exp())
}

impl Mixer {
    /// A mixer for an output of `rate` frames a second.
    pub fn new(rate: u32) -> Mixer {
        Mixer { rate: rate.max(1), voices: Vec::new(), next_id: 1, listener: Listener::default(), master: 1.0 }
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Starts a sound; its number, to change or stop it.
    pub fn play(&mut self, sound: Sound) -> u64 {
        if sound.source != 0 && sound.channel != CHAN_AUTO && sound.channel != CHAN_STATIC {
            self.voices.retain(|v| !(v.sound.source == sound.source && v.sound.channel == sound.channel));
        }
        if self.voices.len() >= MAX_VOICES {
            self.voices.remove(0);
        }
        let id = self.next_id;
        self.next_id += 1;
        let cursor = (sound.start.max(0.0) * f64::from(sound.clip.rate)).min(sound.clip.frames() as f64);
        self.voices.push(Voice { id, sound, cursor });
        id
    }

    /// A playing sound is somewhere else now.
    pub fn move_voice(&mut self, id: u64, position: [f32; 3]) {
        if let Some(voice) = self.voices.iter_mut().find(|v| v.id == id) {
            voice.sound.position = Some(position);
        }
    }

    pub fn stop(&mut self, id: u64) {
        self.voices.retain(|v| v.id != id);
    }

    /// Stops the sounds of a source (0: of nobody) asked for by this name, any case.
    pub fn stop_named(&mut self, source: u64, name: &str) {
        self.voices.retain(|v| !(v.sound.source == source && v.sound.name.eq_ignore_ascii_case(name)));
    }

    /// Stops every sound of a source.
    pub fn stop_source(&mut self, source: u64) {
        self.voices.retain(|v| v.sound.source != source);
    }

    pub fn stop_all(&mut self) {
        self.voices.clear();
    }

    pub fn is_playing(&self, id: u64) -> bool {
        self.voices.iter().any(|v| v.id == id)
    }

    /// How many sounds play.
    pub fn playing(&self) -> usize {
        self.voices.len()
    }

    /// Changes a playing sound's volume and pitch.
    pub fn change(&mut self, id: u64, volume: Option<f32>, pitch: Option<f32>) {
        if let Some(voice) = self.voices.iter_mut().find(|v| v.id == id) {
            if let Some(volume) = volume {
                voice.sound.volume = volume;
            }
            if let Some(pitch) = pitch {
                voice.sound.pitch = pitch;
            }
        }
    }

    /// The sources whose sounds are in the world, to say where they are now.
    pub fn sources(&self) -> Vec<u64> {
        let mut sources: Vec<u64> =
            self.voices.iter().filter(|v| v.sound.source != 0 && v.sound.position.is_some()).map(|v| v.sound.source).collect();
        sources.sort_unstable();
        sources.dedup();
        sources
    }

    /// A source is somewhere else now: its sounds go with it.
    pub fn move_source(&mut self, source: u64, position: [f32; 3]) {
        for voice in self.voices.iter_mut().filter(|v| v.sound.source == source && v.sound.position.is_some()) {
            voice.sound.position = Some(position);
        }
    }

    /// The left and right gains of a sound now.
    fn gains(&self, sound: &Sound) -> (f32, f32) {
        let volume = sound.volume * self.master;
        let Some(at) = sound.position else { return (volume, volume) };
        let to = [at[0] - self.listener.position[0], at[1] - self.listener.position[1], at[2] - self.listener.position[2]];
        let distance = (to[0] * to[0] + to[1] * to[1] + to[2] * to[2]).sqrt();
        let gain = volume * distance_gain(sound.level, distance);
        // side: -1 left, 1 right; up close it is everywhere
        let side = if distance > 1.0 {
            let r = self.listener.right;
            ((to[0] * r[0] + to[1] * r[1] + to[2] * r[2]) / distance).clamp(-1.0, 1.0)
        } else {
            0.0
        };
        // equal power, and the far ear still hears some of it
        let angle = (side * 0.8 + 1.0) * std::f32::consts::FRAC_PI_4;
        (gain * angle.cos() * std::f32::consts::SQRT_2, gain * angle.sin() * std::f32::consts::SQRT_2)
    }

    /// Mixes the next frames into `out`, stereo interleaved; what was there is replaced.
    /// Sounds that end are dropped.
    pub fn mix(&mut self, out: &mut [f32]) {
        out.fill(0.0);
        let frames = out.len() / 2;
        let gains: Vec<(f32, f32)> = self.voices.iter().map(|v| self.gains(&v.sound)).collect();
        let rate = f64::from(self.rate);
        let mut ended = Vec::new();
        for (index, voice) in self.voices.iter_mut().enumerate() {
            let (left, right) = gains[index];
            let clip = &voice.sound.clip;
            let length = clip.frames();
            if length == 0 {
                ended.push(voice.id);
                continue;
            }
            let channels = usize::from(clip.channels.max(1));
            let step = f64::from(clip.rate) / rate * f64::from(voice.sound.pitch.max(0.01));
            let loop_start = clip.loop_start.unwrap_or(0) as f64;
            let in_world = voice.sound.position.is_some();
            for frame in 0..frames {
                if voice.cursor >= length as f64 {
                    if voice.sound.looping && (length as f64) > loop_start {
                        voice.cursor = loop_start + (voice.cursor - length as f64) % (length as f64 - loop_start);
                    } else {
                        ended.push(voice.id);
                        break;
                    }
                }
                let at = voice.cursor as usize;
                let fraction = (voice.cursor - at as f64) as f32;
                let next = if at + 1 < length {
                    at + 1
                } else if voice.sound.looping {
                    clip.loop_start.unwrap_or(0)
                } else {
                    at
                };
                let sample = |channel: usize| {
                    let a = clip.samples[at * channels + channel];
                    let b = clip.samples[next * channels + channel];
                    a + (b - a) * fraction
                };
                let (l, r) = if channels == 1 {
                    let s = sample(0);
                    (s, s)
                } else if in_world {
                    // a sound in the world comes from one place
                    let s = (sample(0) + sample(1)) * 0.5;
                    (s, s)
                } else {
                    (sample(0), sample(1))
                };
                out[frame * 2] += l * left;
                out[frame * 2 + 1] += r * right;
                voice.cursor += step;
            }
        }
        if !ended.is_empty() {
            self.voices.retain(|v| !ended.contains(&v.id));
        }
        for sample in out.iter_mut() {
            *sample = sample.clamp(-1.0, 1.0);
        }
    }
}
