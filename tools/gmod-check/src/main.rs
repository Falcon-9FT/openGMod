//! `gmod-check`: how much of a Garry's Mod installation openGMOD can read, and a look into
//! the game file system as openGMOD sees it. It only reads; old compressed workshop downloads
//! are unpacked into openGMOD's own folder (`~/.opengmod/cache/workshop`).

#![forbid(unsafe_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use gmod_fs::install::Install;
use gmod_fs::mount::{MountOptions, mount_install};
use gmod_fs::{Backing, UserDir, Vfs};

mod check;

const USAGE: &str = "\
gmod-check - what openGMOD can read of a Garry's Mod installation

USAGE:
    gmod-check [OPTIONS] [COMMAND] [ARGUMENT]

COMMANDS:
    check            (default) read every archive, KeyValues file and Lua file
    info             the installation and its mounts in search order
    ls <DIR>         a folder of the game file system
    find <PATTERN>   files and folders like Lua's file.Find (\"lua/autorun/*.lua\")
    cat <FILE>       a file, to standard output
    glua <FILE>      a Lua file as openGMOD hands it to LuaJIT (a file of the game, or
                     one on disk - that needs no installation)

OPTIONS:
    --gmod <DIR>         the Garry's Mod folder (default: GMOD_ROOT, then Steam's libraries)
    --path <ID>          search path for ls/find/cat/glua (default: GAME)
    --no-addons          mount the game only: no workshop items, no addon files
    --server-downloads   also mount the addons servers made GMod download
    --no-unpack          do not unpack old compressed workshop downloads (.bin)
    --crc                check: also verify the CRC-32 of every archive entry (reads everything)
    --all                check: list every failure, not only the first 20 of each kind
    -h, --help           this text";

/// What the command line asked for.
#[derive(Debug, Clone)]
pub struct Args {
    pub command: String,
    pub operand: Option<String>,
    pub gmod: Option<PathBuf>,
    pub path_id: String,
    pub addons: bool,
    pub server_downloads: bool,
    pub unpack: bool,
    pub crc: bool,
    pub all: bool,
    pub help: bool,
}

impl Args {
    fn parse(mut raw: impl Iterator<Item = String>) -> Result<Args, String> {
        let mut args = Args {
            command: String::new(),
            operand: None,
            gmod: None,
            path_id: "GAME".into(),
            addons: true,
            server_downloads: false,
            unpack: true,
            crc: false,
            all: false,
            help: false,
        };
        let mut positional = Vec::new();
        while let Some(arg) = raw.next() {
            match arg.as_str() {
                "--gmod" => args.gmod = Some(raw.next().ok_or("--gmod needs a folder")?.into()),
                "--path" => args.path_id = raw.next().ok_or("--path needs a search path ID")?,
                "--no-addons" => args.addons = false,
                "--server-downloads" => args.server_downloads = true,
                "--no-unpack" => args.unpack = false,
                "--crc" => args.crc = true,
                "--all" => args.all = true,
                "-h" | "--help" => args.help = true,
                flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
                _ => positional.push(arg),
            }
        }
        let mut positional = positional.into_iter();
        args.command = positional.next().unwrap_or_else(|| "check".into());
        args.operand = positional.next();
        if let Some(extra) = positional.next() {
            return Err(format!("unexpected argument {extra:?}"));
        }
        let needs_operand = ["ls", "find", "cat", "glua"].contains(&args.command.as_str());
        if !["check", "info", "ls", "find", "cat", "glua"].contains(&args.command.as_str()) {
            return Err(format!("unknown command {:?}", args.command));
        }
        if needs_operand && args.operand.is_none() && args.command != "ls" {
            return Err(format!("{} needs an argument", args.command));
        }
        Ok(args)
    }
}

fn main() -> ExitCode {
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if args.help {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if args.command == "glua"
        && let Some(file) = args.operand.as_deref().map(Path::new).filter(|p| p.is_file())
    {
        return match std::fs::read(file) {
            Ok(bytes) => print_glua(&bytes, &file.display().to_string()),
            Err(e) => fail(&format!("{}: {e}", file.display())),
        };
    }

    let started = Instant::now();
    let install = match Install::locate(args.gmod.as_deref()) {
        Ok(install) => install,
        Err(e) => return fail(&e.to_string()),
    };
    let unpack_dir = if args.unpack { UserDir::default_root().map(|root| root.join("cache").join("workshop")) } else { None };
    let options = MountOptions { addons: args.addons, server_downloads: args.server_downloads, mount_cfg: true, unpack_dir };
    let (vfs, warnings) = mount_install(&install, &options);
    let mounted_in = started.elapsed();

    let operand = args.operand.as_deref().unwrap_or("");
    match args.command.as_str() {
        "check" => {
            print_install(&install, &options);
            print_mount_summary(&vfs);
            print_warnings(&warnings, args.all);
            println!("Mounted in {:.1} s.", mounted_in.as_secs_f64());
            println!();
            let report = check::run(&vfs, args.crc);
            report.print(args.all);
            println!("Done in {:.1} s.", started.elapsed().as_secs_f64());
            if report.failures() > 0 { ExitCode::FAILURE } else { ExitCode::SUCCESS }
        }
        "info" => {
            print_install(&install, &options);
            print_mounts(&vfs);
            print_warnings(&warnings, true);
            ExitCode::SUCCESS
        }
        "ls" => {
            let found = vfs.find(&format!("{}/*", operand.trim_end_matches(['/', '\\'])), &args.path_id);
            for dir in &found.dirs {
                println!("{dir}/");
            }
            for file in &found.files {
                println!("{file}");
            }
            ExitCode::SUCCESS
        }
        "find" => {
            let found = vfs.find(operand, &args.path_id);
            for dir in &found.dirs {
                println!("{dir}/");
            }
            for file in &found.files {
                println!("{file}");
            }
            ExitCode::SUCCESS
        }
        "cat" => match vfs.read(operand, &args.path_id) {
            Ok(bytes) => {
                let mut out = std::io::stdout().lock();
                if out.write_all(&bytes).and_then(|_| out.flush()).is_err() {
                    return ExitCode::FAILURE;
                }
                ExitCode::SUCCESS
            }
            Err(e) => fail(&e.to_string()),
        },
        "glua" => match vfs.read(operand, &args.path_id) {
            Ok(bytes) => print_glua(&bytes, operand),
            Err(e) => fail(&e.to_string()),
        },
        _ => unreachable!("commands are checked when parsing"),
    }
}

fn fail(message: &str) -> ExitCode {
    eprintln!("{message}");
    ExitCode::from(2)
}

fn print_glua(source: &[u8], name: &str) -> ExitCode {
    match gmod_glua::translate(source) {
        Ok(lua) => {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(&lua).and_then(|_| out.flush());
            ExitCode::SUCCESS
        }
        Err(e) => fail(&format!("{name}: {e}")),
    }
}

fn print_install(install: &Install, options: &MountOptions) {
    println!("Garry's Mod:   {}", install.root.display());
    match (&install.library, install.workshop_dir()) {
        (Some(library), Some(workshop)) => {
            println!("Steam library: {}", library.display());
            println!("Workshop:      {}{}", workshop.display(), if workshop.is_dir() { "" } else { " (not there)" });
        }
        _ => println!("Steam library: unknown - subscribed workshop items cannot be found"),
    }
    if let Some(dir) = &options.unpack_dir {
        println!("Unpacked .bin: {}", dir.display());
    }
    println!();
}

fn entries(backing: &Backing) -> Option<usize> {
    match backing {
        Backing::Dir(_) => None,
        Backing::Vpk(v) => Some(v.len()),
        Backing::Gma(g) => Some(g.len()),
    }
}

fn print_mount_summary(vfs: &Vfs) {
    let (mut packs, mut pack_files, mut addons, mut addon_files, mut folders) = (0, 0, 0, 0, 0);
    for mount in vfs.mounts() {
        match &mount.backing {
            Backing::Dir(_) => folders += 1,
            Backing::Vpk(v) => {
                packs += 1;
                pack_files += v.len();
            }
            Backing::Gma(g) => {
                addons += 1;
                addon_files += g.len();
            }
        }
    }
    println!(
        "Mounted: {packs} packs ({pack_files} files), {addons} addons ({addon_files} files), {folders} folders. `gmod-check info` lists them."
    );
}

fn print_mounts(vfs: &Vfs) {
    println!("Search order (first wins):");
    for (i, mount) in vfs.mounts().iter().enumerate() {
        let files = entries(&mount.backing).map(|n| n.to_string()).unwrap_or_else(|| "-".into());
        let title = match &mount.backing {
            Backing::Gma(g) => format!("  \"{}\"", g.info().title),
            _ => String::new(),
        };
        println!("{:>4}  {:<6} {:>7}  {:<22} {}{title}", i + 1, mount.kind(), files, mount.ids.join("+"), mount.origin().display());
    }
    println!();
}

fn print_warnings(warnings: &[String], all: bool) {
    if warnings.is_empty() {
        return;
    }
    println!("Warnings ({}):", warnings.len());
    let shown = if all { warnings.len() } else { warnings.len().min(20) };
    for warning in &warnings[..shown] {
        println!("  {warning}");
    }
    if shown < warnings.len() {
        println!("  ... {} more (--all shows them)", warnings.len() - shown);
    }
}
