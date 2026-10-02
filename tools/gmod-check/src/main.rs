//! `gmod-check`: how much of a Garry's Mod installation openGMOD can read, and a look into
//! the game file system as openGMOD sees it. It only reads; old compressed workshop downloads
//! are unpacked into openGMOD's own folder (`~/.opengmod/cache/workshop`).
//!
//! Started from Windows Explorer - by a double-click, or with the Garry's Mod folder dropped
//! on it - it runs the check, saves the report as `gmod-check-report.txt` next to itself, and
//! waits for Enter so that the window does not close before it can be read.

#![forbid(unsafe_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use gmod_fs::install::Install;
use gmod_fs::mount::{MountOptions, mount_install};
use gmod_fs::{Backing, UserDir, Vfs};

/// `println!` that does not panic when the reader has gone away (`gmod-check ls | head`).
macro_rules! say {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

mod check;
mod light;
mod lua;
mod out;
mod render;
mod trace;

use out::Out;

const COMMANDS: [&str; 10] = ["check", "info", "ls", "find", "cat", "glua", "lua", "render", "trace", "light"];

const USAGE: &str = "\
gmod-check - what openGMOD can read of a Garry's Mod installation

USAGE:
    gmod-check [OPTIONS] [COMMAND] [ARGUMENT]
    gmod-check <GARRYS MOD FOLDER>       the same as --gmod <FOLDER> check

COMMANDS:
    check            (default) read every archive, KeyValues file and Lua file
    info             the installation and its mounts in search order
    ls <DIR>         a folder of the game file system
    find <PATTERN>   files and folders like Lua's file.Find (\"lua/autorun/*.lua\")
    cat <FILE>       a file, to standard output
    glua <FILE>      a Lua file as openGMOD hands it to LuaJIT (a file of the game, or
                     one on disk - that needs no installation)
    lua [REALM]      start the Lua of the game as the engine does (server, client or
                     menu; default server): list what fails, and how much of GMod's API
                     is there; with --map, the server loads that map's entities
    render <MAP>     a picture of a map (gm_construct, or a .bsp file) drawn on the CPU:
                     the world with textures and light maps, saved as PNG
    light <MAP>      the light a map has for the world and for models, and what a model
                     gets at --from (default: the player start, and above it)
    trace <MAP>      a sweep through a map from --from to --to, as util.TraceLine makes
                     it (util.TraceHull with --mins and --maxs), printed as TraceResult

OPTIONS:
    --gmod <DIR>         the Garry's Mod folder (default: GMOD_ROOT, then Steam's libraries)
    --path <ID>          search path for ls/find/cat/glua (default: GAME)
    --no-addons          mount the game only: no workshop items, no addon files
    --server-downloads   also mount the addons servers made GMod download
    --no-unpack          do not unpack old compressed workshop downloads (.bin)
    --crc                check: also verify the CRC-32 of every archive entry (reads everything)
    --all                check: list every failure, not only the first 20 of each kind;
                         lua: every library, with its missing functions
    --out <FILE>         render: the PNG to write (default: <MAP>.png here)
    --from <X,Y,Z>       render: the eye (default: a player start); trace: the start
    --to <X,Y,Z>         render: the point looked at; trace: the end
    --map <MAP>          lua: the map whose entities the server loads (gm_construct, or a .bsp)
    --mins <X,Y,Z>       trace: the box around the point swept, with --maxs (none: a ray)
    --maxs <X,Y,Z>
    --mask <N>           trace: what stops it, as GMod's MASK_ numbers (default: MASK_SOLID)
    --size <WxH>         render: pixels (default: 1280x720)
    --fov <DEGREES>      render: horizontal field of view (default: 90)
    -V, --version        the version
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
    pub version: bool,
    pub out: Option<PathBuf>,
    pub view: render::View,
    /// `trace`: the box, and what stops it.
    pub mins: Option<[f32; 3]>,
    pub maxs: Option<[f32; 3]>,
    pub mask: Option<u32>,
    /// `lua`: the map whose entities the server loads.
    pub map: Option<String>,
}

/// `X,Y,Z` as three numbers.
fn point(text: &str) -> Result<[f32; 3], String> {
    let numbers: Vec<f32> = text
        .split([',', ' '])
        .filter(|p| !p.is_empty())
        .map(|p| p.trim().parse::<f32>())
        .collect::<Result<_, _>>()
        .map_err(|_| format!("{text:?} is not X,Y,Z"))?;
    numbers.try_into().map_err(|_| format!("{text:?} is not X,Y,Z"))
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
            version: false,
            out: None,
            view: render::View::default(),
            mins: None,
            maxs: None,
            mask: None,
            map: None,
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
                "--out" => args.out = Some(raw.next().ok_or("--out needs a file")?.into()),
                "--from" => args.view.eye = Some(point(&raw.next().ok_or("--from needs X,Y,Z")?)?),
                "--to" => args.view.target = Some(point(&raw.next().ok_or("--to needs X,Y,Z")?)?),
                "--size" => {
                    let size = raw.next().ok_or("--size needs WxH")?;
                    let (w, h) = size.split_once(['x', 'X']).ok_or(format!("{size:?} is not WxH"))?;
                    args.view.width = w.parse().map_err(|_| format!("{size:?} is not WxH"))?;
                    args.view.height = h.parse().map_err(|_| format!("{size:?} is not WxH"))?;
                }
                "--fov" => args.view.fov = raw.next().and_then(|f| f.parse().ok()).ok_or("--fov needs degrees")?,
                "--map" => args.map = Some(raw.next().ok_or("--map needs a map")?),
                "--mins" => args.mins = Some(point(&raw.next().ok_or("--mins needs X,Y,Z")?)?),
                "--maxs" => args.maxs = Some(point(&raw.next().ok_or("--maxs needs X,Y,Z")?)?),
                "--mask" => {
                    let text = raw.next().ok_or("--mask needs a number")?;
                    let number = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
                        Some(hex) => u32::from_str_radix(hex, 16),
                        None => text.parse(),
                    };
                    args.mask = Some(number.map_err(|_| format!("{text:?} is not a mask"))?);
                }
                "-h" | "--help" => args.help = true,
                "-V" | "--version" => args.version = true,
                flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
                _ => positional.push(arg),
            }
        }
        // a folder as the only argument (dropped on the program in Explorer) is the installation
        if positional.len() == 1 && args.gmod.is_none() && !COMMANDS.contains(&positional[0].as_str()) && Path::new(&positional[0]).is_dir()
        {
            args.gmod = Some(positional.remove(0).into());
        }
        let mut positional = positional.into_iter();
        args.command = positional.next().unwrap_or_else(|| "check".into());
        args.operand = positional.next();
        if let Some(extra) = positional.next() {
            return Err(format!("unexpected argument {extra:?}"));
        }
        let needs_operand = ["ls", "find", "cat", "glua", "render", "trace", "light"].contains(&args.command.as_str());
        if !COMMANDS.contains(&args.command.as_str()) {
            return Err(format!("unknown command {:?}", args.command));
        }
        if needs_operand && args.operand.is_none() && args.command != "ls" {
            return Err(format!("{} needs an argument", args.command));
        }
        if args.command == "trace" && (args.view.eye.is_none() || args.view.target.is_none()) {
            return Err("trace needs --from and --to".into());
        }
        if args.mins.is_some() != args.maxs.is_some() {
            return Err("--mins and --maxs go together".into());
        }
        Ok(args)
    }
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let explorer = started_from_explorer(&raw);
    let mut out = Out::default();
    let code = match Args::parse(raw.into_iter()) {
        Ok(args) => run(&args, &mut out),
        Err(message) => {
            eprintln!("{message}\n\n{USAGE}");
            ExitCode::from(2)
        }
    };
    if explorer {
        save_report(&out);
        say!();
        say!("Press Enter to close.");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
    code
}

/// Whether Windows Explorer started the program: by a double-click (no arguments) or with a
/// folder dropped on it (the folder is the only argument). Shells leave traces in the
/// environment that Explorer does not; without them the window closes as soon as the program
/// ends, so the program waits.
fn started_from_explorer(raw: &[String]) -> bool {
    let arguments_of_explorer = raw.is_empty() || (raw.len() == 1 && Path::new(&raw[0]).is_dir());
    let from_shell = ["PROMPT", "WT_SESSION", "TERM_PROGRAM", "TERM", "MSYSTEM"].iter().any(|v| std::env::var_os(v).is_some());
    cfg!(windows) && arguments_of_explorer && !from_shell
}

/// Saves what was printed as `gmod-check-report.txt` next to the program, or in openGMOD's own
/// folder when that is not possible - never inside a Garry's Mod installation.
fn save_report(out: &Out) {
    if out.text().is_empty() {
        return;
    }
    let beside_program = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .filter(|dir| !dir.ancestors().any(|d| gmod_fs::install::missing(d).is_empty()));
    for dir in beside_program.into_iter().chain(UserDir::default_root()) {
        let path = dir.join("gmod-check-report.txt");
        if std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, out.text())).is_ok() {
            say!("The report is saved in {}", path.display());
            return;
        }
    }
    say!("The report could not be saved.");
}

fn run(args: &Args, out: &mut Out) -> ExitCode {
    if args.help {
        say!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if args.version {
        say!("gmod-check {}", env!("CARGO_PKG_VERSION"));
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
        // a map on disk can be drawn without the game, only not with its materials, and
        // traced without it
        Err(e) if args.command == "render" && args.operand.as_deref().is_some_and(|map| Path::new(map).is_file()) => {
            say!("{e}\nDrawing without the game's materials.");
            return render_map(&mut Vfs::new(), args.operand.as_deref().unwrap_or_default(), args);
        }
        Err(_) if args.command == "trace" && args.operand.as_deref().is_some_and(|map| Path::new(map).is_file()) => {
            return trace_map(&mut Vfs::new(), args.operand.as_deref().unwrap_or_default(), args);
        }
        Err(e) => {
            out.line(e.to_string());
            if cfg!(windows) {
                out.line("Or drop the Garry's Mod folder onto gmod-check.exe.");
            }
            return ExitCode::from(2);
        }
    };
    let unpack_dir = if args.unpack { UserDir::default_root().map(|root| root.join("cache").join("workshop")) } else { None };
    let options = MountOptions {
        addons: args.addons,
        server_downloads: args.server_downloads,
        mount_cfg: true,
        unpack_dir,
        ..MountOptions::default()
    };
    let (mut vfs, warnings) = mount_install(&install, &options);
    let mounted_in = started.elapsed();

    let operand = args.operand.as_deref().unwrap_or("");
    match args.command.as_str() {
        "check" => {
            out.line(format!("gmod-check {}", env!("CARGO_PKG_VERSION")));
            print_install(out, &install, &options);
            print_mount_summary(out, &vfs);
            print_warnings(out, &warnings, args.all);
            out.line(format!("Mounted in {:.1} s.", mounted_in.as_secs_f64()));
            out.line("");
            let report = check::run(&mut vfs, args.crc);
            report.print(out, args.all);
            out.line(format!("Done in {:.1} s.", started.elapsed().as_secs_f64()));
            if report.failures() > 0 { ExitCode::FAILURE } else { ExitCode::SUCCESS }
        }
        "info" => {
            print_install(out, &install, &options);
            print_mounts(out, &vfs);
            print_warnings(out, &warnings, true);
            ExitCode::SUCCESS
        }
        "ls" => {
            let found = vfs.find(&format!("{}/*", operand.trim_end_matches(['/', '\\'])), &args.path_id);
            print_found(&found);
            ExitCode::SUCCESS
        }
        "find" => {
            print_found(&vfs.find(operand, &args.path_id));
            ExitCode::SUCCESS
        }
        "cat" => match vfs.read(operand, &args.path_id) {
            Ok(bytes) => {
                let mut stdout = std::io::stdout().lock();
                if stdout.write_all(&bytes).and_then(|_| stdout.flush()).is_err() {
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
        "lua" => {
            let map = match args.map.as_deref().map(|map| read_map(&vfs, map).and_then(|bsp| lua_map(&mut vfs, map, &bsp))).transpose() {
                Ok(map) => map,
                Err(e) => return fail(&e),
            };
            lua::run(std::mem::take(&mut vfs), operand, map.as_ref(), args.all, out)
        }
        "render" => render_map(&mut vfs, operand, args),
        "trace" => trace_map(&mut vfs, operand, args),
        "light" => light_map(&mut vfs, operand, args),
        _ => unreachable!("commands are checked when parsing"),
    }
}

/// What the Lua runtime needs of a map: its name, entities and brush models, what is solid in
/// it and the materials of its surfaces. The map's pakfile is mounted in front while the
/// physics models of its props are read.
fn lua_map(vfs: &mut Vfs, map: &str, bsp: &gmod_bsp::Bsp) -> Result<gmod_lua::Map, String> {
    let name = Path::new(map).file_stem().map_or_else(|| map.to_string(), |s| s.to_string_lossy().into_owned());
    let mounted = gmod_world::mount_pakfile(vfs, bsp).map_err(|e| format!("{}: pakfile: {e}", bsp.label.display()))?;
    let (collision, _) = gmod_world::collision(vfs, bsp);
    if mounted {
        vfs.remove(0);
    }
    Ok(gmod_lua::Map::from_bsp(&name, bsp, Arc::new(collision)))
}

/// A map: a file on disk, or a name in the game (`gm_construct`).
fn read_map(vfs: &Vfs, map: &str) -> Result<gmod_bsp::Bsp, String> {
    let on_disk = Path::new(map).is_file();
    let path = if on_disk {
        map.to_string()
    } else {
        let name = map.trim_end_matches(".bsp");
        if name.contains('/') || name.contains('\\') { format!("{name}.bsp") } else { format!("maps/{name}.bsp") }
    };
    let bytes = if on_disk { std::fs::read(&path) } else { vfs.read(&path, "GAME") };
    let bytes = bytes.map_err(|e| format!("{path}: {e}"))?;
    gmod_bsp::Bsp::parse(PathBuf::from(&path), Arc::from(bytes)).map_err(|e| e.to_string())
}

/// `trace`: a sweep through the map, printed. The map's pakfile is mounted in front while
/// the physics models of its props are read.
fn trace_map(vfs: &mut Vfs, map: &str, args: &Args) -> ExitCode {
    let bsp = match read_map(vfs, map) {
        Ok(bsp) => bsp,
        Err(e) => return fail(&e),
    };
    let (Some(from), Some(to)) = (args.view.eye, args.view.target) else { return fail("trace needs --from and --to") };
    let mounted = match gmod_world::mount_pakfile(vfs, &bsp) {
        Ok(mounted) => mounted,
        Err(e) => return fail(&format!("{}: pakfile: {e}", bsp.label.display())),
    };
    let (collision, failed) = gmod_world::collision(vfs, &bsp);
    if mounted {
        vfs.remove(0);
    }
    let mask = args.mask.unwrap_or(gmod_bsp::mask::SOLID);
    for line in trace::report(&bsp, &collision, from, to, args.mins.unwrap_or_default(), args.maxs.unwrap_or_default(), mask) {
        say!("{line}");
    }
    for physics in failed {
        say!("Not solid, its physics model could not be read: {physics}");
    }
    ExitCode::SUCCESS
}

/// `light`: the map's light, and a model's at a point.
fn light_map(vfs: &mut Vfs, map: &str, args: &Args) -> ExitCode {
    let bsp = match read_map(vfs, map) {
        Ok(bsp) => bsp,
        Err(e) => return fail(&e),
    };
    let mounted = gmod_world::mount_pakfile(vfs, &bsp).unwrap_or(false);
    let (collision, _) = gmod_world::collision(vfs, &bsp);
    if mounted {
        vfs.remove(0);
    }
    let points: Vec<[f32; 3]> = match (args.view.eye, gmod_world::spawn_point(&bsp)) {
        (Some(p), _) => vec![p],
        (None, Some((p, _))) => [0.0, 32.0, 128.0].iter().map(|up| [p[0], p[1], p[2] + up]).collect(),
        (None, None) => Vec::new(),
    };
    for line in light::report(&bsp, &collision, &points) {
        say!("{line}");
    }
    ExitCode::SUCCESS
}

/// `render`: a picture of the map, saved as PNG.
fn render_map(vfs: &mut Vfs, map: &str, args: &Args) -> ExitCode {
    let bsp = match read_map(vfs, map) {
        Ok(bsp) => bsp,
        Err(e) => return fail(&e),
    };
    let path = bsp.label.display().to_string();
    let picture = match render::render(vfs, &bsp, &args.view) {
        Ok(picture) => picture,
        Err(e) => return fail(&format!("{path}: {e}")),
    };
    let stem = Path::new(&path).file_stem().map_or("map".into(), |s| s.to_string_lossy().into_owned());
    let file = args.out.clone().unwrap_or_else(|| PathBuf::from(format!("{stem}.png")));
    if let Err(e) = std::fs::write(&file, &picture.png) {
        return fail(&format!("{}: {e}", file.display()));
    }
    let [ex, ey, ez] = picture.eye;
    let [tx, ty, tz] = picture.target;
    say!("{path}: {} triangles from {ex},{ey},{ez} to {tx},{ty},{tz} in {:.1} s", picture.triangles, picture.seconds);
    let lists = [
        ("materials", "drawn as purple squares", &picture.missing_materials),
        ("textures", "drawn as purple squares", &picture.missing_textures),
        ("models", "not drawn", &picture.missing_models),
    ];
    for (what, how, names) in lists {
        if !names.is_empty() {
            let shown: Vec<&str> = names.iter().take(5).map(String::as_str).collect();
            let more = if names.len() > 5 { format!(" and {} more", names.len() - 5) } else { String::new() };
            say!("Missing {what} ({}), {how}: {}{more}", names.len(), shown.join(", "));
        }
    }
    say!("Saved {}", file.display());
    ExitCode::SUCCESS
}

fn fail(message: &str) -> ExitCode {
    eprintln!("{message}");
    ExitCode::from(2)
}

fn print_found(found: &gmod_fs::vfs::Found) {
    for dir in &found.dirs {
        say!("{dir}/");
    }
    for file in &found.files {
        say!("{file}");
    }
}

fn print_glua(source: &[u8], name: &str) -> ExitCode {
    match gmod_glua::translate(source) {
        Ok(lua) => {
            let mut stdout = std::io::stdout().lock();
            let _ = stdout.write_all(&lua).and_then(|_| stdout.flush());
            ExitCode::SUCCESS
        }
        Err(e) => fail(&format!("{name}: {e}")),
    }
}

fn print_install(out: &mut Out, install: &Install, options: &MountOptions) {
    out.line(format!("Garry's Mod:   {}", install.root.display()));
    match (&install.library, install.workshop_dir()) {
        (Some(library), Some(workshop)) => {
            out.line(format!("Steam library: {}", library.display()));
            out.line(format!("Workshop:      {}{}", workshop.display(), if workshop.is_dir() { "" } else { " (not there)" }));
        }
        _ => out.line("Steam library: unknown - subscribed workshop items cannot be found"),
    }
    if let Some(dir) = &options.unpack_dir {
        out.line(format!("Unpacked .bin: {}", dir.display()));
    }
    out.line("");
}

fn entries(backing: &Backing) -> Option<usize> {
    match backing {
        Backing::Dir(_) => None,
        Backing::Vpk(v) => Some(v.len()),
        Backing::Gma(g) => Some(g.len()),
        Backing::Zip(z) => Some(z.len()),
    }
}

fn print_mount_summary(out: &mut Out, vfs: &Vfs) {
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
            Backing::Zip(z) => {
                packs += 1;
                pack_files += z.len();
            }
        }
    }
    out.line(format!(
        "Mounted: {packs} packs ({pack_files} files), {addons} addons ({addon_files} files), {folders} folders. `gmod-check info` lists them."
    ));
}

fn print_mounts(out: &mut Out, vfs: &Vfs) {
    out.line("Search order (first wins):");
    for (i, mount) in vfs.mounts().iter().enumerate() {
        let files = entries(&mount.backing).map(|n| n.to_string()).unwrap_or_else(|| "-".into());
        let title = match &mount.backing {
            Backing::Gma(g) => format!("  \"{}\"", g.info().title),
            _ => String::new(),
        };
        out.line(format!(
            "{:>4}  {:<6} {:>7}  {:<22} {}{title}",
            i + 1,
            mount.kind(),
            files,
            mount.ids.join("+"),
            mount.origin().display()
        ));
    }
    out.line("");
}

fn print_warnings(out: &mut Out, warnings: &[String], all: bool) {
    if warnings.is_empty() {
        return;
    }
    out.line(format!("Warnings ({}):", warnings.len()));
    let shown = if all { warnings.len() } else { warnings.len().min(20) };
    for warning in &warnings[..shown] {
        out.line(format!("  {warning}"));
    }
    if shown < warnings.len() {
        out.line(format!("  ... {} more (--all shows them)", warnings.len() - shown));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        Args::parse(args.iter().map(|a| a.to_string()))
    }

    #[test]
    fn arguments() {
        let args = parse(&[]).unwrap();
        assert_eq!((args.command.as_str(), args.gmod.as_deref()), ("check", None));

        // a folder dropped on the program: the installation, and the check
        let folder = std::env::temp_dir();
        let args = parse(&[folder.to_str().unwrap()]).unwrap();
        assert_eq!((args.command.as_str(), args.gmod.as_deref()), ("check", Some(folder.as_path())));

        let args = parse(&["--gmod", "/games/gmod", "ls", "lua/autorun", "--path", "LUA"]).unwrap();
        assert_eq!(args.command, "ls");
        assert_eq!(args.operand.as_deref(), Some("lua/autorun"));
        assert_eq!(args.path_id, "LUA");
        assert!(parse(&["-V"]).unwrap().version);

        assert!(parse(&["frobnicate"]).is_err());
        assert!(parse(&["cat"]).is_err(), "cat needs a file");
        assert!(parse(&["--unknown"]).is_err());
        assert!(parse(&["ls", "a", "b"]).is_err());

        let args = parse(&["render", "gm_construct", "--from", "1,2,3", "--to", "4, 5, 6", "--size", "640x480", "--fov", "75"]).unwrap();
        assert_eq!((args.view.eye, args.view.target), (Some([1.0, 2.0, 3.0]), Some([4.0, 5.0, 6.0])));
        assert_eq!((args.view.width, args.view.height, args.view.fov), (640, 480, 75.0));
        assert!(parse(&["render"]).is_err(), "render needs a map");
        assert!(parse(&["render", "x", "--from", "1,2"]).is_err());
        assert!(parse(&["render", "x", "--size", "big"]).is_err());

        let args = parse(&["trace", "gm_flatgrass", "--from", "0,0,100", "--to", "0,0,-100", "--mins", "-16,-16,0", "--maxs", "16,16,72"])
            .unwrap();
        assert_eq!((args.mins, args.maxs, args.mask), (Some([-16.0, -16.0, 0.0]), Some([16.0, 16.0, 72.0]), None));
        assert_eq!(parse(&["trace", "x", "--from", "0,0,0", "--to", "1,1,1", "--mask", "0x4001"]).unwrap().mask, Some(0x4001));
        assert_eq!(parse(&["trace", "x", "--from", "0,0,0", "--to", "1,1,1", "--mask", "33636363"]).unwrap().mask, Some(33636363));
        assert!(parse(&["trace", "x", "--from", "0,0,0"]).is_err(), "trace needs both ends");
        assert!(parse(&["trace", "x", "--from", "0,0,0", "--to", "1,1,1", "--mins", "1,1,1"]).is_err(), "a box needs both corners");
        assert!(parse(&["trace", "x", "--from", "0,0,0", "--to", "1,1,1", "--mask", "solid"]).is_err());
    }
}
