//! `gmod-check lua`: starts the Lua of the game in one realm, as the engine would, and lists
//! what failed - above all the functions of the engine that are not there yet. A client plays
//! a second of frames with a server behind it, the HUD and the panels painted, and opens the
//! spawn menu (Q) and the context menu (C) on the way, clicking the first prop of the spawn menu
//! and the first entity of its Entities tab. On a map the server also spawns every entity of
//! the Entities tab, as a click on it does (`gm_spawnsent`).

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
        // one second of game time: the timers and hooks of the first frames; the menu paints
        // its panels in them
        if realm == Realm::Menu {
            runtime.set_screen_size(1280, 720);
        }
        for _ in 0..60 {
            runtime.think(1.0 / 60.0);
            if realm == Realm::Menu {
                runtime.paint(1.0 / 60.0);
            }
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
        for line in spawn_sents(&runtime, &printed) {
            out.line(line);
        }
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
/// 30th, and the first prop of the spawn menu clicked on the way; C is held from the 35th to
/// the 50th. What showed, whether the click spawned the prop, what sounds played, and how
/// the server did.
fn play_client(client: &Runtime, vfs: Arc<Vfs>, map: Option<&gmod_lua::Map>) -> String {
    let server = match Runtime::new(Realm::Server, vfs.clone()) {
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
    let mut drawn = 0;
    // the first prop of the spawn list that shows, which is clicked
    let mut clicked: Option<String> = None;
    // the files of the sounds played, and whether the game has each
    let mut sounds: Vec<(String, bool)> = Vec::new();
    // the time each part of a frame took, all frames together: the server, the client's Lua,
    // the HUD and panels, the entities to draw
    let mut took = [0.0f64; 4];
    let timed = |slot: &mut f64, f: &mut dyn FnMut()| {
        let start = Instant::now();
        f();
        *slot += start.elapsed().as_secs_f64();
    };
    for frame in 0..60 {
        let dt = 1.0 / 60.0;
        let [server_time, client_time, paint_time, drawn_time] = &mut took;
        timed(server_time, &mut || server.think(dt));
        timed(client_time, &mut || {
            client.apply_snapshot(&server.snapshot());
            see(client, player);
            client.think(dt);
        });
        for (name, value) in client.take_user_info() {
            server.set_user_info(player, &name, &value);
        }
        for line in client.take_server_commands() {
            server.run_player_command(player, &line);
        }
        exchange(&server, client, player);
        if frame == 20 {
            let icon = client.lua().load(FIRST_ICON).eval::<(Option<String>, Option<i32>, Option<i32>)>();
            if let Ok((Some(model), Some(x), Some(y))) = icon {
                client.set_cursor(x, y);
                clicked = Some(model);
            }
        }
        match frame {
            10 => client.button(KEY_Q, true),
            21 if clicked.is_some() => client.button(MOUSE_LEFT, true),
            22 if clicked.is_some() => client.button(MOUSE_LEFT, false),
            30 => client.button(KEY_Q, false),
            35 => client.button(KEY_C, true),
            50 => client.button(KEY_C, false),
            _ => false,
        };
        timed(paint_time, &mut || client.paint(dt));
        for event in server.take_sounds().into_iter().chain(client.take_sounds()) {
            if let gmod_lua::sound::SoundEvent::Play(play) = event {
                let there = vfs.exists(&play.wave, "GAME");
                sounds.push((play.wave, there));
            }
        }
        // what the renderer is given: the entities' Lua draws them
        timed(drawn_time, &mut || {
            drawn = drawn.max(client.drawn(player).len());
            let _ = client.take_primitives();
        });
        vertices = vertices.max(client.with_canvas(|canvas, _| canvas.vertices().len()));
        let shown = client.lua().load("return #vgui.GetAll()").eval::<usize>().unwrap_or(0);
        most_panels = most_panels.max(shown);
        match frame {
            29 => spawn_menu = shown,
            49 => context_menu = shown,
            _ => {}
        }
    }
    let click = match clicked {
        None => "The spawn menu showed no prop to click.".to_string(),
        Some(model) => {
            let spawned = server.lua().load(SPAWNED).call::<Option<String>>(model.as_str()).ok().flatten();
            match spawned {
                Some(class) => format!("A click on {model} in the spawn menu spawned a {class} on the server."),
                None => format!("A click on {model} in the spawn menu spawned nothing on the server (is the model in the game?)."),
            }
        }
    };
    // the Entities tab of the spawn menu: its first category opened, its first icon clicked
    let mut entity_click = "The Entities tab showed no entity to click.".to_string();
    client.button(KEY_Q, true);
    let mut picked: Option<String> = None;
    for frame in 0..40 {
        let dt = 1.0 / 60.0;
        server.think(dt);
        client.apply_snapshot(&server.snapshot());
        client.think(dt);
        for line in client.take_server_commands() {
            entity_click.push_str(&format!(" [sent: {line}]"));
            server.run_player_command(player, &line);
        }
        match frame {
            2 => {
                if let Err(e) = client.run_string(OPEN_ENTITIES, "gmod-check") {
                    entity_click = format!("The Entities tab could not be opened: {e}");
                }
            }
            6 => {
                if let Err(e) = client.run_string(OPEN_CATEGORY, "gmod-check") {
                    entity_click = format!("The Entities tab could not be opened: {e}");
                }
            }
            10 => {
                let icon = client.lua().load(FIRST_ENTITY).eval::<(Option<String>, Option<i32>, Option<i32>)>();
                match icon {
                    Ok((Some(name), Some(x), Some(y))) => {
                        client.set_cursor(x, y);
                        picked = Some(name);
                    }
                    Ok(_) => {}
                    Err(e) => entity_click = format!("The Entities tab could not be looked at: {e}"),
                }
            }
            11 if picked.is_some() => {
                client.button(MOUSE_LEFT, true);
            }
            12 if picked.is_some() => {
                client.button(MOUSE_LEFT, false);
            }
            _ => {}
        }
        client.paint(dt);
    }
    client.button(KEY_Q, false);
    if let Some(name) = picked {
        let on_server = server.lua().load(SPAWNED_CLASS).call::<Option<String>>(name.as_str()).ok().flatten();
        let on_client = client.lua().load(SPAWNED_CLASS).call::<Option<String>>(name.as_str()).ok().flatten();
        entity_click = format!(
            "{} A click on the entity {name}: on the server {}, on the client {}.",
            entity_click.trim_start_matches("The Entities tab showed no entity to click."),
            on_server.unwrap_or_else(|| "nothing".into()),
            on_client.unwrap_or_else(|| "nothing".into())
        );
    }
    let click = format!("{click} {}", entity_click.trim());
    let missing: Vec<&str> = sounds.iter().filter(|(_, there)| !there).map(|(wave, _)| wave.as_str()).collect();
    let heard = match (sounds.len(), missing.first()) {
        (0, _) => "No sounds played.".to_string(),
        (1, None) => "1 sound played, its file there.".to_string(),
        (n, None) => format!("{n} sounds played, their files all there."),
        (n, Some(first)) => format!("{n} sound{} played, {} missing ({first}...).", if n == 1 { "" } else { "s" }, missing.len()),
    };
    format!(
        "Client: 60 frames, at most {most_panels} panels, {vertices} vertices and {drawn} models; {spawn_menu} panels with the spawn menu open, \
         {context_menu} with the context menu. {click} {heard} Server behind it: {} errors. \
         A frame took on average {:.1} ms on the server, {:.1} ms of the client's Lua, {:.1} ms for the HUD and panels, {:.1} ms for the entities to draw.",
        server.errors().len(),
        took[0] * 1000.0 / 60.0,
        took[1] * 1000.0 / 60.0,
        took[2] * 1000.0 / 60.0,
        took[3] * 1000.0 / 60.0
    )
}

/// The view the player sees as the program makes it: from the eyes, as the client's Lua
/// changes it (`CalcView`).
fn see(client: &Runtime, player: gmod_lua::entity::EntityHandle) {
    if let Some(view) = client.view(player) {
        let eyes = gmod_lua::client::View { origin: view.eye, angles: view.angles, fov: view.fov };
        client.set_view(client.calc_view(player, eyes));
    }
}

/// What the server and the client say to each other between frames, as the game passes it:
/// `net` messages both ways, and what the server asks of the client (`SendLua` ...).
fn exchange(server: &Runtime, client: &Runtime, player: gmod_lua::EntityHandle) {
    for message in server.take_net_messages() {
        if message.to.contains(&player.index.unwrap_or(0)) {
            client.receive_net_message(&message.name, &message.data, gmod_lua::EntityHandle::NULL);
        }
    }
    for (to, asked) in server.take_to_clients() {
        if to == player {
            client.from_server(&asked);
        }
    }
    for message in client.take_net_messages() {
        server.receive_net_message(&message.name, &message.data, player);
    }
}

/// `MOUSE_LEFT`.
const MOUSE_LEFT: i32 = 107;

/// The first spawn icon that shows: its model and the middle of it on the screen.
const FIRST_ICON: &str = "for _, p in ipairs(vgui.GetAll()) do
        if p.ClassName == 'SpawnIcon' and p:IsVisible() then
            local x, y = p:LocalToScreen(p:GetWide() / 2, p:GetTall() / 2)
            return p:GetModelName(), math.floor(x), math.floor(y)
        end
    end";

/// Switches the spawn menu to its Entities tab, which makes its panel when it first shows.
const OPEN_ENTITIES: &str = "g_SpawnMenu.CreateMenu:SwitchToName('#spawnmenu.category.entities')";

/// Opens the first category of the Entities tab's tree.
const OPEN_CATEGORY: &str = "local tab = g_SpawnMenu.CreateMenu:GetCreationTab('#spawnmenu.category.entities')
    local content = tab and tab.ContentPanel
    if not content then error('the Entities tab made no panel') end
    for _, node in ipairs(content.ContentNavBar.Tree:Root():GetChildNodes()) do
        node:InternalDoClick() return
    end
    error('the Entities tab has no categories')";

/// The first entity icon that shows: its class, what it shows, and the middle of it on the screen.
const FIRST_ENTITY: &str = "for _, p in ipairs(vgui.GetAll()) do
        if p.ClassName == 'ContentIcon' and p:GetContentType() == 'entity' and p:IsVisible() then
            local x, y = p:LocalToScreen(p:GetWide() / 2, p:GetTall() / 2)
            return p:GetSpawnName(), math.floor(x), math.floor(y)
        end
    end";

/// Something about the entities of a class, if there are any: their model and where.
const SPAWNED_CLASS: &str = "for _, e in ipairs(ents.FindByClass(...)) do
        local p = e:GetPos()
        return string.format('a %s (model %s) at (%.0f %.0f %.0f)', e:GetClass(), tostring(e:GetModel()), p.x, p.y, p.z)
    end";

/// The class of an entity of the server with this model, if there is one.
const SPAWNED: &str = "local model = string.lower(...)
    for _, e in ipairs(ents.GetAll()) do
        if string.lower(e:GetModel() or '') == model then return e:GetClass() end
    end";

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

/// The entities of the spawn menu (`SpawnableEntities`), sorted.
const SPAWNABLE: &str = "local names = {}
    for name in pairs(list.Get('SpawnableEntities')) do names[#names + 1] = name end
    table.sort(names) return names";

/// Every entity of the spawn menu spawned by the player, as a click on its icon does
/// (`gm_spawnsent`), and removed again: which spawned and which did not, with what the
/// scripts said on the way.
fn spawn_sents(runtime: &Runtime, printed: &Rc<RefCell<String>>) -> Vec<String> {
    let Some(player) = runtime.lua().load("return player.GetAll()[1]").eval::<Option<gmod_lua::mlua::AnyUserData>>().ok().flatten() else {
        return vec!["No player to spawn the entities of the spawn menu.".to_string()];
    };
    let player = match player.borrow::<gmod_lua::EntityHandle>() {
        Ok(handle) => *handle,
        Err(_) => return vec!["The player is not an entity.".to_string()],
    };
    // looking at the floor in front, as a player spawning things does: most `SpawnFunction`s
    // spawn nothing when the trace hits nothing
    runtime.run_string("player.GetAll()[1]:SetEyeAngles(Angle(45, 0, 0))", "gmod-check").ok();
    let names = runtime.lua().load(SPAWNABLE).eval::<Vec<String>>().unwrap_or_default();
    let mut lines = vec![format!("Spawn menu entities: {}", names.len())];
    let mut spawned = 0;
    for name in &names {
        let before = runtime.errors().len();
        printed.borrow_mut().clear();
        runtime.lua().globals().set("SPAWNED_SENT", gmod_lua::mlua::Value::Nil).ok();
        runtime.run_string("hook.Add('PlayerSpawnedSENT', 'gmod-check', function(_, e) SPAWNED_SENT = e end)", "gmod-check").ok();
        runtime.run_player_command(player, &format!("gm_spawnsent {name}"));
        for _ in 0..3 {
            runtime.think(1.0 / 60.0);
        }
        let class = runtime
            .lua()
            .load("local e = SPAWNED_SENT if IsValid(e) then local c = e:GetClass() .. ' (' .. tostring(e:GetModel()) .. ')' e:Remove() return c end")
            .eval::<Option<String>>()
            .ok()
            .flatten();
        let errors: Vec<String> = runtime.errors()[before..].iter().map(|e| e.to_string()).collect();
        match class {
            Some(class) if errors.is_empty() => {
                spawned += 1;
                lines.push(format!("  {name}: spawned {class}"));
            }
            Some(class) => lines.push(format!("  {name}: spawned {class}, with errors:\n    {}", errors.join("\n    "))),
            None => {
                let said = printed.borrow().trim().replace('\n', "\n    ");
                lines.push(format!(
                    "  {name}: nothing spawned{}{}",
                    if errors.is_empty() { String::new() } else { format!("\n    {}", errors.join("\n    ")) },
                    if said.is_empty() { String::new() } else { format!("\n    said: {said}") }
                ));
            }
        }
    }
    lines.push(format!("Spawned without errors: {spawned} of {}.", names.len()));
    lines
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
