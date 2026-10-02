use gmod_fs::{Backing, DirMount, Mount, Vfs};

use super::*;
use crate::decode::{half, to_rgba8};
use crate::vtf::{flags, tag};

/// A VTF file to build. Up to 7.2 the thumbnail and the image follow the header; from 7.3
/// resources point at them - the image first, to show that the offsets are followed.
struct Texture<'a> {
    minor: u32,
    size: (u16, u16),
    flags: u32,
    frames: u16,
    first_frame: u16,
    format: Format,
    mipmaps: u8,
    /// A 4 x 4 DXT1 thumbnail, or nothing.
    thumbnail: &'a [u8],
    image: &'a [u8],
    /// Resources without data of their own.
    values: &'a [([u8; 3], u32)],
}

impl Default for Texture<'_> {
    fn default() -> Self {
        Texture {
            minor: 2,
            size: (4, 4),
            flags: 0,
            frames: 1,
            first_frame: 0,
            format: Format::Bgra8888,
            mipmaps: 1,
            thumbnail: &[],
            image: &[],
            values: &[],
        }
    }
}

impl Texture<'_> {
    fn build(&self) -> Vec<u8> {
        let Texture { minor, size, flags, frames, first_frame, format, mipmaps, thumbnail, image, values } = *self;
        let resources = if minor >= 3 { 1 + usize::from(!thumbnail.is_empty()) + values.len() } else { 0 };
        let header_size = match minor {
            0 | 1 => 64,
            2 => 80,
            _ => 80 + 8 * resources,
        };
        let mut out = Vec::new();
        out.extend_from_slice(b"VTF\0");
        for v in [7, minor, header_size as u32] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&size.0.to_le_bytes());
        out.extend_from_slice(&size.1.to_le_bytes());
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&frames.to_le_bytes());
        out.extend_from_slice(&first_frame.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        for v in [0.25f32, 0.5, 0.75] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&1.0f32.to_le_bytes());
        out.extend_from_slice(&format.raw().to_le_bytes());
        out.push(mipmaps);
        let (thumbnail_format, side) = if thumbnail.is_empty() { (-1i32, 0u8) } else { (13, 4) };
        out.extend_from_slice(&thumbnail_format.to_le_bytes());
        out.extend_from_slice(&[side, side]);
        out.extend_from_slice(&1u16.to_le_bytes()); // depth
        out.extend_from_slice(&[0; 3]);
        out.extend_from_slice(&(resources as u32).to_le_bytes());
        out.resize(if minor < 2 { 64 } else { 80 }, 0);
        if minor >= 3 {
            let image_at = header_size as u32;
            let mut entry = |tag: [u8; 3], flags: u8, data: u32| {
                out.extend_from_slice(&tag);
                out.push(flags);
                out.extend_from_slice(&data.to_le_bytes());
            };
            entry(tag::IMAGE, 0, image_at);
            if !thumbnail.is_empty() {
                entry(tag::THUMBNAIL, 0, image_at + image.len() as u32);
            }
            for &(tag, value) in values {
                entry(tag, vtf::RESOURCE_NO_DATA, value);
            }
            out.extend_from_slice(image);
            out.extend_from_slice(thumbnail);
        } else {
            out.extend_from_slice(thumbnail);
            out.extend_from_slice(image);
        }
        out
    }
}

#[test]
fn reads_mipmaps_smallest_first() {
    // BGRA 4 x 4 with three levels: 1 x 1, 2 x 2, 4 x 4 in the file
    let mut image = vec![1, 2, 3, 4];
    image.extend([10, 20, 30, 40].repeat(4));
    image.extend((0..16u8).flat_map(|i| [i, 0, 0, 255]));
    let file = Texture { flags: flags::TRILINEAR, mipmaps: 3, image: &image, ..Texture::default() }.build();
    let texture = Vtf::parse(file).unwrap();
    assert_eq!((texture.version, texture.width, texture.height, texture.depth, texture.mipmaps, texture.faces), (2, 4, 4, 1, 3, 1));
    assert_eq!((texture.reflectivity, texture.bump_scale, texture.thumbnail), ([0.25, 0.5, 0.75], 1.0, None));
    assert_eq!(texture.mip_size(1), (2, 2, 1));
    assert_eq!(texture.mip_size(7), (1, 1, 1));
    assert_eq!(texture.rgba8(2, 0, 0, 0).unwrap().rgba, [3, 2, 1, 4]);
    assert_eq!(texture.rgba8(1, 0, 0, 0).unwrap().rgba, [30, 20, 10, 40].repeat(4));
    let full = texture.rgba8(0, 0, 0, 0).unwrap();
    assert_eq!((full.width, full.height), (4, 4));
    assert_eq!(&full.rgba[4 * 5..4 * 6], [0, 0, 5, 255]);
    assert!(texture.image(3, 0, 0, 0).is_none() && texture.image(0, 1, 0, 0).is_none());
    assert_eq!(texture.level_within(2), 1);
    assert_eq!(texture.level_within(0), 2);
}

#[test]
fn frames_within_each_level() {
    // 2 x 2, two levels, two frames: 1 x 1 frame 0, frame 1, then 2 x 2 frame 0, frame 1
    let mut image = vec![0, 0, 0, 1, 0, 0, 0, 2];
    image.extend([0, 0, 0, 3].repeat(4));
    image.extend([0, 0, 0, 4].repeat(4));
    let texture =
        Vtf::parse(Texture { minor: 1, size: (2, 2), frames: 2, mipmaps: 2, image: &image, ..Texture::default() }.build()).unwrap();
    let alpha = |level, frame| texture.rgba8(level, frame, 0, 0).unwrap().rgba[3];
    assert_eq!([alpha(1, 0), alpha(1, 1), alpha(0, 0), alpha(0, 1)], [1, 2, 3, 4]);
}

#[test]
fn resources_point_at_the_data() {
    let thumbnail = [0xFF, 0xFF, 0, 0, 0, 0, 0, 0]; // white
    let image = [0x00, 0xF8, 0x1F, 0x00, 0, 0, 0, 0]; // one DXT1 block: red
    let file = Texture {
        minor: 4,
        format: Format::Dxt1,
        thumbnail: &thumbnail,
        image: &image,
        values: &[(tag::CRC, 0x1234_5678)],
        ..Texture::default()
    }
    .build();
    let texture = Vtf::parse(file).unwrap();
    assert_eq!(texture.resources.len(), 3);
    assert_eq!(texture.resource_value(tag::CRC), Some(0x1234_5678));
    assert_eq!(texture.rgba8(0, 0, 0, 0).unwrap().rgba, [255, 0, 0, 255].repeat(16));
    assert_eq!(texture.thumbnail_rgba8().unwrap().rgba, [255, 255, 255, 255].repeat(16));
}

#[test]
fn cube_maps_have_a_sphere_map_before_7_5() {
    let face = [0u8; 8]; // one DXT1 block a face
    let parse = |minor, first_frame, faces: usize| {
        Vtf::parse(
            Texture { minor, flags: flags::ENVMAP, first_frame, format: Format::Dxt1, image: &face.repeat(faces), ..Texture::default() }
                .build(),
        )
        .unwrap()
    };
    assert_eq!(parse(4, 0, 7).faces, 7);
    assert_eq!(parse(4, 0, 6).faces, 6, "a file without the sphere map");
    assert_eq!(parse(4, 0xFFFF, 7).faces, 6);
    assert_eq!(parse(5, 0, 7).faces, 6);
    assert!(parse(2, 0, 7).is_cubemap());
}

#[test]
fn rejects_what_does_not_add_up() {
    let short = Texture { image: &[0; 60], ..Texture::default() }.build();
    let err = Vtf::parse(short).unwrap_err().to_string();
    assert!(err.contains("64 bytes of image data"), "{err}");
    let mut wrong = Texture { image: &[0; 64], ..Texture::default() }.build();
    wrong[4] = 8;
    assert!(Vtf::parse(wrong).is_err());
    assert!(Vtf::parse(b"VTF\0".repeat(30)).is_err());
    assert!(Vtf::parse(b"DDS ".repeat(30)).is_err());
    // an unknown format reads, but its pixels do not
    let unknown = Vtf::parse(Texture { format: Format::Unknown(77), image: &[0; 64], ..Texture::default() }.build()).unwrap();
    assert_eq!(unknown.format.name(), "format 77");
    assert!(unknown.image(0, 0, 0, 0).is_none());
}

/// A DXT1 block: two RGB565 colours and the 2-bit indices, first pixel in the lowest bits.
fn dxt1(c0: u16, c1: u16, indices: [u8; 16]) -> Vec<u8> {
    let bits = indices.iter().enumerate().fold(0u32, |acc, (i, &v)| acc | u32::from(v) << (2 * i));
    [c0.to_le_bytes(), c1.to_le_bytes()].concat().into_iter().chain(bits.to_le_bytes()).collect()
}

fn texel(rgba: &[u8], i: usize) -> [u8; 4] {
    rgba[i * 4..i * 4 + 4].try_into().unwrap()
}

#[test]
fn dxt1_blocks() {
    let order = std::array::from_fn(|i| (i % 4) as u8);
    // red above blue: four colours
    let rgba = to_rgba8(Format::Dxt1, 4, 4, &dxt1(0xF800, 0x001F, order)).unwrap();
    assert_eq!(
        [texel(&rgba, 0), texel(&rgba, 1), texel(&rgba, 2), texel(&rgba, 3)],
        [[255, 0, 0, 255], [0, 0, 255, 255], [170, 0, 85, 255], [85, 0, 170, 255]]
    );
    // blue first, not above red: three colours and transparent black
    let rgba = to_rgba8(Format::Dxt1, 4, 4, &dxt1(0x001F, 0xF800, order)).unwrap();
    assert_eq!([texel(&rgba, 2), texel(&rgba, 3)], [[128, 0, 128, 255], [0, 0, 0, 0]]);
    // a 2 x 2 image is the top left of its block; rows go down
    let mut indices = [0u8; 16];
    indices[4] = 1;
    let rgba = to_rgba8(Format::Dxt1, 2, 2, &dxt1(0xFFFF, 0x0000, indices)).unwrap();
    assert_eq!(rgba, [[255, 255, 255, 255], [255; 4], [0, 0, 0, 255], [255; 4]].concat());
    // 8 x 4: two blocks side by side
    let two = [dxt1(0xF800, 0, [0; 16]), dxt1(0x07E0, 0, [0; 16])].concat();
    let rgba = to_rgba8(Format::Dxt1, 8, 4, &two).unwrap();
    assert_eq!([texel(&rgba, 3), texel(&rgba, 4), texel(&rgba, 15)], [[255, 0, 0, 255], [0, 255, 0, 255], [0, 255, 0, 255]]);
    assert!(to_rgba8(Format::Dxt1, 8, 8, &two).is_none(), "too short");
}

#[test]
fn dxt3_and_dxt5_alpha() {
    // DXT3: 4 bits a pixel, and the colours always four (here the first not above the second)
    let alpha: u64 = (0..16).fold(0, |acc, i| acc | (i as u64) << (4 * i));
    let block = [alpha.to_le_bytes().to_vec(), dxt1(0x001F, 0xF800, [3; 16])].concat();
    let rgba = to_rgba8(Format::Dxt3, 4, 4, &block).unwrap();
    assert_eq!(texel(&rgba, 0), [170, 0, 85, 0]);
    assert_eq!(texel(&rgba, 15)[3], 255);
    assert_eq!(texel(&rgba, 1)[3], 17);

    let dxt5 = |a0: u8, a1: u8| {
        let bits: u64 = (0..16).fold(0, |acc, i| acc | ((i % 8) as u64) << (3 * i));
        let mut block = vec![a0, a1];
        block.extend_from_slice(&bits.to_le_bytes()[..6]);
        block.extend(dxt1(0xFFFF, 0xFFFF, [0; 16]));
        let rgba = to_rgba8(Format::Dxt5, 4, 4, &block).unwrap();
        (0..8).map(|i| texel(&rgba, i)[3]).collect::<Vec<u8>>()
    };
    // first above second: six steps between
    assert_eq!(dxt5(255, 0), [255, 0, 219, 182, 146, 109, 73, 36]);
    // otherwise four, then 0 and 255
    assert_eq!(dxt5(0, 255), [0, 255, 51, 102, 153, 204, 0, 255]);
}

#[test]
fn pixel_formats() {
    let one = |format, bytes: &[u8]| to_rgba8(format, 1, 1, bytes).unwrap();
    assert_eq!(one(Format::Rgba8888, &[1, 2, 3, 4]), [1, 2, 3, 4]);
    assert_eq!(one(Format::Abgr8888, &[4, 3, 2, 1]), [1, 2, 3, 4]);
    assert_eq!(one(Format::Argb8888, &[4, 1, 2, 3]), [1, 2, 3, 4]);
    assert_eq!(one(Format::Bgrx8888, &[3, 2, 1, 0]), [1, 2, 3, 255]);
    assert_eq!(one(Format::Rgb888, &[1, 2, 3]), [1, 2, 3, 255]);
    assert_eq!(one(Format::Bgr888, &[3, 2, 1]), [1, 2, 3, 255]);
    assert_eq!(one(Format::Bgr888Bluescreen, &[255, 0, 0]), [0, 0, 0, 0]);
    assert_eq!(one(Format::Rgb888Bluescreen, &[0, 0, 254]), [0, 0, 254, 255]);
    // 16-bit formats: the first named component in the lowest bits
    assert_eq!(one(Format::Bgr565, &0xF800u16.to_le_bytes()), [255, 0, 0, 255]);
    assert_eq!(one(Format::Rgb565, &0xF800u16.to_le_bytes()), [0, 0, 255, 255]);
    assert_eq!(one(Format::Bgr565, &0x07E0u16.to_le_bytes()), [0, 255, 0, 255]);
    assert_eq!(one(Format::Bgra4444, &0x8F00u16.to_le_bytes()), [255, 0, 0, 136]);
    assert_eq!(one(Format::Bgra4444, &0x000Fu16.to_le_bytes()), [0, 0, 255, 0]);
    assert_eq!(one(Format::Bgra5551, &0xFC00u16.to_le_bytes()), [255, 0, 0, 255]);
    assert_eq!(one(Format::Bgrx5551, &0x03E0u16.to_le_bytes()), [0, 255, 0, 255]);
    assert_eq!(one(Format::I8, &[7]), [7, 7, 7, 255]);
    assert_eq!(one(Format::Ia88, &[7, 9]), [7, 7, 7, 9]);
    assert_eq!(one(Format::A8, &[9]), [0, 0, 0, 9]);
    assert_eq!(one(Format::Uv88, &[1, 2]), [1, 2, 0, 255]);
    let halves: Vec<u8> = [0x3C00u16, 0x3800, 0xC000, 0x7C00].iter().flat_map(|h| h.to_le_bytes()).collect();
    assert_eq!(one(Format::Rgba16161616F, &halves), [255, 128, 0, 255]);
    assert_eq!(one(Format::Rgba16161616, &[0, 1, 0, 2, 0, 3, 0, 4]), [1, 2, 3, 4]);
    let floats: Vec<u8> = [0.5f32, 2.0, -1.0, 0.0].iter().flat_map(|f| f.to_le_bytes()).collect();
    assert_eq!(one(Format::Rgba32323232F, &floats), [128, 255, 0, 0]);
    assert_eq!(one(Format::Rgb323232F, &floats[..12]), [128, 255, 0, 255]);
    assert_eq!(one(Format::R32F, &floats[..4]), [128, 128, 128, 255]);
    assert!(to_rgba8(Format::P8, 1, 1, &[0]).is_none());
    assert!(to_rgba8(Format::Bgra8888, 2, 1, &[0; 4]).is_none(), "too short");
}

#[test]
fn half_floats() {
    assert_eq!(half(0x3C00), 1.0);
    assert_eq!(half(0xC000), -2.0);
    assert_eq!(half(0x7BFF), 65504.0);
    assert_eq!(half(0x0001), 2f32.powi(-24));
    assert_eq!(half(0x0000), 0.0);
    assert!(half(0x7E00).is_nan());
    assert_eq!(half(0xFC00), f32::NEG_INFINITY);
}

#[test]
fn halves_from_floats() {
    use crate::decode::to_half;
    assert_eq!(to_half(1.0), 0x3C00);
    assert_eq!(to_half(-2.0), 0xC000);
    assert_eq!(to_half(65504.0), 0x7BFF);
    assert_eq!(to_half(65520.0), 0x7C00, "halfway to infinity rounds to even: infinity");
    assert_eq!(to_half(1e9), 0x7C00);
    assert_eq!(to_half(f32::NEG_INFINITY), 0xFC00);
    assert!(half(to_half(f32::NAN)).is_nan());
    assert_eq!(to_half(2f32.powi(-24)), 1);
    assert_eq!(to_half(1e-8), 0);
    assert_eq!(to_half(0.1), 0x2E66);
    // every half there is comes back as itself
    for bits in 0..=u16::MAX {
        let value = half(bits);
        if !value.is_nan() {
            assert_eq!(to_half(value), bits, "{bits:#x}");
        }
    }
    // a value between two halves goes to the nearer one
    assert_eq!(to_half(1.0 + 0.75 / 1024.0), 0x3C01);
    assert_eq!(to_half(1.0 + 0.25 / 1024.0), 0x3C00);
}

#[test]
fn format_numbers_and_sizes() {
    for raw in 0..30 {
        assert_eq!(Format::from_raw(raw).unwrap().raw(), raw);
    }
    assert_eq!(Format::from_raw(-1), None);
    assert_eq!(Format::from_raw(13), Some(Format::Dxt1));
    assert_eq!(Format::Dxt5.name(), "DXT5");
    assert_eq!(Format::Dxt1.image_size(1, 1), Some(8));
    assert_eq!(Format::Dxt5.image_size(5, 5), Some(64));
    assert_eq!(Format::Dxt1.image_size(0, 0), Some(0));
    assert_eq!(Format::Bgr888.image_size(3, 3), Some(27));
    assert_eq!(Format::Unknown(40).image_size(4, 4), None);
}

const MATERIAL: &str = r#"
"LightmappedGeneric"
{
    "$basetexture" "Concrete\ConcreteFloor001A"
    "$BaseTexture" "ignored/second"
    "$bumpmap" "materials/concrete/concretefloor001a_normal.vtf"
    "$surfaceprop" "concrete"
    "$color" "[1 .5 0]"
    "$color2" "{255 0 0}"
    "$alpha" ".25"
    "$translucent" 1
    "$alphatest" "0"
    "$envmaptint" "[.1 .1 .1]"
    "hdr?$envmaptint" "[.5 .5 .5]"
    "ldr?$envmaptint" "[.9 .9 .9]"
    "!srgb?$color" "[0 0 0]"
    "GPU>=2?$detail" "detail/noise"
    "GPU<2?$detailscale" "9"
    ">=dx90"
    {
        "$surfaceprop" "metal"
    }
    "<dx90_20b"
    {
        "$basetexture" "fallback"
    }
    "LightmappedGeneric_DX8"
    {
        "$basetexture" "dx8"
    }
    "Proxies"
    {
        "AnimatedTexture" { "animatedtexturevar" "$basetexture" }
    }
}
"#;

fn material(text: &str) -> Material {
    match vmt::parse(text).unwrap() {
        Vmt::Material(m) => m,
        Vmt::Patch(_) => panic!("a patch"),
    }
}

#[test]
fn material_parameters() {
    let m = material(MATERIAL);
    assert_eq!(m.shader, "lightmappedgeneric");
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    assert_eq!(m.texture("$basetexture").as_deref(), Some("concrete/concretefloor001a"));
    assert_eq!(m.texture("$bumpmap").as_deref(), Some("concrete/concretefloor001a_normal"));
    assert_eq!(m.get("$SurfaceProp"), Some("metal"), "the >=dx90 block counts over the rest");
    assert_eq!(m.vector("$envmaptint"), Some([0.5; 3]), "hdr holds, ldr does not");
    assert_eq!(m.vector("$color"), Some([1.0, 0.5, 0.0]), "!srgb does not hold");
    assert_eq!(m.vector("$color2"), Some([1.0, 0.0, 0.0]));
    assert_eq!(m.float("$alpha"), Some(0.25));
    assert!(m.flag("$translucent") && !m.flag("$alphatest") && !m.flag("$nocull"));
    assert_eq!(m.texture("$detail").as_deref(), Some("detail/noise"));
    assert_eq!(m.get("$detailscale"), None);
    assert!(m.proxies().is_some_and(|p| p.block("animatedtexture").is_some()));
    assert_eq!(m.params().first().map(|(n, _)| n.as_str()), Some("$basetexture"));

    assert_eq!(vmt::parse_vector("2"), Some([2.0; 3]));
    assert_eq!(vmt::parse_vector("[ .5 ]"), Some([0.5; 3]));
    assert_eq!(vmt::parse_vector("[a b]"), None);
    assert!(vmt::parse("").is_err());
    // as the engine does, a top-level name without a block reads as an empty block
    let odd = material("\"shader\" \"no block\"");
    assert!(odd.params().is_empty() && odd.warnings.len() == 1);
    // a broken file still gives what can be read, with warnings
    let broken = material("\"UnlitGeneric\" { \"$basetexture\" \"x\" ");
    assert_eq!((broken.get("$basetexture"), broken.warnings.len()), (Some("x"), 1));
}

/// A game folder in the temporary directory, removed when dropped.
struct Game(std::path::PathBuf);

impl Game {
    fn new(name: &str, files: &[(&str, &[u8])]) -> Game {
        let root = std::env::temp_dir().join(format!("gmod-material-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (path, data) in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, data).unwrap();
        }
        Game(root)
    }

    fn vfs(&self) -> Vfs {
        let mut vfs = Vfs::new();
        vfs.push(Mount::new(&["GAME"], Backing::Dir(DirMount::new(&self.0))));
        vfs
    }
}

impl Drop for Game {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn patch_materials() {
    let base = b"\"LightmappedGeneric\" { \"$basetexture\" \"glass/window\" \"$envmap\" \"env_cubemap\" }";
    let patch = b"\"patch\" { \"include\" \"materials/GLASS/WINDOW.vmt\" \"replace\" { \"$envmap\" \"maps/test/c1_2_3\" \"$missing\" \"1\" } \"insert\" { \"$envmaptint\" \"[.5 .5 .5]\" } }";
    let texture = Texture { size: (1, 1), format: Format::Bgr888, image: &[3, 2, 1], ..Texture::default() }.build();
    let game = Game::new(
        "patch",
        &[
            ("materials/glass/window.vmt", base),
            ("materials/maps/test/glass/window_1_2_3.vmt", patch),
            ("materials/loop/a.vmt", b"patch { include materials/loop/b.vmt }"),
            ("materials/loop/b.vmt", b"patch { include materials/loop/a.vmt }"),
            ("materials/glass/window.vtf", &texture),
        ],
    );
    let vfs = game.vfs();
    let m = load_material(&vfs, "MAPS\\test/Glass/Window_1_2_3").unwrap();
    assert_eq!(m.shader, "lightmappedgeneric");
    assert_eq!(m.get("$envmap"), Some("maps/test/c1_2_3"));
    assert_eq!(m.get("$missing"), None, "replace changes only what is there");
    assert_eq!(m.vector("$envmaptint"), Some([0.5; 3]));
    assert_eq!(m.get("$basetexture"), Some("glass/window"));

    let err = load_material(&vfs, "loop/a").unwrap_err().to_string();
    assert!(err.contains("more than 8 deep"), "{err}");
    assert!(load_material(&vfs, "glass/nothing").is_err());

    let t = load_texture(&vfs, &m.texture("$basetexture").unwrap()).unwrap();
    assert_eq!(t.rgba8(0, 0, 0, 0).unwrap().rgba, [1, 2, 3, 255]);
    assert_eq!(material_path("Concrete\\Floor.VMT").as_deref(), Some("materials/concrete/floor.vmt"));
    assert_eq!(texture_path("../x"), None);
}

/// A PNG of raw scanlines (each with its filter byte already in front).
fn png_file(width: u32, height: u32, depth: u8, color: u8, interlaced: bool, extra: &[(&[u8; 4], &[u8])], rows: &[u8]) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&gmod_fs::crc32::update(gmod_fs::crc32::crc32(kind), data).to_be_bytes());
    };
    let mut header = Vec::new();
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[depth, color, 0, 0, u8::from(interlaced)]);
    chunk(b"IHDR", &header);
    for (kind, data) in extra {
        chunk(kind, data);
    }
    chunk(b"IDAT", &gmod_fs::deflate::zlib(rows));
    chunk(b"IEND", &[]);
    out
}

#[test]
fn png_files_are_read_as_written() {
    let pixels: Vec<u8> = (0..5 * 3 * 4).map(|i| (i * 37 % 256) as u8).collect();
    let image = png::decode(&png::encode(5, 3, 4, &pixels)).unwrap();
    assert_eq!((image.width, image.height), (5, 3));
    assert_eq!(image.pixels, pixels);

    let rgb: Vec<u8> = (0..4 * 4 * 3).map(|i| (i * 11 % 256) as u8).collect();
    let image = png::decode(&png::encode(4, 4, 3, &rgb)).unwrap();
    let expected: Vec<u8> = rgb.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
    assert_eq!(image.pixels, expected);
    assert!(png::decode(b"GIF89a").is_err());
}

#[test]
fn png_palettes_grey_and_interlacing() {
    // 2-bit palette, 3 x 2: indices 0 1 2 / 2 1 0; the first colour is half transparent
    let palette = [255, 0, 0, 0, 255, 0, 0, 0, 255];
    let rows = [0, 0b00_01_10_00, 0, 0b10_01_00_00];
    let file = png_file(3, 2, 2, 3, false, &[(b"PLTE", &palette), (b"tRNS", &[128])], &rows);
    let image = png::decode(&file).unwrap();
    let px = |i: usize| &image.pixels[i * 4..i * 4 + 4];
    assert_eq!(px(0), [255, 0, 0, 128]);
    assert_eq!(px(1), [0, 255, 0, 255]);
    assert_eq!(px(2), [0, 0, 255, 255]);
    assert_eq!(px(3), [0, 0, 255, 255]);

    // 16-bit grey with alpha, the second row "up"-filtered
    let rows = [0, 0x80, 0x00, 0xFF, 0xFF, 2, 0x10, 0, 0x00, 0];
    let image = png::decode(&png_file(1, 2, 16, 4, false, &[], &rows)).unwrap();
    assert_eq!(image.pixels, [0x80, 0x80, 0x80, 0xFF, 0x90, 0x90, 0x90, 0xFF]);

    // interlaced 8-bit grey, 2 x 2: pass 1 has (0,0), pass 6 (1,0), pass 7 the bottom row
    let rows = [0, 10, 0, 20, 0, 30, 40];
    let image = png::decode(&png_file(2, 2, 8, 0, true, &[], &rows)).unwrap();
    let grey: Vec<u8> = image.pixels.chunks(4).map(|p| p[0]).collect();
    assert_eq!(grey, [10, 20, 30, 40]);
}

#[test]
fn surface_properties_inherit_from_their_base() {
    let mut props = crate::SurfaceProps::default();
    props.add_file(
        r#""default" { "density" "2000" "stepleft" "Default.StepLeft" "gamematerial" "C" }
        "wood" { "base" "default" "density" "700" "stepleft" "Wood.StepLeft" "impacthard" "Wood.ImpactHard" "gamematerial" "W" }
        "wood_crate" { "base" "wood" "impacthard" "Wood_Crate.ImpactHard" }"#,
    );
    let crate_ = props.get("Wood_Crate").unwrap();
    assert_eq!((crate_.density, crate_.step_left.as_str(), crate_.impact_hard.as_str()), (700.0, "Wood.StepLeft", "Wood_Crate.ImpactHard"));
    assert_eq!(crate_.game_material, b'W');
    assert_eq!((props.index("default"), props.index("wood"), props.index("wood_crate")), (Some(0), Some(1), Some(2)));
    // what is not there is the default
    assert_eq!(props.or_default("nonsense").name, "default");
    assert_eq!(props.get("default").unwrap().impact_soft, "Default.ImpactSoft");
}

/// A JPEG of 16 by 8 pixels: red on the left half, blue on the right (made with ImageMagick).
const JPEG: &[u8] = &[
    0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xdb,
    0x00, 0x43, 0x00, 0x02, 0x01, 0x01, 0x01, 0x01, 0x01, 0x02, 0x01, 0x01, 0x01, 0x02, 0x02, 0x02, 0x02, 0x02, 0x04, 0x03, 0x02, 0x02,
    0x02, 0x02, 0x05, 0x04, 0x04, 0x03, 0x04, 0x06, 0x05, 0x06, 0x06, 0x06, 0x05, 0x06, 0x06, 0x06, 0x07, 0x09, 0x08, 0x06, 0x07, 0x09,
    0x07, 0x06, 0x06, 0x08, 0x0b, 0x08, 0x09, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x06, 0x08, 0x0b, 0x0c, 0x0b, 0x0a, 0x0c, 0x09, 0x0a, 0x0a,
    0x0a, 0xff, 0xdb, 0x00, 0x43, 0x01, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x05, 0x03, 0x03, 0x05, 0x0a, 0x07, 0x06, 0x07, 0x0a, 0x0a,
    0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a,
    0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a,
    0x0a, 0x0a, 0x0a, 0x0a, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x00, 0x08, 0x00, 0x10, 0x03, 0x01, 0x11, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11,
    0x01, 0xff, 0xc4, 0x00, 0x14, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x08, 0xff, 0xc4, 0x00, 0x14, 0x10, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0xff, 0xc4, 0x00, 0x16, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x0a, 0x08, 0x09, 0xff, 0xc4, 0x00, 0x14, 0x11, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0xff, 0xda, 0x00, 0x0c, 0x03, 0x01, 0x00, 0x02, 0x11, 0x03, 0x11, 0x00, 0x3f, 0x00, 0x17, 0xa5, 0x36, 0xfe, 0x08,
    0x65, 0x50, 0x35, 0xef, 0xff, 0xd9,
];

#[test]
fn reads_jpeg_images() {
    let image = crate::jpeg::decode_image(JPEG).unwrap();
    assert_eq!((image.width, image.height), (16, 8));
    let at = |x: usize, y: usize| &image.pixels[(y * 16 + x) * 4..][..4];
    let near = |p: &[u8], want: [u8; 4]| p.iter().zip(want).all(|(&a, b)| a.abs_diff(b) <= 8);
    assert!(near(at(2, 4), [255, 0, 0, 255]), "{:?}", at(2, 4));
    assert!(near(at(13, 4), [0, 0, 255, 255]), "{:?}", at(13, 4));
    assert!(crate::jpeg::decode(&JPEG[..100]).is_err());
}
