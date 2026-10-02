//! Textures (`.vtf`) and materials (`.vmt`).
//!
//! A surface in a map or on a model names a material; the material names a shader, its
//! textures and its settings ([`vmt`]). Textures are images in one of 30 pixel formats
//! ([`Format`]), with mipmaps, animation frames and cube map faces ([`vtf`]); [`decode`]
//! turns any of them into RGBA. [`load_material`] and [`load_texture`] read them from the
//! game's files; a [`Library`] keeps what was read. [`png`] writes images.

#![forbid(unsafe_code)]

use std::io;

use gmod_fs::Vfs;

pub mod decode;
pub mod format;
pub mod jpeg;
mod library;
pub mod png;
pub mod surfaceprop;
pub mod vmt;
pub mod vtf;

pub use format::Format;
pub use library::Library;
pub use surfaceprop::{SurfaceProp, SurfaceProps};
pub use vmt::{Material, Patch, Vmt};
pub use vtf::{Image, Vtf};

/// How many patches may stand between a material and the one it finally includes.
const MAX_INCLUDES: usize = 8;

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn context(path: &str, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{path}: {error}"))
}

/// The file of a material, as maps and models name it: `Concrete\ConcreteFloor001a` is
/// `materials/concrete/concretefloor001a.vmt`. `None` for a name that is no path.
pub fn material_path(name: &str) -> Option<String> {
    let path = gmod_fs::path::normalize(name)?;
    let path = path.strip_suffix(".vmt").unwrap_or(&path);
    (!path.is_empty()).then(|| format!("materials/{path}.vmt"))
}

/// The file of a texture: `concrete/concretefloor001a` is
/// `materials/concrete/concretefloor001a.vtf`.
pub fn texture_path(name: &str) -> Option<String> {
    let path = gmod_fs::path::normalize(name)?;
    let path = path.strip_suffix(".vtf").unwrap_or(&path);
    (!path.is_empty()).then(|| format!("materials/{path}.vtf"))
}

/// Reads a material from the game's files (search path `GAME`), patches followed.
pub fn load_material(vfs: &Vfs, name: &str) -> io::Result<Material> {
    let path = material_path(name).ok_or_else(|| invalid(format!("\"{name}\" is not a material name")))?;
    load_material_file(vfs, &path, 0)
}

/// Reads a material file by its path from the game's root (`materials/x.vmt`).
pub fn load_material_file(vfs: &Vfs, path: &str, depth: usize) -> io::Result<Material> {
    let bytes = vfs.read(path, "GAME")?;
    match vmt::parse(&gmod_kv::decode_text(&bytes)).map_err(|e| context(path, e))? {
        Vmt::Material(material) => Ok(material),
        Vmt::Patch(patch) => {
            if depth == MAX_INCLUDES {
                return Err(invalid(format!("{path}: patches include patches more than {MAX_INCLUDES} deep")));
            }
            let include =
                gmod_fs::path::normalize(&patch.include).ok_or_else(|| invalid(format!("{path}: bad include \"{}\"", patch.include)))?;
            let mut material = load_material_file(vfs, &include, depth + 1).map_err(|e| context(path, e))?;
            material.apply(&patch);
            Ok(material)
        }
    }
}

/// Reads a texture from the game's files (search path `GAME`).
pub fn load_texture(vfs: &Vfs, name: &str) -> io::Result<Vtf> {
    let path = texture_path(name).ok_or_else(|| invalid(format!("\"{name}\" is not a texture name")))?;
    Vtf::parse(vfs.read(&path, "GAME")?).map_err(|e| context(&path, e))
}

#[cfg(test)]
mod tests;
