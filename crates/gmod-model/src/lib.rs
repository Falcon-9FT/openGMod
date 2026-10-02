//! Models (`.mdl` with `.vvd` and `.vtx`): props, weapons, characters.
//!
//! A model is three files compiled together and tied by a checksum: the header ([`mdl`]:
//! bones, textures, skins, body parts and meshes), the vertices ([`vvd`]) and the triangles
//! for each level of detail ([`vtx`]). [`mesh::build`] puts them together into triangles to
//! draw, lit by a static prop's baked light ([`vhv`]) if there is one; [`load`] reads a model
//! from the game's files. A model that collides has a physics model as well ([`phy`],
//! [`load_physics`]): its convex pieces, masses and the joints of ragdolls.
//!
//! What is read so far is what static props need, and physics: no animations or flexes.

#![forbid(unsafe_code)]

use std::io;

use gmod_fs::Vfs;

pub mod mdl;
pub mod mesh;
pub mod phy;
mod reader;
pub mod vhv;
pub mod vtx;
pub mod vvd;

pub use mdl::{Attachment, BodyPart, Bone, Mdl, SubModel};
pub use mesh::{Batch, StaticMesh};
pub use phy::{Convex, Phy, RagdollConstraint, Solid};
pub use vhv::Vhv;
pub use vtx::Vtx;
pub use vvd::{Vertex, Vvd};

/// The triangle files a model may come with, in the order they are tried: Direct3D 9 first,
/// as the engine on today's hardware does.
const TRIANGLE_FILES: [&str; 4] = [".dx90.vtx", ".vtx", ".dx80.vtx", ".sw.vtx"];

/// A model's three files, read.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub mdl: Mdl,
    pub vvd: Vvd,
    pub vtx: Vtx,
}

impl Model {
    pub fn parse(mdl: &[u8], vvd: &[u8], vtx: &[u8]) -> io::Result<Model> {
        Ok(Model { mdl: Mdl::parse(mdl)?, vvd: Vvd::parse(vvd)?, vtx: Vtx::parse(vtx)? })
    }

    /// The triangles for body group number `body` and skin `skin`.
    pub fn mesh(&self, body: usize, skin: usize) -> io::Result<StaticMesh> {
        mesh::build(&self.mdl, &self.vvd, &self.vtx, body, skin, None)
    }

    /// The triangles of a static prop, with its baked light when it fits the model.
    pub fn lit_mesh(&self, body: usize, skin: usize, baked: &Vhv) -> io::Result<StaticMesh> {
        mesh::build(&self.mdl, &self.vvd, &self.vtx, body, skin, Some(baked))
    }
}

fn context(path: &str, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{path}: {error}"))
}

/// Reads a model (`models/props_c17/oildrum001.mdl`) from the game's files, search path `GAME`.
pub fn load(vfs: &Vfs, path: &str) -> io::Result<Model> {
    let stem = path.strip_suffix(".mdl").unwrap_or(path);
    let mdl = vfs.read(&format!("{stem}.mdl"), "GAME")?;
    let vvd = vfs.read(&format!("{stem}.vvd"), "GAME")?;
    let vtx = TRIANGLE_FILES
        .iter()
        .find_map(|ending| vfs.read(&format!("{stem}{ending}"), "GAME").ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{stem}: no .vtx file")))?;
    Model::parse(&mdl, &vvd, &vtx).map_err(|e| context(path, e))
}

/// Reads the physics model of a model (`models/props_c17/oildrum001.mdl` has
/// `models/props_c17/oildrum001.phy`), search path `GAME`; `None` when there is none. One
/// compiled with another build of the model than `checksum` says is an error.
pub fn load_physics(vfs: &Vfs, path: &str, checksum: i32) -> io::Result<Option<Phy>> {
    let file = format!("{}.phy", path.strip_suffix(".mdl").unwrap_or(path));
    let bytes = match vfs.read(&file, "GAME") {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let phy = Phy::parse(&bytes).map_err(|e| context(&file, e))?;
    if phy.checksum != checksum {
        return Err(context(
            &file,
            reader::invalid(format!("made for another build of the model (checksum {} instead of {checksum})", phy.checksum)),
        ));
    }
    Ok(Some(phy))
}

/// Small model files written for tests, also those of crates that read models (feature
/// `testmodel`).
#[cfg(any(test, feature = "testmodel"))]
#[doc(hidden)]
pub mod testmodel;
#[cfg(test)]
mod tests;
