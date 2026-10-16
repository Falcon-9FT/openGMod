//! What the surfaces of the world and of bodies are made of ([`gmod_material::SurfaceProps`]):
//! for traces (`SurfaceProps`, `MatType`), footsteps and the sounds of impacts.
//!
//! A surface of the map is its material's `$surfaceprop`; a body's is its model's `.phy`
//! (`PhysObj:GetMaterial`). What names none, or one there is not, is `default`.

use std::cell::RefCell;
use std::collections::HashMap;

use gmod_material::{SurfaceProp, SurfaceProps};
use mlua::Lua;

use crate::host;

/// The surfaces of a runtime, read when first asked for.
#[derive(Default)]
pub(crate) struct Surfaces {
    props: RefCell<Option<SurfaceProps>>,
    /// The surface of each material of the map, by lower-case name.
    by_material: RefCell<HashMap<String, usize>>,
}

/// Something of the game's surfaces.
pub(crate) fn with<R>(lua: &Lua, f: impl FnOnce(&SurfaceProps) -> R) -> R {
    let host = host(lua);
    let vfs = host.vfs.clone();
    let mut props = host.surfaces.props.borrow_mut();
    f(props.get_or_insert_with(|| SurfaceProps::load(&vfs)))
}

/// Surfaces of a `surfaceproperties` file's text (`physenv.AddSurfaceData`), after the game's.
pub(crate) fn add(lua: &Lua, text: &str) {
    with(lua, |_| ());
    if let Some(props) = host(lua).surfaces.props.borrow_mut().as_mut() {
        props.add_file(text);
    }
}

/// `util.GetSurfaceData`, `util.GetSurfacePropName` and `util.GetSurfaceIndex`: the surfaces
/// by number, as traces and bodies give them (`SurfaceProps`, `MaterialOther`).
pub(crate) fn install(lua: &Lua, util: &mlua::Table) -> mlua::Result<()> {
    util.raw_set(
        "GetSurfaceData",
        lua.create_function(|lua, id: Option<f64>| {
            let Some(index) = id.filter(|&i| i >= 0.0).map(|i| i as usize) else { return Ok(mlua::Value::Nil) };
            let Some(prop) = with(lua, |props| props.at(index).cloned()) else { return Ok(mlua::Value::Nil) };
            let t = lua.create_table()?;
            t.raw_set("name", prop.name.as_str())?;
            t.raw_set("density", prop.density)?;
            t.raw_set("elasticity", prop.elasticity)?;
            t.raw_set("friction", prop.friction)?;
            t.raw_set("dampening", prop.dampening)?;
            t.raw_set("thickness", 0.0)?;
            t.raw_set("material", i64::from(prop.game_material))?;
            t.raw_set("jumpFactor", 1.0)?;
            t.raw_set("maxSpeedFactor", 1.0)?;
            t.raw_set("climbable", 0)?;
            t.raw_set("reflectivity", 0.66)?;
            t.raw_set("hardnessFactor", 1.0)?;
            t.raw_set("roughnessFactor", 1.0)?;
            t.raw_set("hardThreshold", 0.5)?;
            t.raw_set("roughThreshold", 0.5)?;
            t.raw_set("hardVelocityThreshold", 0.0)?;
            for (key, sound) in [
                ("stepLeftSound", &prop.step_left),
                ("stepRightSound", &prop.step_right),
                ("impactSoftSound", &prop.impact_soft),
                ("impactHardSound", &prop.impact_hard),
                ("scrapeSmoothSound", &prop.scrape_smooth),
                ("scrapeRoughSound", &prop.scrape_rough),
                ("bulletImpactSound", &prop.bullet_impact),
                ("breakSound", &prop.break_sound),
                ("rollingSound", &String::new()),
                ("strainSound", &String::new()),
            ] {
                t.raw_set(key, sound.as_str())?;
            }
            Ok(mlua::Value::Table(t))
        })?,
    )?;
    util.raw_set(
        "GetSurfacePropName",
        lua.create_function(|lua, id: Option<f64>| {
            let index = id.filter(|&i| i >= 0.0).map(|i| i as usize);
            Ok(index.and_then(|i| with(lua, |props| props.at(i).map(|p| p.name.clone()))).unwrap_or_default())
        })?,
    )?;
    util.raw_set(
        "GetSurfaceIndex",
        lua.create_function(|lua, name: String| Ok(with(lua, |props| props.index(&name).map_or(-1, |i| i as i64))))?,
    )?;
    Ok(())
}

/// A surface by name (`default` for one there is not): its number, and itself.
pub(crate) fn named(lua: &Lua, name: &str) -> (usize, SurfaceProp) {
    with(lua, |props| {
        let index = props.index(name).unwrap_or(0);
        (index, props.at(index).cloned().unwrap_or_default())
    })
}

/// The surface a material of the map is: its `$surfaceprop`.
pub(crate) fn of_material(lua: &Lua, material: &str) -> (usize, SurfaceProp) {
    let key = material.to_lowercase();
    let known = host(lua).surfaces.by_material.borrow().get(&key).copied();
    let index = match known {
        Some(index) => index,
        None => {
            let vfs = host(lua).vfs.clone();
            let name = gmod_material::load_material(&vfs, material).ok().and_then(|m| m.get("$surfaceprop").map(str::to_string));
            let index = with(lua, |props| name.and_then(|n| props.index(&n)).unwrap_or(0));
            host(lua).surfaces.by_material.borrow_mut().insert(key, index);
            index
        }
    };
    (index, with(lua, |props| props.at(index).cloned().unwrap_or_default()))
}
