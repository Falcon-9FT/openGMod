//! What the surfaces of the world and of bodies are made of ([`gmod_material::SurfaceProps`]):
//! for traces (`SurfaceProps`, `MatType`), footsteps and the sounds of impacts.
//!
//! A surface of the map is its material's `$surfaceprop`; a body's is its model's `.phy`
//! (`PhysObj:GetMaterial`). What names none, or one there is not, is `default`.

use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;

use gmod_material::{SurfaceProp, SurfaceProps};
use mlua::Lua;

use crate::host;

/// The surfaces of a runtime, read when first asked for.
#[derive(Default)]
pub(crate) struct Surfaces {
    props: OnceCell<SurfaceProps>,
    /// The surface of each material of the map, by lower-case name.
    by_material: RefCell<HashMap<String, usize>>,
}

/// Something of the game's surfaces.
pub(crate) fn with<R>(lua: &Lua, f: impl FnOnce(&SurfaceProps) -> R) -> R {
    let host = host(lua);
    let vfs = host.vfs.clone();
    f(host.surfaces.props.get_or_init(|| SurfaceProps::load(&vfs)))
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
