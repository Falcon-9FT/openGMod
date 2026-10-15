//! Vehicles players sit in (`prop_vehicle_*`): the use key puts a player into an empty one, if
//! `CanPlayerEnterVehicle` lets them, and takes them out again, if `CanExitVehicle` does;
//! `PlayerEnteredVehicle` and `PlayerLeaveVehicle` hear of it. In a vehicle a player is where
//! its seat is (`vehicle_feet_passenger0`), sees from its eyes (`vehicle_driver_eyes`), does
//! not walk nor collide, and turns with it; `VehicleMove` runs instead of the player's movement.
//!
//! A vehicle is a seat here: there is no engine that drives a car (`prop_vehicle_jeep`) - what
//! moves vehicles in GMod's addons is their own Lua.

use mlua::{Lua, MultiValue, Value};

use crate::entity::{self, EntityHandle};
use crate::host;
use crate::trace::{self, Filter, Sweep};
use crate::vector::{Angle, Vector};

/// Whether an entity is a vehicle of the engine.
pub(crate) fn is_vehicle(lua: &Lua, handle: EntityHandle) -> bool {
    host(lua)
        .entities
        .get(handle)
        .is_some_and(|ent| !ent.scripted && !ent.removed && ent.class.to_ascii_lowercase().starts_with("prop_vehicle"))
}

/// The vehicle a player is in; `NULL` for none.
pub(crate) fn of(lua: &Lua, player: EntityHandle) -> EntityHandle {
    let vehicle = crate::player::state(lua, player, |_, p| p.vehicle).unwrap_or(EntityHandle::NULL);
    if host(lua).entities.is_valid(vehicle) { vehicle } else { EntityHandle::NULL }
}

/// Who sits in a vehicle; `NULL` for no one.
pub(crate) fn driver(lua: &Lua, vehicle: EntityHandle) -> EntityHandle {
    if vehicle.index.is_none() {
        return EntityHandle::NULL;
    }
    crate::player::all(lua).into_iter().find(|&p| of(lua, p) == vehicle).unwrap_or(EntityHandle::NULL)
}

/// Where an attachment of the vehicle's model is, by name.
fn attachment(lua: &Lua, vehicle: EntityHandle, name: &str) -> Option<(Vector, Angle)> {
    let model = host(lua).entities.get(vehicle)?.model.clone();
    let studio = crate::ents::studio(lua, &model)?;
    let index = studio.attachments.iter().position(|n| n.eq_ignore_ascii_case(name))?;
    crate::anim::attachment_world(lua, vehicle, index)
}

/// Where the one in a vehicle sees from, and which way its seat faces: its eyes' attachment,
/// else above its origin. A vehicle's model faces along its +y.
pub(crate) fn eyes(lua: &Lua, vehicle: EntityHandle) -> Option<(Vector, Angle)> {
    if let Some(found) = attachment(lua, vehicle, "vehicle_driver_eyes") {
        return Some(found);
    }
    let (pos, angles) = host(lua).entities.transform(vehicle)?;
    let (_, _, up) = angles.vectors();
    Some((pos + up * 36.0, facing(angles)))
}

/// Where the one in a vehicle is: its feet's attachment, else its origin.
fn feet(lua: &Lua, vehicle: EntityHandle) -> Option<Vector> {
    attachment(lua, vehicle, "vehicle_feet_passenger0")
        .map(|(pos, _)| pos)
        .or_else(|| host(lua).entities.transform(vehicle).map(|(p, _)| p))
}

/// The way a vehicle's model faces: its +y.
fn facing(angles: Angle) -> Angle {
    let (_, right, up) = angles.vectors();
    (right * -1.0).angle_with_up(up)
}

fn hook(lua: &Lua, name: &str, args: impl IntoIterator<Item = Value>) -> MultiValue {
    crate::call_hook(lua, name, MultiValue::from_iter(args))
}

fn value(lua: &Lua, handle: EntityHandle) -> Value {
    entity::push(lua, handle).unwrap_or(Value::Nil)
}

/// A player gets into a vehicle, if it is one, they are alive and in none, it is empty, and
/// `CanPlayerEnterVehicle` (unless `forced`) does not say no. Whether they did.
pub(crate) fn enter(lua: &Lua, player: EntityHandle, vehicle: EntityHandle, forced: bool) -> bool {
    if !is_vehicle(lua, vehicle) || !crate::player::alive(lua, player) || of(lua, player).index.is_some() {
        return false;
    }
    if driver(lua, vehicle).index.is_some() {
        return false;
    }
    let role = Value::Integer(0);
    if !forced {
        let said = hook(lua, "CanPlayerEnterVehicle", [value(lua, player), value(lua, vehicle), role.clone()]);
        if matches!(said.front(), Some(Value::Boolean(false))) {
            return false;
        }
    }
    // what the player carries is let go
    if crate::pickup::carrying(lua, player).is_some() {
        crate::pickup::drop(lua, player, false);
    }
    let seat = host(lua).entities.transform(vehicle).map_or_else(Angle::default, |(_, a)| a);
    let group = host(lua).entities.get(player).map_or(0, |ent| ent.collision_group);
    crate::player::with_player(lua, player, |ent, p| {
        p.vehicle = vehicle;
        p.vehicle_angles = seat;
        p.vehicle_group = group;
        ent.collision_group = trace::group::IN_VEHICLE;
        ent.velocity = Vector::default();
    });
    // the player goes with it, as Source has them: its child
    entity::set_parent(lua, player, vehicle);
    // looking the way the seat faces
    if let Some((_, facing)) = eyes(lua, vehicle) {
        crate::player::set_eye_angles(lua, player, Angle::new(0.0, facing.y, 0.0));
    }
    place(lua, player, vehicle);
    hook(lua, "PlayerEnteredVehicle", [value(lua, player), value(lua, vehicle), role]);
    true
}

/// A player gets out of their vehicle, if `CanExitVehicle` (unless `forced`) does not say no:
/// beside it where there is room, standing, looking the way they did. Whether they did.
pub(crate) fn exit(lua: &Lua, player: EntityHandle, forced: bool) -> bool {
    let vehicle = crate::player::state(lua, player, |_, p| p.vehicle).unwrap_or(EntityHandle::NULL);
    if vehicle.index.is_none() {
        return false;
    }
    let there = host(lua).entities.is_valid(vehicle);
    if !forced && there {
        let said = hook(lua, "CanExitVehicle", [value(lua, vehicle), value(lua, player)]);
        if matches!(said.front(), Some(Value::Boolean(false))) {
            return false;
        }
    }
    crate::player::with_player(lua, player, |ent, p| {
        p.vehicle = EntityHandle::NULL;
        ent.collision_group = p.vehicle_group;
    });
    entity::set_parent(lua, player, EntityHandle::NULL);
    if there && let Some(pos) = way_out(lua, player, vehicle) {
        entity::set_world_pos(lua, player, pos);
        let velocity = host(lua).entities.get(vehicle).map_or_else(Vector::default, |ent| ent.velocity);
        host(lua).entities.with_mut(player, |ent| ent.velocity = velocity);
    }
    let angles = crate::player::eye_angles(lua, player).unwrap_or_default();
    crate::player::set_eye_angles(lua, player, Angle::new(angles.p, angles.y, 0.0));
    hook(lua, "PlayerLeaveVehicle", [value(lua, player), value(lua, vehicle)]);
    true
}

/// Where a player getting out of a vehicle stands: the first place beside it - left, right,
/// in front, behind, above - where their hull has room.
fn way_out(lua: &Lua, player: EntityHandle, vehicle: EntityHandle) -> Option<Vector> {
    let (mins, maxs) = crate::player::state(lua, player, |_, p| p.hull)?;
    let (pos, angles) = host(lua).entities.transform(vehicle)?;
    let base = feet(lua, vehicle).unwrap_or(pos);
    // the model's sides are its x, its front its +y
    let (x, y, z) = angles.vectors();
    let ahead = y * -1.0;
    let reach = 48.0;
    let candidates = [x * -reach, x * reach, ahead * reach, ahead * -reach, z * 72.0, Vector::new(0.0, 0.0, 80.0)];
    for offset in candidates {
        let at = base + offset + Vector::new(0.0, 0.0, 4.0);
        let sweep = Sweep { mins, maxs, mask: gmod_bsp::mask::PLAYERSOLID, ..Sweep::line(at, at, Filter::Entities(vec![player, vehicle])) };
        let hit = trace::trace(lua, &sweep);
        if !hit.start_solid && !hit.all_solid {
            return Some(at);
        }
    }
    Some(base + Vector::new(0.0, 0.0, 80.0))
}

/// The player is where the seat is now, moving with it.
fn place(lua: &Lua, player: EntityHandle, vehicle: EntityHandle) {
    let Some(pos) = feet(lua, vehicle) else { return };
    entity::set_world_pos(lua, player, pos);
    let velocity = host(lua).entities.get(vehicle).map_or_else(Vector::default, |ent| ent.velocity);
    host(lua).entities.with_mut(player, |ent| ent.velocity = velocity);
}

/// A tick of a player in a vehicle, instead of their movement: they stay in its seat and turn
/// as it turned; `VehicleMove` runs; the use key gets them out. Whether they are in one.
pub(crate) fn tick(lua: &Lua, player: EntityHandle, ply: &mlua::AnyUserData, mv: Option<mlua::AnyUserData>, pressed: u32) -> bool {
    let vehicle = crate::player::state(lua, player, |_, p| p.vehicle).unwrap_or(EntityHandle::NULL);
    if vehicle.index.is_none() {
        return false;
    }
    if !host(lua).entities.is_valid(vehicle) || !crate::player::alive(lua, player) {
        exit(lua, player, true);
        return false;
    }
    // the view turns as the vehicle did since the last tick
    let now = host(lua).entities.transform(vehicle).map_or_else(Angle::default, |(_, a)| a);
    let before = crate::player::with_player(lua, player, |_, p| std::mem::replace(&mut p.vehicle_angles, now)).unwrap_or(now);
    let turned = crate::vector::normalize_degrees(now.y - before.y);
    if turned.abs() > 1e-3 {
        let eye = crate::player::eye_angles(lua, player).unwrap_or_default();
        crate::player::set_eye_angles(lua, player, Angle::new(eye.p, eye.y + turned, eye.r));
    }
    place(lua, player, vehicle);
    if let Some(mv) = mv {
        hook(lua, "VehicleMove", [Value::UserData(ply.clone()), value(lua, vehicle), Value::UserData(mv)]);
    }
    if pressed & crate::player::IN_USE != 0 {
        exit(lua, player, false);
    }
    true
}

/// An entity is going: a vehicle lets out who is in it.
pub(crate) fn removed(lua: &Lua, handle: EntityHandle) {
    let driver = driver(lua, handle);
    if driver.index.is_some() {
        exit(lua, driver, true);
    }
}

/// The methods of players and vehicles for sitting in them.
pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let player = crate::meta::meta_table(lua, "Player", crate::meta::type_id::ENTITY)?;
    let vehicle = crate::meta::meta_table(lua, "Vehicle", crate::meta::type_id::ENTITY)?;
    let handle = |values: &MultiValue, i: usize| entity::handle_of(values.get(i)).unwrap_or(EntityHandle::NULL);
    let server = host(lua).realm == crate::Realm::Server;
    player.raw_set("InVehicle", lua.create_function(move |lua, values: MultiValue| Ok(of(lua, handle(&values, 0)).index.is_some()))?)?;
    player.raw_set("GetVehicle", lua.create_function(move |lua, values: MultiValue| entity::push(lua, of(lua, handle(&values, 0))))?)?;
    if server {
        player.raw_set(
            "EnterVehicle",
            lua.create_function(move |lua, values: MultiValue| {
                enter(lua, handle(&values, 0), handle(&values, 1), true);
                Ok(())
            })?,
        )?;
        player.raw_set(
            "ExitVehicle",
            lua.create_function(move |lua, values: MultiValue| {
                exit(lua, handle(&values, 0), true);
                Ok(())
            })?,
        )?;
    } else {
        player.raw_set("EnterVehicle", lua.create_function(|_, _: MultiValue| Ok(()))?)?;
        player.raw_set("ExitVehicle", lua.create_function(|_, _: MultiValue| Ok(()))?)?;
    }
    vehicle
        .raw_set("GetDriver", lua.create_function(move |lua, values: MultiValue| entity::push(lua, driver(lua, handle(&values, 0))))?)?;
    // a seat has the one place: the driver's
    vehicle.raw_set(
        "GetPassenger",
        lua.create_function(move |lua, values: MultiValue| {
            let role = match values.get(1) {
                Some(Value::Integer(n)) => *n,
                Some(Value::Number(n)) => *n as i64,
                _ => 0,
            };
            entity::push(lua, if role == 0 { driver(lua, handle(&values, 0)) } else { EntityHandle::NULL })
        })?,
    )?;
    vehicle.raw_set(
        "GetVehicleViewPosition",
        lua.create_function(move |lua, values: MultiValue| {
            let Some((pos, angles)) = eyes(lua, handle(&values, 0)) else { return Ok(MultiValue::new()) };
            Ok(MultiValue::from_iter([entity::vector(lua, pos)?, entity::angle(lua, angles)?, Value::Number(75.0)]))
        })?,
    )?;
    // the name it was spawned by from the list of vehicles (`list.Get("Vehicles")`)
    vehicle.raw_set(
        "GetVehicleClass",
        lua.create_function(move |lua, values: MultiValue| {
            Ok(host(lua).entities.get(handle(&values, 0)).map(|e| e.vehicle_class.clone()).unwrap_or_default())
        })?,
    )?;
    vehicle.raw_set(
        "SetVehicleClass",
        lua.create_function(move |lua, values: MultiValue| {
            let class = match values.get(1) {
                Some(Value::String(s)) => s.to_string_lossy(),
                _ => String::new(),
            };
            host(lua).entities.with_mut(handle(&values, 0), |e| e.vehicle_class = class);
            Ok(())
        })?,
    )?;
    vehicle.raw_set("IsValidVehicle", lua.create_function(move |lua, values: MultiValue| Ok(is_vehicle(lua, handle(&values, 0))))?)?;
    vehicle.raw_set(
        "GetThirdPersonMode",
        lua.create_function(move |lua, values: MultiValue| Ok(host(lua).entities.get(handle(&values, 0)).is_some_and(|e| e.third_person)))?,
    )?;
    vehicle.raw_set(
        "SetThirdPersonMode",
        lua.create_function(move |lua, values: MultiValue| {
            let on = matches!(values.get(1), Some(Value::Boolean(true)));
            host(lua).entities.with_mut(handle(&values, 0), |e| e.third_person = on);
            Ok(())
        })?,
    )?;
    vehicle.raw_set(
        "GetCameraDistance",
        lua.create_function(move |lua, values: MultiValue| {
            Ok(host(lua).entities.get(handle(&values, 0)).map_or(0.0, |e| f64::from(e.camera_distance)))
        })?,
    )?;
    vehicle.raw_set(
        "SetCameraDistance",
        lua.create_function(move |lua, values: MultiValue| {
            let distance = match values.get(1) {
                Some(Value::Integer(n)) => *n as f32,
                Some(Value::Number(n)) => *n as f32,
                _ => 0.0,
            };
            host(lua).entities.with_mut(handle(&values, 0), |e| e.camera_distance = distance);
            Ok(())
        })?,
    )?;
    Ok(())
}
