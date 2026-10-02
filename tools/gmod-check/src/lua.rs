//! `gmod-check lua`: starts the Lua of the game in one realm, as the engine would, and lists
//! what failed - above all the functions of the engine that are not there yet.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gmod_fs::Vfs;
use gmod_lua::{Output, Realm, Runtime, ScriptError};

use crate::out::Out;

pub fn run(vfs: Vfs, realm: &str, all: bool, out: &mut Out) -> ExitCode {
    let realm = match realm.to_ascii_lowercase().as_str() {
        "" | "server" | "sv" => Realm::Server,
        "client" | "cl" => Realm::Client,
        "menu" => Realm::Menu,
        other => {
            eprintln!("{other:?} is not a realm: server, client or menu");
            return ExitCode::from(2);
        }
    };
    let started = Instant::now();
    let runtime = match Runtime::new(realm, Arc::new(vfs)) {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("LuaJIT could not be started: {e}");
            return ExitCode::FAILURE;
        }
    };
    let printed = Rc::new(RefCell::new(String::new()));
    let sink = printed.clone();
    runtime.set_output(move |output| match output {
        Output::Text(text) => sink.borrow_mut().push_str(text),
        Output::Error(error) => {
            let mut sink = sink.borrow_mut();
            if !sink.is_empty() && !sink.ends_with('\n') {
                sink.push('\n');
            }
            sink.push_str(&error.to_string());
            sink.push('\n');
        }
    });

    if let Some(dir) = gmod_fs::UserDir::default_root().and_then(|root| gmod_fs::UserDir::open(root, None).ok()) {
        runtime.set_data_dir(dir);
    }
    runtime.start("sandbox");
    // one second of game time: the timers and hooks of the first frames
    for _ in 0..60 {
        runtime.think(1.0 / 60.0);
    }

    for line in printed.borrow().lines() {
        out.line(line);
    }
    let errors = runtime.errors();
    out.line("");
    out.line(format!(
        "Lua, {realm}: {} files loaded, {} errors, {} files sent to clients, in {:.2} s.",
        runtime.files_loaded(),
        errors.len(),
        runtime.sent_files().len(),
        started.elapsed().as_secs_f64()
    ));
    let missing = missing_globals(&errors);
    if !missing.is_empty() {
        out.line("Missing in openGMOD (global, errors it caused):");
        for (name, count) in missing {
            out.line(format!("  {name:<32} {count}"));
        }
    }
    print_coverage(&runtime, all, out);
    if errors.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// GMod's Lua API by the wiki: realms (`S`, `C`, `M`) and names (`net.Start`,
/// `Entity:GetPos`, `_G.print`). `scripts/wiki-api.py` rebuilds it.
const API: &str = include_str!("../data/api.txt");

/// Whether the function `name` of the API is there, made by openGMOD or by the game's Lua.
fn exists(lua: &gmod_lua::mlua::Lua, name: &str) -> bool {
    use gmod_lua::mlua::{Table, Value};
    let lookup = || -> gmod_lua::mlua::Result<bool> {
        let globals = lua.globals();
        let value: Value = if let Some((owner, method)) = name.split_once(':') {
            let find: gmod_lua::mlua::Function = globals.get("FindMetaTable")?;
            match find.call::<Option<Table>>(owner)? {
                Some(meta) => meta.get(method)?,
                None => Value::Nil,
            }
        } else if let Some((library, function)) = name.split_once('.') {
            let table: Value = if library == "_G" { Value::Table(globals.clone()) } else { globals.get(library)? };
            match table {
                Value::Table(table) => table.get(function)?,
                _ => Value::Nil,
            }
        } else {
            globals.get(name)?
        };
        Ok(matches!(value, Value::Function(_)))
    };
    lookup().unwrap_or(false)
}

/// How much of the realm's API is there, by library, the gaps first.
fn print_coverage(runtime: &Runtime, all: bool, out: &mut Out) {
    let letter = match runtime.realm() {
        Realm::Server => 'S',
        Realm::Client => 'C',
        Realm::Menu => 'M',
    };
    let mut libraries: BTreeMap<&str, (usize, Vec<&str>)> = BTreeMap::new();
    for line in API.lines().filter(|l| !l.starts_with('#')) {
        let Some((realms, name)) = line.split_once(' ') else { continue };
        if !realms.contains(letter) {
            continue;
        }
        let library = name.split(['.', ':']).next().unwrap_or(name);
        let entry = libraries.entry(library).or_default();
        if exists(runtime.lua(), name) {
            entry.0 += 1;
        } else {
            entry.1.push(name);
        }
    }
    let present: usize = libraries.values().map(|(n, _)| n).sum();
    let total: usize = libraries.values().map(|(n, missing)| n + missing.len()).sum();
    out.line("");
    out.line(format!(
        "API of the {} realm (GMod wiki): {present} of {total} functions are there ({:.1}%).",
        runtime.realm(),
        100.0 * present as f64 / total.max(1) as f64
    ));
    let mut gaps: Vec<_> = libraries.iter().filter(|(_, (_, missing))| !missing.is_empty()).collect();
    gaps.sort_by_key(|(name, (_, missing))| (std::cmp::Reverse(missing.len()), **name));
    let shown = if all { gaps.len() } else { gaps.len().min(15) };
    for (library, (present, missing)) in &gaps[..shown] {
        let line = format!("  {library:<24} {present:>4} of {:<4}", present + missing.len());
        if all {
            out.line(format!("{line} missing: {}", missing.join(", ")));
        } else {
            out.line(line);
        }
    }
    if shown < gaps.len() {
        out.line(format!("  ... {} more libraries and types (--all lists them, and every missing function)", gaps.len() - shown));
    }
}

/// The globals scripts called or indexed while they were nil: what the engine lacks.
fn missing_globals(errors: &[ScriptError]) -> BTreeMap<String, usize> {
    let mut missing = BTreeMap::new();
    for error in errors {
        for pattern in ["attempt to call global '", "attempt to index global '", "attempt to call field '"] {
            if let Some(at) = error.message.find(pattern) {
                let rest = &error.message[at + pattern.len()..];
                if let Some(end) = rest.find('\'') {
                    *missing.entry(rest[..end].to_string()).or_insert(0) += 1;
                }
            }
        }
    }
    missing
}
