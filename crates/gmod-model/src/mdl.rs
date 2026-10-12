//! The model header (`.mdl`): what a model is made of, without its vertices.
//!
//! The header (`"IDST"`, version 44-49) is a list of counts and offsets of tables in the file;
//! names are offsets to strings, mostly counted from the start of the record that holds them.
//! Read here:
//!
//! | Offset | Field |
//! |---|---|
//! | 8 | `i32` checksum, the same in the `.vvd` and `.vtx` files |
//! | 12 | name, 64 bytes |
//! | 80 | eye position, illumination position, hull and view box (`f32`×3 each) |
//! | 152 | `i32` flags |
//! | 156 | bones: count, offset (216-byte records) |
//! | 172 | hitbox sets: count, offset (12-byte records: name, count and offset of 68-byte boxes) |
//! | 180 | animations: count, offset (100-byte records, [`crate::anim`]) |
//! | 188 | sequences: count, offset (212-byte records) |
//! | 204 | textures: count, offset (64-byte records) |
//! | 212 | texture folders: count, offset of a table of string offsets |
//! | 220 | skins: textures per skin, skin count, offset of a `u16` table |
//! | 232 | body parts: count, offset (16-byte records) |
//! | 240 | attachments: count, offset (92-byte records) |
//! | 260 | flex controllers' descriptions: count, offset (4-byte records, a name each) |
//! | 300 | pose parameters: count, offset (20-byte records) |
//! | 308 | surface property name, key values (offset, size), mass |
//! | 336 | included models: count, offset (8-byte records: a label and a path) |
//! | 348 | animation blocks: the `.ani` file's name, count, offset (8 bytes each: start, end) |
//!
//! A body part lists models, one of which is shown (chosen by the body group); a model has
//! meshes, one per material, whose vertices are a run of the model's vertices in the `.vvd`.

use std::io;
use std::sync::Arc;

use crate::anim::{self, Animation, PoseParameter, Sequence};
use crate::reader::{Reader, invalid};

const BONE_SIZE: usize = 216;
const HITBOX_SET_SIZE: usize = 12;
const HITBOX_SIZE: usize = 68;
const ATTACHMENT_SIZE: usize = 92;
const FLEX_SIZE: usize = 4;
const TEXTURE_SIZE: usize = 64;
const BODY_PART_SIZE: usize = 16;
const MODEL_SIZE: usize = 148;
const MESH_SIZE: usize = 116;
/// Bytes of one vertex in the `.vvd`; models give their first vertex as a byte offset.
pub(crate) const VERTEX_SIZE: usize = 48;

/// A point fixed to a bone, where things are attached: a muzzle, a hand, the eyes.
#[derive(Debug, Clone, PartialEq)]
pub struct Attachment {
    pub name: String,
    pub bone: usize,
    /// Where it is in the bone's space: a 3×4 matrix, rotation and translation.
    pub local: [[f32; 4]; 3],
}

/// A box on a bone where shots hit the model: of a hit group (`HITGROUP_HEAD`...).
#[derive(Debug, Clone, PartialEq)]
pub struct Hitbox {
    pub bone: usize,
    /// `HITGROUP_*`: 0 generic, 1 head, 2 chest, 3 stomach, 4 and 5 the left and right arm, 6
    /// and 7 the left and right leg, 10 gear.
    pub group: i32,
    /// The box in the bone's space.
    pub mins: [f32; 3],
    pub maxs: [f32; 3],
    pub name: String,
}

/// The boxes a model is hit by, one set of which is in use (the first, unless Lua chooses).
#[derive(Debug, Clone, PartialEq)]
pub struct HitboxSet {
    pub name: String,
    pub hitboxes: Vec<Hitbox>,
}

/// A bone of the skeleton, in its rest position.
#[derive(Debug, Clone, PartialEq)]
pub struct Bone {
    pub name: String,
    /// Index of the parent bone, or -1.
    pub parent: i32,
    /// Position and rotation (a quaternion, x y z w) relative to the parent.
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    /// The rotation as Euler angles in radians, about x, y and z: what animations add to.
    pub euler: [f32; 3],
    /// What one step of an animation's stored values is in units and in radians.
    pub position_scale: [f32; 3],
    pub rotation_scale: [f32; 3],
    /// The rest pose: from model space into the bone's space (3 rows of 4).
    pub pose_to_bone: [[f32; 4]; 3],
    pub flags: i32,
    /// What it is made of (a surface property), and its contents (`CONTENTS_*`).
    pub surface_prop: String,
    pub contents: i32,
}

/// One mesh of a model: the vertices that use one material.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mesh {
    /// Index into a skin's list of textures.
    pub material: usize,
    /// First vertex, counted from the model's first.
    pub first_vertex: usize,
    pub vertex_count: usize,
}

/// One of the models a body part can show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubModel {
    pub name: String,
    /// The model's first vertex in the `.vvd`.
    pub first_vertex: usize,
    pub vertex_count: usize,
    pub meshes: Vec<Mesh>,
}

/// A part of the model with interchangeable models: a head with or without a hat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyPart {
    pub name: String,
    /// The body group number is a mixed-radix number: this part's model is
    /// `body / base % models.len()`.
    pub base: usize,
    pub models: Vec<SubModel>,
}

/// A model header.
#[derive(Debug, Clone, PartialEq)]
pub struct Mdl {
    pub version: i32,
    pub checksum: i32,
    /// As written when the model was compiled (`props_c17/oildrum001.mdl`).
    pub name: String,
    pub flags: i32,
    pub eye_position: [f32; 3],
    pub illumination_position: [f32; 3],
    pub hull: ([f32; 3], [f32; 3]),
    pub view_box: ([f32; 3], [f32; 3]),
    pub bones: Vec<Bone>,
    /// Material names, without a folder.
    pub textures: Vec<String>,
    /// Folders under `materials/` where the materials are looked for, in order.
    pub texture_folders: Vec<String>,
    /// For each skin, which texture each texture slot shows.
    pub skins: Vec<Vec<usize>>,
    pub body_parts: Vec<BodyPart>,
    pub attachments: Vec<Attachment>,
    pub hitbox_sets: Vec<HitboxSet>,
    /// The names of what moves the face (flexes).
    pub flexes: Vec<String>,
    pub surface_prop: String,
    /// The `$keyvalues` of the model (`prop_data` and others), as text.
    pub key_values: String,
    pub mass: f32,
    pub sequences: Vec<Sequence>,
    pub animations: Vec<Animation>,
    pub pose_parameters: Vec<PoseParameter>,
    /// Models whose sequences this one plays too (`models/m_anm.mdl`), in order.
    pub include_models: Vec<String>,
    /// The `.ani` file with the animations kept out of the model, and the byte range of each
    /// of its blocks; block 0 is the model itself.
    pub animation_file: Option<String>,
    pub animation_blocks: Vec<(usize, usize)>,
    /// The file itself, which animations read their frames from.
    pub(crate) data: Data,
}

/// The bytes of a file, shared.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Data(pub Arc<[u8]>);

impl std::fmt::Debug for Data {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{} bytes", self.0.len())
    }
}

impl Mdl {
    pub fn parse(data: &[u8]) -> io::Result<Mdl> {
        let r = Reader::new(data, "model");
        if data.len() < 408 || !data.starts_with(b"IDST") {
            return Err(invalid("not a model (.mdl)"));
        }
        let version = r.i32(4)?;
        if !(44..=49).contains(&version) {
            return Err(invalid(format!("model version {version} is not known")));
        }
        let name = r.string(12).map(|n| n.chars().take(64).collect())?;
        let bounds = |at: usize| -> io::Result<([f32; 3], [f32; 3])> { Ok((r.vec3(at)?, r.vec3(at + 12)?)) };

        let bone_count = r.count(156, "bone")?;
        let bone_at = r.offset(0, 160)?;
        r.table(bone_at, bone_count, BONE_SIZE, "bones")?;
        let bones = (0..bone_count)
            .map(|i| {
                let at = bone_at + i * BONE_SIZE;
                let mut pose = [[0.0; 4]; 3];
                for (row, values) in pose.iter_mut().enumerate() {
                    for (column, value) in values.iter_mut().enumerate() {
                        *value = r.f32(at + 96 + (row * 4 + column) * 4)?;
                    }
                }
                Ok(Bone {
                    name: r.string(r.offset(at, at)?)?,
                    parent: r.i32(at + 4)?,
                    position: r.vec3(at + 32)?,
                    rotation: [r.f32(at + 44)?, r.f32(at + 48)?, r.f32(at + 52)?, r.f32(at + 56)?],
                    euler: r.vec3(at + 60)?,
                    position_scale: r.vec3(at + 72)?,
                    rotation_scale: r.vec3(at + 84)?,
                    pose_to_bone: pose,
                    flags: r.i32(at + 160)?,
                    surface_prop: match r.i32(at + 176)? {
                        0 => String::new(),
                        _ => r.string(r.offset(at, at + 176)?)?,
                    },
                    contents: r.i32(at + 180)?,
                })
            })
            .collect::<io::Result<Vec<Bone>>>()?;

        let texture_count = r.count(204, "texture")?;
        let texture_at = r.offset(0, 208)?;
        r.table(texture_at, texture_count, TEXTURE_SIZE, "textures")?;
        let textures = (0..texture_count)
            .map(|i| {
                let at = texture_at + i * TEXTURE_SIZE;
                r.string(r.offset(at, at)?)
            })
            .collect::<io::Result<Vec<String>>>()?;

        let folder_count = r.count(212, "texture folder")?;
        let folder_at = r.offset(0, 216)?;
        r.table(folder_at, folder_count, 4, "texture folders")?;
        let texture_folders = (0..folder_count).map(|i| r.string(r.offset(0, folder_at + i * 4)?)).collect::<io::Result<Vec<String>>>()?;

        let (slots, skin_count) = (r.count(220, "skin slot")?, r.count(224, "skin")?);
        let skin_at = r.offset(0, 228)?;
        r.table(skin_at, slots.saturating_mul(skin_count), 2, "skins")?;
        let skins = (0..skin_count)
            .map(|skin| (0..slots).map(|slot| r.i16(skin_at + (skin * slots + slot) * 2).map(|t| t.max(0) as usize)).collect())
            .collect::<io::Result<Vec<Vec<usize>>>>()?;

        let part_count = r.count(232, "body part")?;
        let part_at = r.offset(0, 236)?;
        r.table(part_at, part_count, BODY_PART_SIZE, "body parts")?;
        let mut body_parts = Vec::with_capacity(part_count);
        for i in 0..part_count {
            let at = part_at + i * BODY_PART_SIZE;
            let model_count = r.count(at + 4, "model")?;
            let model_at = r.offset(at, at + 12)?;
            r.table(model_at, model_count, MODEL_SIZE, "models")?;
            let mut models = Vec::with_capacity(model_count);
            for m in 0..model_count {
                let at = model_at + m * MODEL_SIZE;
                let mesh_count = r.count(at + 72, "mesh")?;
                let mesh_at = r.offset(at, at + 76)?;
                r.table(mesh_at, mesh_count, MESH_SIZE, "meshes")?;
                let meshes = (0..mesh_count)
                    .map(|k| {
                        let at = mesh_at + k * MESH_SIZE;
                        Ok(Mesh {
                            material: r.count(at, "material")?,
                            vertex_count: r.count(at + 8, "mesh vertex")?,
                            first_vertex: r.count(at + 12, "mesh vertex offset")?,
                        })
                    })
                    .collect::<io::Result<Vec<Mesh>>>()?;
                models.push(SubModel {
                    name: r.string(at)?.chars().take(64).collect(),
                    vertex_count: r.count(at + 80, "model vertex")?,
                    first_vertex: r.count(at + 84, "model vertex offset")? / VERTEX_SIZE,
                    meshes,
                });
            }
            body_parts.push(BodyPart { name: r.string(r.offset(at, at)?)?, base: r.count(at + 8, "body part base")?.max(1), models });
        }

        let attachment_count = r.count(240, "attachment")?;
        let attachment_at = r.offset(0, 244)?;
        r.table(attachment_at, attachment_count, ATTACHMENT_SIZE, "attachments")?;
        let attachments = (0..attachment_count)
            .map(|i| {
                let at = attachment_at + i * ATTACHMENT_SIZE;
                let mut local = [[0.0; 4]; 3];
                for (row, values) in local.iter_mut().enumerate() {
                    for (column, value) in values.iter_mut().enumerate() {
                        *value = r.f32(at + 12 + (row * 4 + column) * 4)?;
                    }
                }
                Ok(Attachment { name: r.string(r.offset(at, at)?)?, bone: r.count(at + 8, "attachment bone")?, local })
            })
            .collect::<io::Result<Vec<Attachment>>>()?;

        let set_count = r.count(172, "hitbox set")?;
        let set_at = r.offset(0, 176)?;
        r.table(set_at, set_count, HITBOX_SET_SIZE, "hitbox sets")?;
        let hitbox_sets = (0..set_count)
            .map(|i| {
                let at = set_at + i * HITBOX_SET_SIZE;
                let count = r.count(at + 4, "hitbox")?;
                let boxes = r.offset(at, at + 8)?;
                r.table(boxes, count, HITBOX_SIZE, "hitboxes")?;
                let hitboxes = (0..count)
                    .map(|k| {
                        let at = boxes + k * HITBOX_SIZE;
                        Ok(Hitbox {
                            bone: r.count(at, "hitbox bone")?,
                            group: r.i32(at + 4)?,
                            mins: r.vec3(at + 8)?,
                            maxs: r.vec3(at + 20)?,
                            // not to be trusted: models of Half-Life 2 count it from the start
                            // of the file or point it past the end, and the engine never reads
                            // it; a name that is not there is no name
                            name: match r.i32(at + 32)? {
                                0 => String::new(),
                                _ => r.offset(at, at + 32).and_then(|name| r.string(name)).unwrap_or_default(),
                            },
                        })
                    })
                    .collect::<io::Result<Vec<Hitbox>>>()?;
                let name = match r.i32(at)? {
                    0 => String::new(),
                    _ => r.string(r.offset(at, at)?)?,
                };
                Ok(HitboxSet { name, hitboxes })
            })
            .collect::<io::Result<Vec<HitboxSet>>>()?;

        let flex_count = r.count(260, "flex")?;
        let flex_at = r.offset(0, 264)?;
        r.table(flex_at, flex_count, FLEX_SIZE, "flexes")?;
        let flexes = (0..flex_count)
            .map(|i| {
                let at = flex_at + i * FLEX_SIZE;
                r.string(r.offset(at, at)?)
            })
            .collect::<io::Result<Vec<String>>>()?;

        let surface_prop = match r.i32(308)? {
            0 => String::new(),
            _ => r.string(r.offset(0, 308)?)?,
        };
        let key_values = match (r.i32(312)?, r.count(316, "key value byte")?) {
            (0, _) | (_, 0) => String::new(),
            _ => {
                let bytes = r.bytes(r.offset(0, 312)?, r.count(316, "key value byte")?)?;
                let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
                String::from_utf8_lossy(&bytes[..end]).into_owned()
            }
        };

        // animations, and the blocks of the .ani some of them are kept in
        let (animation_file, animation_blocks) = match r.count(352, "animation block")? {
            0 => (None, Vec::new()),
            count => {
                let table = r.offset(0, 356)?;
                r.table(table, count, 8, "animation blocks")?;
                let blocks = (0..count)
                    .map(|i| Ok((r.count(table + i * 8, "animation block start")?, r.count(table + i * 8 + 4, "animation block end")?)))
                    .collect::<io::Result<Vec<(usize, usize)>>>()?;
                (Some(r.string(r.offset(0, 348)?)?), blocks)
            }
        };
        let animations = anim::animations(&r, &animation_blocks)?;
        let sequences = anim::sequences(&r, bones.len(), animations.len())?;
        let pose_parameters = anim::pose_parameters(&r)?;
        let include_count = r.count(336, "included model")?;
        let include_at = r.offset(0, 340)?;
        r.table(include_at, include_count, 8, "included models")?;
        let include_models = (0..include_count)
            .map(|i| {
                let at = include_at + i * 8;
                r.string(r.offset(at, at + 4)?)
            })
            .collect::<io::Result<Vec<String>>>()?;

        Ok(Mdl {
            version,
            checksum: r.i32(8)?,
            name,
            flags: r.i32(152)?,
            eye_position: r.vec3(80)?,
            illumination_position: r.vec3(92)?,
            hull: bounds(104)?,
            view_box: bounds(128)?,
            bones,
            textures,
            texture_folders,
            skins,
            body_parts,
            attachments,
            hitbox_sets,
            flexes,
            surface_prop,
            key_values,
            mass: r.f32(328)?,
            sequences,
            animations,
            pose_parameters,
            include_models,
            animation_file,
            animation_blocks,
            data: Data(Arc::from(data)),
        })
    }

    /// The material names a texture slot may stand for, in the order they are looked for:
    /// each texture folder with the slot's texture (`models/props_c17/oildrum001`).
    pub fn material_candidates(&self, texture: usize) -> Vec<String> {
        let Some(name) = self.textures.get(texture) else { return Vec::new() };
        let name = name.replace('\\', "/");
        let folders: Vec<&str> =
            if self.texture_folders.is_empty() { vec![""] } else { self.texture_folders.iter().map(String::as_str).collect() };
        folders
            .into_iter()
            .map(|folder| {
                let folder = folder.replace('\\', "/");
                let folder = folder.trim_start_matches('/');
                if folder.is_empty() || folder.ends_with('/') { format!("{folder}{name}") } else { format!("{folder}/{name}") }
            })
            .collect()
    }

    /// The texture a material slot of a mesh shows with `skin` (skin 0 if there is no such skin).
    pub fn texture_of(&self, slot: usize, skin: usize) -> usize {
        let family = self.skins.get(skin).or_else(|| self.skins.first());
        family.and_then(|f| f.get(slot)).copied().unwrap_or(slot)
    }

    /// The model each body part shows for body group number `body`.
    pub fn chosen_models(&self, body: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.body_parts
            .iter()
            .enumerate()
            .filter(|(_, part)| !part.models.is_empty())
            .map(move |(i, part)| (i, body / part.base % part.models.len()))
    }
}
