//! The game's console: what the program, the Lua of each realm and their errors say goes to
//! the terminal, as it did, and is kept for the console window (`), each in its colour, as
//! GMod's console shows them. Lines typed into the window run as those typed into the
//! terminal ([`crate::game::typed_lines`]).

use std::io::Write;
use std::sync::Mutex;

/// What is said, as GMod's console colours it.
pub const TEXT: [u8; 3] = [230, 230, 230];
/// The errors of the server's Lua.
pub const SERVER_ERROR: [u8; 3] = [137, 222, 255];
/// The errors of the client's Lua.
pub const CLIENT_ERROR: [u8; 3] = [255, 222, 102];
/// The errors of the menu's Lua.
pub const MENU_ERROR: [u8; 3] = [120, 220, 100];
/// What went wrong in the program itself.
pub const ERROR: [u8; 3] = [255, 110, 110];

/// How many pieces of text the console keeps for a window that opens late.
const KEPT: usize = 4000;

struct Log {
    /// What was said, oldest first, with the number of the first.
    said: Vec<(String, [u8; 3])>,
    first: usize,
}

static LOG: Mutex<Log> = Mutex::new(Log { said: Vec::new(), first: 0 });
static TYPED: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn keep(text: &str, color: [u8; 3]) {
    let mut log = LOG.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    log.said.push((text.to_string(), color));
    if log.said.len() > KEPT {
        let gone = log.said.len() - KEPT;
        log.said.drain(..gone);
        log.first += gone;
    }
}

/// Says something: to the terminal and the console, in a colour; line breaks as given.
pub fn write(text: &str, color: [u8; 3]) {
    print!("{text}");
    let _ = std::io::stdout().flush();
    keep(text, color);
}

/// Says that something went wrong: to the terminal's errors and the console.
pub fn write_error(text: &str, color: [u8; 3]) {
    eprint!("{text}");
    keep(text, color);
}

/// What was said since `seen` (a count of what was said, which this moves on).
pub fn since(seen: &mut usize) -> Vec<(String, [u8; 3])> {
    let log = LOG.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let from = (*seen).max(log.first) - log.first;
    let new = log.said.get(from..).map(<[_]>::to_vec).unwrap_or_default();
    *seen = log.first + log.said.len();
    new
}

/// A line typed into the console window, to run with those typed into the terminal.
pub fn type_line(line: String) {
    TYPED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(line);
}

/// The lines typed into the console window since the last call.
pub fn take_typed() -> Vec<String> {
    std::mem::take(&mut *TYPED.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
}

/// The program's own commands, which no realm's Lua knows.
const PROGRAM_COMMANDS: &[&str] = &[
    "connect",
    "disconnect",
    "quit",
    "exit",
    "screenshot",
    "jpeg",
    "toggleconsole",
    "showconsole",
    "gameui_activate",
    "gameui_hide",
    "lua_run_cl",
    "lua_run_menu",
    "map",
    "gamemode",
    "maxplayers",
    "hostname",
    "undo",
    "gmod_undo",
];

/// The most completions shown under a line.
const MOST_COMPLETIONS: usize = 30;

/// What the console window offers under a line typed so far: what the realms offer (`found`)
/// and the program's own commands, each name once - with its value when one says it - by
/// name, as many as are shown.
pub fn completions(line: &str, found: Vec<String>) -> Vec<String> {
    let mut list = found;
    let arguments = line.trim_start().contains(' ');
    let prefix = line.trim().to_ascii_lowercase();
    if !arguments && !prefix.is_empty() {
        list.extend(PROGRAM_COMMANDS.iter().filter(|c| c.starts_with(&prefix)).map(|c| c.to_string()));
    }
    let key =
        |entry: &String| if arguments { entry.to_ascii_lowercase() } else { entry.split(' ').next().unwrap_or("").to_ascii_lowercase() };
    // those with a value first among those of a name, which then stay
    list.sort_by_key(|entry| (key(entry), !entry.contains(' ')));
    list.dedup_by_key(|entry| key(entry));
    list.truncate(MOST_COMPLETIONS);
    list
}

/// `println!` that the console shows too.
macro_rules! say {
    ($($arg:tt)*) => {
        $crate::console::write(&format!("{}\n", format_args!($($arg)*)), $crate::console::TEXT)
    };
}

/// `eprintln!` that the console shows too, in red.
macro_rules! say_error {
    ($($arg:tt)*) => {
        $crate::console::write_error(&format!("{}\n", format_args!($($arg)*)), $crate::console::ERROR)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_gets_what_it_has_not_had() {
        let mut seen = 0;
        since(&mut seen);
        keep("one\n", TEXT);
        keep("two\n", ERROR);
        assert_eq!(since(&mut seen), [("one\n".to_string(), TEXT), ("two\n".to_string(), ERROR)]);
        assert!(since(&mut seen).is_empty());
        type_line("status".into());
        assert_eq!(take_typed(), ["status"]);
    }

    #[test]
    fn completions_name_each_thing_once() {
        let found = vec!["sv_gravity 600".to_string(), "screenshot".to_string(), "sv_cheats 0".to_string(), "sv_gravity 600".to_string()];
        assert_eq!(completions("s", found), ["screenshot", "showconsole", "sv_cheats 0", "sv_gravity 600"]);
        assert_eq!(completions("dis", Vec::new()), ["disconnect"]);
        assert_eq!(completions("gm_spawn ", vec!["gm_spawn models/a.mdl".to_string()]), ["gm_spawn models/a.mdl"]);
        assert!(completions("", Vec::new()).is_empty());
    }
}
