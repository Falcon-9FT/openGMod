//! `util.ScreenShake`: the view of those near shakes - as hard as they are close, dying away
//! over its time - as Source's view effects shake it, before `CalcView` sees it.

use std::cell::RefCell;

use mlua::{Lua, MultiValue, Value};

use crate::args::Args;
use crate::client::View;
use crate::particles::rand;
use crate::vector::{Angle, Vector};
use crate::{Realm, host};

/// The most a shake moves the view, as in Source.
const MAX_AMPLITUDE: f32 = 16.0;

/// A shake: how hard, how often it changes way, until when, and where it is now.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Shake {
    amplitude: f32,
    frequency: f32,
    duration: f32,
    end: f64,
    next: f64,
    offset: Vector,
    roll: f32,
}

/// The client's shakes going on.
#[derive(Default)]
pub(crate) struct Shakes(RefCell<Vec<Shake>>);

/// A shake for this client: `amplitude` units at most, changing way `frequency` times a second,
/// for `duration` seconds.
pub(crate) fn start(lua: &Lua, amplitude: f32, frequency: f32, duration: f32) {
    if amplitude <= 0.0 || duration <= 0.0 {
        return;
    }
    let now = host(lua).clock.cur_time.get();
    let shake = Shake {
        amplitude: amplitude.min(MAX_AMPLITUDE),
        frequency: frequency.max(0.1),
        duration,
        end: now + f64::from(duration),
        next: now,
        offset: Vector::default(),
        roll: 0.0,
    };
    host(lua).shakes.0.borrow_mut().push(shake);
}

/// How hard a shake at `center` reaching `radius` is felt at `at`: all of it at the middle,
/// nothing past the edge; everywhere for a radius of none.
fn felt(amplitude: f32, center: Vector, radius: f32, at: Vector) -> f32 {
    if radius <= 0.0 {
        return amplitude;
    }
    let distance = (at - center).length();
    if distance >= radius { 0.0 } else { amplitude * (1.0 - distance / radius) }
}

/// The view, shaken by what shakes now.
pub(crate) fn apply(lua: &Lua, view: View) -> View {
    let now = host(lua).clock.cur_time.get();
    let mut shakes: Vec<Shake> = host(lua).shakes.0.borrow().clone();
    shakes.retain(|s| s.end > now);
    let mut out = view;
    for shake in &mut shakes {
        if now >= shake.next {
            shake.next = now + 1.0 / f64::from(shake.frequency);
            shake.offset = Vector::new(rand(lua, -1.0, 1.0), rand(lua, -1.0, 1.0), rand(lua, -1.0, 1.0));
            shake.roll = rand(lua, -1.0, 1.0);
        }
        // it dies away as its time runs out
        let left = ((shake.end - now) as f32 / shake.duration).clamp(0.0, 1.0);
        let strength = shake.amplitude * left * left;
        let wave = ((now as f32) * shake.frequency * std::f32::consts::TAU).sin();
        out.origin = out.origin + shake.offset * (strength * wave);
        out.angles = Angle::new(out.angles.p, out.angles.y, out.angles.r + shake.roll * strength * wave * 0.25);
    }
    *host(lua).shakes.0.borrow_mut() = shakes;
    out
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let util: mlua::Table = lua.globals().get("util")?;
    // `util.ScreenShake(pos, amplitude, frequency, duration, radius, airshake, filter)`
    util.raw_set(
        "ScreenShake",
        lua.create_function(|lua, values: MultiValue| {
            let a = Args::new(lua, values, "ScreenShake");
            let center = a.userdata::<Vector>(1, "Vector")?;
            let (amplitude, frequency, duration) = (a.number(2)? as f32, a.number(3)? as f32, a.number(4)? as f32);
            let radius = a.opt_number(5)?.unwrap_or(0.0) as f32;
            if host(lua).realm == Realm::Server {
                let players = match a.get(7) {
                    Some(value @ Value::UserData(_)) => crate::recipients::players_of(lua, Some(value)),
                    _ => crate::player::all(lua),
                };
                for player in players {
                    let Some((at, _)) = host(lua).entities.transform(player) else { continue };
                    let strength = felt(amplitude, center, radius, at);
                    if strength > 0.0 {
                        crate::player::to_client(lua, player, crate::player::ToClient::Shake(strength, frequency, duration));
                    }
                }
            } else {
                let at = host(lua).client.view.get().origin;
                start(lua, felt(amplitude, center, radius, at), frequency, duration);
            }
            Ok(())
        })?,
    )
}
