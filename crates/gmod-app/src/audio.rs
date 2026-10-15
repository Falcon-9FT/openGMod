//! The game's sound: what Lua plays ([`gmod_lua::sound::SoundEvent`]) goes into a
//! [`gmod_audio::Mixer`], heard from the camera, and out of the sound card.
//!
//! The mixer is shared with the sound card's thread, which asks it for the next frames as it
//! needs them. Sound files are read and decoded the first time they play, and kept. Sounds an
//! entity makes go where it goes. On Windows the sound goes out through WASAPI; elsewhere
//! there is no output yet, and the mixer plays to nobody.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use gmod_audio::{Clip, Listener, Mixer, Sound};
use gmod_fs::Vfs;
use gmod_lua::entity::EntityHandle;
use gmod_lua::sound::SoundEvent;
use gmod_lua::vector::Vector;

/// What plays the sound of the game.
pub struct Audio {
    mixer: Arc<Mutex<Mixer>>,
    vfs: Arc<Vfs>,
    /// Sound files decoded, by game path; `None` for those that could not be.
    clips: HashMap<String, Option<Arc<Clip>>>,
    /// The voice of each `CSoundPatch` or looping sound that plays.
    patches: HashMap<u64, u64>,
    /// The entities whose sounds play, by the mixer's number for them.
    sources: HashMap<u64, EntityHandle>,
    #[cfg(windows)]
    _stream: Option<cpal::Stream>,
}

/// The mixer's number for an entity: never 0, which is nobody.
fn source_of(entity: EntityHandle) -> u64 {
    entity.index.map_or(0, |index| (u64::from(index) + 1) << 32 | u64::from(entity.serial))
}

fn array(v: Vector) -> [f32; 3] {
    [v.x, v.y, v.z]
}

impl Audio {
    /// Opens the sound card. Without one the game runs silent.
    pub fn new(vfs: Arc<Vfs>) -> Audio {
        #[cfg(windows)]
        {
            match output::open() {
                Ok((mixer, stream)) => return Audio::with(vfs, mixer, Some(stream)),
                Err(e) => println!("No sound: {e}"),
            }
            Audio::with(vfs, Arc::new(Mutex::new(Mixer::new(48_000))), None)
        }
        #[cfg(not(windows))]
        {
            Audio::with(vfs, Arc::new(Mutex::new(Mixer::new(48_000))))
        }
    }

    #[cfg(windows)]
    fn with(vfs: Arc<Vfs>, mixer: Arc<Mutex<Mixer>>, stream: Option<cpal::Stream>) -> Audio {
        Audio { mixer, vfs, clips: HashMap::new(), patches: HashMap::new(), sources: HashMap::new(), _stream: stream }
    }

    #[cfg(not(windows))]
    fn with(vfs: Arc<Vfs>, mixer: Arc<Mutex<Mixer>>) -> Audio {
        Audio { mixer, vfs, clips: HashMap::new(), patches: HashMap::new(), sources: HashMap::new() }
    }

    /// A sound file, decoded the first time.
    fn clip(&mut self, path: &str) -> Option<Arc<Clip>> {
        let key = path.to_ascii_lowercase();
        if let Some(clip) = self.clips.get(&key) {
            return clip.clone();
        }
        let extension = key.rsplit('.').next().unwrap_or_default().to_string();
        let clip = match self.vfs.read(path, "GAME") {
            Ok(data) => match gmod_audio::decode(&data, &extension) {
                Ok(clip) => Some(Arc::new(clip)),
                Err(e) => {
                    println!("{path}: {e}");
                    None
                }
            },
            // the game says nothing of sounds that are not there either
            Err(_) => None,
        };
        self.clips.insert(key, clip.clone());
        clip
    }

    /// Plays, changes and stops what the game asked for; `position` says where an entity is.
    pub fn handle(&mut self, events: Vec<SoundEvent>, position: impl Fn(EntityHandle) -> Option<Vector>) {
        for event in events {
            match event {
                SoundEvent::Play(play) => {
                    let Some(clip) = self.clip(&play.wave) else { continue };
                    let place = match (play.entity, play.position) {
                        (_, Some(at)) => Some(array(at)),
                        (Some(entity), None) => match position(entity) {
                            Some(at) => Some(array(at)),
                            None => continue,
                        },
                        (None, None) => None,
                    };
                    let source = play.entity.map_or(0, source_of);
                    if source != 0 {
                        self.sources.insert(source, play.entity.expect("a source is an entity"));
                    }
                    let looping = play.looping.unwrap_or(clip.loop_start.is_some());
                    let sound = Sound {
                        clip,
                        volume: play.volume,
                        pitch: play.pitch,
                        level: play.level,
                        position: place,
                        looping,
                        source,
                        channel: play.channel,
                        name: play.name,
                        start: f64::from(play.start),
                    };
                    let voice = self.mixer.lock().expect("the mixer").play(sound);
                    if let Some(patch) = play.patch {
                        self.patches.insert(patch, voice);
                    }
                }
                SoundEvent::Stop { entity, name } => {
                    self.mixer.lock().expect("the mixer").stop_named(entity.map_or(0, source_of), &name);
                }
                SoundEvent::StopPatch(patch) => {
                    if let Some(voice) = self.patches.remove(&patch) {
                        self.mixer.lock().expect("the mixer").stop(voice);
                    }
                }
                SoundEvent::MovePatch { patch, position } => {
                    if let Some(&voice) = self.patches.get(&patch) {
                        self.mixer.lock().expect("the mixer").move_voice(voice, array(position));
                    }
                }
                SoundEvent::ChangePatch { patch, volume, pitch } => {
                    if let Some(&voice) = self.patches.get(&patch) {
                        self.mixer.lock().expect("the mixer").change(voice, Some(volume), Some(pitch));
                    }
                }
            }
        }
    }

    /// The camera is here, facing `yaw` degrees: the sounds are heard from it, and those of
    /// entities move with them; those of entities gone stop.
    pub fn listen(&mut self, eye: [f32; 3], yaw: f32, position: impl Fn(EntityHandle) -> Option<Vector>) {
        let yaw = yaw.to_radians();
        let mut mixer = self.mixer.lock().expect("the mixer");
        mixer.listener = Listener { position: eye, right: [yaw.sin(), -yaw.cos(), 0.0] };
        for source in mixer.sources() {
            match self.sources.get(&source).and_then(|&entity| position(entity)) {
                Some(at) => mixer.move_source(source, array(at)),
                None => {
                    mixer.stop_source(source);
                    self.sources.remove(&source);
                }
            }
        }
        let playing: Vec<u64> = self.patches.iter().filter(|(_, voice)| !mixer.is_playing(**voice)).map(|(patch, _)| *patch).collect();
        for patch in playing {
            self.patches.remove(&patch);
        }
    }
}

#[cfg(windows)]
mod output {
    use std::sync::{Arc, Mutex};

    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
    use gmod_audio::Mixer;

    /// The default output of the system, playing a mixer at its rate.
    pub fn open() -> Result<(Arc<Mutex<Mixer>>, cpal::Stream), String> {
        let device = cpal::default_host().default_output_device().ok_or("there is no sound card")?;
        let supported = device.default_output_config().map_err(|e| e.to_string())?;
        let format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let mixer = Arc::new(Mutex::new(Mixer::new(config.sample_rate)));
        let stream = match format {
            SampleFormat::F32 => stream::<f32>(&device, config, mixer.clone()),
            SampleFormat::I16 => stream::<i16>(&device, config, mixer.clone()),
            SampleFormat::I32 => stream::<i32>(&device, config, mixer.clone()),
            SampleFormat::U16 => stream::<u16>(&device, config, mixer.clone()),
            other => return Err(format!("the sound card wants samples as {other}")),
        }?;
        stream.play().map_err(|e| e.to_string())?;
        Ok((mixer, stream))
    }

    fn stream<T: SizedSample + FromSample<f32>>(
        device: &cpal::Device,
        config: StreamConfig,
        mixer: Arc<Mutex<Mixer>>,
    ) -> Result<cpal::Stream, String> {
        let channels = usize::from(config.channels.max(1));
        let mut stereo: Vec<f32> = Vec::new();
        device
            .build_output_stream(
                config,
                move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                    let frames = data.len() / channels;
                    stereo.resize(frames * 2, 0.0);
                    match mixer.lock() {
                        Ok(mut mixer) => mixer.mix(&mut stereo),
                        Err(_) => stereo.fill(0.0),
                    }
                    for (frame, out) in data.chunks_mut(channels).enumerate() {
                        let (l, r) = (stereo[frame * 2], stereo[frame * 2 + 1]);
                        for (i, sample) in out.iter_mut().enumerate() {
                            // left and right; other speakers get both, quieter
                            let value = match i {
                                0 => l,
                                1 => r,
                                _ => (l + r) * 0.25,
                            };
                            *sample = T::from_sample(value);
                        }
                    }
                },
                |e| eprintln!("Sound: {e}"),
                None,
            )
            .map_err(|e| e.to_string())
    }
}
