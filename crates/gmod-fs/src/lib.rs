//! The game's files, read the way Garry's Mod reads them - and never written.
//!
//! * [`install`] finds the player's Garry's Mod (a folder given, `GMOD_ROOT`, Steam's
//!   libraries) and checks that it is complete.
//! * [`vpk`] and [`gma`] read the archives the content ships in: Valve's packs and GMod's
//!   addon files. [`DirMount`] reads a folder of loose files without regard to case.
//! * [`workshop`] finds the player's addons: subscribed workshop items, `.gma` files in
//!   `garrysmod/addons`, server downloads; old compressed downloads are unpacked with
//!   [`lzma`] into openGMOD's own folder.
//! * [`vfs`] stacks folders and archives into one file system searched in order, each mount
//!   tagged with search path IDs (`GAME`, `MOD`, ...) as in the engine; [`mount`] builds the
//!   stack GMod builds from `gameinfo.txt`, the installed addons and `cfg/mount.cfg`.
//! * [`user`] is openGMOD's own folder, the only place it writes to.
//!
//! Nothing here can write into the installation: the file system has no write operations,
//! and the user folder refuses to be created inside the installation.

#![forbid(unsafe_code)]

pub mod crc32;
mod dir;
pub mod gameinfo;
pub mod gma;
mod index;
pub mod install;
mod io_util;
pub mod lzma;
pub mod mount;
pub mod path;
pub mod user;
pub mod vfs;
pub mod vpk;
pub mod workshop;

pub use dir::DirMount;
pub use gma::GmaArchive;
pub use install::Install;
pub use user::UserDir;
pub use vfs::{Backing, Mount, Vfs};
pub use vpk::VpkArchive;

#[cfg(test)]
mod testutil;
