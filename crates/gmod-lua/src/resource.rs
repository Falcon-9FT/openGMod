//! `resource`: the files a server has its clients download beside the client Lua - models,
//! materials, sounds of its addons ([`crate::Runtime::downloads`]). The program sends a client
//! those it lacks or has otherwise when it connects.
//!
//! `AddFile` adds the files that go with the one named, as the engine does: a model its
//! vertices, meshes, physics and icon (`.vvd`, `.dx80.vtx`, `.dx90.vtx`, `.sw.vtx`, `.phy`,
//! `.ani`, `.jpg`), a material its texture of the same name; those the server does not have are
//! left out. Workshop items (`AddWorkshop`) are noted: there is no Steam to fetch them from.

use std::cell::RefCell;

use mlua::{Lua, MultiValue, Value};

use crate::args::Args;
use crate::host;

/// What clients are to download.
#[derive(Default)]
pub(crate) struct Resources {
    files: RefCell<Vec<String>>,
    workshop: RefCell<Vec<String>>,
}

/// What a model brings along, after its `.mdl`.
const MODEL_FILES: &[&str] = &[".vvd", ".ani", ".dx80.vtx", ".dx90.vtx", ".sw.vtx", ".phy", ".jpg"];

fn add(lua: &Lua, path: &str) {
    let Some(path) = gmod_fs::path::normalize(path) else { return };
    let host = host(lua);
    if host.vfs.locate(&path, "GAME").is_none() {
        return;
    }
    let mut files = host.resources.files.borrow_mut();
    if !files.contains(&path) {
        files.push(path);
    }
}

fn add_file(a: &Args) -> mlua::Result<Value> {
    let path = a.string(1)?.replace('\\', "/");
    let lower = path.to_ascii_lowercase();
    if host(a.lua).vfs.locate(&lower, "GAME").is_none() {
        host(a.lua).print(&format!("resource.AddFile: couldn't find '{path}'\n"));
    }
    add(a.lua, &path);
    if let Some(stem) = lower.strip_suffix(".mdl") {
        for extension in MODEL_FILES {
            add(a.lua, &format!("{stem}{extension}"));
        }
    } else if let Some(stem) = lower.strip_suffix(".vmt") {
        add(a.lua, &format!("{stem}.vtf"));
    }
    Ok(Value::Nil)
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let resource = lua.create_table()?;
    let def = |name: &'static str, f: fn(&Args) -> mlua::Result<Value>| -> mlua::Result<()> {
        resource.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
    };
    def("AddFile", add_file)?;
    def("AddSingleFile", |a| {
        add(a.lua, &a.string(1)?);
        Ok(Value::Nil)
    })?;
    def("AddWorkshop", |a| {
        let id = a.string(1)?;
        let host = host(a.lua);
        let mut workshop = host.resources.workshop.borrow_mut();
        if !workshop.contains(&id) {
            workshop.push(id);
        }
        Ok(Value::Nil)
    })?;
    lua.globals().raw_set("resource", resource)?;
    Ok(())
}

impl crate::Runtime {
    /// The files the server's Lua has clients download (`resource.AddFile`), as game paths,
    /// in the order they were added.
    pub fn downloads(&self) -> Vec<String> {
        host(&self.lua).resources.files.borrow().clone()
    }

    /// The workshop items the server's Lua has clients download (`resource.AddWorkshop`).
    pub fn workshop_downloads(&self) -> Vec<String> {
        host(&self.lua).resources.workshop.borrow().clone()
    }

    /// The gamemodes running, the base first (`sandbox` on `base`).
    pub fn gamemodes(&self) -> Vec<String> {
        host(&self.lua).game.chain.borrow().clone()
    }
}
