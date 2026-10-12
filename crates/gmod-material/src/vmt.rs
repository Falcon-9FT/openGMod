//! Materials (`.vmt`).
//!
//! A material is a KeyValues file with one top-level block, named after the shader, holding
//! the shader's parameters:
//!
//! ```text
//! "LightmappedGeneric"
//! {
//!     "$basetexture" "concrete/concretefloor001a"
//!     "$surfaceprop" "concrete"
//! }
//! ```
//!
//! Names are compared without regard to case; the first of two equal names counts. Besides
//! parameters the block may hold:
//!
//! * `Proxies`: code that changes parameters as the game runs (animated textures and more);
//! * blocks named after a DirectX level condition (`">=dx90"`, `"<dx90_20b"`): their parameters
//!   count when it holds, over the others. openGMOD answers as DirectX 9 hardware does (`dx95`);
//! * fallback blocks for older hardware (`LightmappedGeneric_DX8`), not used;
//! * parameters behind a condition, `"hdr?$envmaptint"` or `"!srgb?$color"` ([`condition`]).
//!
//! A `patch` material takes another and changes it:
//!
//! ```text
//! "patch"
//! {
//!     "include" "materials/glass/glasswindow001a.vmt"
//!     "replace" { "$envmap" "maps/mymap/c1984_352_32" }
//! }
//! ```
//!
//! `insert` sets parameters, adding those the material lacks; `replace` changes only those it
//! has. Maps carry such patches to give materials the cube map nearest to each face.

use std::io;

use gmod_kv::{Block, Options, Pair, Value};

/// A material: its shader and parameters.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Material {
    /// Lower case: `lightmappedgeneric`, `vertexlitgeneric`, `unlitgeneric`, `water` ...
    pub shader: String,
    /// The shader's block as written, with patches applied.
    pub block: Block,
    /// The parameters that count, conditions applied: lower-case names, in file order.
    params: Vec<(String, String)>,
    /// What was wrong with the files, as the engine would have warned and gone on.
    pub warnings: Vec<String>,
}

/// One `.vmt` file: a material, or a patch of another.
#[derive(Debug, Clone, PartialEq)]
pub enum Vmt {
    Material(Material),
    Patch(Patch),
}

/// A material made from another one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Patch {
    /// The material it starts from, a path from the game's root (`materials/x.vmt`).
    pub include: String,
    pub insert: Block,
    pub replace: Block,
    pub warnings: Vec<String>,
}

/// The DirectX level openGMOD answers conditions with.
const DX_LEVEL: u32 = 95;

/// Whether a condition in front of a parameter (`hdr` in `hdr?$envmaptint`) holds. openGMOD
/// renders as a PC with HDR and sRGB-correct sampling: `hdr`, `srgb` and `GPU>=` levels up to
/// 3 hold; `ldr`, `lowfill`, consoles and anything unknown do not. `!` negates.
pub fn condition(name: &str) -> bool {
    let name = name.trim().to_ascii_lowercase();
    if let Some(rest) = name.strip_prefix('!') {
        return !condition(rest);
    }
    if let Some(level) = name.strip_prefix("gpu") {
        return compare(level, 3);
    }
    matches!(name.as_str(), "hdr" | "srgb")
}

/// `<2`, `>=1` and the like against `actual`; false if it does not read as a comparison.
fn compare(condition: &str, actual: u32) -> bool {
    let (op, number) = match condition.as_bytes() {
        [b'<', b'=', ..] => ("<=", &condition[2..]),
        [b'>', b'=', ..] => (">=", &condition[2..]),
        [b'<', ..] => ("<", &condition[1..]),
        [b'>', ..] => (">", &condition[1..]),
        _ => return false,
    };
    let Ok(n) = number.parse::<u32>() else { return false };
    match op {
        "<=" => actual <= n,
        ">=" => actual >= n,
        "<" => actual < n,
        _ => actual > n,
    }
}

/// For a block named like `>=dx90` or `<dx90_20b`: whether its parameters count. `None` if the
/// name is no DirectX level condition.
fn dx_condition(name: &str) -> Option<bool> {
    let name = name.to_ascii_lowercase();
    let at = name.find("dx")?;
    let (op, rest) = name.split_at(at);
    let digits: String = rest[2..].chars().take_while(char::is_ascii_digit).collect();
    let rest = &rest[2 + digits.len()..];
    if digits.is_empty() || !(rest.is_empty() || rest.starts_with('_')) || !matches!(op, "<" | "<=" | ">" | ">=") {
        return None;
    }
    Some(compare(&format!("{op}{digits}"), DX_LEVEL))
}

fn set(params: &mut Vec<(String, String)>, name: String, value: &str) {
    match params.iter_mut().find(|(n, _)| *n == name) {
        Some(slot) => slot.1 = value.to_string(),
        None => params.push((name, value.to_string())),
    }
}

/// Text to read: the engine reads a material no matter what bytes are in it.
fn read_text(text: &str, what: &str, warnings: &mut Vec<String>) -> Block {
    let (document, errors) = gmod_kv::parse_lenient(text, Options::default());
    warnings.extend(errors.iter().map(|e| format!("{what}: {e}")));
    document.root
}

/// Reads a `.vmt` file. Fails only when there is no shader block in it at all.
pub fn parse(text: &str) -> io::Result<Vmt> {
    let mut warnings = Vec::new();
    let root = read_text(text, "material", &mut warnings);
    let Some(Pair { key, value, .. }) = root.pairs.into_iter().next() else {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "empty material"));
    };
    let Value::Block(block) = value else {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("material \"{key}\" has no block of parameters")));
    };
    if key.eq_ignore_ascii_case("patch") {
        let Some(include) = block.str("include") else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "patch material without \"include\""));
        };
        return Ok(Vmt::Patch(Patch {
            include: include.to_string(),
            insert: block.block("insert").cloned().unwrap_or_default(),
            replace: block.block("replace").cloned().unwrap_or_default(),
            warnings,
        }));
    }
    let mut material = Material { shader: key.to_ascii_lowercase(), block, params: Vec::new(), warnings };
    material.resolve();
    Ok(Vmt::Material(material))
}

impl Material {
    /// Works out the parameters that count from [`Material::block`].
    fn resolve(&mut self) {
        let mut params = Vec::new();
        let mut later = Vec::new();
        for pair in &self.block.pairs {
            let name = pair.key.to_ascii_lowercase();
            match &pair.value {
                Value::Str(value) => match name.rsplit_once('?') {
                    Some((conditions, param)) => {
                        if conditions.split('?').all(condition) {
                            later.push((param.to_string(), value.as_str()));
                        }
                    }
                    None if params.iter().any(|(n, _)| *n == name) => {}
                    None => params.push((name, value.clone())),
                },
                Value::Block(block) => {
                    if dx_condition(&name) == Some(true) {
                        for inner in &block.pairs {
                            if let Value::Str(value) = &inner.value {
                                later.push((inner.key.to_ascii_lowercase(), value.as_str()));
                            }
                        }
                    }
                }
            }
        }
        for (name, value) in later {
            set(&mut params, name, value);
        }
        self.params = params;
    }

    /// Applies a patch to this material, the one it includes.
    pub fn apply(&mut self, patch: &Patch) {
        for pair in &patch.insert.pairs {
            match self.block.pairs.iter_mut().find(|p| p.key.eq_ignore_ascii_case(&pair.key)) {
                Some(existing) => existing.value = pair.value.clone(),
                None => self.block.pairs.push(pair.clone()),
            }
        }
        for pair in &patch.replace.pairs {
            if let Some(existing) = self.block.pairs.iter_mut().find(|p| p.key.eq_ignore_ascii_case(&pair.key)) {
                existing.value = pair.value.clone();
            }
        }
        self.warnings.extend(patch.warnings.iter().cloned());
        self.resolve();
    }

    /// The parameters that count, lower-case names, in file order.
    pub fn params(&self) -> &[(String, String)] {
        &self.params
    }

    /// A parameter (`$basetexture`), conditions applied.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.params.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// A texture parameter as a path under `materials/` without `.vtf`
    /// (`concrete/concretefloor001a`); `None` if unset or empty.
    pub fn texture(&self, name: &str) -> Option<String> {
        let value = self.get(name)?;
        let path = gmod_fs::path::normalize(value)?;
        let path = path.strip_prefix("materials/").unwrap_or(&path);
        let path = path.strip_suffix(".vtf").unwrap_or(path);
        (!path.is_empty()).then(|| path.to_string())
    }

    /// A number; the engine reads the leading number of a value and ignores the rest.
    pub fn float(&self, name: &str) -> Option<f32> {
        leading_number(self.get(name)?)
    }

    /// An on/off parameter (`$translucent 1`).
    pub fn flag(&self, name: &str) -> bool {
        self.float(name).is_some_and(|v| v != 0.0)
    }

    /// A colour or vector: `"[1 0.5 0]"`, `"{255 128 0}"` (0-255) or a single number for all three.
    pub fn vector(&self, name: &str) -> Option<[f32; 3]> {
        parse_vector(self.get(name)?)
    }

    /// The `Proxies` block, if any.
    pub fn proxies(&self) -> Option<&Block> {
        self.block.block("proxies")
    }
}

pub(crate) fn leading_number(text: &str) -> Option<f32> {
    let text = text.trim_start();
    let end = text
        .char_indices()
        .find(|&(i, c)| !(c.is_ascii_digit() || c == '.' || ((c == '-' || c == '+') && i == 0)))
        .map_or(text.len(), |(i, _)| i);
    text[..end].parse().ok()
}

/// Reads `[x y z]`, `{r g b}` (0-255) or one number.
pub fn parse_vector(text: &str) -> Option<[f32; 3]> {
    let text = text.trim();
    let (inner, scale) = if let Some(inner) = text.strip_prefix('[') {
        (inner.trim_end_matches(']'), 1.0)
    } else if let Some(inner) = text.strip_prefix('{') {
        (inner.trim_end_matches('}'), 1.0 / 255.0)
    } else {
        let v = leading_number(text)?;
        return Some([v; 3]);
    };
    let numbers: Vec<f32> = inner.split_whitespace().filter_map(leading_number).collect();
    match numbers[..] {
        [x, y, z, ..] => Some([x * scale, y * scale, z * scale]),
        [v] => Some([v * scale; 3]),
        _ => None,
    }
}
