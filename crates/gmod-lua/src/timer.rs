//! Time and timers: `CurTime`, `RealTime`, `FrameTime`, the `timer` library.
//!
//! Game time moves only when the engine says so ([`crate::Runtime::think`]); timers are
//! checked on every think, after the clock moves and before the `Think` hook. A timer that is
//! late fires once and is scheduled from the current time, not caught up.

use std::cell::{Cell, RefCell};

use mlua::{Function, Lua, MultiValue, Table, Value};

use crate::args::Args;
use crate::{files, host};

/// GMod's default tick: 66 ticks a second.
pub const TICK_INTERVAL: f64 = 0.015;

#[derive(Debug, Clone, PartialEq)]
enum Id {
    Named(String),
    /// `timer.Simple`: no name, cannot be touched.
    Simple(u64),
}

struct Timer {
    id: Id,
    delay: f64,
    /// Repetitions left; 0 runs forever.
    reps: u32,
    function: Function,
    next: f64,
    /// Time left when paused.
    paused: Option<f64>,
    /// Its last repetition is running: it goes once that returns.
    finished: bool,
}

#[derive(Default)]
pub(crate) struct Clock {
    pub cur_time: Cell<f64>,
    pub frame_time: Cell<f64>,
    /// While a client predicts a tick of its player ([`crate::player::predict`]): the time
    /// outside the prediction (`UnPredictedCurTime`), `CurTime` being the tick's.
    pub unpredicted: Cell<Option<f64>>,
    /// The tick predicted is predicted for the first time (`IsFirstTimePredicted`): what it
    /// makes heard happens once, though the tick is predicted again every frame until the
    /// server's answer comes.
    pub first_prediction: Cell<bool>,
    /// How far into the player's commands - in seconds of them since the first - the
    /// prediction has gone: a tick ending there or before has been predicted.
    pub predicted_until: Cell<f64>,
    pub ticks: Cell<u64>,
    /// A client was given the server's time since its last frame ([`crate::Runtime::apply_snapshot`]):
    /// its frame ends there rather than a frame's time later.
    pub given: Cell<bool>,
    /// Where a client's last frame ended.
    pub thought: Cell<f64>,
    timers: RefCell<Vec<Timer>>,
    next_simple: Cell<u64>,
}

/// Whether what runs now runs for the first time: anything but a tick a client predicts again.
pub(crate) fn first_time_predicted(lua: &Lua) -> bool {
    let clock = &host(lua).clock;
    clock.unpredicted.get().is_none() || clock.first_prediction.get()
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    let def = |table: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<MultiValue>| -> mlua::Result<()> {
        table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
    };
    globals.raw_set("CurTime", lua.create_function(|lua, ()| Ok(host(lua).clock.cur_time.get()))?)?;
    globals.raw_set(
        "UnPredictedCurTime",
        lua.create_function(|lua, ()| {
            let clock = &host(lua).clock;
            Ok(clock.unpredicted.get().unwrap_or(clock.cur_time.get()))
        })?,
    )?;
    globals.raw_set("RealTime", lua.create_function(|lua, ()| Ok(host(lua).started.elapsed().as_secs_f64()))?)?;
    globals.raw_set("FrameTime", lua.create_function(|lua, ()| Ok(host(lua).clock.frame_time.get()))?)?;
    // the server runs each command once, as does a client outside its prediction
    globals.raw_set("IsFirstTimePredicted", lua.create_function(|lua, ()| Ok(first_time_predicted(lua)))?)?;

    let timer = lua.create_table()?;
    def(&timer, "Create", |a| {
        let id = Id::Named(a.string(1)?);
        let delay = a.number(2)?;
        let reps = a.number(3)?.max(0.0) as u32;
        let function = a.function(4)?;
        let clock = &host(a.lua).clock;
        let mut timers = clock.timers.borrow_mut();
        timers.retain(|t| t.id != id);
        timers.push(Timer { id, delay, reps, function, next: clock.cur_time.get() + delay, paused: None, finished: false });
        Ok(MultiValue::new())
    })?;
    def(&timer, "Simple", |a| {
        let delay = a.number(1)?;
        let function = a.function(2)?;
        let clock = &host(a.lua).clock;
        let n = clock.next_simple.get();
        clock.next_simple.set(n + 1);
        let next = clock.cur_time.get() + delay;
        clock.timers.borrow_mut().push(Timer { id: Id::Simple(n), delay, reps: 1, function, next, paused: None, finished: false });
        Ok(MultiValue::new())
    })?;
    def(&timer, "Remove", |a| {
        let id = Id::Named(a.string(1)?);
        host(a.lua).clock.timers.borrow_mut().retain(|t| t.id != id);
        Ok(MultiValue::new())
    })?;
    timer.raw_set("Destroy", timer.raw_get::<Value>("Remove")?)?;
    def(&timer, "Exists", |a| {
        let found = with_timer(a, |_, _| ())?.is_some();
        Ok(MultiValue::from_iter([Value::Boolean(found)]))
    })?;
    def(&timer, "Adjust", |a| {
        let delay = a.number(2)?;
        let reps = a.opt_number(3)?.map(|r| r.max(0.0) as u32);
        let function = match a.get(4) {
            Some(Value::Function(f)) => Some(f.clone()),
            _ => None,
        };
        let found = with_timer(a, |t, now| {
            t.delay = delay;
            t.next = now + delay;
            if let Some(reps) = reps {
                t.reps = reps;
            }
            if let Some(function) = function {
                t.function = function;
            }
        })?;
        Ok(MultiValue::from_iter([Value::Boolean(found.is_some())]))
    })?;
    def(&timer, "Start", |a| {
        let found = with_timer(a, |t, now| {
            t.next = now + t.delay;
            t.paused = None;
        })?;
        Ok(MultiValue::from_iter([Value::Boolean(found.is_some())]))
    })?;
    def(&timer, "Stop", |a| {
        let found = with_timer(a, |t, _| t.paused = Some(t.delay))?;
        Ok(MultiValue::from_iter([Value::Boolean(found.is_some())]))
    })?;
    def(&timer, "Pause", |a| {
        let found = with_timer(a, |t, now| {
            if t.paused.is_none() {
                t.paused = Some(t.next - now);
            }
        })?;
        Ok(MultiValue::from_iter([Value::Boolean(found.is_some())]))
    })?;
    def(&timer, "UnPause", |a| {
        let found = with_timer(a, |t, now| {
            if let Some(left) = t.paused.take() {
                t.next = now + left;
            }
        })?;
        Ok(MultiValue::from_iter([Value::Boolean(found.is_some())]))
    })?;
    def(&timer, "Toggle", |a| {
        let state = with_timer(a, |t, now| match t.paused.take() {
            Some(left) => {
                t.next = now + left;
                true
            }
            None => {
                t.paused = Some(t.next - now);
                false
            }
        })?;
        Ok(MultiValue::from_iter([state.map_or(Value::Nil, Value::Boolean)]))
    })?;
    def(&timer, "TimeLeft", |a| {
        let left = with_timer(a, |t, now| t.paused.unwrap_or(t.next - now))?;
        Ok(MultiValue::from_iter([left.map_or(Value::Nil, Value::Number)]))
    })?;
    def(&timer, "RepsLeft", |a| {
        let left = with_timer(a, |t, _| t.reps)?;
        Ok(MultiValue::from_iter([left.map_or(Value::Nil, |n| Value::Integer(n.into()))]))
    })?;
    // deprecated, and a no-op since long ago
    def(&timer, "Check", |_| Ok(MultiValue::new()))?;
    globals.raw_set("timer", timer)?;
    Ok(())
}

/// Runs `f` on the named timer (argument 1) with the current time; `None` when there is none.
fn with_timer<R>(args: &Args, f: impl FnOnce(&mut Timer, f64) -> R) -> mlua::Result<Option<R>> {
    let id = Id::Named(args.string(1)?);
    let clock = &host(args.lua).clock;
    let now = clock.cur_time.get();
    Ok(clock.timers.borrow_mut().iter_mut().find(|t| t.id == id).map(|t| f(t, now)))
}

/// Fires the timers that are due. A timer's function may create and remove timers.
pub(crate) fn run_timers(lua: &Lua) {
    let due: Vec<(Id, Function)> = {
        let clock = &host(lua).clock;
        let now = clock.cur_time.get();
        let mut timers = clock.timers.borrow_mut();
        let mut due = Vec::new();
        for t in timers.iter_mut().filter(|t| t.paused.is_none() && t.next <= now) {
            due.push((t.id.clone(), t.function.clone()));
            t.next = now + t.delay;
        }
        due
    };
    for (id, function) in due {
        // removed or paused by a timer that ran before it
        let still_due = {
            let clock = &host(lua).clock;
            let mut timers = clock.timers.borrow_mut();
            match timers.iter().position(|t| t.id == id) {
                Some(i) if timers[i].paused.is_none() => {
                    if timers[i].reps > 0 {
                        timers[i].reps -= 1;
                        timers[i].finished = timers[i].reps == 0;
                    }
                    true
                }
                _ => false,
            }
        };
        if still_due {
            let _ = files::call_reporting(lua, function, MultiValue::new());
            host(lua).clock.timers.borrow_mut().retain(|t| !(t.id == id && t.finished));
        }
    }
}
