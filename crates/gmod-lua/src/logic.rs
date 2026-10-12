//! The map's logic: the engine's point entities that only pass inputs and outputs around, as
//! mappers wire them ([`crate::outputs`]).
//!
//! * `logic_auto` fires `OnMapSpawn`, `OnNewGame` and `OnMultiNewMap` once the map's entities
//!   are there (unless it waits for a `globalstate`, which nothing sets); flag 1 removes it
//!   after.
//! * `logic_relay`: `Trigger` fires `OnTrigger` while it is enabled (`Enable`, `Disable`,
//!   `Toggle`, `StartDisabled`). Unless flag 2 lets it fire again at once, it waits until what
//!   it sent last time has been given; flag 1 removes it after the first time. `OnSpawn` when it
//!   spawns, `CancelPending` drops what it sent that still waits.
//! * `logic_timer` fires `OnTimer` every `RefireTime` seconds while enabled - or, with
//!   `UseRandomTime`, after a time between `LowerRandomBound` and `UpperRandomBound` - and an
//!   oscillator (flag 1) `OnTimerHigh` and `OnTimerLow` in turn. `FireTimer`, `ResetTimer`,
//!   `AddToTimer`, `SubtractFromTimer` and the bounds as inputs.
//! * `math_counter` keeps a number (`startvalue`), held between `min` and `max` when they are
//!   not both 0: `Add`, `Subtract`, `Multiply`, `Divide`, `SetValue` fire `OutValue` with it,
//!   and `OnHitMin`/`OnHitMax` when it reaches an end, `OnChangedFromMin`/`OnChangedFromMax`
//!   when it leaves one; `SetValueNoFire`, `GetValue` (`OnGetValue`), `SetHitMin`,
//!   `SetHitMax`. A disabled counter takes nothing but `Enable`.
//! * `logic_branch` keeps a boolean (`InitialValue`): `SetValue`, `Toggle`, their `...Test`
//!   forms and `Test` fire `OnTrue` or `OnFalse`.
//! * `logic_compare` compares its value with `CompareValue`: `SetValueCompare` and `Compare`
//!   fire `OnLessThan`, `OnEqualTo` or `OnGreaterThan`, and `OnNotEqualTo` unless equal.
//! * `logic_case`: `InValue` fires the `OnCaseNN` whose `CaseNN` it equals, else `OnDefault`
//!   with the value; `PickRandom` and `PickRandomShuffle` fire one of the cases that are set.
//!
//! Numbers are compared and kept as Source keeps them, as `f32`.

use mlua::Lua;

use crate::entity::EntityHandle;
use crate::host;

/// What an entity of the logic keeps.
#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Relay {
        enabled: bool,
    },
    Timer(Timer),
    Counter {
        value: f32,
        min: f32,
        max: f32,
        enabled: bool,
    },
    Branch {
        value: bool,
    },
    Compare {
        value: f32,
        compare: f32,
    },
    /// The cases still to pick from by `PickRandomShuffle`.
    Case {
        deck: Vec<usize>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Timer {
    enabled: bool,
    refire: f32,
    random: bool,
    lower: f32,
    upper: f32,
    /// When it fires next.
    next: f64,
    /// An oscillator's next output is `OnTimerHigh`.
    high: bool,
}

/// A number given as text, as Source reads one: what is not a number is 0.
fn number(text: &str) -> f32 {
    crate::convar::atof(text) as f32
}

/// A number as an output passes it on: whole numbers without a point.
fn text(value: f32) -> String {
    if value.fract() == 0.0 && value.abs() < 1e9 { format!("{}", value as i64) } else { format!("{value}") }
}

fn keyvalue(lua: &Lua, handle: EntityHandle, key: &str) -> Option<String> {
    host(lua).entities.get(handle).and_then(|ent| ent.keyvalue(key).map(str::to_string))
}

fn flags(lua: &Lua, handle: EntityHandle) -> i64 {
    host(lua).entities.get(handle).map_or(0, |ent| ent.spawn_flags)
}

fn class(lua: &Lua, handle: EntityHandle) -> String {
    host(lua).entities.get(handle).map(|ent| ent.class.to_ascii_lowercase()).unwrap_or_default()
}

fn state(lua: &Lua, handle: EntityHandle) -> Option<State> {
    host(lua).entities.get(handle).and_then(|ent| ent.logic.as_deref().cloned())
}

fn set_state(lua: &Lua, handle: EntityHandle, state: State) {
    host(lua).entities.with_mut(handle, |ent| ent.logic = Some(Box::new(state)));
}

fn start_disabled(lua: &Lua, handle: EntityHandle) -> bool {
    keyvalue(lua, handle, "StartDisabled").is_some_and(|v| number(&v) != 0.0)
}

/// A spawned entity of the engine takes up its part, if it is one of the logic's.
pub(crate) fn spawned(lua: &Lua, handle: EntityHandle) {
    let enabled = !start_disabled(lua, handle);
    let get = |key: &str| keyvalue(lua, handle, key).map_or(0.0, |v| number(&v));
    let state = match class(lua, handle).as_str() {
        "logic_relay" => State::Relay { enabled },
        "logic_timer" => {
            let mut timer = Timer {
                enabled,
                refire: get("RefireTime"),
                random: get("UseRandomTime") != 0.0,
                lower: get("LowerRandomBound"),
                upper: get("UpperRandomBound"),
                next: f64::INFINITY,
                high: true,
            };
            reset(lua, &mut timer);
            State::Timer(timer)
        }
        "math_counter" => {
            let (min, max) = (get("min"), get("max"));
            State::Counter { value: clamp(get("startvalue"), min, max), min, max, enabled }
        }
        "logic_branch" => State::Branch { value: get("InitialValue") != 0.0 },
        "logic_compare" => State::Compare { value: get("InitialValue"), compare: get("CompareValue") },
        "logic_case" => State::Case { deck: Vec::new() },
        _ => return,
    };
    let relay = matches!(state, State::Relay { .. });
    set_state(lua, handle, state);
    if relay {
        crate::outputs::fire(lua, handle, "OnSpawn", handle, handle, None);
    }
}

/// Once the map's entities are all there: the `logic_auto`s fire.
pub(crate) fn map_spawned(lua: &Lua) {
    for handle in crate::ents::all_of_class(lua, "logic_auto") {
        if keyvalue(lua, handle, "globalstate").is_some_and(|g| !g.trim().is_empty()) {
            continue;
        }
        for output in ["OnMapSpawn", "OnNewGame", "OnMultiNewMap"] {
            crate::outputs::fire(lua, handle, output, EntityHandle::NULL, handle, None);
        }
        if flags(lua, handle) & 1 != 0 {
            crate::ents::remove(lua, handle);
        }
    }
}

/// The next time a timer fires, from now.
fn reset(lua: &Lua, timer: &mut Timer) {
    let now = host(lua).clock.cur_time.get();
    let wait = if timer.random {
        let (lo, hi) = (timer.lower.min(timer.upper), timer.lower.max(timer.upper));
        lo + (hi - lo) * crate::anim::random(lua, 1 << 20) as f32 / (1 << 20) as f32
    } else {
        timer.refire
    };
    // once a tick at most, however short it is told to be
    timer.next = if timer.enabled { now + f64::from(wait).max(crate::timer::TICK_INTERVAL) } else { f64::INFINITY };
}

/// One tick: the timers whose time has come fire.
pub(crate) fn tick(lua: &Lua) {
    let now = host(lua).clock.cur_time.get();
    let due: Vec<EntityHandle> = {
        let entities = &host(lua).entities;
        entities
            .handles()
            .into_iter()
            .filter(|&h| {
                entities.get(h).is_some_and(|ent| !ent.removed && matches!(ent.logic.as_deref(), Some(State::Timer(t)) if t.next <= now))
            })
            .collect()
    };
    for handle in due {
        fire_timer(lua, handle);
    }
}

fn fire_timer(lua: &Lua, handle: EntityHandle) {
    let Some(State::Timer(mut timer)) = state(lua, handle) else { return };
    let output = if flags(lua, handle) & 1 == 0 {
        "OnTimer"
    } else if timer.high {
        "OnTimerHigh"
    } else {
        "OnTimerLow"
    };
    timer.high = !timer.high;
    reset(lua, &mut timer);
    set_state(lua, handle, State::Timer(timer));
    crate::outputs::fire(lua, handle, output, handle, handle, None);
}

fn clamp(value: f32, min: f32, max: f32) -> f32 {
    if min == 0.0 && max == 0.0 { value } else { value.clamp(min.min(max), max.max(min)) }
}

/// An input to an entity of the logic. Whether it took it.
pub(crate) fn input(lua: &Lua, handle: EntityHandle, input: &str, param: &str, activator: EntityHandle, caller: EntityHandle) -> bool {
    let Some(state) = state(lua, handle) else { return false };
    let lower = input.to_ascii_lowercase();
    let fire = |output: &str, value: Option<&str>| crate::outputs::fire(lua, handle, output, activator, caller, value);
    match state {
        State::Relay { enabled } => match lower.as_str() {
            "trigger" => {
                let flags = flags(lua, handle);
                // a relay waits for what it sent to be given, unless it may fire again at once
                if !enabled || (flags & 2 == 0 && crate::inputs::pending(lua, handle)) {
                    return true;
                }
                fire("OnTrigger", None);
                if flags & 1 != 0 {
                    crate::ents::remove(lua, handle);
                }
            }
            "enable" | "enablerefire" => set_state(lua, handle, State::Relay { enabled: lower == "enable" || enabled }),
            "disable" => set_state(lua, handle, State::Relay { enabled: false }),
            "toggle" => set_state(lua, handle, State::Relay { enabled: !enabled }),
            "cancelpending" => crate::inputs::cancel(lua, handle),
            _ => return false,
        },
        State::Timer(mut timer) => {
            let value = number(param);
            match lower.as_str() {
                "enable" => timer.enabled = true,
                "disable" => timer.enabled = false,
                "toggle" => timer.enabled = !timer.enabled,
                "refiretime" => timer.refire = value,
                "lowerrandombound" => timer.lower = value,
                "upperrandombound" => timer.upper = value,
                "resettimer" => {}
                "firetimer" => {
                    set_state(lua, handle, State::Timer(timer));
                    fire_timer(lua, handle);
                    return true;
                }
                "addtotimer" | "subtractfromtimer" => {
                    let by = if lower == "addtotimer" { value } else { -value };
                    timer.next += f64::from(by);
                    set_state(lua, handle, State::Timer(timer));
                    return true;
                }
                _ => return false,
            }
            reset(lua, &mut timer);
            set_state(lua, handle, State::Timer(timer));
        }
        State::Counter { value, min, max, enabled } => {
            let given = number(param);
            if !enabled && lower != "enable" {
                return matches!(
                    lower.as_str(),
                    "disable" | "add" | "subtract" | "multiply" | "divide" | "setvalue" | "setvaluenofire" | "getvalue"
                );
            }
            let new = match lower.as_str() {
                "add" => value + given,
                "subtract" => value - given,
                "multiply" => value * given,
                // dividing by 0 leaves it as it is
                "divide" if given != 0.0 => value / given,
                "divide" => value,
                "setvalue" | "setvaluenofire" => given,
                "getvalue" => {
                    fire("OnGetValue", Some(&text(value)));
                    return true;
                }
                "sethitmax" | "setmaxvaluenofire" => {
                    set_state(lua, handle, State::Counter { value: clamp(value, min, given), min, max: given, enabled });
                    return true;
                }
                "sethitmin" | "setminvaluenofire" => {
                    set_state(lua, handle, State::Counter { value: clamp(value, given, max), min: given, max, enabled });
                    return true;
                }
                "enable" | "disable" => {
                    set_state(lua, handle, State::Counter { value, min, max, enabled: lower == "enable" });
                    return true;
                }
                _ => return false,
            };
            let new = clamp(new, min, max);
            set_state(lua, handle, State::Counter { value: new, min, max, enabled });
            if lower == "setvaluenofire" {
                return true;
            }
            let bounded = !(min == 0.0 && max == 0.0);
            if bounded && value == min && new != min {
                fire("OnChangedFromMin", None);
            }
            if bounded && value == max && new != max {
                fire("OnChangedFromMax", None);
            }
            fire("OutValue", Some(&text(new)));
            if bounded && new >= max && value < max {
                fire("OnHitMax", None);
            }
            if bounded && new <= min && value > min {
                fire("OnHitMin", None);
            }
        }
        State::Branch { value } => {
            let test = |value: bool| fire(if value { "OnTrue" } else { "OnFalse" }, None);
            match lower.as_str() {
                "setvalue" | "setvaluetest" => {
                    let new = number(param) != 0.0;
                    set_state(lua, handle, State::Branch { value: new });
                    if lower == "setvaluetest" {
                        test(new);
                    }
                }
                "toggle" | "toggletest" => {
                    set_state(lua, handle, State::Branch { value: !value });
                    if lower == "toggletest" {
                        test(!value);
                    }
                }
                "test" => test(value),
                _ => return false,
            }
        }
        State::Compare { value, compare } => {
            let given = number(param);
            let (value, compare) = match lower.as_str() {
                "setvalue" | "setvaluecompare" => (given, compare),
                "setcomparevalue" => (value, given),
                "compare" => (value, compare),
                _ => return false,
            };
            set_state(lua, handle, State::Compare { value, compare });
            if lower != "setvalue" && lower != "setcomparevalue" {
                let shown = text(value);
                let output = if value < compare {
                    "OnLessThan"
                } else if value > compare {
                    "OnGreaterThan"
                } else {
                    "OnEqualTo"
                };
                fire(output, Some(&shown));
                if value != compare {
                    fire("OnNotEqualTo", Some(&shown));
                }
            }
        }
        State::Case { mut deck } => {
            let cases: Vec<(usize, String)> =
                (1..=16).filter_map(|n| keyvalue(lua, handle, &format!("Case{n:02}")).filter(|c| !c.is_empty()).map(|c| (n, c))).collect();
            match lower.as_str() {
                "invalue" => match cases.iter().find(|(_, c)| c.eq_ignore_ascii_case(param.trim())) {
                    Some((n, _)) => fire(&format!("OnCase{n:02}"), None),
                    None => fire("OnDefault", Some(param)),
                },
                "pickrandom" | "pickrandomshuffle" => {
                    if cases.is_empty() {
                        return true;
                    }
                    let picked = if lower == "pickrandom" {
                        cases[crate::anim::random(lua, cases.len() as i64) as usize].0
                    } else {
                        // each case once before any comes again
                        deck.retain(|n| cases.iter().any(|(c, _)| c == n));
                        if deck.is_empty() {
                            deck = cases.iter().map(|(n, _)| *n).collect();
                        }
                        deck.swap_remove(crate::anim::random(lua, deck.len() as i64) as usize)
                    };
                    set_state(lua, handle, State::Case { deck });
                    fire(&format!("OnCase{picked:02}"), None);
                }
                _ => return false,
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_as_outputs_pass_them() {
        assert_eq!(text(3.0), "3");
        assert_eq!(text(-2.0), "-2");
        assert_eq!(text(0.5), "0.5");
        assert_eq!(clamp(12.0, 0.0, 0.0), 12.0);
        assert_eq!(clamp(12.0, 0.0, 10.0), 10.0);
        assert_eq!(clamp(-1.0, 0.0, 10.0), 0.0);
    }
}
