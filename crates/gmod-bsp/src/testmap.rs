//! A small map written for the tests, lump by lump: a floor, a wall, a displacement, a sky face
//! and a nodraw face, with lightmaps of known colours, an LZMA-compressed entity lump, one
//! static prop and a pakfile.

use crate::lumps::index;

pub const RED: [u8; 4] = [255, 0, 0, 0];
pub const GREEN: [u8; 4] = [0, 255, 0, 0];
pub const BLUE: [u8; 4] = [0, 0, 255, 0];

fn f32s(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn i32s(values: &[i32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    let s: String = s.split_whitespace().collect();
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

/// A zip with stored entries.
fn zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut directory = Vec::new();
    for (name, data) in files {
        let crc = gmod_fs::crc32::crc32(data);
        let offset = out.len() as u32;
        out.extend_from_slice(&0x0403_4B50u32.to_le_bytes());
        out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        directory.extend_from_slice(&0x0201_4B50u32.to_le_bytes());
        directory.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        directory.extend_from_slice(&crc.to_le_bytes());
        directory.extend_from_slice(&(data.len() as u32).to_le_bytes());
        directory.extend_from_slice(&(data.len() as u32).to_le_bytes());
        directory.extend_from_slice(&(name.len() as u16).to_le_bytes());
        directory.extend_from_slice(&[0; 12]);
        directory.extend_from_slice(&offset.to_le_bytes());
        directory.extend_from_slice(name.as_bytes());
    }
    let directory_offset = out.len() as u32;
    out.extend_from_slice(&directory);
    out.extend_from_slice(&0x0605_4B50u32.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(directory.len() as u32).to_le_bytes());
    out.extend_from_slice(&directory_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

fn face(plane: u16, first_edge: i32, texinfo: i16, dispinfo: i16, light_offset: i32, mins: [i32; 2], size: [i32; 2]) -> Vec<u8> {
    let mut f = Vec::new();
    f.extend_from_slice(&plane.to_le_bytes());
    f.extend_from_slice(&[0, 0]);
    f.extend_from_slice(&first_edge.to_le_bytes());
    f.extend_from_slice(&4i16.to_le_bytes());
    f.extend_from_slice(&texinfo.to_le_bytes());
    f.extend_from_slice(&dispinfo.to_le_bytes());
    f.extend_from_slice(&(-1i16).to_le_bytes());
    f.extend_from_slice(&[0, 255, 255, 255]);
    f.extend_from_slice(&light_offset.to_le_bytes());
    f.extend_from_slice(&f32s(&[128.0 * 128.0]));
    f.extend_from_slice(&i32s(&[mins[0], mins[1], size[0], size[1], -1]));
    f.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(f.len(), 56);
    f
}

fn texinfo(s: [f32; 4], t: [f32; 4], ls: [f32; 4], lt: [f32; 4], flags: i32, texdata: i32) -> Vec<u8> {
    let mut out = f32s(&[s, t, ls, lt].concat());
    out.extend_from_slice(&i32s(&[flags, texdata]));
    out
}

/// The map's bytes.
pub fn bytes() -> Vec<u8> {
    let quads: [[[f32; 3]; 4]; 5] = [
        [[0.0, 0.0, 0.0], [128.0, 0.0, 0.0], [128.0, 128.0, 0.0], [0.0, 128.0, 0.0]],
        [[0.0, 0.0, 0.0], [0.0, 0.0, 128.0], [128.0, 0.0, 128.0], [128.0, 0.0, 0.0]],
        [[256.0, 0.0, 0.0], [384.0, 0.0, 0.0], [384.0, 128.0, 0.0], [256.0, 128.0, 0.0]],
        [[0.0, 0.0, 256.0], [0.0, 128.0, 256.0], [128.0, 128.0, 256.0], [128.0, 0.0, 256.0]],
        [[0.0, 128.0, 0.0], [128.0, 128.0, 0.0], [128.0, 128.0, 128.0], [0.0, 128.0, 128.0]],
    ];
    let vertices: Vec<f32> = quads.iter().flatten().flatten().copied().collect();
    // edge k joins vertex k to the next corner of its quad
    let mut edges = vec![0u8, 0, 0, 0];
    for q in 0..5u16 {
        for c in 0..4u16 {
            edges.extend_from_slice(&(q * 4 + c).to_le_bytes());
            edges.extend_from_slice(&(q * 4 + (c + 1) % 4).to_le_bytes());
        }
    }
    let surfedges: Vec<i32> = (1..=20).collect();
    let planes =
        f32s(&[0.0, 0.0, 1.0, 0.0]).into_iter().chain(i32s(&[2])).chain(f32s(&[0.0, 1.0, 0.0, 0.0])).chain(i32s(&[1])).collect::<Vec<u8>>();
    let sixteenth = 1.0 / 16.0;
    let texinfos = [
        texinfo([1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [sixteenth, 0.0, 0.0, 0.0], [0.0, sixteenth, 0.0, 0.0], 0, 0),
        texinfo([1.0, 0.0, 0.0, 0.0], [0.0, 0.0, -1.0, 0.0], [sixteenth, 0.0, 0.0, 0.0], [0.0, 0.0, sixteenth, 0.0], 0, 1),
        texinfo([1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [sixteenth, 0.0, 0.0, 0.0], [0.0, sixteenth, 0.0, 0.0], 0, 2),
        texinfo([1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0; 4], [0.0; 4], crate::surf::SKY, 3),
        texinfo([1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0; 4], [0.0; 4], crate::surf::NODRAW, 4),
    ]
    .concat();
    let names = ["DEV/DEV_MEASUREGENERIC01", "BRICK/BRICKWALL001A", "NATURE/BLENDGROUNDTOGRASS", "TOOLS/TOOLSSKYBOX", "TOOLS/TOOLSNODRAW"];
    let mut string_data = Vec::new();
    let mut string_table = Vec::new();
    let mut texdata = Vec::new();
    for (i, name) in names.iter().enumerate() {
        string_table.extend_from_slice(&(string_data.len() as i32).to_le_bytes());
        string_data.extend_from_slice(name.as_bytes());
        string_data.push(0);
        texdata.extend_from_slice(&f32s(&[0.5, 0.5, 0.5]));
        texdata.extend_from_slice(&i32s(&[i as i32, 512, 256, 512, 256]));
    }
    let samples = 9 * 9;
    let faces = [
        face(0, 0, 0, -1, 0, [0, 0], [8, 8]),
        face(1, 4, 1, -1, samples * 4, [0, 0], [8, 8]),
        face(0, 8, 2, 0, samples * 8, [16, 0], [8, 8]),
        face(0, 12, 3, -1, -1, [0, 0], [0, 0]),
        face(1, 16, 4, -1, -1, [0, 0], [0, 0]),
    ]
    .concat();
    let lighting: Vec<u8> = [RED, GREEN, BLUE].iter().flat_map(|c| std::iter::repeat_n(*c, samples as usize).flatten()).collect();
    let models = [f32s(&[-1.0, -1.0, -1.0, 385.0, 129.0, 257.0, 0.0, 0.0, 0.0]), i32s(&[0, 0, 5])].concat();

    // a displacement of power 2 (5 x 5) on face 2, rising along its columns
    let mut dispinfo = f32s(&[256.0, 0.0, 0.0]);
    dispinfo.extend_from_slice(&i32s(&[0, 0, 2, 0]));
    dispinfo.extend_from_slice(&f32s(&[45.0]));
    dispinfo.extend_from_slice(&i32s(&[1]));
    dispinfo.extend_from_slice(&2u16.to_le_bytes());
    dispinfo.extend_from_slice(&[0, 0]);
    dispinfo.resize(176, 0);
    let mut disp_verts = Vec::new();
    for row in 0..5 {
        for column in 0..5 {
            disp_verts.extend_from_slice(&f32s(&[0.0, 0.0, 1.0, column as f32 * 4.0, row as f32 / 4.0 * 255.0]));
        }
    }

    // the entity lump, LZMA-compressed (made by Python's lzma module, raw LZMA1, dict 64 KiB)
    let entities = include_bytes!("../testdata/entities.txt");
    let stream = unhex(include_str!("../testdata/entities.lzma.hex"));
    let mut compressed = b"LZMA".to_vec();
    compressed.extend_from_slice(&(entities.len() as u32).to_le_bytes());
    compressed.extend_from_slice(&(stream.len() as u32).to_le_bytes());
    compressed.extend_from_slice(&[0x5D, 0x00, 0x00, 0x01, 0x00]);
    compressed.extend_from_slice(&stream);

    let pak = zip(&[("materials/maps/test/patched.vmt", b"\"patch\" { \"include\" \"materials/brick/brickwall001a.vmt\" }")]);
    let cubemaps = i32s(&[64, 64, 64, 0]);

    let lumps: Vec<(usize, Vec<u8>, u32)> = vec![
        (index::ENTITIES, compressed, entities.len() as u32),
        (index::PLANES, planes, 0),
        (index::TEXDATA, texdata, 0),
        (index::VERTEXES, f32s(&vertices), 0),
        (index::TEXINFO, texinfos, 0),
        (index::FACES, faces, 0),
        (index::LIGHTING, lighting, 0),
        (index::EDGES, edges, 0),
        (index::SURFEDGES, i32s(&surfedges), 0),
        (index::MODELS, models, 0),
        (index::DISPINFO, dispinfo, 0),
        (index::DISP_VERTS, disp_verts, 0),
        (index::PAKFILE, pak, 0),
        (index::CUBEMAPS, cubemaps, 0),
        (index::TEXDATA_STRING_DATA, string_data, 0),
        (index::TEXDATA_STRING_TABLE, string_table, 0),
    ];

    let mut out = vec![0u8; 8 + 64 * 16 + 4];
    out[0..4].copy_from_slice(b"VBSP");
    out[4..8].copy_from_slice(&20i32.to_le_bytes());
    let set_entry = |out: &mut Vec<u8>, index: usize, offset: usize, length: usize, fourcc: u32| {
        let at = 8 + index * 16;
        out[at..at + 4].copy_from_slice(&(offset as u32).to_le_bytes());
        out[at + 4..at + 8].copy_from_slice(&(length as u32).to_le_bytes());
        out[at + 12..at + 16].copy_from_slice(&fourcc.to_le_bytes());
    };
    for (index, data, fourcc) in lumps {
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
        let offset = out.len();
        out.extend_from_slice(&data);
        set_entry(&mut out, index, offset, data.len(), fourcc);
    }

    // the game lump: one sprp of version 6, then the closing empty entry
    let mut sprp = 1i32.to_le_bytes().to_vec();
    let mut name = b"models/props_c17/oildrum001.mdl".to_vec();
    name.resize(128, 0);
    sprp.extend_from_slice(&name);
    sprp.extend_from_slice(&i32s(&[1]));
    sprp.extend_from_slice(&7u16.to_le_bytes());
    sprp.extend_from_slice(&i32s(&[1]));
    sprp.extend_from_slice(&f32s(&[32.0, 64.0, 0.0, 0.0, 90.0, 0.0]));
    sprp.extend_from_slice(&[0, 0, 0, 0, 1, 0, 6, 0]);
    sprp.extend_from_slice(&i32s(&[1]));
    sprp.extend_from_slice(&f32s(&[100.0, 2000.0, 32.0, 64.0, 16.0, 1.0]));
    sprp.extend_from_slice(&[0, 0, 0, 0]);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
    let directory_at = out.len();
    let data_at = directory_at + 4 + 2 * 16;
    let mut game = 2i32.to_le_bytes().to_vec();
    game.extend_from_slice(&u32::from_be_bytes(*b"sprp").to_le_bytes());
    game.extend_from_slice(&0u16.to_le_bytes());
    game.extend_from_slice(&6u16.to_le_bytes());
    game.extend_from_slice(&i32s(&[data_at as i32, sprp.len() as i32]));
    game.extend_from_slice(&[0; 16]);
    game.extend_from_slice(&sprp);
    out.extend_from_slice(&game);
    set_entry(&mut out, index::GAME_LUMP, directory_at, game.len(), 0);
    out
}
