//! Materials and textures by name, each read once: what the engine's material system keeps.

use std::collections::HashMap;
use std::sync::Arc;

use gmod_fs::Vfs;

use crate::{Material, Vtf, load_material, load_texture, material_path, texture_path};

/// A cache of materials and textures. A name that failed is remembered with its error, so a
/// map that names a missing material a thousand times asks the file system once.
#[derive(Debug, Default)]
pub struct Library {
    materials: HashMap<String, Result<Arc<Material>, String>>,
    textures: HashMap<String, Result<Arc<Vtf>, String>>,
}

impl Library {
    pub fn new() -> Library {
        Library::default()
    }

    /// The material `name` (`concrete/concretefloor001a`), patches followed.
    pub fn material(&mut self, vfs: &Vfs, name: &str) -> Result<Arc<Material>, String> {
        let key = material_path(name).unwrap_or_else(|| name.to_string());
        self.materials.entry(key).or_insert_with(|| load_material(vfs, name).map(Arc::new).map_err(|e| e.to_string())).clone()
    }

    /// The texture `name` (`concrete/concretefloor001a`).
    pub fn texture(&mut self, vfs: &Vfs, name: &str) -> Result<Arc<Vtf>, String> {
        let key = texture_path(name).unwrap_or_else(|| name.to_string());
        self.textures.entry(key).or_insert_with(|| load_texture(vfs, name).map(Arc::new).map_err(|e| e.to_string())).clone()
    }

    /// Forgets everything, as when the map changes and its pakfile goes.
    pub fn clear(&mut self) {
        self.materials.clear();
        self.textures.clear();
    }
}
