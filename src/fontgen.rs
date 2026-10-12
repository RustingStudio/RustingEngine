//! A placeholder pixel font: `rusting asset generate <root> font pixel`
//! writes a TrueType file of printable ASCII drawn on a 5 x 7 grid, so HUD
//! text and signs match the chunky placeholder textures, sprites and
//! meshes. Each pixel run becomes a square-cornered contour; the file is
//! the same bytes on every machine.

/// Preset names `synth` accepts.
pub const PRESETS: [&str; 2] = ["pixel", "bold"];

/// Font units per pixel; the em is 8 pixels.
const PIXEL: i16 = 128;
const UNITS_PER_EM: u16 = 1024;

/// Rows of each glyph from `' '` to `'~'`, top first: seven rows down to
/// the baseline, then two descender rows. Bit 4 is the left column.
#[rustfmt::skip]
const GLYPHS: [[u8; 9]; 95] = [
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], // ' '
    [0x04, 0x04, 0x04, 0x04, 0x04, 0x00, 0x04, 0x00, 0x00], // !
    [0x0A, 0x0A, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], // "
    [0x0A, 0x0A, 0x1F, 0x0A, 0x1F, 0x0A, 0x0A, 0x00, 0x00], // #
    [0x04, 0x0F, 0x14, 0x0E, 0x05, 0x1E, 0x04, 0x00, 0x00], // $
    [0x18, 0x19, 0x02, 0x04, 0x08, 0x13, 0x03, 0x00, 0x00], // %
    [0x0C, 0x12, 0x14, 0x08, 0x15, 0x12, 0x0D, 0x00, 0x00], // &
    [0x04, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], // '
    [0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02, 0x00, 0x00], // (
    [0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08, 0x00, 0x00], // )
    [0x00, 0x04, 0x15, 0x0E, 0x15, 0x04, 0x00, 0x00, 0x00], // *
    [0x00, 0x04, 0x04, 0x1F, 0x04, 0x04, 0x00, 0x00, 0x00], // +
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x04, 0x08, 0x00], // ,
    [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00, 0x00, 0x00], // -
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C, 0x00, 0x00], // .
    [0x00, 0x01, 0x02, 0x04, 0x08, 0x10, 0x00, 0x00, 0x00], // /
    [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E, 0x00, 0x00], // 0
    [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E, 0x00, 0x00], // 1
    [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F, 0x00, 0x00], // 2
    [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E, 0x00, 0x00], // 3
    [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02, 0x00, 0x00], // 4
    [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E, 0x00, 0x00], // 5
    [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E, 0x00, 0x00], // 6
    [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08, 0x00, 0x00], // 7
    [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E, 0x00, 0x00], // 8
    [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C, 0x00, 0x00], // 9
    [0x00, 0x0C, 0x0C, 0x00, 0x0C, 0x0C, 0x00, 0x00, 0x00], // :
    [0x00, 0x0C, 0x0C, 0x00, 0x00, 0x0C, 0x04, 0x08, 0x00], // ;
    [0x02, 0x04, 0x08, 0x10, 0x08, 0x04, 0x02, 0x00, 0x00], // <
    [0x00, 0x00, 0x1F, 0x00, 0x1F, 0x00, 0x00, 0x00, 0x00], // =
    [0x08, 0x04, 0x02, 0x01, 0x02, 0x04, 0x08, 0x00, 0x00], // >
    [0x0E, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04, 0x00, 0x00], // ?
    [0x0E, 0x11, 0x01, 0x0D, 0x15, 0x15, 0x0E, 0x00, 0x00], // @
    [0x0E, 0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x00, 0x00], // A
    [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E, 0x00, 0x00], // B
    [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E, 0x00, 0x00], // C
    [0x1C, 0x12, 0x11, 0x11, 0x11, 0x12, 0x1C, 0x00, 0x00], // D
    [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F, 0x00, 0x00], // E
    [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10, 0x00, 0x00], // F
    [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F, 0x00, 0x00], // G
    [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11, 0x00, 0x00], // H
    [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E, 0x00, 0x00], // I
    [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C, 0x00, 0x00], // J
    [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11, 0x00, 0x00], // K
    [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F, 0x00, 0x00], // L
    [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11, 0x00, 0x00], // M
    [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x00, 0x00], // N
    [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E, 0x00, 0x00], // O
    [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10, 0x00, 0x00], // P
    [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D, 0x00, 0x00], // Q
    [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11, 0x00, 0x00], // R
    [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E, 0x00, 0x00], // S
    [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x00, 0x00], // T
    [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E, 0x00, 0x00], // U
    [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04, 0x00, 0x00], // V
    [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A, 0x00, 0x00], // W
    [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11, 0x00, 0x00], // X
    [0x11, 0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x00, 0x00], // Y
    [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F, 0x00, 0x00], // Z
    [0x0E, 0x08, 0x08, 0x08, 0x08, 0x08, 0x0E, 0x00, 0x00], // [
    [0x00, 0x10, 0x08, 0x04, 0x02, 0x01, 0x00, 0x00, 0x00], // \
    [0x0E, 0x02, 0x02, 0x02, 0x02, 0x02, 0x0E, 0x00, 0x00], // ]
    [0x04, 0x0A, 0x11, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], // ^
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1F, 0x00], // _
    [0x08, 0x04, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], // `
    [0x00, 0x00, 0x0E, 0x01, 0x0F, 0x11, 0x0F, 0x00, 0x00], // a
    [0x10, 0x10, 0x16, 0x19, 0x11, 0x11, 0x1E, 0x00, 0x00], // b
    [0x00, 0x00, 0x0E, 0x10, 0x10, 0x11, 0x0E, 0x00, 0x00], // c
    [0x01, 0x01, 0x0D, 0x13, 0x11, 0x11, 0x0F, 0x00, 0x00], // d
    [0x00, 0x00, 0x0E, 0x11, 0x1F, 0x10, 0x0E, 0x00, 0x00], // e
    [0x06, 0x09, 0x08, 0x1C, 0x08, 0x08, 0x08, 0x00, 0x00], // f
    [0x00, 0x00, 0x0F, 0x11, 0x11, 0x11, 0x0F, 0x01, 0x0E], // g
    [0x10, 0x10, 0x16, 0x19, 0x11, 0x11, 0x11, 0x00, 0x00], // h
    [0x04, 0x00, 0x0C, 0x04, 0x04, 0x04, 0x0E, 0x00, 0x00], // i
    [0x02, 0x00, 0x06, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C], // j
    [0x10, 0x10, 0x12, 0x14, 0x18, 0x14, 0x12, 0x00, 0x00], // k
    [0x0C, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E, 0x00, 0x00], // l
    [0x00, 0x00, 0x1A, 0x15, 0x15, 0x11, 0x11, 0x00, 0x00], // m
    [0x00, 0x00, 0x16, 0x19, 0x11, 0x11, 0x11, 0x00, 0x00], // n
    [0x00, 0x00, 0x0E, 0x11, 0x11, 0x11, 0x0E, 0x00, 0x00], // o
    [0x00, 0x00, 0x1E, 0x11, 0x11, 0x11, 0x1E, 0x10, 0x10], // p
    [0x00, 0x00, 0x0F, 0x11, 0x11, 0x11, 0x0F, 0x01, 0x01], // q
    [0x00, 0x00, 0x16, 0x19, 0x10, 0x10, 0x10, 0x00, 0x00], // r
    [0x00, 0x00, 0x0F, 0x10, 0x0E, 0x01, 0x1E, 0x00, 0x00], // s
    [0x08, 0x08, 0x1C, 0x08, 0x08, 0x09, 0x06, 0x00, 0x00], // t
    [0x00, 0x00, 0x11, 0x11, 0x11, 0x13, 0x0D, 0x00, 0x00], // u
    [0x00, 0x00, 0x11, 0x11, 0x11, 0x0A, 0x04, 0x00, 0x00], // v
    [0x00, 0x00, 0x11, 0x11, 0x15, 0x15, 0x0A, 0x00, 0x00], // w
    [0x00, 0x00, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x00, 0x00], // x
    [0x00, 0x00, 0x11, 0x11, 0x11, 0x11, 0x0F, 0x01, 0x0E], // y
    [0x00, 0x00, 0x1F, 0x02, 0x04, 0x08, 0x1F, 0x00, 0x00], // z
    [0x02, 0x04, 0x04, 0x08, 0x04, 0x04, 0x02, 0x00, 0x00], // {
    [0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x00, 0x00], // |
    [0x08, 0x04, 0x04, 0x02, 0x04, 0x04, 0x08, 0x00, 0x00], // }
    [0x00, 0x00, 0x08, 0x15, 0x02, 0x00, 0x00, 0x00, 0x00], // ~
];

/// The missing-glyph box, in the same rows.
const NOTDEF: [u8; 9] = [0x1F, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1F, 0, 0];

/// A square-cornered contour, clockwise with y up: `[x0, y0, x1, y1]`
/// in font units.
type Rect = [i16; 4];

/// One rectangle per run of set pixels in each row. Row 6 sits on the
/// baseline. `bold` widens every run by a pixel to the right.
fn outline(rows: &[u8; 9], bold: bool) -> Vec<Rect> {
    let mut rects = Vec::new();
    for (row, &bits) in rows.iter().enumerate() {
        // Column 0 is bit 5; bit 0 is the column bold spills into.
        let bits =
            u16::from(bits) << 1 | if bold { u16::from(bits) } else { 0 };
        let top = (7 - row as i16) * PIXEL;
        let mut column = 0;
        while column < 6 {
            let set = |column: i16| bits >> (5 - column) & 1 == 1;
            if !set(column) {
                column += 1;
                continue;
            }
            let start = column;
            while column < 6 && set(column) {
                column += 1;
            }
            rects.push([start * PIXEL, top - PIXEL, column * PIXEL, top]);
        }
    }
    rects
}

/// The pixel rectangles `[left, bottom, right, top]` of `c` in the pixel
/// preset, in pixels: the baseline is at 0, capitals are 7 tall and each
/// glyph advances 6. Characters outside printable ASCII draw as `?`.
pub fn glyph_pixels(c: char) -> Vec<[i16; 4]> {
    let index = if (' '..='~').contains(&c) {
        c as usize - usize::from(b' ')
    } else {
        usize::from(b'?' - b' ')
    };
    outline(&GLYPHS[index], false)
        .into_iter()
        .map(|rect| rect.map(|value| value / PIXEL))
        .collect()
}

/// A `glyf` record for the rectangles; empty for a blank glyph.
fn glyph_record(rects: &[Rect]) -> Vec<u8> {
    if rects.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let min = |i: usize| rects.iter().map(|r| r[i]).min().unwrap_or(0);
    let max = |i: usize| rects.iter().map(|r| r[i]).max().unwrap_or(0);
    put16(&mut out, rects.len() as u16);
    for value in [min(0), min(1), max(2), max(3)] {
        put16(&mut out, value as u16);
    }
    for contour in 0..rects.len() {
        put16(&mut out, (contour * 4 + 3) as u16);
    }
    put16(&mut out, 0); // no instructions
    out.extend(std::iter::repeat_n(1, rects.len() * 4)); // on-curve points
    let points: Vec<[i16; 2]> = rects
        .iter()
        .flat_map(|&[x0, y0, x1, y1]| [[x0, y1], [x1, y1], [x1, y0], [x0, y0]])
        .collect();
    for axis in 0..2 {
        let mut last = 0;
        for point in &points {
            put16(&mut out, (point[axis] - last) as u16);
            last = point[axis];
        }
    }
    while out.len() % 4 != 0 {
        out.push(0);
    }
    out
}

fn put16(out: &mut Vec<u8>, value: u16) {
    out.extend(value.to_be_bytes());
}

fn put32(out: &mut Vec<u8>, value: u32) {
    out.extend(value.to_be_bytes());
}

fn checksum(bytes: &[u8]) -> u32 {
    bytes.chunks(4).fold(0u32, |sum, chunk| {
        let mut word = [0; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum.wrapping_add(u32::from_be_bytes(word))
    })
}

/// The font for a preset, as TrueType file bytes, or `None` for an unknown
/// preset.
#[must_use]
pub fn synth(preset: &str) -> Option<Vec<u8>> {
    let bold = match preset {
        "pixel" => false,
        "bold" => true,
        _ => return None,
    };
    let style = if bold { "Bold" } else { "Regular" };
    let advance = (if bold { 7 } else { 6 }) * PIXEL;
    let outlines: Vec<Vec<Rect>> = std::iter::once(&NOTDEF)
        .chain(&GLYPHS)
        .map(|rows| outline(rows, bold))
        .collect();
    let glyphs = outlines.len() as u16;

    let mut glyf = Vec::new();
    let mut loca = Vec::new();
    let mut hmtx = Vec::new();
    for rects in &outlines {
        put32(&mut loca, glyf.len() as u32);
        glyf.extend(glyph_record(rects));
        put16(&mut hmtx, advance as u16);
        let left = rects.iter().map(|r| r[0]).min().unwrap_or(0);
        put16(&mut hmtx, left as u16);
    }
    put32(&mut loca, glyf.len() as u32);
    let all = outlines.iter().flatten();
    let bounds = [
        all.clone().map(|r| r[0]).min().unwrap_or(0),
        all.clone().map(|r| r[1]).min().unwrap_or(0),
        all.clone().map(|r| r[2]).max().unwrap_or(0),
        all.clone().map(|r| r[3]).max().unwrap_or(0),
    ];
    let max_contours = outlines.iter().map(Vec::len).max().unwrap_or(0) as u16;

    let ascent: i16 = 8 * PIXEL;
    let descent: i16 = -2 * PIXEL;
    let mut head = Vec::new();
    put32(&mut head, 0x0001_0000);
    put32(&mut head, 0x0001_0000); // font revision 1.0
    put32(&mut head, 0); // checksum adjustment, set below
    put32(&mut head, 0x5F0F_3CF5);
    put16(&mut head, 0x0003); // baseline and left sidebearing at 0
    put16(&mut head, UNITS_PER_EM);
    head.extend([0; 16]); // created and modified: none, for stable bytes
    for value in bounds {
        put16(&mut head, value as u16);
    }
    put16(&mut head, u16::from(bold)); // macStyle
    put16(&mut head, 8); // smallest readable size
    put16(&mut head, 2);
    put16(&mut head, 1); // long loca offsets
    put16(&mut head, 0);

    let mut hhea = Vec::new();
    put32(&mut hhea, 0x0001_0000);
    for value in [ascent, descent, 0, advance, bounds[0], 0, bounds[2], 1, 0] {
        put16(&mut hhea, value as u16);
    }
    hhea.extend([0; 10]); // caret offset and reserved words
    put16(&mut hhea, 0);
    put16(&mut hhea, glyphs);

    let mut maxp = Vec::new();
    put32(&mut maxp, 0x0001_0000);
    for value in [glyphs, max_contours * 4, max_contours, 0, 0, 2] {
        put16(&mut maxp, value);
    }
    maxp.extend([0; 16]); // no hinting programs or composite glyphs

    // One segment maps ' '..='~' to glyphs 1..=95, then the end marker.
    let mut cmap = Vec::new();
    for value in [0, 1, 3, 1, 0, 12, 4, 32, 0, 4, 4, 1, 0] {
        put16(&mut cmap, value);
    }
    for value in [0x7E, 0xFFFF, 0, 0x20, 0xFFFF] {
        put16(&mut cmap, value);
    }
    for value in [1i16 - 0x20, 1, 0, 0] {
        put16(&mut cmap, value as u16);
    }

    let family = "Rusting Pixel";
    let names = [
        (0, "Generated by Rusting Engine; dedicated to the public domain (CC0 1.0)"),
        (1, family),
        (2, style),
        (3, &format!("{family} {style} 1.0")),
        (4, &format!("{family} {style}")),
        (5, "Version 1.0"),
        (6, &format!("RustingPixel-{style}")),
        (13, "CC0 1.0 Universal"),
        (14, "https://creativecommons.org/publicdomain/zero/1.0/"),
    ];
    let mut name = Vec::new();
    let mut strings = Vec::new();
    put16(&mut name, 0);
    put16(&mut name, names.len() as u16);
    put16(&mut name, 6 + 12 * names.len() as u16);
    for (id, text) in names {
        let utf16: Vec<u8> =
            text.encode_utf16().flat_map(u16::to_be_bytes).collect();
        for value in [3, 1, 0x0409, id] {
            put16(&mut name, value);
        }
        put16(&mut name, utf16.len() as u16);
        put16(&mut name, strings.len() as u16);
        strings.extend(utf16);
    }
    name.extend(strings);

    let mut os2 = Vec::new();
    for value in [4, advance as u16, if bold { 700 } else { 400 }, 5, 0] {
        put16(&mut os2, value);
    }
    // Sub- and superscript sizes and offsets, then the strikeout.
    for value in [
        5 * PIXEL,
        5 * PIXEL,
        0,
        PIXEL,
        5 * PIXEL,
        5 * PIXEL,
        0,
        4 * PIXEL,
        PIXEL,
        3 * PIXEL,
        0,
    ] {
        put16(&mut os2, value as u16);
    }
    os2.extend([0; 10]); // PANOSE
    put32(&mut os2, 1); // Basic Latin
    os2.extend([0; 12]);
    os2.extend(*b"NONE");
    put16(&mut os2, 0x80 | if bold { 0x20 } else { 0x40 }); // typo metrics
    for value in [0x20, 0x7E, ascent as u16, descent as u16, 0] {
        put16(&mut os2, value);
    }
    put16(&mut os2, ascent as u16);
    put16(&mut os2, -descent as u16);
    put32(&mut os2, 1); // Latin 1 code page
    put32(&mut os2, 0);
    for value in [5 * PIXEL, 7 * PIXEL, 0, 0x20, 1] {
        put16(&mut os2, value as u16);
    }

    let mut post = Vec::new();
    put32(&mut post, 0x0003_0000); // no glyph names
    put32(&mut post, 0);
    put16(&mut post, (-PIXEL) as u16);
    put16(&mut post, PIXEL as u16);
    put32(&mut post, 1); // monospaced
    post.extend([0; 16]);

    // Sorted by tag, as the table directory requires.
    let tables: [(&[u8; 4], Vec<u8>); 10] = [
        (b"OS/2", os2),
        (b"cmap", cmap),
        (b"glyf", glyf),
        (b"head", head),
        (b"hhea", hhea),
        (b"hmtx", hmtx),
        (b"loca", loca),
        (b"maxp", maxp),
        (b"name", name),
        (b"post", post),
    ];
    let mut font = Vec::new();
    put32(&mut font, 0x0001_0000);
    for value in [10, 128, 3, 32] {
        put16(&mut font, value);
    }
    let mut offset = 12 + 16 * tables.len();
    let mut head_at = 0;
    for (tag, table) in &tables {
        if *tag == b"head" {
            head_at = offset;
        }
        font.extend(*tag);
        put32(&mut font, checksum(table));
        put32(&mut font, offset as u32);
        put32(&mut font, table.len() as u32);
        offset += table.len().next_multiple_of(4);
    }
    for (_, table) in &tables {
        font.extend(table);
        font.resize(font.len().next_multiple_of(4), 0);
    }
    let adjustment = 0xB1B0_AFBAu32.wrapping_sub(checksum(&font));
    font[head_at + 8..head_at + 12].copy_from_slice(&adjustment.to_be_bytes());
    Some(font)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read16(bytes: &[u8], at: usize) -> u16 {
        u16::from_be_bytes([bytes[at], bytes[at + 1]])
    }

    #[test]
    fn fonts_are_stable_well_formed_truetype() {
        for preset in PRESETS {
            let font = synth(preset).unwrap();
            assert_eq!(font, synth(preset).unwrap(), "{preset} is stable");
            assert_eq!(&font[..4], &[0, 1, 0, 0]);
            assert_eq!(checksum(&font), 0xB1B0_AFBA, "{preset} checksum");
            // Every table lies inside the file, word aligned.
            let tables = usize::from(read16(&font, 4));
            for record in 0..tables {
                let at = 12 + 16 * record;
                let word = |i: usize| {
                    u32::from_be_bytes(
                        font[at + i..at + i + 4].try_into().unwrap(),
                    ) as usize
                };
                assert_eq!(word(8) % 4, 0);
                assert!(word(8) + word(12) <= font.len());
            }
        }
        assert_ne!(synth("pixel"), synth("bold"));
        assert_eq!(synth("serif"), None);
    }

    #[test]
    fn outlines_follow_the_bitmaps() {
        // 'H' (0x11, 0x11, 0x11, 0x1F, ...): two runs per row, one across.
        let h = outline(&GLYPHS[usize::from(b'H' - b' ')], false);
        assert_eq!(h.len(), 2 * 6 + 1);
        assert!(h.contains(&[0, 3 * PIXEL, 5 * PIXEL, 4 * PIXEL]));
        // Bold widens each run, so the bar of 'H' grows by a pixel.
        let bold = outline(&GLYPHS[usize::from(b'H' - b' ')], true);
        assert!(bold.contains(&[0, 3 * PIXEL, 6 * PIXEL, 4 * PIXEL]));
        assert!(outline(&GLYPHS[0], false).is_empty());
        // Descenders go below the baseline.
        let g = outline(&GLYPHS[usize::from(b'g' - b' ')], false);
        assert_eq!(g.iter().map(|r| r[1]).min(), Some(-2 * PIXEL));
    }

    #[cfg(feature = "ui")]
    #[test]
    fn egui_draws_text_in_the_generated_font() {
        use crate::text_texture::{
            text_texture, text_texture_in_font, TextStyle,
        };
        let style = TextStyle {
            size: 16.0,
            ..TextStyle::default()
        };
        for preset in PRESETS {
            let font = synth(preset).unwrap();
            let drawn = text_texture_in_font("Score 42", style, &font)
                .expect("egui reads the font");
            let builtin = text_texture("Score 42", style);
            assert_ne!(
                drawn.rgba8, builtin.rgba8,
                "{preset} is not the built-in font"
            );
            assert!(drawn.rgba8.chunks(4).any(|texel| texel[0] > 200));
            // Monospaced with a real advance: four of any glyph take four
            // times one.
            let width = |text: &str| {
                let drawn = text_texture_in_font(text, style, &font).unwrap();
                drawn.size[0] - 2 * style.padding
            };
            assert!(width("M") > 4, "{preset} advances");
            assert_eq!(width("iiii"), width("MMMM"));
            assert!(width("MMMM").abs_diff(4 * width("M")) <= 2);
        }
    }
}
