use super::*;

/// A TrueType font made here: every character a filled box 500 units wide and 700 high on
/// the base line, 600 apart, in an em of 1000 with Windows ascent 800 and descent 200.
struct TestFont<'a> {
    family: &'a str,
    style: &'a str,
    weight: u16,
    chars: &'a [char],
    /// Characters where a symbol font keeps them (U+F000 on), in a symbol cmap.
    symbol: bool,
}

impl TestFont<'_> {
    fn build(&self) -> Vec<u8> {
        let glyphs = self.chars.len() + 1;
        let be16 = |v: u16| v.to_be_bytes();
        let mut tables: Vec<(&[u8; 4], Vec<u8>)> = Vec::new();

        let mut head = Vec::new();
        head.extend(0x0001_0000u32.to_be_bytes());
        head.extend(0x0001_0000u32.to_be_bytes());
        head.extend(0u32.to_be_bytes());
        head.extend(0x5F0F_3CF5u32.to_be_bytes());
        head.extend(be16(0));
        head.extend(be16(1000));
        head.extend([0; 16]);
        for v in [0i16, 0, 500, 700] {
            head.extend(v.to_be_bytes());
        }
        head.extend(be16(0));
        head.extend(be16(8));
        head.extend(2i16.to_be_bytes());
        head.extend(0i16.to_be_bytes());
        head.extend(0i16.to_be_bytes());
        tables.push((b"head", head));

        let mut hhea = Vec::new();
        hhea.extend(0x0001_0000u32.to_be_bytes());
        for v in [800i16, -200, 0] {
            hhea.extend(v.to_be_bytes());
        }
        hhea.extend(be16(600));
        for v in [0i16, 0, 500, 1, 0, 0, 0, 0, 0, 0, 0] {
            hhea.extend(v.to_be_bytes());
        }
        hhea.extend(be16(glyphs as u16));
        tables.push((b"hhea", hhea));

        let mut maxp = Vec::new();
        maxp.extend(0x0000_5000u32.to_be_bytes());
        maxp.extend(be16(glyphs as u16));
        tables.push((b"maxp", maxp));

        let mut hmtx = Vec::new();
        for _ in 0..glyphs {
            hmtx.extend(be16(600));
            hmtx.extend(be16(0));
        }
        tables.push((b"hmtx", hmtx));

        // .notdef is empty; the rest are boxes
        let mut glyf = Vec::new();
        let mut loca = vec![be16(0)];
        loca.push(be16(0));
        for _ in self.chars {
            for v in [1i16, 0, 0, 500, 700] {
                glyf.extend(v.to_be_bytes());
            }
            glyf.extend(be16(3));
            glyf.extend(be16(0));
            glyf.extend([1u8; 4]);
            for dx in [0i16, 0, 500, 0] {
                glyf.extend(dx.to_be_bytes());
            }
            for dy in [0i16, 700, 0, -700] {
                glyf.extend(dy.to_be_bytes());
            }
            loca.push(be16((glyf.len() / 2) as u16));
        }
        tables.push((b"loca", loca.concat()));
        tables.push((b"glyf", glyf));

        let base = if self.symbol { 0xF000 } else { 0 };
        let mut codes: Vec<(u16, u16)> = self.chars.iter().enumerate().map(|(i, &c)| (base | c as u16, i as u16 + 1)).collect();
        codes.sort();
        codes.push((0xFFFF, 0));
        let segments = codes.len() as u16;
        let mut sub = Vec::new();
        sub.extend(be16(4));
        sub.extend(be16(16 + 8 * segments));
        sub.extend(be16(0));
        sub.extend(be16(segments * 2));
        sub.extend([0u8; 6]);
        codes.iter().for_each(|&(c, _)| sub.extend(be16(c)));
        sub.extend(be16(0));
        codes.iter().for_each(|&(c, _)| sub.extend(be16(c)));
        codes.iter().for_each(|&(c, g)| sub.extend(be16(if c == 0xFFFF { 1 } else { g.wrapping_sub(c) })));
        codes.iter().for_each(|_| sub.extend(be16(0)));
        let mut cmap = Vec::new();
        cmap.extend(be16(0));
        cmap.extend(be16(1));
        cmap.extend(be16(3));
        cmap.extend(be16(if self.symbol { 0 } else { 1 }));
        cmap.extend(12u32.to_be_bytes());
        cmap.extend(sub);
        tables.push((b"cmap", cmap));

        let full = format!("{} {}", self.family, self.style);
        let strings = [(1u16, self.family), (2, self.style), (4, full.as_str())];
        let mut records = Vec::new();
        let mut text = Vec::new();
        for (id, s) in strings {
            let utf16: Vec<u8> = s.encode_utf16().flat_map(u16::to_be_bytes).collect();
            for v in [3u16, 1, 0x409, id, utf16.len() as u16, text.len() as u16] {
                records.extend(be16(v));
            }
            text.extend(utf16);
        }
        let mut name = Vec::new();
        name.extend(be16(0));
        name.extend(be16(strings.len() as u16));
        name.extend(be16(6 + records.len() as u16));
        name.extend(records);
        name.extend(text);
        tables.push((b"name", name));

        let mut os2 = vec![0u8; 78];
        os2[4..6].copy_from_slice(&be16(self.weight));
        os2[62..64].copy_from_slice(&be16(if self.style.contains("Italic") { 1 } else { 0x40 }));
        os2[74..76].copy_from_slice(&be16(800));
        os2[76..78].copy_from_slice(&be16(200));
        tables.push((b"OS/2", os2));

        tables.sort_by(|a, b| a.0.cmp(b.0));
        let mut font = Vec::new();
        font.extend(0x0001_0000u32.to_be_bytes());
        font.extend(be16(tables.len() as u16));
        font.extend([0u8; 6]);
        let mut offset = 12 + 16 * tables.len();
        let mut data = Vec::new();
        for (tag, table) in &tables {
            font.extend(*tag);
            font.extend(0u32.to_be_bytes());
            font.extend((offset as u32).to_be_bytes());
            font.extend((table.len() as u32).to_be_bytes());
            data.extend(table);
            while data.len() % 4 != 0 {
                data.push(0);
            }
            offset = 12 + 16 * tables.len() + data.len();
        }
        font.extend(data);
        font
    }
}

fn plain(family: &str, weight: u16, chars: &[char]) -> Vec<u8> {
    let style = if weight >= 600 { "Bold" } else { "Regular" };
    TestFont { family, style, weight, chars, symbol: false }.build()
}

fn spec(family: &str, size: u32) -> FontSpec {
    FontSpec { family: family.to_string(), size, ..FontSpec::default() }
}

#[test]
fn fonts_are_measured_as_vgui_measures_them() {
    let mut fonts = Fonts::new();
    assert_eq!(fonts.add_font(plain("Test Sans", 400, &['a', 'b'])), 1);
    let font = fonts.font(&spec("test sans", 20));
    assert_eq!(fonts.face_name(font), Some("test sans"));
    // the cell is the size asked for: 800 + 200 units in 20 pixels
    assert_eq!(fonts.tall(font), 20);
    // 600 units at 20 pixels to the em
    assert_eq!(fonts.text_size(font, "ab"), (24, 20));
    assert_eq!(fonts.text_size(font, "ab\na"), (24, 40));
    assert_eq!(fonts.text_size(font, ""), (0, 20));
    // the same spec is the same font
    assert_eq!(fonts.font(&spec("Test Sans", 20)), font);

    // the box: 500 x 700 units, its bottom on the base line 16 pixels down
    let glyph = fonts.glyph(font, 'a');
    assert_eq!((glyph.width, glyph.height, glyph.left, glyph.top, glyph.advance), (10, 14, 0, 2, 12));
    let page = &fonts.pages()[glyph.page as usize];
    let at = |x: f32, y: f32| {
        let (px, py) = ((x * font::PAGE_SIZE as f32) as usize, (y * font::PAGE_SIZE as f32) as usize);
        let i = (py * font::PAGE_SIZE as usize + px) * 4;
        [page.pixels[i], page.pixels[i + 1], page.pixels[i + 2], page.pixels[i + 3]]
    };
    let middle = ((glyph.uv[0] + glyph.uv[2]) / 2.0, (glyph.uv[1] + glyph.uv[3]) / 2.0);
    assert_eq!(at(middle.0, middle.1), [255, 255, 255, 255]);
    assert_eq!(fonts.take_changed(), vec![0]);
    assert!(fonts.take_changed().is_empty());
    // a glyph already baked does not change the page again
    fonts.glyph(font, 'a');
    assert!(fonts.take_changed().is_empty());
}

#[test]
fn the_face_nearest_the_weight_is_drawn() {
    let mut fonts = Fonts::new();
    fonts.add_font(plain("Duo", 400, &['a']));
    fonts.add_font(plain("Duo", 700, &['a']));
    fonts.add_font(plain("Solo", 400, &['a']));
    let regular = fonts.font(&FontSpec { weight: 500, ..spec("Duo", 20) });
    let bold = fonts.font(&FontSpec { weight: 800, ..spec("Duo", 20) });
    assert_eq!(fonts.glyph(regular, 'a').advance, 12);
    assert_eq!(fonts.glyph(bold, 'a').advance, 12);
    // a full name is that face
    let by_name = fonts.font(&spec("Duo Bold", 20));
    assert_eq!(fonts.glyph(by_name, 'a').width, fonts.glyph(bold, 'a').width);
    // no bold face: made bolder, a pixel wider
    let made_bold = fonts.font(&FontSpec { weight: 800, ..spec("Solo", 20) });
    let glyph = fonts.glyph(made_bold, 'a');
    assert_eq!((glyph.width, glyph.advance), (11, 13));
}

#[test]
fn missing_families_and_characters_come_from_the_fallbacks() {
    let mut fonts = Fonts::new();
    fonts.add_font(plain("Only Latin", 400, &['a']));
    fonts.add_font(plain("Tahoma", 400, &['a', 'ж']));
    // not there at all: the first fallback that is
    let unknown = fonts.font(&spec("Nowhere Grotesk", 20));
    assert_eq!(fonts.face_name(unknown), Some("tahoma"));
    // a character the font lacks is drawn by a fallback; one nobody has is the empty box
    let latin = fonts.font(&spec("Only Latin", 20));
    assert_eq!(fonts.glyph(latin, 'ж').width, 10);
    let none = fonts.glyph(latin, 'ψ');
    assert_eq!((none.width, none.advance), (0, 12));
    // nothing to draw with at all: text still has a size
    let mut empty = Fonts::new();
    let font = empty.font(&spec("Arial", 20));
    assert_eq!(empty.text_size(font, "ab"), (20, 20));
}

#[test]
fn symbol_fonts_answer_for_their_characters() {
    let mut fonts = Fonts::new();
    fonts.add_font(TestFont { family: "Marlett", style: "Regular", weight: 400, chars: &['r', '0'], symbol: true }.build());
    let marlett = fonts.font(&FontSpec { symbol: true, ..spec("Marlett", 20) });
    assert_eq!(fonts.glyph(marlett, 'r').width, 10);
    assert_eq!(fonts.glyph(marlett, '0').width, 10);
}

#[test]
fn effects_are_baked_into_the_glyphs() {
    let mut fonts = Fonts::new();
    fonts.add_font(plain("Test Sans", 400, &['a']));
    let shadowed = fonts.font(&FontSpec { shadow: true, outline: true, ..spec("Test Sans", 20) });
    // a pixel lower for the shadow, two for the outline
    assert_eq!(fonts.tall(shadowed), 23);
    let glyph = fonts.glyph(shadowed, 'a');
    // the outline all round, the shadow right and below
    assert_eq!((glyph.width, glyph.height, glyph.left, glyph.top), (13, 17, -1, 1));
    assert_eq!(glyph.advance, 12);
    let page = &fonts.pages()[glyph.page as usize];
    let pixel = |x: u32, y: u32| {
        let px = (glyph.uv[0] * font::PAGE_SIZE as f32) as u32 + x;
        let py = (glyph.uv[1] * font::PAGE_SIZE as f32) as u32 + y;
        let i = ((py * font::PAGE_SIZE + px) * 4) as usize;
        [page.pixels[i], page.pixels[i + 3]]
    };
    // inside: white; the outline at the left edge: black and solid
    assert_eq!(pixel(5, 8), [255, 255]);
    assert_eq!(pixel(0, 8), [0, 255]);

    let glow = fonts.font(&FontSpec { blur: 4, scanlines: 2, additive: true, ..spec("Test Sans", 20) });
    let glyph = fonts.glyph(glow, 'a');
    assert_eq!((glyph.width, glyph.height), (18, 22));
    assert!(fonts.spec(glow).additive);
}

#[test]
fn system_folders_are_read_for_names_only_until_a_face_is_used() {
    let dir = std::env::temp_dir().join(format!("gmod-ui-fonts-{}", std::process::id()));
    let nested = dir.join("truetype").join("test");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("folded.ttf"), plain("Folded Sans", 700, &['a'])).unwrap();
    std::fs::write(dir.join("broken.ttf"), b"not a font").unwrap();
    let mut fonts = Fonts::new();
    fonts.use_font_folders(vec![dir.clone()]);
    let font = fonts.font(&FontSpec { weight: 700, ..spec("Folded Sans", 20) });
    assert_eq!(fonts.face_name(font), Some("folded sans"));
    assert_eq!(fonts.glyph(font, 'a').width, 10);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn canvas_clips_and_batches() {
    let mut canvas = Canvas::new(100, 50);
    canvas.rect(10.0, 10.0, 20.0, 20.0, [255, 0, 0, 255]);
    canvas.rect(90.0, 40.0, 20.0, 20.0, [0, 255, 0, 255]);
    // out of the screen entirely
    canvas.rect(200.0, 10.0, 5.0, 5.0, [0, 0, 255, 255]);
    assert_eq!(canvas.batches().len(), 1);
    assert_eq!(canvas.vertices().len(), 12);
    // cut at the screen's edge
    let v = &canvas.vertices()[6..12];
    assert_eq!((v[0].pos, v[2].pos), ([90.0, 40.0], [100.0, 50.0]));

    // a clip cuts images, with their texture
    canvas.clip = Some(Clip::new(0, 0, 50, 50));
    canvas.image(Texture::Image(3), false, [40.0, 0.0, 20.0, 10.0], [0.0, 0.0, 1.0, 1.0], [255; 4]);
    let last = canvas.batches().last().unwrap();
    assert_eq!((last.texture, last.clip), (Texture::Image(3), None));
    let v = &canvas.vertices()[last.start as usize..];
    assert_eq!((v[1].pos, v[1].uv), ([50.0, 0.0], [0.5, 0.0]));
    // what turns carries the clip
    canvas.rotated(Texture::White, false, [25.0, 25.0], [10.0, 10.0], 90.0, [255; 4]);
    assert_eq!(canvas.batches().last().unwrap().clip, Some(Clip::new(0, 0, 50, 50)));
    let v = &canvas.vertices()[canvas.batches().last().unwrap().start as usize..];
    // turned anticlockwise: the top left corner goes to the bottom left
    assert!((v[0].pos[0] - 20.0).abs() < 1e-4 && (v[0].pos[1] - 30.0).abs() < 1e-4);
    canvas.polygon(
        Texture::White,
        true,
        &[[0.0, 0.0, 0.0, 0.0], [10.0, 0.0, 1.0, 0.0], [10.0, 10.0, 1.0, 1.0], [0.0, 10.0, 0.0, 1.0]],
        [255; 4],
    );
    let last = canvas.batches().last().unwrap();
    assert!(last.additive);
    assert_eq!(last.count, 6);
    canvas.reset(10, 10);
    assert!(canvas.is_empty() && canvas.batches().is_empty());
}

#[test]
fn text_is_drawn_from_the_atlas() {
    let mut fonts = Fonts::new();
    fonts.add_font(plain("Test Sans", 400, &['a', 'b']));
    let font = fonts.font(&spec("Test Sans", 20));
    let mut canvas = Canvas::new(200, 100);
    let width = canvas.text(&mut fonts, font, 10.0, 5.0, "ab a", [255, 255, 0, 255]);
    // the space has no glyph in this font: the empty box, as wide as the others
    assert_eq!(width, 48.0);
    assert_eq!(canvas.batches().len(), 1);
    assert_eq!(canvas.batches()[0].texture, Texture::Glyphs(0));
    // three boxes; the first one's top left two pixels below the line's top
    assert_eq!(canvas.vertices().len(), 18);
    assert_eq!(canvas.vertices()[0].pos, [10.0, 7.0]);
    assert_eq!(canvas.vertices()[6].pos, [22.0, 7.0]);
    assert_eq!(canvas.vertices()[0].color, [255, 255, 0, 255]);
    let width = canvas.text(&mut fonts, font, 0.0, 0.0, "a\nab", [255; 4]);
    assert_eq!(width, 24.0);
    assert_eq!(canvas.vertices()[24].pos, [0.0, 22.0]);
}
