//! What a player's tick of movement goes through in Lua: `CUserCmd`, what the player asked for
//! (the keys held, where they look, how far forward and sideways), and `CMoveData`, the
//! movement being worked out from it - where the player is, how fast, how fast they may go.
//! The hooks `StartCommand`, `SetupMove`, `Move` and `FinishMove` get them (see
//! [`crate::player`]).

use mlua::{AnyUserData, Lua, MultiValue, UserData, UserDataFields, UserDataMethods, Value};

use crate::args::Args;
use crate::entity;
use crate::meta::{self, type_id};
use crate::vector::{Angle, Vector};

/// What a player asks for in a tick, as a client sends it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct UserCmd {
    /// Where the player looks.
    pub view_angles: Angle,
    /// Units a second forward (back when negative), to the right and up; a key held asks for
    /// more than any speed ([`gmod_movement::FULL_MOVE`]).
    pub forward_move: f32,
    pub side_move: f32,
    pub up_move: f32,
    /// `IN_` keys held.
    pub buttons: u32,
    pub impulse: u8,
    pub mouse_wheel: i32,
    pub mouse_x: i32,
    pub mouse_y: i32,
}

/// `CUserCmd`: a command with its number and tick.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CmdData {
    pub cmd: UserCmd,
    pub number: i64,
    pub tick: i64,
    /// The weapon `SelectWeapon` asked for: its entity index.
    pub select: Option<u32>,
}

/// `CMoveData`: one tick of a player's movement.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MoveData {
    pub origin: Vector,
    pub velocity: Vector,
    /// Where the player looks, and where they looked before.
    pub angles: Angle,
    pub old_angles: Angle,
    /// What the movement goes by.
    pub move_angles: Angle,
    pub abs_move_angles: Angle,
    pub buttons: u32,
    pub old_buttons: u32,
    pub forward: f32,
    pub side: f32,
    pub up: f32,
    pub max_speed: f32,
    pub max_client_speed: f32,
    pub impulse: u8,
    pub constraint_center: Vector,
    pub constraint_radius: f32,
    pub constraint_width: f32,
    pub constraint_speed_scale: f32,
    pub final_ideal_velocity: Vector,
    pub final_jump_velocity: Vector,
    pub final_step_height: f32,
}

impl UserData for CmdData {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("MetaName", "CUserCmd");
        fields.add_meta_field("MetaID", type_id::USERCMD);
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function("__index", |lua, (_, key): (AnyUserData, Value)| meta::lookup(lua, "CUserCmd", key));
        methods.add_meta_function("__tostring", |_, _: AnyUserData| Ok("CUserCmd"));
    }
}

impl UserData for MoveData {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("MetaName", "CMoveData");
        fields.add_meta_field("MetaID", type_id::MOVEDATA);
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function("__index", |lua, (_, key): (AnyUserData, Value)| meta::lookup(lua, "CMoveData", key));
        methods.add_meta_function("__tostring", |_, _: AnyUserData| Ok("CMoveData"));
    }
}

const CMD: &str = "CUserCmd";
const MV: &str = "CMoveData";

fn cmd(a: &Args) -> mlua::Result<CmdData> {
    a.userdata::<CmdData>(1, CMD)
}

fn mv(a: &Args) -> mlua::Result<MoveData> {
    a.userdata::<MoveData>(1, MV)
}

fn set_cmd(a: &Args, f: impl FnOnce(&mut CmdData)) -> mlua::Result<Value> {
    a.change(1, CMD, f)
}

fn set_mv(a: &Args, f: impl FnOnce(&mut MoveData)) -> mlua::Result<Value> {
    a.change(1, MV, f)
}

fn number(n: f32) -> mlua::Result<Value> {
    Ok(Value::Number(f64::from(n)))
}

fn integer(n: impl Into<i64>) -> mlua::Result<Value> {
    Ok(Value::Integer(n.into()))
}

fn keys(a: &Args) -> mlua::Result<u32> {
    Ok(a.number(2)? as i64 as u32)
}

fn def_all(lua: &Lua, name: &str, id: i32, methods: &[Method]) -> mlua::Result<()> {
    let meta = meta::meta_table(lua, name, id)?;
    for &(method, f) in methods {
        meta.raw_set(method, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, method)))?)?;
    }
    Ok(())
}

type Method = (&'static str, fn(&Args) -> mlua::Result<Value>);

const CMD_METHODS: &[Method] = &[
    ("CommandNumber", |a| integer(cmd(a)?.number)),
    ("TickCount", |a| integer(cmd(a)?.tick)),
    ("IsForced", |a| {
        cmd(a)?;
        Ok(Value::Boolean(false))
    }),
    ("GetButtons", |a| integer(cmd(a)?.cmd.buttons)),
    ("SetButtons", |a| {
        let b = keys(a)?;
        set_cmd(a, |c| c.cmd.buttons = b)
    }),
    ("AddKey", |a| {
        let b = keys(a)?;
        set_cmd(a, |c| c.cmd.buttons |= b)
    }),
    ("RemoveKey", |a| {
        let b = keys(a)?;
        set_cmd(a, |c| c.cmd.buttons &= !b)
    }),
    ("KeyDown", |a| {
        let b = keys(a)?;
        Ok(Value::Boolean(cmd(a)?.cmd.buttons & b != 0))
    }),
    ("ClearButtons", |a| set_cmd(a, |c| c.cmd.buttons = 0)),
    ("ClearMovement", |a| {
        set_cmd(a, |c| {
            c.cmd.forward_move = 0.0;
            c.cmd.side_move = 0.0;
            c.cmd.up_move = 0.0;
        })
    }),
    ("GetForwardMove", |a| number(cmd(a)?.cmd.forward_move)),
    ("SetForwardMove", |a| {
        let n = a.number(2)? as f32;
        set_cmd(a, |c| c.cmd.forward_move = n)
    }),
    ("GetSideMove", |a| number(cmd(a)?.cmd.side_move)),
    ("SetSideMove", |a| {
        let n = a.number(2)? as f32;
        set_cmd(a, |c| c.cmd.side_move = n)
    }),
    ("GetUpMove", |a| number(cmd(a)?.cmd.up_move)),
    ("SetUpMove", |a| {
        let n = a.number(2)? as f32;
        set_cmd(a, |c| c.cmd.up_move = n)
    }),
    ("GetViewAngles", |a| entity::angle(a.lua, cmd(a)?.cmd.view_angles)),
    ("SetViewAngles", |a| {
        let angles = a.userdata::<Angle>(2, "Angle")?;
        set_cmd(a, |c| c.cmd.view_angles = angles)
    }),
    ("GetImpulse", |a| integer(cmd(a)?.cmd.impulse)),
    ("SetImpulse", |a| {
        let n = a.number(2)? as u8;
        set_cmd(a, |c| c.cmd.impulse = n)
    }),
    ("GetMouseWheel", |a| integer(cmd(a)?.cmd.mouse_wheel)),
    ("SetMouseWheel", |a| {
        let n = a.number(2)? as i32;
        set_cmd(a, |c| c.cmd.mouse_wheel = n)
    }),
    ("GetMouseX", |a| integer(cmd(a)?.cmd.mouse_x)),
    ("SetMouseX", |a| {
        let n = a.number(2)? as i32;
        set_cmd(a, |c| c.cmd.mouse_x = n)
    }),
    ("GetMouseY", |a| integer(cmd(a)?.cmd.mouse_y)),
    ("SetMouseY", |a| {
        let n = a.number(2)? as i32;
        set_cmd(a, |c| c.cmd.mouse_y = n)
    }),
    ("SelectWeapon", |a| {
        let weapon = entity::handle_of(a.get(2)).ok_or_else(|| a.bad(2, "Weapon"))?;
        set_cmd(a, |c| c.select = weapon.index)
    }),
];

const MV_METHODS: &[Method] = &[
    ("GetOrigin", |a| entity::vector(a.lua, mv(a)?.origin)),
    ("SetOrigin", |a| {
        let v = a.userdata::<Vector>(2, "Vector")?;
        set_mv(a, |m| m.origin = v)
    }),
    ("GetVelocity", |a| entity::vector(a.lua, mv(a)?.velocity)),
    ("SetVelocity", |a| {
        let v = a.userdata::<Vector>(2, "Vector")?;
        set_mv(a, |m| m.velocity = v)
    }),
    ("GetAngles", |a| entity::angle(a.lua, mv(a)?.angles)),
    ("SetAngles", |a| {
        let v = a.userdata::<Angle>(2, "Angle")?;
        set_mv(a, |m| m.angles = v)
    }),
    ("GetOldAngles", |a| entity::angle(a.lua, mv(a)?.old_angles)),
    ("SetOldAngles", |a| {
        let v = a.userdata::<Angle>(2, "Angle")?;
        set_mv(a, |m| m.old_angles = v)
    }),
    ("GetMoveAngles", |a| entity::angle(a.lua, mv(a)?.move_angles)),
    ("SetMoveAngles", |a| {
        let v = a.userdata::<Angle>(2, "Angle")?;
        set_mv(a, |m| m.move_angles = v)
    }),
    ("GetAbsMoveAngles", |a| entity::angle(a.lua, mv(a)?.abs_move_angles)),
    ("SetAbsMoveAngles", |a| {
        let v = a.userdata::<Angle>(2, "Angle")?;
        set_mv(a, |m| m.abs_move_angles = v)
    }),
    ("GetButtons", |a| integer(mv(a)?.buttons)),
    ("SetButtons", |a| {
        let b = keys(a)?;
        set_mv(a, |m| m.buttons = b)
    }),
    ("GetOldButtons", |a| integer(mv(a)?.old_buttons)),
    ("SetOldButtons", |a| {
        let b = keys(a)?;
        set_mv(a, |m| m.old_buttons = b)
    }),
    ("AddKey", |a| {
        let b = keys(a)?;
        set_mv(a, |m| m.buttons |= b)
    }),
    ("RemoveKey", |a| {
        let b = keys(a)?;
        set_mv(a, |m| m.buttons &= !b)
    }),
    ("ClearButtons", |a| set_mv(a, |m| m.buttons = 0)),
    ("ClearMovement", |a| {
        set_mv(a, |m| {
            m.forward = 0.0;
            m.side = 0.0;
            m.up = 0.0;
        })
    }),
    ("KeyDown", |a| {
        let b = keys(a)?;
        Ok(Value::Boolean(mv(a)?.buttons & b != 0))
    }),
    ("KeyWasDown", |a| {
        let b = keys(a)?;
        Ok(Value::Boolean(mv(a)?.old_buttons & b != 0))
    }),
    ("KeyPressed", |a| {
        let (b, m) = (keys(a)?, mv(a)?);
        Ok(Value::Boolean(m.buttons & b != 0 && m.old_buttons & b == 0))
    }),
    ("KeyReleased", |a| {
        let (b, m) = (keys(a)?, mv(a)?);
        Ok(Value::Boolean(m.buttons & b == 0 && m.old_buttons & b != 0))
    }),
    ("GetForwardSpeed", |a| number(mv(a)?.forward)),
    ("SetForwardSpeed", |a| {
        let n = a.number(2)? as f32;
        set_mv(a, |m| m.forward = n)
    }),
    ("GetSideSpeed", |a| number(mv(a)?.side)),
    ("SetSideSpeed", |a| {
        let n = a.number(2)? as f32;
        set_mv(a, |m| m.side = n)
    }),
    ("GetUpSpeed", |a| number(mv(a)?.up)),
    ("SetUpSpeed", |a| {
        let n = a.number(2)? as f32;
        set_mv(a, |m| m.up = n)
    }),
    ("GetMaxSpeed", |a| number(mv(a)?.max_speed)),
    ("SetMaxSpeed", |a| {
        let n = a.number(2)? as f32;
        set_mv(a, |m| m.max_speed = n)
    }),
    ("GetMaxClientSpeed", |a| number(mv(a)?.max_client_speed)),
    ("SetMaxClientSpeed", |a| {
        let n = a.number(2)? as f32;
        set_mv(a, |m| m.max_client_speed = n)
    }),
    ("GetImpulseCommand", |a| integer(mv(a)?.impulse)),
    ("SetImpulseCommand", |a| {
        let n = a.number(2)? as u8;
        set_mv(a, |m| m.impulse = n)
    }),
    ("GetConstraintCenter", |a| entity::vector(a.lua, mv(a)?.constraint_center)),
    ("SetConstraintCenter", |a| {
        let v = a.userdata::<Vector>(2, "Vector")?;
        set_mv(a, |m| m.constraint_center = v)
    }),
    ("GetConstraintRadius", |a| number(mv(a)?.constraint_radius)),
    ("SetConstraintRadius", |a| {
        let n = a.number(2)? as f32;
        set_mv(a, |m| m.constraint_radius = n)
    }),
    ("GetConstraintWidth", |a| number(mv(a)?.constraint_width)),
    ("SetConstraintWidth", |a| {
        let n = a.number(2)? as f32;
        set_mv(a, |m| m.constraint_width = n)
    }),
    ("GetConstraintSpeedScale", |a| number(mv(a)?.constraint_speed_scale)),
    ("SetConstraintSpeedScale", |a| {
        let n = a.number(2)? as f32;
        set_mv(a, |m| m.constraint_speed_scale = n)
    }),
    ("GetFinalIdealVelocity", |a| entity::vector(a.lua, mv(a)?.final_ideal_velocity)),
    ("SetFinalIdealVelocity", |a| {
        let v = a.userdata::<Vector>(2, "Vector")?;
        set_mv(a, |m| m.final_ideal_velocity = v)
    }),
    ("GetFinalJumpVelocity", |a| entity::vector(a.lua, mv(a)?.final_jump_velocity)),
    ("SetFinalJumpVelocity", |a| {
        let v = a.userdata::<Vector>(2, "Vector")?;
        set_mv(a, |m| m.final_jump_velocity = v)
    }),
    ("GetFinalStepHeight", |a| number(mv(a)?.final_step_height)),
    ("SetFinalStepHeight", |a| {
        let n = a.number(2)? as f32;
        set_mv(a, |m| m.final_step_height = n)
    }),
];

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    def_all(lua, CMD, type_id::USERCMD, CMD_METHODS)?;
    def_all(lua, MV, type_id::MOVEDATA, MV_METHODS)
}
