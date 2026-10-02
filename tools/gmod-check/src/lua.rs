//! `gmod-check lua`: starts the Lua of the game in one realm, as the engine would, and lists
//! what failed - above all the functions of the engine that are not there yet. A client plays
//! a second of frames with a server behind it, the HUD and the panels painted, and opens the
//! spawn menu (Q) and the context menu (C) on the way.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gmod_fs::Vfs;
use gmod_lua::{Output, PlayerInfo, Realm, Runtime, ScriptError};

use crate::out::Out;

pub fn run(vfs: Vfs, realm: &str, map: Option<&gmod_lua::Map>, all: bool, out: &mut Out) -> ExitCode {
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
    let vfs = Arc::new(vfs);
    let runtime = match Runtime::new(realm, vfs.clone()) {
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
    let mut client_report = None;
    if realm == Realm::Client {
        client_report = Some(play_client(&runtime, vfs, map));
    } else {
        if let Some(map) = map.filter(|_| realm == Realm::Server) {
            runtime.spawn_map(map);
            // and the player of a single-player game joins
            runtime.join(PlayerInfo { name: "Player".into(), ..PlayerInfo::default() });
        }
        // one second of game time: the timers and hooks of the first frames
        for _ in 0..60 {
            runtime.think(1.0 / 60.0);
        }
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
    if let Some(map) = map.filter(|_| realm == Realm::Server) {
        out.line(format!("Map {}: {} entities.", map.name, runtime.entity_count()));
        out.line(player_report(&runtime));
    }
    if let Some(report) = client_report {
        out.line(report);
    }
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

/// `KEY_Q` and `KEY_C`, which open the spawn menu and the context menu.
const KEY_Q: i32 = 27;
const KEY_C: i32 = 13;

/// A second of a client's frames, with a server of its own behind it (on the map, if one is
/// given): what the server has comes to the client every frame, what the client sends goes to
/// the server, the HUD and the panels are painted; Q is held from the 10th frame to the
/// 30th, C from the 35th to the 50th. What showed, and how the server did.
fn play_client(client: &Runtime, vfs: Arc<Vfs>, map: Option<&gmod_lua::Map>) -> String {
    let server = match Runtime::new(Realm::Server, vfs) {
        Ok(server) => server,
        Err(e) => return format!("The server could not be started: {e}"),
    };
    server.set_output(|_| {});
    server.start("sandbox");
    if let Some(map) = map {
        server.spawn_map(map);
        client.spawn_map(map);
    }
    let Some(player) = server.join(PlayerInfo { name: "Player".into(), ..PlayerInfo::default() }) else {
        return "No player could join the server.".to_string();
    };
    client.set_screen_size(1280, 720);
    client.set_local_player(player);
    let (mut most_panels, mut spawn_menu, mut context_menu) = (0, 0, 0);
    let mut vertices = 0;
    for frame in 0..60 {
        let dt = 1.0 / 60.0;
        server.think(dt);
        client.apply_snapshot(&server.snapshot());
        client.think(dt);
        for (name, value) in client.take_user_info() {
            server.set_user_info(player, &name, &value);
        }
        for line in client.take_server_commands() {
            server.run_player_command(player, &line);
        }
        match frame {
            10 => client.button(KEY_Q, true),
            30 => client.button(KEY_Q, false),
            35 => client.button(KEY_C, true),
            50 => client.button(KEY_C, false),
            _ => false,
        };
        client.paint(dt);
        vertices = vertices.max(client.with_canvas(|canvas, _| canvas.vertices().len()));
        let shown = client.lua().load("return #vgui.GetAll()").eval::<usize>().unwrap_or(0);
        most_panels = most_panels.max(shown);
        match frame {
            29 => spawn_menu = shown,
            49 => context_menu = shown,
            _ => {}
        }
    }
    format!(
        "Client: 60 frames, at most {most_panels} panels and {vertices} vertices; {spawn_menu} panels with the spawn menu open, \
         {context_menu} with the context menu. Server behind it: {} errors.",
        server.errors().len()
    )
}

/// Where the player got to in the first second, and what they hold.
fn player_report(runtime: &Runtime) -> String {
    const REPORT: &str = "local p = player.GetAll()[1]
        if not IsValid(p) then return 'No player could join.' end
        local pos, w = p:GetPos(), p:GetActiveWeapon()
        return string.format('Player %s: %s at (%.0f %.0f %.0f), %s, health %d, %d weapons, holding %s.', p:Nick(),
            p:Alive() and 'alive' or 'dead', pos.x, pos.y, pos.z, p:OnGround() and 'on the ground' or 'in the air',
            p:Health(), #p:GetWeapons(), IsValid(w) and w:GetClass() or 'nothing')";
    runtime.lua().load(REPORT).eval::<String>().unwrap_or_else(|e| format!("The player could not be looked at: {e}"))
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
