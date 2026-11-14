//! Document model: pixels, palette, layers, tile set, selection.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// One pixel. In an indexed document: a palette index 0..=255, or `TRANSP`. In a true-colour
/// document: `TRANSP`, or the colour packed by `pack` (its alpha, 1..=255, is the top byte, so
/// a colour is never below 2^24 and can never be mistaken for an index or for `TRANSP`).
pub type Cell = u32;
pub const TRANSP: Cell = 256;
pub const MAX_DIM: u32 = 4096;

/// What a document's pixels are. A document is one or the other for its whole life unless
/// `convert_mode` is called; nothing converts it implicitly.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ColorMode {
    Indexed,
    Rgba,
}

impl ColorMode {
    pub fn name(self) -> &'static str {
        match self {
            ColorMode::Indexed => "indexed",
            ColorMode::Rgba => "truecolor",
        }
    }
    pub fn parse(s: &str) -> Option<ColorMode> {
        match s.to_ascii_lowercase().replace(['-', '_', ' '], "").as_str() {
            "indexed" | "index" | "palette" => Some(ColorMode::Indexed),
            "truecolor" | "truecolour" | "rgba" | "rgb" => Some(ColorMode::Rgba),
            _ => None,
        }
    }
}

/// True for a transparent pixel, in either kind of document: `TRANSP`, and any other value
/// that is neither an index nor a packed colour.
#[inline]
pub fn cell_empty(c: Cell) -> bool {
    (256..1 << 24).contains(&c)
}
/// A true-colour pixel from r, g, b, a. Alpha 0 is `TRANSP` whatever the colour.
#[inline]
pub fn pack(c: [u8; 4]) -> Cell {
    if c[3] == 0 {
        TRANSP
    } else {
        u32::from_le_bytes(c)
    }
}
/// r, g, b, a of a true-colour pixel.
#[inline]
pub fn unpack(c: Cell) -> [u8; 4] {
    if c >> 24 == 0 {
        [0, 0, 0, 0]
    } else {
        c.to_le_bytes()
    }
}
/// Draw `s` over `dst` (both straight, not premultiplied, alpha).
#[inline]
pub fn blend(dst: &mut [u8], s: [u8; 4]) {
    let sa = s[3] as u32;
    if sa == 0 {
        return;
    }
    let da = dst[3] as u32;
    if sa == 255 || da == 0 {
        dst[..4].copy_from_slice(&s);
        return;
    }
    let db = da * (255 - sa) / 255;
    let oa = sa + db;
    for i in 0..3 {
        dst[i] = ((s[i] as u32 * sa + dst[i] as u32 * db) / oa) as u8;
    }
    dst[3] = oa as u8;
}
pub fn luma(c: [u8; 3]) -> i32 {
    (c[0] as i32 * 299 + c[1] as i32 * 587 + c[2] as i32 * 114) / 1000
}
pub fn dist(a: [u8; 3], b: [u8; 3]) -> i32 {
    let dr = a[0] as i32 - b[0] as i32;
    let dg = a[1] as i32 - b[1] as i32;
    let db = a[2] as i32 - b[2] as i32;
    30 * dr * dr + 59 * dg * dg + 11 * db * db
}
pub fn hex6(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}
pub fn hex8(c: [u8; 4]) -> String {
    format!("#{:02x}{:02x}{:02x}{:02x}", c[0], c[1], c[2], c[3])
}
pub fn parse_hex(s: &str) -> Option<[u8; 4]> {
    let s = s.trim().trim_start_matches('#');
    if !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let n = |i: usize| u8::from_str_radix(&s[i..i + 1], 16).ok().map(|v| v * 17);
    let b = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
    match s.len() {
        3 => Some([n(0)?, n(1)?, n(2)?, 255]),
        4 => Some([n(0)?, n(1)?, n(2)?, n(3)?]),
        6 => Some([b(0)?, b(2)?, b(4)?, 255]),
        8 => Some([b(0)?, b(2)?, b(4)?, b(6)?]),
        _ => None,
    }
}
/// h in degrees, s and v in 0..1.
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [u8; 3] {
    let h = h.rem_euclid(360.0) / 60.0;
    let i = h.floor();
    let f = h - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    let (r, g, b) = match i as i32 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    let c = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    [c(r), c(g), c(b)]
}
pub fn rgb_to_hsv(c: [u8; 3]) -> (f32, f32, f32) {
    let (r, g, b) = (c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, if max == 0.0 { 0.0 } else { d / max }, max)
}

// ---------------------------------------------------------------- palette

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Ramp {
    pub name: String,
    pub indices: Vec<u8>,
    pub cycle: bool,
    /// Colour-cycling speed in steps per second.
    pub speed: f32,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Palette {
    pub colors: Vec<[u8; 3]>,
    pub ramps: Vec<Ramp>,
}

pub const PRESETS: &[&str] = &[
    "default", "db32", "db16", "pico8", "sweetie16", "endesga32", "gameboy", "c64", "ega", "cga", "grayscale",
];

fn from_hex_list(s: &str) -> Palette {
    Palette {
        colors: s.split_whitespace().filter_map(parse_hex).map(|c| [c[0], c[1], c[2]]).collect(),
        ramps: vec![],
    }
}

impl Palette {
    pub fn preset(name: &str) -> Option<Palette> {
        Some(match name.to_ascii_lowercase().as_str() {
            "default" => Palette::default_256(),
            "db32" => from_hex_list(
                "000000 222034 45283c 663931 8f563b df7126 d9a066 eec39a fbf236 99e550 6abe30 37946e 4b692f 524b24 \
                 323c39 3f3f74 306082 5b6ee1 639bff 5fcde4 cbdbfc ffffff 9badb7 847e87 696a6a 595652 76428a ac3232 \
                 d95763 d77bba 8f974a 8a6f30",
            ),
            "db16" => from_hex_list(
                "140c1c 442434 30346d 4e4a4e 854c30 346524 d04648 757161 597dce d27d2c 8595a1 6daa2c d2aa99 6dc2ca \
                 dad45e deeed6",
            ),
            "pico8" => from_hex_list(
                "000000 1d2b53 7e2553 008751 ab5236 5f574f c2c3c7 fff1e8 ff004d ffa300 ffec27 00e436 29adff 83769c \
                 ff77a8 ffccaa",
            ),
            "sweetie16" => from_hex_list(
                "1a1c2c 5d275d b13e53 ef7d57 ffcd75 a7f070 38b764 257179 29366f 3b5dc9 41a6f6 73eff7 f4f4f4 94b0c2 \
                 566c86 333c57",
            ),
            "endesga32" => from_hex_list(
                "be4a2f d77643 ead4aa e4a672 b86f50 733e39 3e2731 a22633 e43b44 f77622 feae34 fee761 63c74d 3e8948 \
                 265c42 193c3e 124e89 0099db 2ce8f5 ffffff c0cbdc 8b9bb4 5a6988 3a4466 262b44 181425 ff0044 68386c \
                 b55088 f6757a e8b796 c28569",
            ),
            "gameboy" => from_hex_list("0f380f 306230 8bac0f 9bbc0f"),
            "c64" => from_hex_list(
                "000000 ffffff 68372b 70a4b2 6f3d86 588d43 352879 b8c76f 6f4f25 433900 9a6759 444444 6c6c6c 9ad284 \
                 6c5eb5 959595",
            ),
            "ega" => from_hex_list(
                "000000 0000aa 00aa00 00aaaa aa0000 aa00aa aa5500 aaaaaa 555555 5555ff 55ff55 55ffff ff5555 ff55ff \
                 ffff55 ffffff",
            ),
            "cga" => from_hex_list("000000 55ffff ff55ff ffffff"),
            "grayscale" => Palette {
                colors: (0..=255u8).map(|v| [v, v, v]).collect(),
                ramps: vec![Ramp { name: "Grey".into(), indices: (0..=255).collect(), cycle: false, speed: 8.0 }],
            },
            _ => return None,
        })
    }

    /// 16 greys followed by 15 hue-shifted ramps of 16 shades, each registered as a ramp.
    pub fn default_256() -> Palette {
        let mut colors = Vec::with_capacity(256);
        let mut ramps = Vec::new();
        for i in 0..16 {
            let v = (i as f32 / 15.0 * 255.0).round() as u8;
            colors.push([v, v, v]);
        }
        ramps.push(Ramp { name: "Grey".into(), indices: (0..16).collect(), cycle: false, speed: 8.0 });
        let names = [
            "Red", "Orange", "Amber", "Yellow", "Lime", "Green", "Emerald", "Teal", "Cyan", "Sky", "Blue", "Indigo",
            "Violet", "Magenta", "Rose",
        ];
        for (row, name) in names.iter().enumerate() {
            let base = row as f32 * 24.0;
            let start = colors.len() as u8;
            for i in 0..16 {
                let t = i as f32 / 15.0;
                let h = base + (t - 0.5) * 28.0;
                let v = 0.14 + 0.86 * t.powf(0.85);
                let s = (0.92 - 0.62 * t * t).clamp(0.0, 1.0);
                colors.push(hsv_to_rgb(h, s, v));
            }
            ramps.push(Ramp { name: (*name).into(), indices: (start..=start + 15).collect(), cycle: false, speed: 8.0 });
        }
        Palette { colors, ramps }
    }

    pub fn rgba(&self, i: usize) -> [u8; 4] {
        self.colors.get(i).map(|c| [c[0], c[1], c[2], 255]).unwrap_or([0, 0, 0, 255])
    }

    pub fn nearest(&self, rgb: [u8; 3]) -> u8 {
        let mut best = 0;
        let mut bd = i32::MAX;
        for (i, c) in self.colors.iter().enumerate() {
            let d = dist(*c, rgb);
            if d < bd {
                bd = d;
                best = i;
                if d == 0 {
                    break;
                }
            }
        }
        best as u8
    }

    /// (ramp index, position in ramp) for a palette index, preferring `prefer`.
    pub fn ramp_pos(&self, idx: u8, prefer: Option<usize>) -> Option<(usize, usize)> {
        if let Some(r) = prefer {
            if let Some(p) = self.ramps.get(r).and_then(|rp| rp.indices.iter().position(|&i| i == idx)) {
                return Some((r, p));
            }
        }
        self.ramps.iter().enumerate().find_map(|(r, rp)| rp.indices.iter().position(|&i| i == idx).map(|p| (r, p)))
    }

    /// Palette as shown on screen; cycling ramps are rotated by `time` when `cycling`.
    pub fn display(&self, cycling: bool, time: f64) -> Vec<[u8; 3]> {
        let mut out = self.colors.clone();
        if cycling {
            for r in &self.ramps {
                let n = r.indices.len();
                if !r.cycle || n < 2 {
                    continue;
                }
                let shift = ((time * r.speed as f64).floor() as i64).rem_euclid(n as i64) as usize;
                for i in 0..n {
                    let (dst, src) = (r.indices[i] as usize, r.indices[(i + n - shift) % n] as usize);
                    if dst < out.len() && src < self.colors.len() {
                        out[dst] = self.colors[src];
                    }
                }
            }
        }
        out
    }
}

// ---------------------------------------------------------------- document

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Layer {
    pub id: u32,
    pub name: String,
    pub visible: bool,
    /// Kept for the project file format only: layers are always opaque, and this is
    /// reset to 255 when a file is loaded.
    pub opacity: u8,
    pub locked: bool,
    pub alpha_lock: bool,
    pub data: Arc<Vec<Cell>>,
    /// Tile ids per cell (row-major) when this layer is tile mapped.
    pub map: Option<Arc<Vec<u16>>>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct TileSet {
    pub tw: u32,
    pub th: u32,
    /// Tile 0 is always the blank tile.
    pub tiles: Vec<Arc<Vec<Cell>>>,
}

/// Floating pixels (pasted or lifted) hovering above the active layer.
#[derive(Clone, PartialEq, Debug)]
pub struct Float {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub data: Arc<Vec<Cell>>,
    /// Selection shape local to the float; becomes the selection on commit.
    pub sel: Option<Arc<Vec<u8>>>,
    /// Layer pixels and selection from before a lift, restored on cancel.
    pub restore: Option<(Arc<Vec<Cell>>, Option<Arc<Vec<u8>>>)>,
}

/// The frames of a grid animation: a rectangle of the image cut into equal cells, read left
/// to right, then top to bottom.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Grid {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    /// Size of one frame.
    pub fw: u32,
    pub fh: u32,
    /// Frames to play; 0 plays every whole frame in the region.
    pub limit: u32,
}

impl Grid {
    pub fn cols(&self) -> u32 {
        self.w / self.fw.max(1)
    }
    pub fn rows(&self) -> u32 {
        self.h / self.fh.max(1)
    }
    /// Whole frames in the region; part frames at the right and bottom edges are left out.
    pub fn total(&self) -> u32 {
        self.cols().saturating_mul(self.rows())
    }
    /// Frames that play.
    pub fn count(&self) -> u32 {
        if self.limit == 0 {
            self.total()
        } else {
            self.limit.min(self.total())
        }
    }
    /// Top-left corner of frame `i`.
    pub fn origin(&self, i: u32) -> (u32, u32) {
        let c = self.cols().max(1);
        (self.x + i % c * self.fw, self.y + i / c * self.fh)
    }
    /// True when the region lies inside a `w` x `h` image and holds at least one frame.
    pub fn fits(&self, w: u32, h: u32) -> bool {
        self.fw >= 1 && self.fh >= 1 && self.fw <= self.w && self.fh <= self.h && self.x as u64 + self.w as u64 <= w as u64 && self.y as u64 + self.h as u64 <= h as u64
    }
}

/// One frame of an animation: a rectangle of the image, and where its top-left corner is
/// drawn relative to the animation's origin (so trimmed frames of different sizes line up).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Frame {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub ox: i32,
    pub oy: i32,
}

impl Frame {
    pub fn fits(&self, w: u32, h: u32) -> bool {
        self.w >= 1 && self.h >= 1 && self.x as u64 + self.w as u64 <= w as u64 && self.y as u64 + self.h as u64 <= h as u64 && self.ox.unsigned_abs() <= MAX_DIM && self.oy.unsigned_abs() <= MAX_DIM
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Frames {
    Grid(Grid),
    /// Each frame given as its own rectangle. Shared, so undo steps do not copy the list.
    List(Arc<Vec<Frame>>),
}

/// A named animation: frames cut from the image and a speed. Part of the document, so it is
/// saved, undone and redone with it. No pixels belong to it; it only says where to look.
#[derive(Clone, PartialEq, Debug)]
pub struct Anim {
    pub name: String,
    pub fps: f32,
    pub frames: Frames,
}

impl Anim {
    pub const FPS: std::ops::RangeInclusive<f32> = 1.0..=60.0;
    /// Most animations in one document.
    pub const MAX: usize = 256;
    /// Most frames in one frame list.
    pub const MAX_FRAMES: usize = 4096;
    /// Most frame-list frames in one document, all animations together. It keeps the stored
    /// and exported lists small enough to be read back.
    pub const MAX_LIST_FRAMES: usize = 32768;
    pub const NAME_MAX: usize = 64;

    pub fn grid(&self) -> Option<Grid> {
        match &self.frames {
            Frames::Grid(g) => Some(*g),
            Frames::List(_) => None,
        }
    }
    /// Frames that play.
    pub fn count(&self) -> u32 {
        match &self.frames {
            Frames::Grid(g) => g.count(),
            Frames::List(l) => l.len() as u32,
        }
    }
    pub fn frame(&self, i: u32) -> Option<Frame> {
        match &self.frames {
            Frames::Grid(g) => (i < g.count()).then(|| {
                let (x, y) = g.origin(i);
                Frame { x, y, w: g.fw, h: g.fh, ox: 0, oy: 0 }
            }),
            Frames::List(l) => l.get(i as usize).copied(),
        }
    }
    /// The box that holds every frame at its offset: left, top, width, height.
    pub fn bounds(&self) -> (i32, i32, u32, u32) {
        match &self.frames {
            Frames::Grid(g) => (0, 0, g.fw, g.fh),
            Frames::List(l) => {
                let (mut x0, mut y0, mut x1, mut y1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
                for f in l.iter() {
                    (x0, y0) = (x0.min(f.ox as i64), y0.min(f.oy as i64));
                    (x1, y1) = (x1.max(f.ox as i64 + f.w as i64), y1.max(f.oy as i64 + f.h as i64));
                }
                if x1 <= x0 || y1 <= y0 {
                    return (0, 0, 1, 1);
                }
                (x0 as i32, y0 as i32, (x1 - x0) as u32, (y1 - y0) as u32)
            }
        }
    }
    /// True when every frame lies inside a `w` x `h` image and there is at least one.
    pub fn fits(&self, w: u32, h: u32) -> bool {
        match &self.frames {
            Frames::Grid(g) => g.fits(w, h),
            Frames::List(l) => !l.is_empty() && l.len() <= Self::MAX_FRAMES && l.iter().all(|f| f.fits(w, h)),
        }
    }
    /// Frame-list frames in a set of animations.
    pub fn list_frames(anims: &[Anim]) -> usize {
        anims.iter().map(|a| if let Frames::List(l) = &a.frames { l.len() } else { 0 }).sum()
    }
    /// A name that can be shown and listed as it is: not blank, not padded, a sensible length,
    /// and with no control characters or characters that reorder text.
    pub fn name_ok(s: &str) -> bool {
        !s.is_empty()
            && s.trim() == s
            && s.chars().count() <= Self::NAME_MAX
            && !s.chars().any(|c| c.is_control() || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
    }
    /// True when the animation is sound for a `w` x `h` image.
    pub fn valid(&self, w: u32, h: u32) -> bool {
        Self::name_ok(&self.name) && Self::FPS.contains(&self.fps) && self.fits(w, h)
    }
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Doc {
    pub w: u32,
    pub h: u32,
    pub mode: ColorMode,
    pub palette: Palette,
    pub layers: Vec<Layer>,
    pub active: usize,
    pub tileset: Option<TileSet>,
    pub selection: Option<Arc<Vec<u8>>>,
    #[serde(skip)]
    pub float: Option<Float>,
    pub next_id: u32,
    /// Not in the serialised layout (which published builds read): `io` stores them after the document.
    #[serde(skip)]
    pub anims: Vec<Anim>,
}

impl Doc {
    pub fn new(w: u32, h: u32, palette: Palette) -> Doc {
        let mut d = Doc {
            w,
            h,
            mode: ColorMode::Indexed,
            palette,
            layers: vec![],
            active: 0,
            tileset: None,
            selection: None,
            float: None,
            next_id: 1,
            anims: vec![],
        };
        let l = d.new_layer("Layer 1");
        d.layers.push(l);
        d
    }
    /// A blank true-colour document. Its palette is a set of swatches: no pixel refers to it.
    pub fn new_rgba(w: u32, h: u32, palette: Palette) -> Doc {
        let mut d = Doc::new(w, h, palette);
        d.mode = ColorMode::Rgba;
        d
    }
    pub fn len(&self) -> usize {
        (self.w * self.h) as usize
    }
    /// True for a true-colour document.
    pub fn rgba(&self) -> bool {
        self.mode == ColorMode::Rgba
    }
    pub fn to_rgba(&self, c: Cell) -> [u8; 4] {
        if self.rgba() {
            unpack(c)
        } else if c >= 256 {
            [0, 0, 0, 0]
        } else {
            self.palette.rgba(c as usize)
        }
    }
    /// The pixel for a colour. Indexed: the nearest palette colour (transparent for mostly
    /// transparent input). True colour: exactly that colour.
    pub fn from_rgba(&self, c: [u8; 4]) -> Cell {
        if self.rgba() {
            pack(c)
        } else if c[3] < 128 {
            TRANSP
        } else {
            self.palette.nearest([c[0], c[1], c[2]]) as Cell
        }
    }
    pub fn new_layer(&mut self, name: &str) -> Layer {
        let id = self.next_id;
        self.next_id += 1;
        Layer {
            id,
            name: name.to_string(),
            visible: true,
            opacity: 255,
            locked: false,
            alpha_lock: false,
            data: Arc::new(vec![TRANSP; self.len()]),
            map: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn put_cells(&self, out: &mut [u8], cells: &[Cell], ox: i32, oy: i32, cw: i32, ch: i32, pal: &[[u8; 3]]) {
        let (w, h) = (self.w as i32, self.h as i32);
        for cy in 0..ch {
            let y = oy + cy;
            if y < 0 || y >= h {
                continue;
            }
            for cx in 0..cw {
                let x = ox + cx;
                if x < 0 || x >= w {
                    continue;
                }
                let c = cells[(cy * cw + cx) as usize];
                let o = ((y * w + x) * 4) as usize;
                if self.rgba() {
                    blend(&mut out[o..o + 4], unpack(c));
                    continue;
                }
                if c >= 256 {
                    continue;
                }
                let p = pal.get(c as usize).copied().unwrap_or([0, 0, 0]);
                out[o..o + 4].copy_from_slice(&[p[0], p[1], p[2], 255]);
            }
        }
    }

    /// Flattened RGBA image. `only` renders a single layer regardless of visibility.
    pub fn composite(&self, pal: &[[u8; 3]], only: Option<usize>, checker: bool) -> Vec<u8> {
        let (w, h) = (self.w as usize, self.h as usize);
        let mut out = vec![0u8; w * h * 4];
        if checker {
            for y in 0..h {
                for x in 0..w {
                    let v = if ((x >> 3) + (y >> 3)) & 1 == 0 { 0x4e } else { 0x5e };
                    let o = (y * w + x) * 4;
                    out[o..o + 4].copy_from_slice(&[v, v, v, 255]);
                }
            }
        }
        for (li, l) in self.layers.iter().enumerate() {
            match only {
                Some(o) if o != li => continue,
                None if !l.visible => continue,
                _ => {}
            }
            self.put_cells(&mut out, &l.data, 0, 0, w as i32, h as i32, pal);
            if li == self.active {
                if let Some(f) = &self.float {
                    self.put_cells(&mut out, &f.data, f.x, f.y, f.w as i32, f.h as i32, pal);
                }
            }
        }
        out
    }

    /// Flattened cells: the topmost visible non-transparent pixel wins.
    pub fn flat_cells(&self) -> Vec<Cell> {
        if self.rgba() {
            // Layers with part-transparent pixels blend, as they do on screen.
            let mut d = self.clone();
            d.float = None;
            return d.composite(&[], None, false).chunks_exact(4).map(|c| pack([c[0], c[1], c[2], c[3]])).collect();
        }
        let mut out = vec![TRANSP; self.len()];
        for l in self.layers.iter().filter(|l| l.visible) {
            for (o, &c) in out.iter_mut().zip(l.data.iter()) {
                if c < 256 {
                    *o = c;
                }
            }
        }
        out
    }

    pub fn sel_at(&self, x: i32, y: i32) -> bool {
        match &self.selection {
            None => true,
            Some(s) => x >= 0 && y >= 0 && x < self.w as i32 && y < self.h as i32 && s[(y * self.w as i32 + x) as usize] != 0,
        }
    }

    fn bounds_of(&self, f: impl Fn(usize) -> bool) -> Option<(i32, i32, i32, i32)> {
        let (w, h) = (self.w as i32, self.h as i32);
        let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, -1, -1);
        for y in 0..h {
            for x in 0..w {
                if f((y * w + x) as usize) {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
        }
        (x1 >= 0).then_some((x0, y0, x1, y1))
    }
    /// Inclusive bounds of the selection.
    pub fn sel_bounds(&self) -> Option<(i32, i32, i32, i32)> {
        let s = self.selection.as_ref()?;
        self.bounds_of(|i| s[i] != 0)
    }
    /// Inclusive bounds of a layer's non-empty pixels.
    pub fn layer_bounds(&self, li: usize) -> Option<(i32, i32, i32, i32)> {
        let d = &self.layers[li].data;
        self.bounds_of(|i| !cell_empty(d[i]))
    }

    /// Cut the selection (or the whole layer content) out of the active layer into a float.
    pub fn float_lift(&mut self) -> Result<(), String> {
        if self.float.is_some() {
            return Ok(());
        }
        let li = self.active;
        let (x0, y0, x1, y1) = match self.sel_bounds() {
            Some(b) => b,
            None if self.selection.is_some() => return Err("selection is empty".into()),
            None => self.layer_bounds(li).ok_or("nothing to move on this layer")?,
        };
        let (fw, fh) = ((x1 - x0 + 1) as usize, (y1 - y0 + 1) as usize);
        let empty = TRANSP;
        let w = self.w as usize;
        let old = self.layers[li].data.clone();
        let old_sel = self.selection.clone();
        let mut data = vec![empty; fw * fh];
        let mut sel = self.selection.as_ref().map(|_| vec![0u8; fw * fh]);
        let layer = Arc::make_mut(&mut self.layers[li].data);
        for fy in 0..fh {
            for fx in 0..fw {
                let i = (y0 as usize + fy) * w + x0 as usize + fx;
                if old_sel.as_ref().map(|s| s[i] != 0).unwrap_or(true) {
                    data[fy * fw + fx] = layer[i];
                    layer[i] = empty;
                    if let Some(s) = sel.as_mut() {
                        s[fy * fw + fx] = 1;
                    }
                }
            }
        }
        self.float = Some(Float {
            x: x0,
            y: y0,
            w: fw as u32,
            h: fh as u32,
            data: Arc::new(data),
            sel: sel.map(Arc::new),
            restore: Some((old, old_sel)),
        });
        self.selection = None;
        Ok(())
    }

    /// Stamp the float into the active layer. Returns true if there was one.
    pub fn float_commit(&mut self) -> bool {
        let Some(f) = self.float.take() else { return false };
        let (w, h) = (self.w as i32, self.h as i32);
        let a = self.active.min(self.layers.len() - 1);
        let data = Arc::make_mut(&mut self.layers[a].data);
        let mut sel = f.sel.as_ref().map(|_| vec![0u8; (w * h) as usize]);
        let mut any = false;
        for fy in 0..f.h as i32 {
            for fx in 0..f.w as i32 {
                let (x, y) = (f.x + fx, f.y + fy);
                if x < 0 || y < 0 || x >= w || y >= h {
                    continue;
                }
                let fi = (fy * f.w as i32 + fx) as usize;
                let c = f.data[fi];
                if !cell_empty(c) {
                    data[(y * w + x) as usize] = c;
                }
                if let (Some(s), Some(fs)) = (sel.as_mut(), f.sel.as_ref()) {
                    if fs[fi] != 0 {
                        s[(y * w + x) as usize] = 1;
                        any = true;
                    }
                }
            }
        }
        // Pasted pixels carry no selection shape and leave the current selection alone.
        if f.sel.is_some() {
            self.selection = if any { sel.map(Arc::new) } else { None };
        }
        true
    }

    /// Drop the float; a lifted float is put back where it came from.
    pub fn float_cancel(&mut self) -> bool {
        let Some(f) = self.float.take() else { return false };
        if let Some((data, sel)) = f.restore {
            let a = self.active.min(self.layers.len() - 1);
            self.layers[a].data = data;
            self.selection = sel;
        }
        true
    }
}
