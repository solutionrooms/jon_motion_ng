//! Native project files (.jmn) and PNG import/export.

use crate::doc::*;
use std::io::{Read, Write};
use std::path::Path;

const MAGIC: &[u8; 6] = b"JMNG\x01\n";

pub fn save_doc(doc: &Doc, path: &Path) -> Result<(), String> {
    let mut d = doc.clone();
    d.float_commit();
    let raw = bincode::serialize(&d).map_err(|e| e.to_string())?;
    let mut out = MAGIC.to_vec();
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
    if bytes.len() < MAGIC.len() || &bytes[..MAGIC.len()] != MAGIC {
        return Err(format!("{}: not a Jon Motion project file", path.display()));
    }
    let mut raw = Vec::new();
    flate2::read::DeflateDecoder::new(&bytes[MAGIC.len()..]).read_to_end(&mut raw).map_err(|e| e.to_string())?;
    let mut d: Doc = bincode::deserialize(&raw).map_err(|e| format!("corrupt project file: {e}"))?;
    validate(&mut d)?;
    Ok(d)
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
    match &d.tileset {
        Some(ts) => {
            let tn = (ts.tw * ts.th) as usize;
            if ts.tw == 0 || ts.th == 0 || ts.tiles.is_empty() || ts.tiles.iter().any(|t| t.len() != tn) {
                return Err("corrupt project file: bad tile set".into());
            }
        }
        None => d.layers.iter_mut().for_each(|l| l.map = None),
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

/// Encode cells in the document's colour mode: an indexed PNG when possible, else RGBA.
pub fn encode_cells(doc: &Doc, w: u32, h: u32, cells: &[Cell]) -> Result<Vec<u8>, String> {
    if doc.mode == ColorMode::Indexed {
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
            used.iter().position(|u| !u).map(|i| i as u8)
        };
        if !has_transp || t.is_some() {
            let idx: Vec<u8> = cells.iter().map(|&c| if c >= 256 { t.unwrap_or(0) } else { c as u8 }).collect();
            return encode_png_indexed(w, h, &idx, &pal, t);
        }
    }
    let rgba: Vec<u8> = cells.iter().flat_map(|&c| doc.to_rgba(c)).collect();
    encode_png_rgba(w, h, &rgba)
}

/// Encode the document (or one layer). Indexed documents stay indexed when nothing forces RGBA.
pub fn export_png(doc: &Doc, layer: Option<usize>, scale: u32) -> Result<Vec<u8>, String> {
    let scale = scale.clamp(1, 64);
    let mut d = doc.clone();
    d.float_commit();
    let plain = d.layers.iter().all(|l| !l.visible || l.opacity == 255);
    if d.mode == ColorMode::Indexed && scale == 1 && (plain || layer.is_some()) {
        let cells = match layer {
            Some(li) => d.layers[li].data.to_vec(),
            None => d.flat_cells(),
        };
        return encode_cells(&d, d.w, d.h, &cells);
    }
    let rgba = d.composite(&d.palette.colors, layer, false);
    let big = upscale(&rgba, d.w as usize, d.h as usize, scale as usize);
    encode_png_rgba(d.w * scale, d.h * scale, &big)
}
