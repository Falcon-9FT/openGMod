//! The client's particles of Lua: `ParticleEmitter` (`CLuaEmitter`) and what it adds
//! (`CLuaParticle`). Each frame they move - their gravity, the air holding them back, the world
//! stopping or bouncing them - grow old and go; they are drawn among the effects, facing the
//! camera (a 3D emitter's as their angles turn them), fading and growing from their start to
//! their end, or stretched along their way when they have a length.

use std::cell::RefCell;
use std::rc::Rc;

use mlua::{AnyUserData, Function, IntoLuaMulti, Lua, MultiValue, Table, UserData, UserDataFields, UserDataMethods, Value};

use crate::args::Args;
use crate::host;
use crate::meta::{self, type_id};
use crate::render3d::{BeamPoint, Material3d, Shape};
use crate::vector::{Angle, Vector};

const EMITTER: &str = "CLuaEmitter";
const PARTICLE: &str = "CLuaParticle";

/// The most particles an emitter has at once: `Add` gives none past it.
const MAX_PARTICLES: usize = 8192;

/// A particle: where it is and goes, how it looks over its life, what it does.
pub(crate) struct ParticleState {
    pub(crate) material: Material3d,
    pub(crate) pos: Vector,
    pub(crate) velocity: Vector,
    pub(crate) gravity: Vector,
    pub(crate) angles: Angle,
    pub(crate) angle_velocity: Angle,
    pub(crate) air_resistance: f32,
    pub(crate) bounce: f32,
    pub(crate) collide: bool,
    pub(crate) velocity_scale: bool,
    pub(crate) color: [f32; 3],
    pub(crate) start_alpha: f32,
    pub(crate) end_alpha: f32,
    pub(crate) start_size: f32,
    pub(crate) end_size: f32,
    pub(crate) start_length: f32,
    pub(crate) end_length: f32,
    pub(crate) roll: f32,
    pub(crate) roll_delta: f32,
    /// Seconds it has lived, and how many it lives.
    pub(crate) life_time: f32,
    pub(crate) die_time: f32,
    pub(crate) collide_callback: Option<Function>,
    pub(crate) think: Option<Function>,
    /// When (`CurTime`) its think function runs next; none: not again.
    pub(crate) next_think: Option<f64>,
    /// It died: it is not moved or drawn any more.
    pub(crate) dead: bool,
}

impl ParticleState {
    pub(crate) fn new(material: Material3d, pos: Vector) -> ParticleState {
        ParticleState {
            material,
            pos,
            velocity: Vector::default(),
            gravity: Vector::default(),
            angles: Angle::default(),
            angle_velocity: Angle::default(),
            air_resistance: 0.0,
            bounce: 0.0,
            collide: false,
            velocity_scale: false,
            color: [255.0; 3],
            start_alpha: 255.0,
            end_alpha: 255.0,
            start_size: 0.0,
            end_size: 0.0,
            start_length: 0.0,
            end_length: 0.0,
            roll: 0.0,
            roll_delta: 0.0,
            life_time: 0.0,
            die_time: 1.0,
            collide_callback: None,
            think: None,
            next_think: None,
            dead: false,
        }
    }

    /// How far through its life it is, from 0 to 1.
    fn age(&self) -> f32 {
        if self.die_time > 0.0 { (self.life_time / self.die_time).clamp(0.0, 1.0) } else { 1.0 }
    }
}

/// An emitter: where it is, whether it is drawn by itself, and its particles.
pub(crate) struct EmitterState {
    pos: Vector,
    three_d: bool,
    no_draw: bool,
    /// Lua let go of it (`Finish`): it goes when its particles have.
    finished: bool,
    particles: Vec<Rc<RefCell<ParticleState>>>,
}

/// The emitters of the client.
#[derive(Default)]
pub(crate) struct Particles {
    emitters: RefCell<Vec<Rc<RefCell<EmitterState>>>>,
    /// The dice of the engine's own effects.
    seed: std::cell::Cell<u64>,
}

/// Particles of the engine's own (an effect's): an emitter of their own, gone with them.
pub(crate) fn emit(lua: &Lua, three_d: bool, particles: Vec<ParticleState>) {
    if particles.is_empty() {
        return;
    }
    let pos = particles[0].pos;
    let particles = particles.into_iter().map(|p| Rc::new(RefCell::new(p))).collect();
    let state = EmitterState { pos, three_d, no_draw: false, finished: true, particles };
    host(lua).particles.emitters.borrow_mut().push(Rc::new(RefCell::new(state)));
}

/// A number from `low` to `high`, for the engine's effects.
pub(crate) fn rand(lua: &Lua, low: f32, high: f32) -> f32 {
    let cell = &host(lua).particles.seed;
    let mut x = cell.get();
    if x == 0 {
        x = 0x2545_f491_4f6c_dd1d;
    }
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    cell.set(x);
    low + (high - low) * ((x >> 11) as f32 / (1u64 << 53) as f32)
}

/// A direction picked at random, every way as likely.
pub(crate) fn rand_dir(lua: &Lua) -> Vector {
    loop {
        let v = Vector::new(rand(lua, -1.0, 1.0), rand(lua, -1.0, 1.0), rand(lua, -1.0, 1.0));
        let length = v.length();
        if length > 1e-3 && length <= 1.0 {
            return v * (1.0 / length);
        }
    }
}

/// `CLuaEmitter`, as Lua holds it.
struct Emitter(Rc<RefCell<EmitterState>>);

impl Drop for Emitter {
    // forgotten by Lua: its particles live on, and then it goes
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.try_borrow_mut() {
            state.finished = true;
        }
    }
}

impl UserData for Emitter {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("MetaName", EMITTER);
        fields.add_meta_field("MetaID", type_id::PARTICLEEMITTER);
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function("__index", |lua, (ud, key): (AnyUserData, Value)| meta::index(lua, EMITTER, ud, key));
        methods.add_meta_function("__newindex", |lua, (ud, key, value): (AnyUserData, Value, Value)| {
            meta::new_index(lua, EMITTER, ud, key, value)
        });
        methods.add_meta_method("__tostring", |_, e, ()| {
            Ok(if e.0.borrow().finished { "CLuaEmitter [NULL]".to_string() } else { format!("CLuaEmitter [{:p}]", Rc::as_ptr(&e.0)) })
        });
    }
}

/// `CLuaParticle`, as Lua holds it.
struct Particle(Rc<RefCell<ParticleState>>);

impl UserData for Particle {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("MetaName", PARTICLE);
        fields.add_meta_field("MetaID", type_id::PARTICLE);
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function("__index", |lua, (ud, key): (AnyUserData, Value)| meta::index(lua, PARTICLE, ud, key));
        methods.add_meta_function("__newindex", |lua, (ud, key, value): (AnyUserData, Value, Value)| {
            meta::new_index(lua, PARTICLE, ud, key, value)
        });
        methods.add_meta_method("__tostring", |_, p, ()| Ok(format!("CLuaParticle [{:p}]", Rc::as_ptr(&p.0))));
    }
}

fn emitter(a: &Args) -> mlua::Result<Rc<RefCell<EmitterState>>> {
    match a.get(1) {
        Some(Value::UserData(ud)) => Ok(ud.borrow::<Emitter>().map_err(|_| a.bad(1, EMITTER))?.0.clone()),
        _ => Err(a.bad(1, EMITTER)),
    }
}

fn particle(a: &Args) -> mlua::Result<Rc<RefCell<ParticleState>>> {
    match a.get(1) {
        Some(Value::UserData(ud)) => Ok(ud.borrow::<Particle>().map_err(|_| a.bad(1, PARTICLE))?.0.clone()),
        _ => Err(a.bad(1, PARTICLE)),
    }
}

/// A material as Lua gives one: its name, or an `IMaterial`.
fn material_arg(a: &Args, position: usize) -> mlua::Result<Material3d> {
    match a.get(position) {
        Some(Value::UserData(ud)) => crate::material::render_info(ud).ok_or_else(|| a.bad(position, "IMaterial")),
        Some(Value::String(_)) => Ok(Material3d { name: a.string(position)?, texture: None, additive: None }),
        _ => Err(a.bad(position, "string")),
    }
}

fn vector(a: &Args, position: usize) -> mlua::Result<Vector> {
    a.userdata::<Vector>(position, "Vector")
}

fn angle(a: &Args, position: usize) -> mlua::Result<Angle> {
    a.userdata::<Angle>(position, "Angle")
}

fn float(a: &Args, position: usize) -> mlua::Result<f32> {
    Ok(a.number(position)? as f32)
}

fn def(lua: &Lua, table: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<Value>) -> mlua::Result<()> {
    table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
}

fn def_multi(lua: &Lua, table: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<MultiValue>) -> mlua::Result<()> {
    table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    // `ParticleEmitter(pos, use3D)`
    def(lua, &lua.globals(), "ParticleEmitter", |a| {
        let pos = vector(a, 1)?;
        let state =
            Rc::new(RefCell::new(EmitterState { pos, three_d: a.boolean(2), no_draw: false, finished: false, particles: Vec::new() }));
        host(a.lua).particles.emitters.borrow_mut().push(state.clone());
        Ok(Value::UserData(a.lua.create_userdata(Emitter(state))?))
    })?;

    let t = meta::meta_table(lua, EMITTER, type_id::PARTICLEEMITTER)?;
    def(lua, &t, "Add", |a| {
        let state = emitter(a)?;
        let material = material_arg(a, 2)?;
        let pos = vector(a, 3)?;
        let mut emitter = state.borrow_mut();
        if emitter.finished || emitter.particles.len() >= MAX_PARTICLES {
            return Ok(Value::Nil);
        }
        let particle = Rc::new(RefCell::new(ParticleState::new(material, pos)));
        emitter.particles.push(particle.clone());
        Ok(Value::UserData(a.lua.create_userdata(Particle(particle))?))
    })?;
    def(lua, &t, "Draw", |a| {
        let state = emitter(a)?;
        draw_emitter(a.lua, &state.borrow());
        Ok(Value::Nil)
    })?;
    def(lua, &t, "Finish", |a| {
        emitter(a)?.borrow_mut().finished = true;
        Ok(Value::Nil)
    })?;
    def(lua, &t, "IsValid", |a| Ok(Value::Boolean(!emitter(a)?.borrow().finished)))?;
    def(lua, &t, "GetNumActiveParticles", |a| Ok(Value::Integer(emitter(a)?.borrow().particles.len() as i64)))?;
    def(lua, &t, "GetPos", |a| crate::entity::vector(a.lua, emitter(a)?.borrow().pos))?;
    def(lua, &t, "SetPos", |a| {
        let pos = vector(a, 2)?;
        emitter(a)?.borrow_mut().pos = pos;
        Ok(Value::Nil)
    })?;
    def(lua, &t, "Is3D", |a| Ok(Value::Boolean(emitter(a)?.borrow().three_d)))?;
    def(lua, &t, "SetNoDraw", |a| {
        let no_draw = a.boolean(2);
        emitter(a)?.borrow_mut().no_draw = no_draw;
        Ok(Value::Nil)
    })?;
    // what is culled is the renderer's business: every particle is drawn
    for name in ["SetBBox", "SetNearClip", "SetParticleCullRadius"] {
        def(lua, &t, name, |a| emitter(a).map(|_| Value::Nil))?;
    }

    let t = meta::meta_table(lua, PARTICLE, type_id::PARTICLE)?;
    macro_rules! number {
        ($get:literal, $set:literal, $field:ident) => {
            def(lua, &t, $get, |a| Ok(Value::Number(f64::from(particle(a)?.borrow().$field))))?;
            def(lua, &t, $set, |a| {
                let n = float(a, 2)?;
                particle(a)?.borrow_mut().$field = n;
                Ok(Value::Nil)
            })?;
        };
    }
    number!("GetAirResistance", "SetAirResistance", air_resistance);
    number!("GetBounce", "SetBounce", bounce);
    number!("GetDieTime", "SetDieTime", die_time);
    number!("GetLifeTime", "SetLifeTime", life_time);
    number!("GetStartAlpha", "SetStartAlpha", start_alpha);
    number!("GetEndAlpha", "SetEndAlpha", end_alpha);
    number!("GetStartSize", "SetStartSize", start_size);
    number!("GetEndSize", "SetEndSize", end_size);
    number!("GetStartLength", "SetStartLength", start_length);
    number!("GetEndLength", "SetEndLength", end_length);
    number!("GetRoll", "SetRoll", roll);
    number!("GetRollDelta", "SetRollDelta", roll_delta);
    macro_rules! vector_field {
        ($get:literal, $set:literal, $field:ident) => {
            def(lua, &t, $get, |a| crate::entity::vector(a.lua, particle(a)?.borrow().$field))?;
            def(lua, &t, $set, |a| {
                let v = vector(a, 2)?;
                particle(a)?.borrow_mut().$field = v;
                Ok(Value::Nil)
            })?;
        };
    }
    vector_field!("GetPos", "SetPos", pos);
    vector_field!("GetVelocity", "SetVelocity", velocity);
    vector_field!("GetGravity", "SetGravity", gravity);
    macro_rules! angle_field {
        ($get:literal, $set:literal, $field:ident) => {
            def(lua, &t, $get, |a| crate::entity::angle(a.lua, particle(a)?.borrow().$field))?;
            def(lua, &t, $set, |a| {
                let v = angle(a, 2)?;
                particle(a)?.borrow_mut().$field = v;
                Ok(Value::Nil)
            })?;
        };
    }
    angle_field!("GetAngles", "SetAngles", angles);
    angle_field!("GetAngleVelocity", "SetAngleVelocity", angle_velocity);
    macro_rules! flag {
        ($set:literal, $field:ident) => {
            def(lua, &t, $set, |a| {
                let on = a.boolean(2);
                particle(a)?.borrow_mut().$field = on;
                Ok(Value::Nil)
            })?;
        };
    }
    flag!("SetCollide", collide);
    // the world's light on it: particles are drawn as bright as they are
    def(lua, &t, "SetLighting", |a| particle(a).map(|_| Value::Nil))?;
    flag!("SetVelocityScale", velocity_scale);
    def(lua, &t, "SetColor", |a| {
        let rgb = [float(a, 2)?, float(a, 3)?, float(a, 4)?].map(|c| c.clamp(0.0, 255.0));
        particle(a)?.borrow_mut().color = rgb;
        Ok(Value::Nil)
    })?;
    def_multi(lua, &t, "GetColor", |a| {
        let [r, g, b] = particle(a)?.borrow().color;
        (f64::from(r), f64::from(g), f64::from(b)).into_lua_multi(a.lua)
    })?;
    def(lua, &t, "SetMaterial", |a| {
        let material = material_arg(a, 2)?;
        particle(a)?.borrow_mut().material = material;
        Ok(Value::Nil)
    })?;
    def(lua, &t, "GetMaterial", |a| {
        let name = particle(a)?.borrow().material.name.clone();
        crate::material::push_named(a.lua, &name)
    })?;
    def(lua, &t, "SetCollideCallback", |a| {
        let callback = match a.get(2) {
            Some(Value::Function(f)) => Some(f.clone()),
            _ => None,
        };
        particle(a)?.borrow_mut().collide_callback = callback;
        Ok(Value::Nil)
    })?;
    def(lua, &t, "SetThinkFunction", |a| {
        let think = match a.get(2) {
            Some(Value::Function(f)) => Some(f.clone()),
            _ => None,
        };
        particle(a)?.borrow_mut().think = think;
        Ok(Value::Nil)
    })?;
    def(lua, &t, "SetNextThink", |a| {
        let when = a.number(2)?;
        particle(a)?.borrow_mut().next_think = Some(when);
        Ok(Value::Nil)
    })?;
    Ok(())
}

/// A frame of the client: the particles move and grow old, the dead go, and the emitters Lua
/// let go of go with their last particle.
pub(crate) fn simulate(lua: &Lua, seconds: f32) {
    let emitters: Vec<Rc<RefCell<EmitterState>>> = host(lua).particles.emitters.borrow().clone();
    let now = host(lua).clock.cur_time.get();
    for emitter in &emitters {
        let particles: Vec<Rc<RefCell<ParticleState>>> = emitter.borrow().particles.clone();
        for p in &particles {
            step(lua, p, seconds, now);
        }
        emitter.borrow_mut().particles.retain(|p| !p.borrow().dead);
    }
    host(lua).particles.emitters.borrow_mut().retain(|e| {
        let e = e.borrow();
        !(e.finished && e.particles.is_empty())
    });
}

/// One particle's frame: older, moved, and what Lua asked to be told.
fn step(lua: &Lua, p: &Rc<RefCell<ParticleState>>, seconds: f32, now: f64) {
    let (from, to, collide) = {
        let mut s = p.borrow_mut();
        s.life_time += seconds;
        if s.life_time >= s.die_time {
            s.dead = true;
            return;
        }
        s.roll += s.roll_delta * seconds;
        let spin = s.angle_velocity;
        s.angles = Angle::new(s.angles.p + spin.p * seconds, s.angles.y + spin.y * seconds, s.angles.r + spin.r * seconds);
        s.velocity = s.velocity + s.gravity * seconds;
        if s.air_resistance > 0.0 {
            s.velocity = s.velocity * (-s.air_resistance * 0.01 * seconds).exp();
        }
        let from = s.pos;
        s.pos = from + s.velocity * seconds;
        (from, s.pos, s.collide)
    };
    let particle_value = || lua.create_userdata(Particle(p.clone())).map(Value::UserData);
    if collide && from != to {
        // the world stops it: it bounces off as much as it was told, and Lua hears of it
        let sweep =
            crate::trace::Sweep { whitelist: true, ..crate::trace::Sweep::line(from, to, crate::trace::Filter::Entities(Vec::new())) };
        let hit = crate::trace::trace(lua, &sweep);
        if hit.fraction < 1.0 && !hit.start_solid {
            let callback = {
                let mut s = p.borrow_mut();
                let normal = hit.normal;
                let v = s.velocity;
                s.velocity = (v - normal * (2.0 * v.dot(normal))) * s.bounce;
                s.pos = hit.end + normal * 0.1;
                s.collide_callback.clone()
            };
            if let Some(callback) = callback
                && let (Ok(particle), Ok(pos), Ok(normal)) =
                    (particle_value(), crate::entity::vector(lua, hit.end), crate::entity::vector(lua, hit.normal))
            {
                let _ = crate::files::call_reporting(lua, callback, MultiValue::from_iter([particle, pos, normal]));
            }
        }
    }
    let think = {
        let mut s = p.borrow_mut();
        match (s.think.clone(), s.next_think) {
            (Some(think), Some(when)) if when <= now => {
                s.next_think = None;
                Some(think)
            }
            _ => None,
        }
    };
    if let Some(think) = think
        && let Ok(particle) = particle_value()
    {
        let _ = crate::files::call_reporting(lua, think, MultiValue::from_iter([particle]));
    }
}

/// The emitters that draw themselves, among the effects of the frame.
pub(crate) fn draw_all(lua: &Lua) {
    let emitters: Vec<Rc<RefCell<EmitterState>>> = host(lua).particles.emitters.borrow().clone();
    for emitter in emitters {
        let emitter = emitter.borrow();
        if !emitter.no_draw {
            draw_emitter(lua, &emitter);
        }
    }
}

/// An emitter's particles, drawn now.
fn draw_emitter(lua: &Lua, emitter: &EmitterState) {
    let view = host(lua).client.view.get();
    let (_, view_right, view_up) = view.angles.vectors();
    for p in &emitter.particles {
        let s = p.borrow();
        if s.dead {
            continue;
        }
        let t = s.age();
        let lerp = |a: f32, b: f32| a + (b - a) * t;
        let alpha = lerp(s.start_alpha, s.end_alpha).clamp(0.0, 255.0) / 255.0;
        let size = lerp(s.start_size, s.end_size).max(0.0);
        let length = lerp(s.start_length, s.end_length);
        if alpha <= 0.0 {
            continue;
        }
        let color = [s.color[0] / 255.0, s.color[1] / 255.0, s.color[2] / 255.0, alpha];
        let material = s.material.clone();
        let shape = if length > 0.0 || s.velocity_scale {
            // stretched along its way: from where it is back along where it goes
            let speed = s.velocity.length();
            let dir = if speed > 1e-4 { s.velocity * (1.0 / speed) } else { view.angles.vectors().0 };
            let length = if s.velocity_scale { length.max(0.0) * speed * 0.01 } else { length };
            let point = |pos, v| BeamPoint { pos, width: size, v, color };
            Shape::Beam(vec![point(s.pos, 0.0), point(s.pos - dir * length, 1.0)])
        } else {
            let (right, up) = if emitter.three_d {
                let (_, right, up) = s.angles.vectors();
                (right, up)
            } else {
                (view_right, view_up)
            };
            // turned about its middle by its roll
            let (sin, cos) = s.roll.sin_cos();
            let (r, u) = (right * cos + up * sin, up * cos - right * sin);
            let (r, u) = (r * size, u * size);
            Shape::Quad { corners: [s.pos - r + u, s.pos + r + u, s.pos + r - u, s.pos - r - u], color }
        };
        crate::render3d::push_with(lua, material, shape);
    }
}
