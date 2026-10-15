use std::sync::Arc;

use crate::clip::{decode, wav_loop_start};
use crate::mixer::{Listener, Mixer, Sound, distance_gain};
use crate::script::{self, CHAN_AUTO, Scripts};

/// A 16-bit PCM WAV of these samples, with a cue point if one is given.
fn wav(rate: u32, channels: u16, samples: &[i16], cue: Option<u32>) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(b"WAVE");
    body.extend_from_slice(b"fmt ");
    body.extend_from_slice(&16u32.to_le_bytes());
    body.extend_from_slice(&1u16.to_le_bytes());
    body.extend_from_slice(&channels.to_le_bytes());
    body.extend_from_slice(&rate.to_le_bytes());
    body.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
    body.extend_from_slice(&(channels * 2).to_le_bytes());
    body.extend_from_slice(&16u16.to_le_bytes());
    body.extend_from_slice(b"data");
    body.extend_from_slice(&(samples.len() as u32 * 2).to_le_bytes());
    for s in samples {
        body.extend_from_slice(&s.to_le_bytes());
    }
    if let Some(position) = cue {
        body.extend_from_slice(b"cue ");
        body.extend_from_slice(&28u32.to_le_bytes());
        body.extend_from_slice(&1u32.to_le_bytes());
        for value in [1, position, u32::from_le_bytes(*b"data"), 0, 0, position] {
            body.extend_from_slice(&value.to_le_bytes());
        }
    }
    let mut file = b"RIFF".to_vec();
    file.extend_from_slice(&(body.len() as u32).to_le_bytes());
    file.extend_from_slice(&body);
    file
}

fn clip(samples: &[i16], cue: Option<u32>) -> Arc<crate::Clip> {
    Arc::new(decode(&wav(44100, 1, samples, cue), "wav").unwrap())
}

fn sound(clip: Arc<crate::Clip>) -> Sound {
    Sound {
        clip,
        volume: 1.0,
        pitch: 1.0,
        level: 75.0,
        position: None,
        looping: false,
        source: 0,
        channel: CHAN_AUTO,
        name: "test".into(),
        start: 0.0,
    }
}

#[test]
fn wavs_decode_with_their_loops() {
    let samples: Vec<i16> = (0..100).map(|i| (i * 100) as i16).collect();
    let file = wav(22050, 1, &samples, Some(40));
    assert_eq!(wav_loop_start(&file), Some(40));
    let clip = decode(&file, "wav").unwrap();
    assert_eq!((clip.rate, clip.channels, clip.frames(), clip.loop_start), (22050, 1, 100, Some(40)));
    assert!((clip.samples[50] - 5000.0 / 32768.0).abs() < 1e-4);
    // stereo stays stereo; no cue, no loop
    let stereo = decode(&wav(44100, 2, &[1000, -1000, 2000, -2000], None), "wav").unwrap();
    assert_eq!((stereo.channels, stereo.frames(), stereo.loop_start), (2, 2, None));
    assert!(decode(b"not a sound at all", "wav").is_err());
}

#[test]
fn scripts_read_as_the_engine_reads_them() {
    let mut scripts = Scripts::default();
    scripts.add_file(
        r#""Weapon_Pistol.Single"
        {
            "channel"     "CHAN_WEAPON"
            "volume"      "0.55"
            "soundlevel"  "SNDLVL_GUNFIRE"
            "pitch"       "95, PITCH_NORM"
            "wave"        ")weapons/pistol/pistol_fire2.wav"
        }
        "Test.Random"
        {
            "soundlevel" "SNDLVL_80dB"
            "rndwave" { "wave" "a.wav" "wave" "^b\c.wav" }
        }
        "weapon_pistol.single" { "wave" "other.wav" }"#,
    );
    let pistol = scripts.get("WEAPON_PISTOL.SINGLE").unwrap();
    assert_eq!((pistol.channel, pistol.volume, pistol.level, pistol.pitch), (1, (0.55, 0.55), 140.0, (95.0, 100.0)));
    // the first definition stands
    assert_eq!(pistol.waves, [")weapons/pistol/pistol_fire2.wav"]);
    let random = scripts.get("test.random").unwrap();
    assert_eq!((random.level, random.waves.len()), (80.0, 2));
    assert_eq!(script::wave_path(")weapons/pistol/pistol_fire2.wav"), "sound/weapons/pistol/pistol_fire2.wav");
    assert_eq!(script::wave_path("^b\\c.wav"), "sound/b/c.wav");
    assert_eq!(script::wave_path("sound/ui/click.wav"), "sound/ui/click.wav");
}

#[test]
fn sounds_fall_off_with_distance_as_in_source() {
    // a normal sound is at full volume nearby, and falls as one over the distance far away
    assert!(distance_gain(75.0, 10.0) > 0.95);
    let (far, farther) = (distance_gain(75.0, 1000.0), distance_gain(75.0, 2000.0));
    assert!((far / farther - 2.0).abs() < 0.01, "{far} {farther}");
    // gunfire carries
    assert!(distance_gain(140.0, 3000.0) > 0.9);
    // level 0 is everywhere
    assert_eq!(distance_gain(0.0, 1e6), 1.0);
}

#[test]
fn the_mixer_plays_loops_and_cuts_off() {
    let mut mixer = Mixer::new(44100);
    let short = clip(&[16384; 10], None);
    mixer.play(sound(short.clone()));
    let mut out = vec![0.0; 40];
    mixer.mix(&mut out);
    assert!((out[0] - 0.5).abs() < 1e-3 && (out[1] - 0.5).abs() < 1e-3);
    assert_eq!(out[30], 0.0, "it ended");
    assert_eq!(mixer.playing(), 0);

    // a loop plays on, from its cue point
    let looping = clip(&[0, 0, 0, 0, 16384, 16384], Some(4));
    let id = mixer.play(Sound { looping: true, ..sound(looping) });
    mixer.mix(&mut out);
    assert!(mixer.is_playing(id));
    assert!((out[38] - 0.5).abs() < 1e-3, "after the first pass only the loop plays");

    // a new sound on a channel of a source cuts off the old one
    let a = mixer.play(Sound { source: 7, channel: 1, ..sound(short.clone()) });
    let b = mixer.play(Sound { source: 7, channel: 1, ..sound(short.clone()) });
    assert!(!mixer.is_playing(a) && mixer.is_playing(b));
    mixer.stop_named(7, "TEST");
    assert!(!mixer.is_playing(b));
}

#[test]
fn sounds_come_from_their_side() {
    let mut mixer = Mixer::new(44100);
    mixer.listener = Listener { position: [0.0; 3], right: [0.0, -1.0, 0.0] };
    // to the right of someone looking along x is -y
    mixer.play(Sound { position: Some([0.0, -200.0, 0.0]), source: 3, ..sound(clip(&[16384; 100], None)) });
    let mut out = vec![0.0; 20];
    mixer.mix(&mut out);
    assert!(out[1] > out[0] * 3.0, "right {} left {}", out[1], out[0]);
    // it moves to the left with its source
    mixer.move_source(3, [0.0, 200.0, 0.0]);
    mixer.mix(&mut out);
    assert!(out[0] > out[1] * 3.0);
}
