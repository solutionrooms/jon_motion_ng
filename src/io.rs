//! Native project files (.jmn) and PNG import/export.

use crate::doc::*;
use crate::quant;
use serde_json::{json, Value};
use std::sync::Arc;
use std::io::{Read, Write};
use std::path::Path;

const MAGIC: &[u8; 6] = b"JMNG\x01\n";
/// True-colour projects start differently, so builds that only know indexed documents
/// (0.1.3 and 0.1.4, which would turn the pixels into a 256-colour palette) refuse the
/// file instead of converting it. Everything after the first six bytes has the same layout.
const MAGIC_RGBA: &[u8; 6] = b"JMNG\x02\n";
const MAX_EXPORT_PIXELS: u64 = 8192 * 8192;
/// Largest uncompressed project accepted, so a small crafted file cannot inflate without bound.
const MAX_RAW: u64 = 4 << 30;

pub fn save_doc(doc: &Doc, path: &Path) -> Result<(), String> {
    let mut d = doc.clone();
    d.float_commit();
    let mut raw = bincode::serialize(&d).map_err(|e| e.to_string())?;
    if !d.anims.is_empty() {
        // 0.1.4 knows one grid animation, in this chunk; it is given the first one there is.
        // (It cannot open a true-colour project at all, so those do not carry the chunk.)
        if let Some((a, g)) = d.anims.iter().find_map(|a| a.grid().map(|g| (a, g))).filter(|_| !d.rgba()) {
            let old = AnimV1 { x: g.x, y: g.y, w: g.w, h: g.h, fw: g.fw, fh: g.fh, limit: g.limit, fps: a.fps };
            chunk(&mut raw, b"ANIM", &bincode::serialize(&old).map_err(|e| e.to_string())?);
        }
        chunk(&mut raw, b"ANMS", &serde_json::to_vec(&anims_to_json(&d)).map_err(|e| e.to_string())?);
    }
    if raw.len() as u64 > MAX_RAW {
        return Err("the project is too large to save (over 4 GB of pixel data); merge or remove layers".into());
    }
    let mut out = if d.rgba() { MAGIC_RGBA.to_vec() } else { MAGIC.to_vec() };
    let mut enc = flate2::write::DeflateEncoder::new(&mut out, flate2::Compression::default());
    enc.write_all(&raw).map_err(|e| e.to_string())?;
    enc.finish().map_err(|e| e.to_string())?;
    // Write beside the target then rename, so a failed save never truncates the file.
    let tmp = path.with_extension("jmn.tmp");
    std::fs::write(&tmp, &out).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn load_doc(path: &Path) -> Result<Doc, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let head = bytes.get(..MAGIC.len());
    let rgba = head == Some(MAGIC_RGBA);
    if !rgba && head != Some(MAGIC) {
        return Err(format!("{}: not a Jon Motion project file", path.display()));
    }
    let mut raw = Vec::new();
    flate2::read::DeflateDecoder::new(&bytes[MAGIC.len()..]).take(MAX_RAW + 1).read_to_end(&mut raw).map_err(|e| e.to_string())?;
    if raw.len() as u64 > MAX_RAW {
        return Err(format!("{}: project file is too large", path.display()));
    }
    let mut d: Doc = bincode::deserialize(&raw).map_err(|e| format!("corrupt project file: {e}"))?;
    // The document re-serialises to exactly the bytes it was read from, which locates the chunks after it.
    let used = bincode::serialized_size(&d).map_err(|e| e.to_string())? as usize;
    read_chunks(&mut d, raw.get(used..).unwrap_or(&[]));
    validate(&mut d)?;
    if rgba != d.rgba() && rgba {
        return Err("corrupt project file: a true-colour file holding an indexed document".into());
    }
    if !rgba && d.rgba() {
        // A true-colour project from 0.1.0-0.1.2, which is opened as an indexed document.
        to_indexed(&mut d);
    }
    tidy_cells(&mut d);
    Ok(d)
}

/// Data added after 0.1.3 follows the document as tagged chunks: a four-byte tag, a
/// little-endian u32 length, then the payload. The document's own layout is unchanged, so
/// older builds still open these files (they ignore the chunks and drop them when they save),
/// and a build that does not know a tag skips it.
fn chunk(out: &mut Vec<u8>, tag: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(tag);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
}

/// The one animation 0.1.4 stores, in its "ANIM" chunk.
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct AnimV1 {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    fw: u32,
    fh: u32,
    limit: u32,
    fps: f32,
}

/// "ANMS" holds every animation, as the same JSON that anim_export_json writes; "ANIM" is
/// 0.1.4's single animation, used when there is no "ANMS" (a file 0.1.4 saved).
fn read_chunks(d: &mut Doc, mut rest: &[u8]) {
    let (mut v1, mut all): (Option<AnimV1>, Option<Vec<Anim>>) = (None, None);
    while rest.len() >= 8 {
        let len = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]) as usize;
        let Some(payload) = rest[8..].get(..len) else { break };
        match &rest[..4] {
            b"ANIM" => {
                if let Ok(a) = bincode::deserialize(payload) {
                    v1 = Some(a);
                }
            }
            // A list larger than this program ever writes is not parsed at all.
            b"ANMS" if payload.len() as u64 <= ANIM_JSON_MAX => {
                // Entries that are not sound for this image are left out; the rest are kept.
                if let Some(list) = serde_json::from_slice::<Value>(payload).ok().and_then(|v| anims_from_json(&v, d.w, d.h).ok()) {
                    let mut kept: Vec<Anim> = vec![];
                    for a in list.into_iter().flatten() {
                        if !kept.iter().any(|k| k.name == a.name) {
                            kept.push(a);
                        }
                    }
                    all = Some(kept);
                }
            }
            _ => {}
        }
        rest = &rest[8 + len..];
    }
    d.anims = match (all, v1) {
        (Some(list), v1) if !list.is_empty() || v1.is_none() => list,
        (_, Some(a)) => {
            let mut g = Grid { x: a.x, y: a.y, w: a.w, h: a.h, fw: a.fw, fh: a.fh, limit: a.limit };
            if g.limit >= g.total() {
                g.limit = 0;
            }
            vec![Anim { name: "Animation".into(), fps: if Anim::FPS.contains(&a.fps) { a.fps } else { 8.0 }, frames: Frames::Grid(g) }]
        }
        (_, None) => vec![],
    };
}

// ------------------------------------------------------------ animations as JSON

pub const ANIM_FORMAT: &str = "jon-motion-animations";
/// Largest animations file read, and largest stored list parsed. Everything this program
/// writes is far smaller (see `Anim::MAX_LIST_FRAMES`).
pub const ANIM_JSON_MAX: u64 = 8 << 20;

/// A frame as [x, y, w, h], or [x, y, w, h, offset_x, offset_y] when it has an offset.
pub fn frame_to_json(f: &Frame) -> Value {
    if f.ox == 0 && f.oy == 0 {
        json!([f.x, f.y, f.w, f.h])
    } else {
        json!([f.x, f.y, f.w, f.h, f.ox, f.oy])
    }
}

/// The speed as the number the user sees (8.3, not 8.300000190734863).
pub fn fps_json(fps: f32) -> Value {
    json!(fps.to_string().parse::<f64>().unwrap_or(fps as f64))
}

pub fn anim_to_json(a: &Anim) -> Value {
    match &a.frames {
        Frames::Grid(g) => json!({"name": a.name, "fps": fps_json(a.fps),
            "grid": {"x": g.x, "y": g.y, "w": g.w, "h": g.h, "frame_w": g.fw, "frame_h": g.fh, "frames": g.limit}}),
        Frames::List(l) => json!({"name": a.name, "fps": fps_json(a.fps), "frames": l.iter().map(frame_to_json).collect::<Vec<_>>()}),
    }
}

/// The animations as a file for people to read: one animation per block, one frame per line.
pub fn anims_to_text(d: &Doc) -> String {
    let q = |s: &str| serde_json::to_string(s).unwrap_or_default();
    let mut out = format!("{{\n  \"format\": {},\n  \"version\": 1,\n  \"image\": {{ \"width\": {}, \"height\": {} }},\n  \"animations\": [\n", q(ANIM_FORMAT), d.w, d.h);
    for (i, a) in d.anims.iter().enumerate() {
        out += &format!("    {{ \"name\": {}, \"fps\": {}, ", q(&a.name), a.fps);
        match &a.frames {
            Frames::Grid(g) => {
                out += &format!("\"grid\": {{ \"x\": {}, \"y\": {}, \"w\": {}, \"h\": {}, \"frame_w\": {}, \"frame_h\": {}, \"frames\": {} }} }}", g.x, g.y, g.w, g.h, g.fw, g.fh, g.limit)
            }
            Frames::List(l) => {
                let rows: Vec<String> = l.iter().map(|f| format!("      {}", frame_to_json(f).to_string().replace(',', ", "))).collect();
                out += &format!("\"frames\": [\n{}\n    ] }}", rows.join(",\n"));
            }
        }
        out += if i + 1 < d.anims.len() { ",\n" } else { "\n" };
    }
    out + "  ]\n}\n"
}

/// Every animation of the document: what anim_export_json writes and the project file stores.
pub fn anims_to_json(d: &Doc) -> Value {
    json!({"format": ANIM_FORMAT, "version": 1, "image": {"width": d.w, "height": d.h}, "animations": d.anims.iter().map(anim_to_json).collect::<Vec<_>>()})
}

/// A whole number in a range; anything else (text, a fraction, out of range) is an error.
fn whole(v: Option<&Value>, what: &str, lo: i64, hi: i64) -> Result<i64, String> {
    let n = v.and_then(|v| v.as_f64()).ok_or_else(|| format!("{what} must be a number"))?;
    if n.fract() != 0.0 || n < lo as f64 || n > hi as f64 {
        return Err(format!("{what} must be a whole number from {lo} to {hi}"));
    }
    Ok(n as i64)
}

/// One frame, as {"x","y","w","h"} with optional "offset_x"/"offset_y", or as
/// [x, y, w, h] / [x, y, w, h, offset_x, offset_y]. It must lie inside the image.
pub fn frame_from_json(v: &Value, w: u32, h: u32) -> Result<Frame, String> {
    let m = MAX_DIM as i64;
    let (x, y, fw, fh, ox, oy) = match v {
        Value::Array(a) if a.len() == 4 || a.len() == 6 => (a.first(), a.get(1), a.get(2), a.get(3), a.get(4), a.get(5)),
        Value::Object(o) => (o.get("x"), o.get("y"), o.get("w"), o.get("h"), o.get("offset_x"), o.get("offset_y")),
        _ => return Err("a frame must be [x, y, w, h], [x, y, w, h, offset_x, offset_y] or an object with x, y, w, h".into()),
    };
    let off = |v: Option<&Value>, what| if v.is_none_or(|v| v.is_null()) { Ok(0) } else { whole(v, what, -m, m) };
    let f = Frame {
        x: whole(x, "x", 0, m)? as u32,
        y: whole(y, "y", 0, m)? as u32,
        w: whole(fw, "w", 1, m)? as u32,
        h: whole(fh, "h", 1, m)? as u32,
        ox: off(ox, "offset_x")? as i32,
        oy: off(oy, "offset_y")? as i32,
    };
    if !f.fits(w, h) {
        return Err(format!("the frame at {},{} ({}x{}) is not inside the {w}x{h} image", f.x, f.y, f.w, f.h));
    }
    Ok(f)
}

pub fn frames_from_json(list: &[Value], w: u32, h: u32) -> Result<Frames, String> {
    if list.is_empty() || list.len() > Anim::MAX_FRAMES {
        return Err(format!("an animation has 1 to {} frames (this one has {})", Anim::MAX_FRAMES, list.len()));
    }
    let mut out = Vec::with_capacity(list.len());
    for (i, v) in list.iter().enumerate() {
        out.push(frame_from_json(v, w, h).map_err(|e| format!("frame {i}: {e}"))?);
    }
    Ok(Frames::List(Arc::new(out)))
}

/// One animation from JSON, checked against the image it is for.
pub fn anim_from_json(v: &Value, w: u32, h: u32) -> Result<Anim, String> {
    let o = v.as_object().ok_or("an animation must be an object")?;
    let name = o.get("name").and_then(|n| n.as_str()).ok_or("an animation needs a \"name\"")?;
    if !Anim::name_ok(name) {
        return Err(format!("a name must be 1 to {} characters, with no control characters or spaces at the ends", Anim::NAME_MAX));
    }
    let fps = match o.get("fps").filter(|v| !v.is_null()) {
        None => 8.0,
        Some(v) => v.as_f64().map(|f| f as f32).filter(|f| Anim::FPS.contains(f)).ok_or("fps must be a number from 1 to 60")?,
    };
    let frames = match (o.get("grid").filter(|v| !v.is_null()), o.get("frames").filter(|v| !v.is_null())) {
        (Some(g), None) => {
            let m = MAX_DIM as i64;
            let n = |k: &str, lo: i64| whole(g.get(k), k, lo, m).map(|v| v as u32);
            let mut grid = Grid { x: n("x", 0)?, y: n("y", 0)?, w: n("w", 1)?, h: n("h", 1)?, fw: n("frame_w", 1)?, fh: n("frame_h", 1)?, limit: 0 };
            if !grid.fits(w, h) {
                return Err(format!("the {}x{} region at {},{} with {}x{} frames does not fit the {w}x{h} image", grid.w, grid.h, grid.x, grid.y, grid.fw, grid.fh));
            }
            if g.get("frames").is_some_and(|v| !v.is_null()) {
                let limit = whole(g.get("frames"), "frames", 0, u32::MAX as i64)? as u32;
                grid.limit = if limit >= grid.total() { 0 } else { limit };
            }
            Frames::Grid(grid)
        }
        (None, Some(Value::Array(list))) => frames_from_json(list, w, h)?,
        _ => return Err("an animation needs either a \"grid\" object or a \"frames\" list".into()),
    };
    Ok(Anim { name: name.to_string(), fps, frames })
}

/// The animations in a file: each entry parsed, or why it was refused (named where it can be).
pub fn anims_from_json(v: &Value, w: u32, h: u32) -> Result<Vec<Result<Anim, String>>, String> {
    if v.get("format").and_then(|f| f.as_str()) != Some(ANIM_FORMAT) {
        return Err(format!("not a Jon Motion animations file (it needs \"format\": \"{ANIM_FORMAT}\")"));
    }
    let list = v.get("animations").and_then(|a| a.as_array()).ok_or("the file has no \"animations\" list")?;
    if list.len() > Anim::MAX {
        return Err(format!("the file has {} animations; the limit is {}", list.len(), Anim::MAX));
    }
    Ok(list
        .iter()
        .enumerate()
        .map(|(i, a)| {
            anim_from_json(a, w, h).map_err(|e| match a.get("name").and_then(|n| n.as_str()).filter(|n| Anim::name_ok(n)) {
                Some(n) => format!("animation \"{n}\": {e}"),
                None => format!("animation {i}: {e}"),
            })
        })
        .collect())
}

/// Convert a true-colour project written by 0.1.x: its cells are packed RGBA
/// (little-endian r, g, b, a). It gets a generated palette of up to 256 colours.
fn to_indexed(d: &mut Doc) {
    let px = |cells: &[Cell]| -> Vec<[u8; 4]> { cells.iter().map(|c| c.to_le_bytes()).collect() };
    let layers: Vec<Vec<[u8; 4]>> = d.layers.iter().map(|l| px(&l.data)).collect();
    let tiles: Vec<Vec<[u8; 4]>> = d.tileset.iter().flat_map(|t| t.tiles.iter()).map(|t| px(t)).collect();
    d.palette = quant::palette_for(layers.iter().chain(&tiles).flatten(), 256);
    let (w, h) = (d.w as usize, d.h as usize);
    for (l, p) in d.layers.iter_mut().zip(&layers) {
        l.data = Arc::new(quant::to_indexed(p, w, h, &d.palette, quant::Dither::None));
    }
    if let Some(ts) = &mut d.tileset {
        let (tw, th) = (ts.tw as usize, ts.th as usize);
        for (t, p) in ts.tiles.iter_mut().zip(&tiles) {
            *t = Arc::new(quant::to_indexed(p, tw, th, &d.palette, quant::Dither::None));
        }
    }
    d.mode = ColorMode::Indexed;
}

/// Make every pixel a value its kind of document can hold: an index or `TRANSP` in an
/// indexed one (anything else was always read as transparent), a colour or `TRANSP` in a
/// true-colour one. A sound file is left exactly as it is.
fn tidy_cells(d: &mut Doc) {
    let rgba = d.rgba();
    let bad = move |c: Cell| if rgba { c < 1 << 24 && c != TRANSP } else { c > TRANSP };
    let fix = |data: &mut Arc<Vec<Cell>>| {
        if data.iter().any(|&c| bad(c)) {
            Arc::make_mut(data).iter_mut().filter(|c| bad(**c)).for_each(|c| *c = TRANSP);
        }
    };
    d.layers.iter_mut().for_each(|l| fix(&mut l.data));
    if let Some(ts) = &mut d.tileset {
        ts.tiles.iter_mut().for_each(fix);
    }
}

fn validate(d: &mut Doc) -> Result<(), String> {
    if d.w == 0 || d.h == 0 || d.w > MAX_DIM || d.h > MAX_DIM || d.layers.is_empty() {
        return Err("corrupt project file: bad dimensions".into());
    }
    let n = d.len();
    if d.layers.iter().any(|l| l.data.len() != n) || d.selection.as_ref().is_some_and(|s| s.len() != n) {
        return Err("corrupt project file: layer size mismatch".into());
    }
    if d.palette.colors.is_empty() || d.palette.colors.len() > 256 {
        return Err("corrupt project file: bad palette".into());
    }
    d.active = d.active.min(d.layers.len() - 1);
    d.layers.iter_mut().for_each(|l| l.opacity = 255);
    let colors = d.palette.colors.len();
    for r in &mut d.palette.ramps {
        r.indices.retain(|&i| (i as usize) < colors);
    }
    match &d.tileset {
        Some(ts) => {
            let tn = (ts.tw * ts.th) as usize;
            if ts.tw == 0 || ts.th == 0 || ts.tiles.is_empty() || ts.tiles.iter().any(|t| t.len() != tn) {
                return Err("corrupt project file: bad tile set".into());
            }
        }
        None => d.layers.iter_mut().for_each(|l| l.map = None),
    }
    // The animations are as untrusted as the rest of the file.
    let (w, h) = (d.w, d.h);
    d.anims.retain(|a| a.valid(w, h));
    d.anims.truncate(Anim::MAX);
    while Anim::list_frames(&d.anims) > Anim::MAX_LIST_FRAMES {
        d.anims.pop();
    }
    Ok(())
}

pub enum PngPixels {
    Indexed { data: Vec<u8>, palette: Vec<[u8; 3]>, trns: Vec<u8> },
    Rgba(Vec<[u8; 4]>),
}

pub struct PngImage {
    pub w: u32,
    pub h: u32,
    pub pixels: PngPixels,
}

pub fn load_png(path: &Path) -> Result<PngImage, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    decode_png(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn decode_png(bytes: &[u8]) -> Result<PngImage, String> {
    let es = |e: png::DecodingError| e.to_string();
    let mut reader = png::Decoder::new(bytes).read_info().map_err(es)?;
    let (w, h) = (reader.info().width, reader.info().height);
    if w == 0 || h == 0 || w > MAX_DIM || h > MAX_DIM {
        return Err(format!("image is {w}x{h}; the limit is {MAX_DIM}x{MAX_DIM}"));
    }
    if reader.info().color_type == png::ColorType::Indexed {
        let palette: Vec<[u8; 3]> =
            reader.info().palette.as_ref().map(|p| p.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect()).unwrap_or_default();
        let trns = reader.info().trns.as_ref().map(|t| t.to_vec()).unwrap_or_default();
        let bits = reader.info().bit_depth as usize;
        let mut buf = vec![0; reader.output_buffer_size()];
        let frame = reader.next_frame(&mut buf).map_err(es)?;
        let mut data = Vec::with_capacity((w * h) as usize);
        for row in buf[..frame.buffer_size()].chunks(frame.line_size) {
            for x in 0..w as usize {
                data.push(match bits {
                    8 => row[x],
                    b => (row[x * b / 8] >> (8 - b - (x * b) % 8)) & ((1 << b) - 1),
                });
            }
        }
        if !palette.is_empty() {
            return Ok(PngImage { w, h, pixels: PngPixels::Indexed { data, palette, trns } });
        }
    }
    let mut dec = png::Decoder::new(bytes);
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().map_err(es)?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut buf).map_err(es)?;
    let buf = &buf[..frame.buffer_size()];
    let px: Vec<[u8; 4]> = match frame.color_type {
        png::ColorType::Rgba => buf.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect(),
        png::ColorType::Rgb => buf.chunks_exact(3).map(|c| [c[0], c[1], c[2], 255]).collect(),
        png::ColorType::Grayscale => buf.iter().map(|&v| [v, v, v, 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf.chunks_exact(2).map(|c| [c[0], c[0], c[0], c[1]]).collect(),
        png::ColorType::Indexed => return Err("indexed PNG without a palette".into()),
    };
    Ok(PngImage { w, h, pixels: PngPixels::Rgba(px) })
}

pub fn encode_png_rgba(w: u32, h: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().map_err(|e| e.to_string())?;
        wr.write_image_data(rgba).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

pub fn encode_png_indexed(w: u32, h: u32, idx: &[u8], pal: &[[u8; 3]], transparent: Option<u8>) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Indexed);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_palette(pal.iter().flatten().copied().collect::<Vec<u8>>());
        if let Some(t) = transparent {
            let mut trns = vec![255u8; t as usize + 1];
            trns[t as usize] = 0;
            enc.set_trns(trns);
        }
        let mut wr = enc.write_header().map_err(|e| e.to_string())?;
        wr.write_image_data(idx).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// Nearest-neighbour integer upscale of an RGBA buffer.
pub fn upscale(rgba: &[u8], w: usize, h: usize, s: usize) -> Vec<u8> {
    if s <= 1 {
        return rgba.to_vec();
    }
    let mut out = vec![0u8; w * s * h * s * 4];
    for y in 0..h * s {
        for x in 0..w * s {
            let (si, di) = (((y / s) * w + x / s) * 4, (y * w * s + x) * 4);
            out[di..di + 4].copy_from_slice(&rgba[si..si + 4]);
        }
    }
    out
}

/// Nearest-neighbour integer upscale of cells.
fn upscale_cells(src: &[Cell], w: usize, h: usize, s: usize) -> Vec<Cell> {
    if s <= 1 {
        return src.to_vec();
    }
    let mut out = Vec::with_capacity(w * s * h * s);
    for y in 0..h * s {
        let row = &src[(y / s) * w..(y / s + 1) * w];
        out.extend(row.iter().flat_map(|&c| std::iter::repeat_n(c, s)));
    }
    out
}

/// Encode cells as a PNG: indexed, carrying the document's palette, or for a true-colour
/// document RGBA with every pixel exactly as it is.
pub fn encode_cells(doc: &Doc, w: u32, h: u32, cells: &[Cell]) -> Result<Vec<u8>, String> {
    if doc.rgba() {
        let rgba: Vec<u8> = cells.iter().flat_map(|&c| unpack(c)).collect();
        return encode_png_rgba(w, h, &rgba);
    }
    let has_transp = cells.iter().any(|&c| c >= 256);
    let mut pal = doc.palette.colors.clone();
    // Transparency needs a palette slot: a spare entry, or failing that one the image never uses.
    let t = if !has_transp {
        None
    } else if pal.len() < 256 {
        pal.push([0, 0, 0]);
        Some((pal.len() - 1) as u8)
    } else {
        let mut used = [false; 256];
        cells.iter().filter(|&&c| c < 256).for_each(|&c| used[c as usize] = true);
        match used.iter().position(|u| !u) {
            Some(i) => Some(i as u8),
            None => {
                return Err("this image uses all 256 palette colours and also has transparent pixels, which an indexed PNG \
                            cannot hold; fill the transparent areas or free one colour (Palette > remove unused / reduce colours)"
                    .into())
            }
        }
    };
    let idx: Vec<u8> = cells.iter().map(|&c| if c >= 256 { t.unwrap_or(0) } else { c as u8 }).collect();
    encode_png_indexed(w, h, &idx, &pal, t)
}

/// Encode the document (or one layer) as a PNG: indexed, or RGBA for a true-colour document.
pub fn export_png(doc: &Doc, layer: Option<usize>, scale: u32) -> Result<Vec<u8>, String> {
    let scale = scale.clamp(1, 64);
    let (ow, oh) = (doc.w as u64 * scale as u64, doc.h as u64 * scale as u64);
    if ow * oh > MAX_EXPORT_PIXELS {
        return Err(format!("scale {scale} would make a {ow}x{oh} image; use a smaller scale (the limit is 8192x8192 pixels in area)"));
    }
    let mut d = doc.clone();
    d.float_commit();
    let cells = match layer {
        Some(li) => d.layers[li].data.to_vec(),
        None => d.flat_cells(),
    };
    let big = upscale_cells(&cells, d.w as usize, d.h as usize, scale as usize);
    encode_cells(&d, d.w * scale, d.h * scale, &big)
}
