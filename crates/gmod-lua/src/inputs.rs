//! Inputs of entities: what `Entity:Input` gives one now, and `Entity:Fire` after a delay -
//! the engine's queue of events, served every tick before the entities think (a delay of 0
//! is the next tick).
//!
//! The `AcceptInput` hook may take an input (returning true), then a SENT's
//! `ENT:AcceptInput`; else the engine does what it knows: `Kill` and `KillHierarchy` remove the
//! entity, `AddOutput` gives it a key-value (`key value`; outputs are not kept yet),
//! `SetParent` (by name) and `ClearParent`, `Color`, `Alpha` and `Skin`, and the constraints
//! take theirs ([`crate::constraint`]). Inputs it does not know are dropped.

use std::cell::RefCell;

use mlua::{Lua, MultiValue, Value};

use crate::args::Args;
use crate::entity::{self, EntityHandle};
use crate::host;

/// An input fired, waiting for its time.
struct Event {
    at: f64,
    target: EntityHandle,
    input: String,
    param: String,
    activator: EntityHandle,
    caller: EntityHandle,
}

/// The inputs fired and not yet given.
#[derive(Default)]
pub(crate) struct Queue {
    events: RefCell<Vec<Event>>,
}

/// Gives an entity an input now. Whether something took it.
pub(crate) fn input(lua: &Lua, target: EntityHandle, input: &str, activator: EntityHandle, caller: EntityHandle, param: &str) -> bool {
    if !host(lua).entities.is_valid(target) {
        return false;
    }
    let value = |h: EntityHandle| entity::push(lua, h).unwrap_or(Value::Nil);
    let string = |s: &str| lua.create_string(s).map(Value::String).unwrap_or(Value::Nil);
    let taken = |r: &MultiValue| matches!(r.front(), Some(Value::Boolean(true)));
    let args = [value(target), string(input), value(activator), value(caller), string(param)];
    if taken(&crate::call_hook(lua, "AcceptInput", MultiValue::from_iter(args))) {
        return true;
    }
    let scripted = host(lua).entities.get(target).is_some_and(|ent| ent.scripted);
    if scripted && let Value::UserData(ud) = value(target) {
        let args = MultiValue::from_iter([string(input), value(activator), value(caller), string(param)]);
        if crate::ents::call_method(lua, &ud, "AcceptInput", args).is_some_and(|r| taken(&r)) {
            return true;
        }
    }
    engine_input(lua, target, input, param)
}

/// What the engine does with an input of its own.
fn engine_input(lua: &Lua, target: EntityHandle, input: &str, param: &str) -> bool {
    match input.to_ascii_lowercase().as_str() {
        "kill" => crate::ents::remove(lua, target),
        "killhierarchy" => {
            let children: Vec<EntityHandle> = {
                let entities = &host(lua).entities;
                entities.handles().into_iter().filter(|&h| entities.get(h).is_some_and(|ent| ent.parent == target)).collect()
            };
            for child in children {
                engine_input(lua, child, "KillHierarchy", "");
            }
            crate::ents::remove(lua, target);
        }
        "addoutput" => {
            let (key, value) = param.trim().split_once(char::is_whitespace).unwrap_or((param.trim(), ""));
            if !key.is_empty() && !key.contains(':') {
                crate::ents::key_value(lua, target, key, value.trim());
            }
        }
        "setparent" => {
            let parent = crate::ents::named(lua, param.trim()).unwrap_or(EntityHandle::NULL);
            entity::set_parent(lua, target, parent);
        }
        "clearparent" => entity::set_parent(lua, target, EntityHandle::NULL),
        "color" => crate::ents::key_value(lua, target, "rendercolor", param),
        "alpha" => crate::ents::key_value(lua, target, "renderamt", param),
        "skin" => crate::ents::key_value(lua, target, "skin", param),
        _ => return crate::constraint::input(lua, target, input),
    }
    true
}

/// Fires an input at an entity, to be given `delay` seconds from now.
pub(crate) fn fire(lua: &Lua, target: EntityHandle, input: &str, param: &str, delay: f64, activator: EntityHandle, caller: EntityHandle) {
    let host = host(lua);
    let at = host.clock.cur_time.get() + delay.max(0.0);
    host.inputs.events.borrow_mut().push(Event { at, target, input: input.to_string(), param: param.to_string(), activator, caller });
}

/// One tick: the inputs whose time has come are given, in the order of their times and, at
/// one time, as they were fired. Those fired meanwhile wait for the next tick.
pub(crate) fn service(lua: &Lua) {
    let due: Vec<Event> = {
        let host = host(lua);
        let now = host.clock.cur_time.get();
        let mut events = host.inputs.events.borrow_mut();
        let (mut due, waiting): (Vec<Event>, Vec<Event>) = events.drain(..).partition(|e| e.at <= now);
        *events = waiting;
        due.sort_by(|a, b| a.at.total_cmp(&b.at));
        due
    };
    for event in due {
        input(lua, event.target, &event.input, event.activator, event.caller, &event.param);
    }
}

/// An argument given as a value of an input: text, or nothing.
fn param(a: &Args, position: usize) -> String {
    match a.get(position) {
        None | Some(Value::Nil) => String::new(),
        Some(Value::Boolean(b)) => if *b { "1" } else { "0" }.to_string(),
        Some(value) => a.lua.coerce_string(value.clone()).ok().flatten().map(|s| s.to_string_lossy()).unwrap_or_default(),
    }
}

fn opt_entity(a: &Args, position: usize) -> EntityHandle {
    entity::handle_of(a.get(position)).unwrap_or(EntityHandle::NULL)
}

pub(crate) fn install_methods(def: &entity::Def) -> mlua::Result<()> {
    // ent:Input(input, activator, caller, param)
    def("Input", |a| {
        let target = entity::this(a, 1)?;
        let name = a.string(2)?;
        input(a.lua, target, &name, opt_entity(a, 3), opt_entity(a, 4), &param(a, 5));
        Ok(Value::Nil)
    })?;
    // ent:Fire(input, param, delay, activator, caller)
    def("Fire", |a| {
        let target = entity::this(a, 1)?;
        let name = a.string(2)?;
        let delay = if matches!(a.get(4), None | Some(Value::Nil)) { 0.0 } else { a.number(4)? };
        fire(a.lua, target, &name, &param(a, 3), delay, opt_entity(a, 5), opt_entity(a, 6));
        Ok(Value::Nil)
    })?;
    Ok(())
}
