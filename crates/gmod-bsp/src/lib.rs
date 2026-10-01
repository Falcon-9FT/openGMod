//! Source engine maps (`.bsp`, versions 19 to 21): what Garry's Mod maps are.
//!
//! A map is a header - `"VBSP"`, `i32` version, 64 lump entries (`u32` offset, `u32` length,
//! `u32` version, `u32` unpacked size of a compressed lump), `i32` map revision - and the lumps
//! it points to. Any lump may be LZMA-compressed (`bspzip -repack`); see [`lumps`]. The
//! pakfile lump is a zip of the files the map brings along ([`Bsp::pakfile`]); the game lump
//! holds the static props ([`props`]). What is solid - the brushes and the BSP trees over them -
//! and the sweeps of boxes and rays through it are in [`collision`].
//!
//! A map is read into memory as a whole: maps from workshop addons live inside `.gma` files
//! and are no files of their own.

#![forbid(unsafe_code)]

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gmod_fs::ZipArchive;

pub mod collision;
mod entities;
mod lighting;
pub mod lumps;
mod math;
pub mod mesh;
pub mod props;

pub use collision::{Collision, Trace, mask};
pub use entities::{Entity, parse_entities};
pub use lighting::{luxels, rgbe_to_linear};
pub use lumps::{
    Brush, BrushSide, Cubemap, DispInfo, DispVert, Face, Leaf, LumpEntry, Model, Node, Plane, TexData, TexInfo, contents, surf,
};
pub use props::{GameLump, StaticProp, StaticProps};

use lumps::{index, invalid, lump_bytes, records};

const HEADER_SIZE: usize = 8 + 64 * 16 + 4;

/// A map.
pub struct Bsp {
    /// Where the bytes came from, for messages.
    pub label: PathBuf,
    pub version: i32,
    pub revision: i32,
    pub lumps: [LumpEntry; 64],
    pub entities: Vec<Entity>,
    pub planes: Vec<Plane>,
    pub vertices: Vec<[f32; 3]>,
    pub edges: Vec<[u16; 2]>,
    pub surfedges: Vec<i32>,
    pub faces: Vec<Face>,
    /// The faces again with HDR lightmap offsets; empty in maps compiled without HDR.
    pub faces_hdr: Vec<Face>,
    pub texinfo: Vec<TexInfo>,
    pub texdata: Vec<TexData>,
    /// Material names, one per `texdata`.
    pub materials: Vec<String>,
    pub models: Vec<Model>,
    /// The BSP trees of the brush models, each from its model's head node.
    pub nodes: Vec<Node>,
    pub leaves: Vec<Leaf>,
    /// Brushes by leaf: indices into `brushes`.
    pub leaf_brushes: Vec<u16>,
    pub brushes: Vec<Brush>,
    pub brush_sides: Vec<BrushSide>,
    pub lighting: Vec<u8>,
    pub lighting_hdr: Vec<u8>,
    pub displacements: Vec<DispInfo>,
    pub disp_verts: Vec<DispVert>,
    pub cubemaps: Vec<Cubemap>,
    pub game_lumps: Vec<GameLump>,
    pub static_props: StaticProps,
    bytes: Arc<[u8]>,
}

impl std::fmt::Debug for Bsp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bsp").field("label", &self.label).field("version", &self.version).field("faces", &self.faces.len()).finish()
    }
}

fn read_lump_table(bytes: &[u8], version: i32) -> [LumpEntry; 64] {
    let mut lumps = [LumpEntry::default(); 64];
    for (i, lump) in lumps.iter_mut().enumerate() {
        let mut f = lumps::Fields::new(&bytes[8 + i * 16..8 + i * 16 + 16]);
        *lump = LumpEntry { offset: f.u32(), length: f.u32(), version: f.u32(), fourcc: f.u32() };
    }
    // Left 4 Dead 2's version 21 orders the fields version, offset, length: the planes lump,
    // which every map has, then has an "offset" inside the header
    let planes = lumps[index::PLANES];
    if version == 21 && planes.offset < HEADER_SIZE as u32 && planes.length >= HEADER_SIZE as u32 {
        for lump in &mut lumps {
            *lump = LumpEntry { offset: lump.length, length: lump.version, version: lump.offset, fourcc: lump.fourcc };
        }
    }
    lumps
}

impl Bsp {
    /// Reads a map file.
    pub fn read(path: &Path) -> io::Result<Bsp> {
        let bytes = std::fs::read(path).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
        Bsp::parse(path.to_path_buf(), bytes.into())
    }

    /// Reads a map from its bytes; `label` names where they came from.
    pub fn parse(label: PathBuf, bytes: Arc<[u8]>) -> io::Result<Bsp> {
        let shown = label.display().to_string();
        let context = |e: io::Error| io::Error::new(e.kind(), format!("{shown}: {e}"));
        if bytes.len() < HEADER_SIZE || &bytes[0..4] != b"VBSP" {
            return Err(context(invalid("not a Source map (VBSP)")));
        }
        let version = i32::from_le_bytes(bytes[4..8].try_into().unwrap());
        if !(19..=21).contains(&version) {
            return Err(context(invalid(format!("map version {version} is not supported (19-21 are)"))));
        }
        let lumps = read_lump_table(&bytes, version);
        let revision = i32::from_le_bytes(bytes[HEADER_SIZE - 4..HEADER_SIZE].try_into().unwrap());
        let lump = |i: usize, what: &str| lump_bytes(&bytes, &lumps[i], what);

        let entities = parse_entities(&lump(index::ENTITIES, "entities")?)?;
        let planes = records(&lump(index::PLANES, "planes")?, Plane::SIZE, "planes", Plane::parse)?;
        let vertices = records(&lump(index::VERTEXES, "vertices")?, 12, "vertices", |f| f.vec3())?;
        let edges = records(&lump(index::EDGES, "edges")?, 4, "edges", |f| [f.u16(), f.u16()])?;
        let surfedges = records(&lump(index::SURFEDGES, "surfedges")?, 4, "surfedges", |f| f.i32())?;
        let faces = records(&lump(index::FACES, "faces")?, Face::SIZE, "faces", Face::parse)?;
        let faces_hdr = records(&lump(index::FACES_HDR, "HDR faces")?, Face::SIZE, "HDR faces", Face::parse)?;
        let texinfo = records(&lump(index::TEXINFO, "texinfo")?, TexInfo::SIZE, "texinfo", TexInfo::parse)?;
        let texdata = records(&lump(index::TEXDATA, "texdata")?, TexData::SIZE, "texdata", TexData::parse)?;
        let models = records(&lump(index::MODELS, "models")?, Model::SIZE, "models", Model::parse)?;
        let nodes = records(&lump(index::NODES, "nodes")?, Node::SIZE, "nodes", Node::parse)?;
        let leaf_bytes = lump(index::LEAFS, "leaves")?;
        let leaf_size = match lumps[index::LEAFS].version {
            0 if leaf_bytes.len().is_multiple_of(Leaf::SIZE_WITH_LIGHT) => Leaf::SIZE_WITH_LIGHT,
            _ if leaf_bytes.len().is_multiple_of(Leaf::SIZE) => Leaf::SIZE,
            _ => Leaf::SIZE_WITH_LIGHT,
        };
        let leaves = records(&leaf_bytes, leaf_size, "leaves", Leaf::parse)?;
        drop(leaf_bytes);
        let leaf_brushes = records(&lump(index::LEAF_BRUSHES, "leaf brushes")?, 2, "leaf brushes", |f| f.u16())?;
        let brushes = records(&lump(index::BRUSHES, "brushes")?, Brush::SIZE, "brushes", Brush::parse)?;
        let brush_sides = records(&lump(index::BRUSH_SIDES, "brush sides")?, BrushSide::SIZE, "brush sides", BrushSide::parse)?;
        let displacements = records(&lump(index::DISPINFO, "displacements")?, DispInfo::SIZE, "displacements", DispInfo::parse)?;
        let disp_verts =
            records(&lump(index::DISP_VERTS, "displacement vertices")?, DispVert::SIZE, "displacement vertices", DispVert::parse)?;
        let cubemaps = records(&lump(index::CUBEMAPS, "cubemaps")?, Cubemap::SIZE, "cubemaps", Cubemap::parse)?;
        let lighting = lump(index::LIGHTING, "lighting")?.into_owned();
        let lighting_hdr = lump(index::LIGHTING_HDR, "HDR lighting")?.into_owned();

        let string_data = lump(index::TEXDATA_STRING_DATA, "texture names")?;
        let string_table = records(&lump(index::TEXDATA_STRING_TABLE, "texture name table")?, 4, "texture name table", |f| f.i32())?;
        let materials = texdata
            .iter()
            .map(|td| {
                let start =
                    usize::try_from(*string_table.get(td.name_index as usize).unwrap_or(&-1)).ok().filter(|&s| s < string_data.len());
                start.map_or_else(String::new, |start| {
                    let rest = &string_data[start..];
                    String::from_utf8_lossy(&rest[..rest.iter().position(|&c| c == 0).unwrap_or(rest.len())]).into_owned()
                })
            })
            .collect();

        let game_lumps = props::game_lumps(&lump(index::GAME_LUMP, "game lump")?, &bytes).map_err(context)?;
        let static_props = match game_lumps.iter().find(|g| &g.id == b"sprp") {
            Some(sprp) => props::static_props(sprp).map_err(context)?,
            None => StaticProps::default(),
        };
        drop(string_data);

        let bsp = Bsp {
            label,
            version,
            revision,
            lumps,
            entities,
            planes,
            vertices,
            edges,
            surfedges,
            faces,
            faces_hdr,
            texinfo,
            texdata,
            materials,
            models,
            nodes,
            leaves,
            leaf_brushes,
            brushes,
            brush_sides,
            lighting,
            lighting_hdr,
            displacements,
            disp_verts,
            cubemaps,
            game_lumps,
            static_props,
            bytes,
        };
        bsp.check().map_err(context)?;
        Ok(bsp)
    }

    /// Indices in faces, edges and models point where they should, so that the rest of the
    /// engine can index without checking again.
    fn check(&self) -> io::Result<()> {
        for (i, edge) in self.edges.iter().enumerate() {
            if edge.iter().any(|&v| v as usize >= self.vertices.len()) {
                return Err(invalid(format!("edge {i} names a vertex that does not exist")));
            }
        }
        if let Some(se) = self.surfedges.iter().find(|se| se.unsigned_abs() as usize >= self.edges.len()) {
            return Err(invalid(format!("surfedge {se} names an edge that does not exist")));
        }
        for faces in [&self.faces, &self.faces_hdr] {
            for (i, face) in faces.iter().enumerate() {
                let edges = face.first_edge.max(0) as usize..face.first_edge.max(0) as usize + face.num_edges.max(0) as usize;
                if face.first_edge < 0 || face.num_edges < 0 || edges.end > self.surfedges.len() {
                    return Err(invalid(format!("face {i}: edges out of range")));
                }
                if face.texinfo >= 0 && face.texinfo as usize >= self.texinfo.len() {
                    return Err(invalid(format!("face {i}: texinfo {} does not exist", face.texinfo)));
                }
                if face.dispinfo >= 0 && face.dispinfo as usize >= self.displacements.len() {
                    return Err(invalid(format!("face {i}: displacement {} does not exist", face.dispinfo)));
                }
                if face.plane as usize >= self.planes.len() {
                    return Err(invalid(format!("face {i}: plane {} does not exist", face.plane)));
                }
            }
        }
        for (i, info) in self.texinfo.iter().enumerate() {
            if info.texdata >= 0 && info.texdata as usize >= self.texdata.len() {
                return Err(invalid(format!("texinfo {i}: texdata {} does not exist", info.texdata)));
            }
        }
        for (i, model) in self.models.iter().enumerate() {
            if model.first_face < 0 || model.num_faces < 0 || (model.first_face + model.num_faces) as usize > self.faces.len() {
                return Err(invalid(format!("model {i}: faces out of range")));
            }
            if !self.nodes.is_empty() && !usize::try_from(model.head_node).is_ok_and(|n| n < self.nodes.len()) {
                return Err(invalid(format!("model {i}: head node {} does not exist", model.head_node)));
            }
        }
        for (i, node) in self.nodes.iter().enumerate() {
            if !usize::try_from(node.plane).is_ok_and(|p| p < self.planes.len()) {
                return Err(invalid(format!("node {i}: plane {} does not exist", node.plane)));
            }
            for child in node.children {
                // the compiler writes a node before its children, so a child node comes later;
                // that also rules out loops, which would never end
                let fine = match usize::try_from(child) {
                    Ok(next) => next > i && next < self.nodes.len(),
                    Err(_) => ((-1 - child) as usize) < self.leaves.len(),
                };
                if !fine {
                    return Err(invalid(format!("node {i}: child {child} out of range")));
                }
            }
        }
        for (i, leaf) in self.leaves.iter().enumerate() {
            if leaf.first_leaf_brush as usize + leaf.num_leaf_brushes as usize > self.leaf_brushes.len() {
                return Err(invalid(format!("leaf {i}: brushes out of range")));
            }
        }
        if let Some(brush) = self.leaf_brushes.iter().find(|&&b| b as usize >= self.brushes.len()) {
            return Err(invalid(format!("leaf brush {brush} names a brush that does not exist")));
        }
        for (i, brush) in self.brushes.iter().enumerate() {
            if brush.first_side < 0 || brush.num_sides < 0 || brush.first_side as usize + brush.num_sides as usize > self.brush_sides.len()
            {
                return Err(invalid(format!("brush {i}: sides out of range")));
            }
        }
        if let Some(side) = self.brush_sides.iter().find(|s| s.plane as usize >= self.planes.len()) {
            return Err(invalid(format!("a brush side names plane {}, which does not exist", side.plane)));
        }
        for (i, disp) in self.displacements.iter().enumerate() {
            let count = disp.side() * disp.side();
            if disp.first_vert < 0 || disp.first_vert as usize + count > self.disp_verts.len() {
                return Err(invalid(format!("displacement {i}: vertices out of range")));
            }
            if !(1..=4).contains(&disp.power) {
                return Err(invalid(format!("displacement {i}: power {}", disp.power)));
            }
        }
        Ok(())
    }

    /// The pakfile: the files the map brings along, if it has any.
    pub fn pakfile(&self) -> io::Result<Option<ZipArchive>> {
        let entry = self.lumps[index::PAKFILE];
        if entry.length == 0 {
            return Ok(None);
        }
        if self.bytes.get(entry.offset as usize..).is_some_and(|b| b.starts_with(b"LZMA")) {
            return Err(invalid(format!("{}: the pakfile lump is compressed as a whole; that is not supported", self.label.display())));
        }
        ZipArchive::from_memory(self.label.clone(), self.bytes.clone(), entry.offset as u64, entry.length as u64).map(Some)
    }

    /// The vertices of a face, in order.
    pub fn face_vertices(&self, face: &Face) -> impl Iterator<Item = [f32; 3]> + '_ {
        let first = face.first_edge.max(0) as usize;
        self.surfedges[first..first + face.num_edges.max(0) as usize].iter().map(|&se| {
            let edge = self.edges[se.unsigned_abs() as usize];
            self.vertices[if se >= 0 { edge[0] } else { edge[1] } as usize]
        })
    }

    /// The material of a face, if it has one.
    pub fn face_material(&self, face: &Face) -> Option<&str> {
        let info = self.texinfo.get(usize::try_from(face.texinfo).ok()?)?;
        self.materials.get(usize::try_from(info.texdata).ok()?).map(String::as_str)
    }

    /// The `worldspawn` entity.
    pub fn worldspawn(&self) -> Option<&Entity> {
        self.entities.iter().find(|e| e.class_name() == "worldspawn")
    }
}

/// A small map written for tests, also those of crates that read maps (feature `testmap`).
#[cfg(any(test, feature = "testmap"))]
#[doc(hidden)]
pub mod testmap;
#[cfg(test)]
mod tests;
