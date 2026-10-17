//! `physenv`: the physics' surroundings - their gravity (`sv_gravity` straight down, unless set
//! otherwise until that changes), whether and how fast they go on, how fast anything may move
//! (the performance settings, `MaxVelocity` and `MaxAngularVelocity`), the air's density (which
//! holds back what moves through it), and surfaces added (`AddSurfaceData`). As GMod has them,
//! measured.

use std::cell::{Cell, RefCell};

use mlua::{Lua, MultiValue, Table, Value};

use crate::args::Args;
use crate::host;
use crate::vector::Vector;

/// The performance settings, GMod's to begin with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Performance {
    pub max_velocity: f64,
    pub max_angular_velocity: f64,
    max_collision_checks: f64,
    max_collisions_per_object: f64,
    look_ahead_objects: f64,
    look_ahead_world: f64,
    min_friction_mass: f64,
    max_friction_mass: f64,
}

impl Default for Performance {
    fn default() -> Performance {
        Performance {
            max_velocity: f64::from(gmod_physics::MAX_VELOCITY),
            max_angular_velocity: f64::from(gmod_physics::MAX_ANGULAR_VELOCITY),
            max_collision_checks: 50000.0,
            max_collisions_per_object: 10.0,
            look_ahead_objects: 0.5,
            look_ahead_world: 1.0,
            min_friction_mass: 10.0,
            max_friction_mass: 2500.0,
        }
    }
}

/// The physics' surroundings.
pub(crate) struct Environment {
    /// Gravity set by `SetGravity`, and the `sv_gravity` it was set at: it holds until that
    /// changes.
    pub gravity: Cell<Option<(Vector, f32)>>,
    air_density: Cell<f64>,
    pub paused: Cell<bool>,
    pub time_scale: Cell<f64>,
    /// Seconds the last step of the physics took.
    pub last_simulation: Cell<f64>,
    pub performance: RefCell<Performance>,
}

impl Default for Environment {
    fn default() -> Environment {
        Environment {
            gravity: Cell::new(None),
            air_density: Cell::new(2.0),
            paused: Cell::new(false),
            time_scale: Cell::new(1.0),
            last_simulation: Cell::new(0.0),
            performance: RefCell::new(Performance::default()),
        }
    }
}

/// Gravity this tick, `sv_gravity` being `sv_gravity`: what `SetGravity` set while that holds.
pub(crate) fn gravity(lua: &Lua, sv_gravity: f32) -> Vector {
    let env = &host(lua).physics.environment;
    match env.gravity.get() {
        Some((set, at)) if at == sv_gravity => set,
        _ => {
            env.gravity.set(None);
            Vector::new(0.0, 0.0, -sv_gravity)
        }
    }
}

fn sv_gravity(lua: &Lua) -> f32 {
    crate::convar::value(lua, "sv_gravity").and_then(|g| g.trim().parse::<f32>().ok()).unwrap_or(600.0)
}

fn performance_table(lua: &Lua, p: &Performance) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.raw_set("MaxVelocity", p.max_velocity)?;
    t.raw_set("MaxAngularVelocity", p.max_angular_velocity)?;
    t.raw_set("MaxCollisionChecksPerTimestep", p.max_collision_checks)?;
    t.raw_set("MaxCollisionsPerObjectPerTimestep", p.max_collisions_per_object)?;
    t.raw_set("LookAheadTimeObjectsVsObject", p.look_ahead_objects)?;
    t.raw_set("LookAheadTimeObjectsVsWorld", p.look_ahead_world)?;
    t.raw_set("MinFrictionMass", p.min_friction_mass)?;
    t.raw_set("MaxFrictionMass", p.max_friction_mass)?;
    Ok(t)
}

fn def(lua: &Lua, table: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<Value>) -> mlua::Result<()> {
    table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let physenv = lua.create_table()?;
    def(lua, &physenv, "GetGravity", |a| {
        let g = gravity(a.lua, sv_gravity(a.lua));
        crate::entity::vector(a.lua, g)
    })?;
    def(lua, &physenv, "SetGravity", |a| {
        let g = a.userdata::<Vector>(1, "Vector")?;
        host(a.lua).physics.environment.gravity.set(Some((g, sv_gravity(a.lua))));
        Ok(Value::Nil)
    })?;
    def(lua, &physenv, "GetAirDensity", |a| Ok(Value::Number(host(a.lua).physics.environment.air_density.get())))?;
    def(lua, &physenv, "SetAirDensity", |a| {
        let density = a.number(1)?.max(0.0);
        host(a.lua).physics.environment.air_density.set(density);
        host(a.lua).physics.physics.borrow_mut().set_air_density(density as f32);
        Ok(Value::Nil)
    })?;
    def(lua, &physenv, "GetPhysicsPaused", |a| Ok(Value::Boolean(host(a.lua).physics.environment.paused.get())))?;
    def(lua, &physenv, "SetPhysicsPaused", |a| {
        host(a.lua).physics.environment.paused.set(a.boolean(1));
        Ok(Value::Nil)
    })?;
    def(lua, &physenv, "GetTimeScale", |a| Ok(Value::Number(host(a.lua).physics.environment.time_scale.get())))?;
    def(lua, &physenv, "SetTimeScale", |a| {
        host(a.lua).physics.environment.time_scale.set(a.number(1)?.max(0.0));
        Ok(Value::Nil)
    })?;
    def(lua, &physenv, "GetLastSimulationTime", |a| Ok(Value::Number(host(a.lua).physics.environment.last_simulation.get())))?;
    def(lua, &physenv, "GetPerformanceSettings", |a| {
        let p = *host(a.lua).physics.environment.performance.borrow();
        Ok(Value::Table(performance_table(a.lua, &p)?))
    })?;
    // what the table names changes; the rest stays
    def(lua, &physenv, "SetPerformanceSettings", |a| {
        let t = a.table(1)?;
        let env = &host(a.lua).physics.environment;
        let mut p = *env.performance.borrow();
        let take = |key: &str, slot: &mut f64| -> mlua::Result<()> {
            if let Some(v) = t.raw_get::<Option<f64>>(key)? {
                *slot = v;
            }
            Ok(())
        };
        take("MaxVelocity", &mut p.max_velocity)?;
        take("MaxAngularVelocity", &mut p.max_angular_velocity)?;
        take("MaxCollisionChecksPerTimestep", &mut p.max_collision_checks)?;
        take("MaxCollisionsPerObjectPerTimestep", &mut p.max_collisions_per_object)?;
        take("LookAheadTimeObjectsVsObject", &mut p.look_ahead_objects)?;
        take("LookAheadTimeObjectsVsWorld", &mut p.look_ahead_world)?;
        take("MinFrictionMass", &mut p.min_friction_mass)?;
        take("MaxFrictionMass", &mut p.max_friction_mass)?;
        *env.performance.borrow_mut() = p;
        host(a.lua).physics.physics.borrow_mut().set_speed_limits(p.max_velocity as f32, p.max_angular_velocity as f32);
        Ok(Value::Nil)
    })?;
    // surfaces of a `surfaceproperties` file's text, for bodies and traces from now on
    def(lua, &physenv, "AddSurfaceData", |a| {
        let text = a.string(1)?;
        crate::surfaceprops::add(a.lua, &text);
        Ok(Value::Nil)
    })?;
    lua.globals().raw_set("physenv", physenv)
}
