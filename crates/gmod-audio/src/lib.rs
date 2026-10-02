//! Sounds, as the Source engine plays them.
//!
//! A sound file under `sound/` - WAV (PCM or ADPCM, with the loop its cue point marks), MP3 or
//! OGG - is decoded into a [`Clip`] ([`clip`]). Sounds are named by file or by a sound script
//! of `scripts/game_sounds*.txt` ([`script`]): its waves to pick from, volume, pitch, sound
//! level and channel. The [`Mixer`] plays clips into a stereo stream: each from where it is
//! in the world, as loud as Source makes a sound of its level at that distance, panned by
//! where the listener faces ([`mixer`]).

#![forbid(unsafe_code)]

pub mod clip;
pub mod mixer;
pub mod script;

pub use clip::{Clip, decode};
pub use mixer::{Listener, Mixer, Sound};
pub use script::{Script, Scripts};

#[cfg(test)]
mod tests;
