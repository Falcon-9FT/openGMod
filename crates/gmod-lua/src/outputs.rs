//! Outputs of entities: Source's entity I/O. A map wires its entities together with
//! key-values such as `"OnTrigger" "door1,Open,,2,-1"` - when the entity fires its output
//! `OnTrigger`, the entities named `door1` get the input `Open` two seconds later, every time
//! (a count of `-1`; a positive count is used up). Newer compilers separate the fields with
//! the character 0x1B instead of commas; `AddOutput` gives them with colons
//! (`"OnTrigger door1:Open::2:-1"`).
//!
//! The targets are looked for when the input is given, not when the output fires: by name
//! (ASCII case ignored, a trailing `*` matching any end), else by class, or one of the names
//! the engine knows - `!self` (the entity firing), `!activator` (what started the chain: the
//! player who pressed, the thing that touched), `!caller` (the entity whose input made this
//! one fire), `!player` and `!pvsplayer` (the player). The entity that gets the input sees the
//! one that fired as its caller. A connection without a parameter passes the value the output
//! fires with, if any (`math_counter`'s `OutValue`).
//!
//! Scripted entities keep their outputs in Lua (`ENT:StoreOutput`, `ENT:TriggerOutput` of the
//! base entity); these are the engine's entities' own.

use mlua::Lua;

use crate::entity::{EntityHandle, Kind};
use crate::host;

/// One connection of an output.
#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    /// The output's name, `OnTrigger`.
    pub name: String,
    pub target: String,
    pub input: String,
    pub param: String,
    pub delay: f64,
    /// How many more times it fires; -1 for always.
    pub times: i64,
}

/// A connection as the map writes it, `target,input,param,delay,times`, the fields split by
/// `separator`. The parameter is what is left between the input and the delay, so commas in
/// it survive. `None` when it is not one.
fn connection(name: &str, value: &str, separator: char) -> Option<Output> {
    let mut left = value.splitn(3, separator);
    let (target, input, rest) = (left.next()?, left.next()?, left.next()?);
    let mut right = rest.rsplitn(3, separator);
    let (times, delay, param) = (right.next()?, right.next()?, right.next()?);
    let times = times.trim();
    let times = if times.is_empty() { -1 } else { times.parse::<i64>().ok()? };
    let delay = delay.trim();
    let delay = if delay.is_empty() { 0.0 } else { delay.parse::<f64>().ok().filter(|d| d.is_finite())? };
    Some(Output {
        name: name.to_string(),
        target: target.trim().to_string(),
        input: input.trim().to_string(),
        param: param.to_string(),
        delay: delay.max(0.0),
        times: if times < 0 { -1 } else { times },
    })
}

/// The connection a key-value of a map gives, if the key names an output (`On...`, `Out...`)
/// and the value is one.
pub(crate) fn parse(key: &str, value: &str) -> Option<Output> {
    let lower = key.to_ascii_lowercase();
    if !(lower.starts_with("on") || lower.starts_with("out")) {
        return None;
    }
    let separator = if value.contains('\u{1b}') { '\u{1b}' } else { ',' };
    connection(key, value, separator)
}

/// Keeps a connection the map or `SetKeyValue` gives an engine's entity. Whether it was one.
pub(crate) fn add(lua: &Lua, handle: EntityHandle, key: &str, value: &str) -> bool {
    let Some(output) = parse(key, value) else { return false };
    host(lua).entities.with_mut(handle, |ent| ent.outputs.push(output)).is_some()
}

/// `AddOutput` with an output: `OnTrigger target:input:param:delay:times`. Whether it was one.
pub(crate) fn add_from_input(lua: &Lua, handle: EntityHandle, param: &str) -> bool {
    let param = param.trim();
    let Some((name, value)) = param.split_once(char::is_whitespace) else { return false };
    let lower = name.to_ascii_lowercase();
    if !(lower.starts_with("on") || lower.starts_with("out")) {
        return false;
    }
    let value = value.trim();
    let separator = if value.contains('\u{1b}') {
        '\u{1b}'
    } else if value.contains(':') {
        ':'
    } else {
        ','
    };
    let Some(output) = connection(name, value, separator) else { return false };
    host(lua).entities.with_mut(handle, |ent| ent.outputs.push(output)).is_some()
}

/// Fires an engine entity's output: every connection of that name sends its input on, after
/// its delay. `activator` started the chain; `caller` is whoever made `owner` fire; `value`
/// goes to the connections that have no parameter of their own.
pub(crate) fn fire(lua: &Lua, owner: EntityHandle, name: &str, activator: EntityHandle, caller: EntityHandle, value: Option<&str>) {
    let fired: Vec<Output> = host(lua)
        .entities
        .with_mut(owner, |ent| {
            let mut fired = Vec::new();
            ent.outputs.retain_mut(|output| {
                if !output.name.eq_ignore_ascii_case(name) {
                    return true;
                }
                fired.push(output.clone());
                if output.times > 0 {
                    output.times -= 1;
                }
                output.times != 0
            });
            fired
        })
        .unwrap_or_default();
    for output in fired {
        let param = if output.param.is_empty() { value.unwrap_or("") } else { &output.param };
        crate::inputs::fire_named(lua, &output.target, &output.input, param, output.delay, activator, owner, caller);
    }
}

/// The entities a target names, as the engine finds them when the input is given: `owner`
/// fired the output, `activator` started the chain, `caller` made `owner` fire.
pub(crate) fn targets(lua: &Lua, target: &str, owner: EntityHandle, activator: EntityHandle, caller: EntityHandle) -> Vec<EntityHandle> {
    let valid = |h: EntityHandle| host(lua).entities.is_valid(h);
    let one = |h: EntityHandle| if valid(h) { vec![h] } else { Vec::new() };
    match target.to_ascii_lowercase().as_str() {
        "" => Vec::new(),
        "!self" => one(owner),
        "!activator" => one(activator),
        "!caller" => one(caller),
        "!player" | "!pvsplayer" => {
            let entities = &host(lua).entities;
            entities
                .handles()
                .into_iter()
                .filter(|&h| h.kind == Kind::Player && entities.get(h).is_some_and(|e| !e.removed))
                .take(1)
                .collect()
        }
        _ => {
            let named = crate::ents::all_named(lua, target);
            if named.is_empty() { crate::ents::all_of_class(lua, target) } else { named }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connections_as_maps_write_them() {
        let o = parse("OnTrigger", "door1,Open,,2.5,-1").unwrap();
        assert_eq!((o.target.as_str(), o.input.as_str(), o.param.as_str(), o.delay, o.times), ("door1", "Open", "", 2.5, -1));
        // the newer separator; a parameter with commas in it
        let o = parse("OnPressed", "txt\u{1b}AddOutput\u{1b}message a, b\u{1b}0\u{1b}1").unwrap();
        assert_eq!((o.input.as_str(), o.param.as_str(), o.times), ("AddOutput", "message a, b", 1));
        let o = parse("OnTrigger", "x,Display,1,2,3,0,-1").unwrap();
        assert_eq!((o.param.as_str(), o.delay, o.times), ("1,2,3", 0.0, -1));
        // not outputs: a key that is not one, a value that is not a connection
        assert!(parse("targetname", "a,b,c,0,-1").is_none());
        assert!(parse("OnTrigger", "door1").is_none());
        assert!(parse("OnTrigger", "a,b,c,soon,-1").is_none());
        assert!(parse("OnTrigger", "a,b,c,nan,-1").is_none());
        // empty delay and count: now and always; a negative delay is now
        let o = parse("OutValue", "a,b,,,").unwrap();
        assert_eq!((o.delay, o.times), (0.0, -1));
        assert_eq!(parse("OnTrigger", "a,b,,-3,5").unwrap().delay, 0.0);
    }
}
