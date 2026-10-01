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

pub fn run(vfs: Vfs, realm: &str, out: &mut Out) -> ExitCode {
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

    runtime.init();

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
    if errors.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
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
