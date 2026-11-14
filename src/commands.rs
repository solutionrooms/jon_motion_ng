//! The command registry. Every feature of the editor is a named command with
//! JSON parameters; the GUI, Lua and MCP all call these.

use crate::doc::*;
use crate::engine::*;
use crate::io;
use crate::paint::*;
use crate::quant::{self, Dither};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

pub type R = Result<Value, String>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Does not change the document (may change editor state).
    Read,
    /// Paints pixels on a layer; honours the tile sync mode.
    Paint,
    /// Any other document change.
    Edit,
    /// Changes which documents are open or which one is active. Runs between commands only:
    /// never inside a script or batch.
    Tab,
}

pub struct Param {
    pub name: String,
    pub ty: String,
    pub req: bool,
    pub desc: String,
    pub common: bool,
}

pub struct Cmd {
    pub name: &'static str,
    pub cat: &'static str,
    pub desc: &'static str,
    pub params: Vec<Param>,
    pub kind: Kind,
    pub float_ok: bool,
    pub run: fn(&mut Engine, &Args) -> R,
}

pub struct Args<'a>(pub &'a Map<String, Value>);

impl<'a> Args<'a> {
    pub fn get(&self, k: &str) -> Option<&'a Value> {
        self.0.get(k).filter(|v| !v.is_null())
    }
    pub fn of(&self, k: &str) -> Result<Option<f64>, String> {
        match self.get(k) {
            None => Ok(None),
            Some(v) => v.as_f64().map(Some).ok_or_else(|| format!("'{k}' must be a number")),
        }
    }
    pub fn oi(&self, k: &str) -> Result<Option<i64>, String> {
        // Clamped so arithmetic on the result can never overflow.
        Ok(self.of(k)?.map(|v| (v.floor() as i64).clamp(-(1 << 40), 1 << 40)))
    }
    /// A pixel coordinate or offset, clamped to a range far larger than any canvas.
    pub fn c(&self, k: &str) -> Result<i32, String> {
        Ok(self.i(k)?.clamp(-COORD as i64, COORD as i64) as i32)
    }
    pub fn oc(&self, k: &str) -> Result<Option<i32>, String> {
        Ok(self.oi(k)?.map(|v| v.clamp(-COORD as i64, COORD as i64) as i32))
    }
    /// A width or height for a shape.
    pub fn len(&self, k: &str) -> Result<i32, String> {
        Ok(self.i(k)?.clamp(1, 2 * COORD as i64) as i32)
    }
    pub fn i(&self, k: &str) -> Result<i64, String> {
        self.oi(k)?.ok_or_else(|| format!("missing '{k}'"))
    }
    pub fn os(&self, k: &str) -> Result<Option<&'a str>, String> {
        match self.get(k) {
            None => Ok(None),
            Some(v) => v.as_str().map(Some).ok_or_else(|| format!("'{k}' must be a string")),
        }
    }
    pub fn s(&self, k: &str) -> Result<&'a str, String> {
        self.os(k)?.ok_or_else(|| format!("missing '{k}'"))
    }
    pub fn ob(&self, k: &str) -> Result<Option<bool>, String> {
        match self.get(k) {
            None => Ok(None),
            Some(v) => v.as_bool().map(Some).ok_or_else(|| format!("'{k}' must be true or false")),
        }
    }
    pub fn b(&self, k: &str) -> Result<bool, String> {
        Ok(self.ob(k)?.unwrap_or(false))
    }
    pub fn arr(&self, k: &str) -> Result<&'a Vec<Value>, String> {
        self.get(k).and_then(|v| v.as_array()).ok_or_else(|| format!("'{k}' must be an array"))
    }
}

impl Cmd {
    pub fn validate(&self, m: &Map<String, Value>) -> Result<(), String> {
        for k in m.keys() {
            if !self.params.iter().any(|p| p.name == *k) {
                let known: Vec<&str> = self.params.iter().map(|p| p.name.as_str()).collect();
                return Err(format!("{}: unknown parameter '{k}' (accepts: {})", self.name, known.join(", ")));
            }
        }
        for p in &self.params {
            if p.req && m.get(&p.name).map(|v| v.is_null()).unwrap_or(true) {
                return Err(format!("{}: missing required parameter '{}' ({})", self.name, p.name, p.ty));
            }
        }
        Ok(())
    }

    pub fn signature(&self, full: bool) -> String {
        let mut parts: Vec<String> =
            self.params.iter().filter(|p| full || !p.common).map(|p| format!("{}{}:{}", p.name, if p.req { "" } else { "?" }, p.ty)).collect();
        if !full && self.params.iter().any(|p| p.common) {
            parts.push("+paint".into());
        }
        format!("{}({})", self.name, parts.join(", "))
    }

    pub fn schema(&self) -> Value {
        let mut props = Map::new();
        for p in &self.params {
            let ty = match p.ty.as_str() {
                "int" => json!("integer"),
                "num" => json!("number"),
                "bool" => json!("boolean"),
                "str" => json!("string"),
                "array" => json!("array"),
                "color" => json!(["integer", "string", "array"]),
                "layer" => json!(["integer", "string"]),
                _ => Value::Null,
            };
            let mut s = Map::new();
            if !ty.is_null() {
                s.insert("type".into(), ty);
            }
            s.insert("description".into(), json!(p.desc));
            props.insert(p.name.clone(), Value::Object(s));
        }
        json!({
            "name": self.name, "category": self.cat, "description": self.desc,
            "changes_document": matches!(self.kind, Kind::Paint | Kind::Edit),
            "params": { "type": "object", "properties": props,
                        "required": self.params.iter().filter(|p| p.req).map(|p| p.name.clone()).collect::<Vec<_>>() }
        })
    }
}

fn parse_params(spec: &str, common: bool) -> Vec<Param> {
    spec.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            let (head, desc) = s.split_once(' ').unwrap_or((s, ""));
            let (name, ty) = head.split_once(':').unwrap_or((head, "any"));
            Param { name: name.into(), ty: ty.trim_end_matches('?').into(), req: !ty.ends_with('?'), desc: desc.trim().into(), common }
        })
        .collect()
}

const PAINT: &str = "color:color? Colour (default: current foreground); \
    size:int? Brush diameter in pixels (default 1); \
    shape:str? Brush shape: round or square (default square); \
    brush:str? 'custom' to stamp the grabbed custom brush; \
    mode:str? Paint mode: normal, dither, lighten, darken, smooth, smear, replace, behind (default normal); \
    dither:int? Dither level 1-15 of 16 for mode=dither (default 8); \
    dither_bg:bool? In dither mode also paint the background colour in the gaps; \
    bg:color? Background colour used by replace and dither modes (default: current background); \
    symmetry:str? Mirror about the canvas centre: x, y or xy; \
    layer:layer? Layer index (0 = bottom) or name (default: active layer)";

struct B(Vec<Cmd>);

impl B {
    fn add(&mut self, cat: &'static str, name: &'static str, kind: Kind, params: &str, desc: &'static str, run: fn(&mut Engine, &Args) -> R) {
        self.0.push(Cmd { name, cat, desc, params: parse_params(params, false), kind, float_ok: false, run });
    }
    fn paint(&mut self, name: &'static str, params: &str, desc: &'static str, run: fn(&mut Engine, &Args) -> R) {
        let mut p = parse_params(params, false);
        p.extend(parse_params(PAINT, true));
        self.0.push(Cmd { name, cat: "draw", desc, params: p, kind: Kind::Paint, float_ok: false, run });
    }
}

pub fn registry() -> &'static Vec<Cmd> {
    static REG: OnceLock<Vec<Cmd>> = OnceLock::new();
    REG.get_or_init(|| {
        let mut b = B(vec![]);
        document_cmds(&mut b);
        layer_cmds(&mut b);
        draw_cmds(&mut b);
        selection_cmds(&mut b);
        palette_cmds(&mut b);
        tile_cmds(&mut b);
        editor_cmds(&mut b);
        animation_cmds(&mut b);
        tab_cmds(&mut b);
        for c in &mut b.0 {
            c.float_ok = matches!(c.name, "float_move" | "float_commit" | "float_cancel" | "float_lift" | "flip" | "anim_set" | "anim_clear" | "anim_frames" | "anim_rename" | "anim_import_json");
        }
        b.0
    })
}

pub fn find(name: &str) -> Option<&'static Cmd> {
    static IDX: OnceLock<HashMap<&'static str, usize>> = OnceLock::new();
    let idx = IDX.get_or_init(|| registry().iter().enumerate().map(|(i, c)| (c.name, i)).collect());
    idx.get(name).map(|&i| &registry()[i])
}

pub const CATEGORIES: &[&str] = &["document", "layer", "draw", "selection", "palette", "tiles", "animation", "editor", "tabs"];

/// Human/AI readable command reference.
pub fn reference(cat: Option<&str>, search: Option<&str>) -> String {
    let mut out = String::new();
    let needle = search.map(|s| s.to_ascii_lowercase());
    for c in CATEGORIES {
        if cat.is_some_and(|x| x != *c) {
            continue;
        }
        let cmds: Vec<&Cmd> = registry()
            .iter()
            .filter(|k| k.cat == *c)
            .filter(|k| needle.as_ref().is_none_or(|n| k.name.contains(n.as_str()) || k.desc.to_ascii_lowercase().contains(n.as_str())))
            .collect();
        if cmds.is_empty() {
            continue;
        }
        out.push_str(&format!("\n## {c}\n"));
        if *c == "draw" {
            out.push_str("+paint = optional paint parameters shared by drawing commands:\n");
            for p in parse_params(PAINT, true) {
                out.push_str(&format!("    {}:{} - {}\n", p.name, p.ty, p.desc));
            }
        }
        for k in cmds {
            out.push_str(&format!("{} - {}\n", k.signature(false), k.desc));
            for p in k.params.iter().filter(|p| !p.common && !p.desc.is_empty()) {
                out.push_str(&format!("    {}: {}\n", p.name, p.desc));
            }
        }
    }
    if out.is_empty() {
        out.push_str("no matching commands");
    }
    out
}

// ------------------------------------------------------------------ helpers

fn ok() -> R {
    Ok(Value::Null)
}

fn dim(v: i64) -> Result<u32, String> {
    if (1..=MAX_DIM as i64).contains(&v) {
        Ok(v as u32)
    } else {
        Err(format!("size {v} out of range (1..{MAX_DIM})"))
    }
}

pub fn layer_arg(e: &Engine, a: &Args) -> Result<usize, String> {
    match a.get("layer") {
        None => Ok(e.doc.active),
        Some(Value::String(s)) => e.doc.layers.iter().position(|l| l.name == *s).ok_or_else(|| format!("no layer named '{s}'")),
        Some(v) => {
            let i = v.as_f64().ok_or("'layer' must be an index or a name")? as i64;
            if i < 0 || i as usize >= e.doc.layers.len() {
                return Err(format!("layer index {i} out of range (document has {} layers)", e.doc.layers.len()));
            }
            Ok(i as usize)
        }
    }
}

fn layer_data(e: &mut Engine, li: usize) -> Result<&mut Vec<Cell>, String> {
    let l = &mut e.doc.layers[li];
    if l.locked {
        return Err(format!("layer '{}' is locked", l.name));
    }
    Ok(Arc::make_mut(&mut l.data))
}

/// Destination for a save or export. The extension must match the format, so
/// scripts and AI clients can only ever write .jmn, .png, .csv and (animations) .json files.
fn path_arg(a: &Args, ext: &str) -> Result<PathBuf, String> {
    let mut p = PathBuf::from(a.s("path")?);
    match p.extension() {
        None => {
            p.set_extension(ext);
        }
        Some(x) if x.eq_ignore_ascii_case(ext) => {}
        Some(_) => return Err(format!("'{}' must end in .{ext}", p.display())),
    }
    Ok(p)
}

fn write_file(p: &PathBuf, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(p, bytes).map_err(|e| format!("{}: {e}", p.display()))
}

pub fn info(e: &Engine) -> Value {
    let d = &e.doc;
    json!({
        "width": d.w, "height": d.h, "mode": "indexed",
        "layers": d.layers.iter().enumerate().map(|(i, l)| json!({
            "index": i, "name": l.name, "visible": l.visible,
            "locked": l.locked, "alpha_lock": l.alpha_lock, "tile_mapped": l.map.is_some() })).collect::<Vec<_>>(),
        "active_layer": d.active,
        "palette_count": d.palette.colors.len(),
        "ramps": d.palette.ramps.len(),
        "tileset": d.tileset.as_ref().map(|t| json!({
            "tile_w": t.tw, "tile_h": t.th, "count": t.tiles.len(), "cols": d.w / t.tw, "rows": d.h / t.th })),
        // An empty selection (nothing can be drawn) reports w = h = 0; no selection is null.
        "selection": d.selection.as_ref().map(|_| match d.sel_bounds() {
            Some((x0, y0, x1, y1)) => json!({"x": x0, "y": y0, "w": x1 - x0 + 1, "h": y1 - y0 + 1}),
            None => json!({"x": 0, "y": 0, "w": 0, "h": 0}),
        }),
        "floating": d.float.is_some(),
        "fg": e.cell_json(e.o.fg), "bg": e.cell_json(e.o.bg),
        "path": e.path.as_ref().map(|p| p.display().to_string()),
        "modified": e.modified, "undo_steps": e.undo.len(), "redo_steps": e.redo.len(),
        "host": e.host,
        // Which of the open documents (tabs) this is.
        "document": {"id": e.id, "name": e.title()}, "open_documents": e.tab_list().len(),
    })
}

fn check_discard(e: &Engine, a: &Args) -> Result<(), String> {
    if e.modified && !a.b("force")? {
        return Err("the open document has unsaved changes; call save_document first, or pass force=true to discard them".into());
    }
    Ok(())
}

fn install_doc(e: &mut Engine, doc: Doc, path: Option<PathBuf>, blank: bool) {
    e.replace_doc(doc);
    e.blank = blank;
    e.o.custom_brush = None;
    e.o.active_ramp = None;
    e.o.active_tile = 0;
    e.path = path;
    e.modified = false;
    e.reset_history = true;
    e.view.fit = true;
}

pub fn doc_from_png(img: io::PngImage) -> Doc {
    match img.pixels {
        io::PngPixels::Indexed { data, mut palette, trns } => {
            // Trailing transparent entries are only there to carry transparency (our own
            // exports add one); every pixel using them becomes transparent below.
            while palette.len() > 1 && trns.get(palette.len() - 1).is_some_and(|a| *a < 128) {
                palette.pop();
            }
            let mut d = Doc::new(img.w, img.h, Palette { colors: palette, ramps: vec![] });
            let n = d.palette.colors.len();
            d.layers[0].data = Arc::new(
                data.iter()
                    .map(|&i| if trns.get(i as usize).is_some_and(|a| *a < 128) || i as usize >= n { TRANSP } else { i as Cell })
                    .collect(),
            );
            d
        }
        // A true-colour image gets a palette made for it (up to 256 colours).
        io::PngPixels::Rgba(px) => {
            let mut d = Doc::new(img.w, img.h, quant::palette_for(px.iter(), 256));
            d.layers[0].data = Arc::new(quant::to_indexed(&px, img.w as usize, img.h as usize, &d.palette, Dither::None));
            d
        }
    }
}

/// Rebuild every layer by pulling each destination pixel from a source position.
fn transform(e: &mut Engine, nw: u32, nh: u32, src: impl Fn(i32, i32) -> (i32, i32)) {
    let (ow, oh) = (e.doc.w as i32, e.doc.h as i32);
    let empty = TRANSP;
    for l in &mut e.doc.layers {
        let mut data = vec![empty; (nw * nh) as usize];
        for y in 0..nh as i32 {
            for x in 0..nw as i32 {
                let (sx, sy) = src(x, y);
                if sx >= 0 && sy >= 0 && sx < ow && sy < oh {
                    data[(y * nw as i32 + x) as usize] = l.data[(sy * ow + sx) as usize];
                }
            }
        }
        l.data = Arc::new(data);
    }
    if (nw, nh) != (e.doc.w, e.doc.h) {
        e.view.fit = true;
    }
    e.doc.w = nw;
    e.doc.h = nh;
    e.doc.selection = None;
}

/// Remap every pixel onto a smaller generated palette, or onto the current one.
fn reduce(e: &mut Engine, colors: usize, dither: Dither, generate: bool) {
    let (fg, bg) = (e.doc.to_rgba(e.o.fg), e.doc.to_rgba(e.o.bg));
    let d = &mut e.doc;
    let (w, h) = (d.w as usize, d.h as usize);
    let layers: Vec<Vec<[u8; 4]>> = d.layers.iter().map(|l| l.data.iter().map(|&c| d.to_rgba(c)).collect()).collect();
    let tiles: Option<Vec<Vec<[u8; 4]>>> =
        d.tileset.as_ref().map(|t| t.tiles.iter().map(|t| t.iter().map(|&c| d.to_rgba(c)).collect()).collect());
    if generate {
        d.palette = quant::palette_for(layers.iter().flatten(), colors);
    }
    for (l, px) in d.layers.iter_mut().zip(&layers) {
        l.data = Arc::new(quant::to_indexed(px, w, h, &d.palette, dither));
    }
    if let (Some(ts), Some(tp)) = (d.tileset.as_mut(), &tiles) {
        let (tw, th) = (ts.tw as usize, ts.th as usize);
        for (t, px) in ts.tiles.iter_mut().zip(tp) {
            *t = Arc::new(quant::to_indexed(px, tw, th, &d.palette, Dither::None));
        }
    }
    e.o.fg = e.doc.from_rgba(fg);
    e.o.bg = e.doc.from_rgba(bg);
    e.o.custom_brush = None;
}

/// Renumber palette indices everywhere they are stored.
fn remap_indices(e: &mut Engine, map: &[Cell]) {
    let f = |c: Cell| if c < 256 { map.get(c as usize).copied().unwrap_or(c) } else { c };
    for l in &mut e.doc.layers {
        if l.data.iter().any(|&c| f(c) != c) {
            Arc::make_mut(&mut l.data).iter_mut().for_each(|c| *c = f(*c));
        }
    }
    if let Some(ts) = &mut e.doc.tileset {
        for t in &mut ts.tiles {
            if t.iter().any(|&c| f(c) != c) {
                Arc::make_mut(t).iter_mut().for_each(|c| *c = f(*c));
            }
        }
    }
    e.o.fg = f(e.o.fg);
    e.o.bg = f(e.o.bg);
    if let Some(cells) = e.o.custom_brush.as_mut().and_then(|b| b.cells.as_mut()) {
        cells.iter_mut().for_each(|c| *c = f(*c));
    }
    for r in &mut e.doc.palette.ramps {
        r.indices = r.indices.iter().map(|&i| f(i as Cell)).filter(|&n| n < 256).map(|n| n as u8).collect();
    }
}

fn usage(e: &Engine) -> Vec<u64> {
    let mut u = vec![0u64; e.doc.palette.colors.len()];
    let tiles = e.doc.tileset.iter().flat_map(|t| t.tiles.iter());
    for data in e.doc.layers.iter().map(|l| &l.data).chain(tiles) {
        for &c in data.iter() {
            if let Some(n) = u.get_mut(c as usize) {
                *n += 1;
            }
        }
    }
    u
}

fn idx_arg(e: &Engine, a: &Args, k: &str) -> Result<usize, String> {
    let i = a.i(k)?;
    if i < 0 || i as usize >= e.doc.palette.colors.len() {
        return Err(format!("'{k}' = {i} is outside the palette (0..{})", e.doc.palette.colors.len() - 1));
    }
    Ok(i as usize)
}

fn rgb_arg(v: &Value) -> Result<[u8; 3], String> {
    match v {
        Value::String(s) => parse_hex(s).map(|c| [c[0], c[1], c[2]]).ok_or_else(|| format!("bad colour '{s}', expected '#rrggbb'")),
        Value::Array(a) if a.len() >= 3 => {
            let ch = |i: usize| a[i].as_f64().map(|v| v.clamp(0.0, 255.0) as u8).ok_or("bad colour array");
            Ok([ch(0)?, ch(1)?, ch(2)?])
        }
        _ => Err("colour must be '#rrggbb' or [r,g,b]".into()),
    }
}

fn paint_args(e: &Engine, a: &Args) -> Result<PaintOpts, String> {
    let mut o = PaintOpts {
        color: e.o.fg,
        bg: e.o.bg,
        mode: PaintMode::Normal,
        dither: 8,
        dither_bg: false,
        brush: Brush::new(1, BrushShape::Square),
        sym_x: false,
        sym_y: false,
        pixel_perfect: false,
        ramp: e.o.active_ramp,
    };
    let size = a.oi("size")?.unwrap_or(1).clamp(1, 128) as u32;
    let shape = match a.os("shape")? {
        Some(s) => BrushShape::parse(s).ok_or_else(|| format!("unknown brush shape '{s}'"))?,
        None => BrushShape::Square,
    };
    o.brush = Brush::new(size, shape);
    if a.os("brush")? == Some("custom") {
        o.brush = e.o.custom_brush.clone().ok_or("no custom brush; call grab_brush first")?;
    }
    if let Some(v) = a.get("color") {
        o.color = e.parse_color(v)?;
        o.brush.cells = None;
    }
    if let Some(v) = a.get("bg") {
        o.bg = e.parse_color(v)?;
    }
    if let Some(m) = a.os("mode")? {
        o.mode = PaintMode::parse(m).ok_or_else(|| format!("unknown paint mode '{m}'"))?;
    }
    if let Some(d) = a.oi("dither")? {
        o.dither = d.clamp(0, 16) as u8;
    }
    o.dither_bg = a.b("dither_bg")?;
    if let Some(s) = a.os("symmetry")? {
        o.sym_x = s.contains('x');
        o.sym_y = s.contains('y');
    }
    Ok(o)
}

/// Run a painting closure as one stroke on the requested layer.
/// The reply for a paint command: says so when an empty selection meant nothing could change.
fn painted(e: &Engine) -> R {
    if e.doc.selection.as_ref().is_some_and(|s| s.iter().all(|&v| v == 0)) {
        return Ok(json!({"note": "nothing was drawn: the selection is empty (select_none clears it)"}));
    }
    ok()
}

fn draw(e: &mut Engine, a: &Args, o: PaintOpts, f: impl FnOnce(&mut Engine)) -> R {
    let li = layer_arg(e, a)?;
    let prev = e.doc.active;
    e.doc.active = li;
    let r = e.stroke_begin(o);
    if r.is_ok() {
        f(e);
        e.stroke_end();
    }
    e.doc.active = prev.min(e.doc.layers.len() - 1);
    r?;
    painted(e)
}

fn points_arg(a: &Args, k: &str) -> Result<Vec<(i32, i32)>, String> {
    let arr = a.arr(k)?;
    let num = |v: &Value| v.as_f64().map(|f| (f.floor() as i32).clamp(-COORD, COORD)).ok_or_else(|| format!("'{k}' must contain numbers"));
    if arr.first().is_some_and(|v| v.is_array()) {
        arr.iter()
            .map(|p| {
                let p = p.as_array().filter(|p| p.len() >= 2).ok_or_else(|| format!("'{k}' must be [[x,y],...]"))?;
                Ok((num(&p[0])?, num(&p[1])?))
            })
            .collect()
    } else {
        if arr.len() % 2 != 0 {
            return Err(format!("'{k}' as a flat list needs an even number of values"));
        }
        arr.chunks(2).map(|c| Ok((num(&c[0])?, num(&c[1])?))).collect()
    }
}

fn rows_json(e: &Engine, cells: &[Cell], w: usize) -> Value {
    Value::Array(cells.chunks(w).map(|r| Value::Array(r.iter().map(|&c| e.cell_json(c)).collect())).collect())
}

/// Parse `rows` (array of arrays of colours - null leaves a pixel alone) into cells.
fn rows_arg(e: &Engine, a: &Args) -> Result<Vec<Vec<Option<Cell>>>, String> {
    let mut cache: HashMap<String, Cell> = HashMap::new();
    a.arr("rows")?
        .iter()
        .map(|row| {
            row.as_array()
                .ok_or("'rows' must be an array of arrays")?
                .iter()
                .map(|v| match v {
                    Value::Null => Ok(None),
                    Value::String(s) => match cache.get(s) {
                        Some(c) => Ok(Some(*c)),
                        None => {
                            let c = e.parse_color(v)?;
                            cache.insert(s.clone(), c);
                            Ok(Some(c))
                        }
                    },
                    _ => e.parse_color(v).map(Some),
                })
                .collect()
        })
        .collect()
}

fn sel_op(e: &mut Engine, mask: Vec<bool>, op: Option<&str>) -> Result<(), String> {
    let old = e.doc.selection.clone();
    let has = |i: usize| old.as_ref().is_some_and(|s| s[i] != 0);
    let out: Vec<u8> = match op.unwrap_or("replace") {
        "replace" => mask.iter().map(|&m| m as u8).collect(),
        "add" => mask.iter().enumerate().map(|(i, &m)| (m || has(i)) as u8).collect(),
        "subtract" => mask.iter().enumerate().map(|(i, &m)| (!m && has(i)) as u8).collect(),
        "intersect" => mask.iter().enumerate().map(|(i, &m)| (m && has(i)) as u8).collect(),
        o => return Err(format!("unknown op '{o}' (replace, add, subtract, intersect)")),
    };
    // A result with no pixels stays an (empty) selection, so later drawing and
    // selection_fill do nothing instead of silently applying to the whole layer.
    e.doc.selection = Some(Arc::new(out));
    Ok(())
}

fn tileset(e: &Engine) -> Result<&TileSet, String> {
    e.doc.tileset.as_ref().ok_or_else(|| "this document has no tile set; call tileset_create first".to_string())
}

fn tile_arg(e: &Engine, a: &Args, k: &str) -> Result<usize, String> {
    let n = tileset(e)?.tiles.len();
    let t = a.oi(k)?.unwrap_or(e.o.active_tile as i64);
    if t < 0 || t as usize >= n {
        return Err(format!("tile {t} out of range (tile set has {n} tiles)"));
    }
    Ok(t as usize)
}

fn map_layer(e: &Engine, a: &Args) -> Result<(usize, usize, usize), String> {
    let ts = tileset(e)?;
    let (cols, rows) = ((e.doc.w / ts.tw) as usize, (e.doc.h / ts.th) as usize);
    let li = match a.get("layer") {
        Some(_) => layer_arg(e, a)?,
        None if e.doc.layers[e.doc.active].map.is_some() => e.doc.active,
        None => e.doc.layers.iter().position(|l| l.map.is_some()).ok_or("no tile-mapped layer; call layer_to_tiles first")?,
    };
    if e.doc.layers[li].map.is_none() {
        return Err(format!("layer '{}' is not tile mapped; call layer_to_tiles first", e.doc.layers[li].name));
    }
    Ok((li, cols, rows))
}

fn cell_arg(a: &Args, cols: usize, rows: usize) -> Result<usize, String> {
    let (c, r) = (a.i("col")?, a.i("row")?);
    if c < 0 || r < 0 || c as usize >= cols || r as usize >= rows {
        return Err(format!("cell ({c}, {r}) is outside the {cols}x{rows} map"));
    }
    Ok(r as usize * cols + c as usize)
}

// ------------------------------------------------------------------ document

fn document_cmds(b: &mut B) {
    use Kind::*;
    b.add("document", "get_info", Read, "", "Document summary: size, layers, tile set, selection, colours, file path.", |e, _| Ok(info(e)));
    b.add(
        "document",
        "new_document",
        Edit,
        "width:int Width in pixels; height:int Height in pixels; \
         palette:str? Palette preset name (default 'default'); force:bool? Discard unsaved changes",
        "Replace the open document with a blank one. Clears undo history.",
        |e, a| {
            check_discard(e, a)?;
            let (w, h) = (dim(a.i("width")?)?, dim(a.i("height")?)?);
            let name = a.os("palette")?.unwrap_or("default");
            let pal = Palette::preset(name).ok_or_else(|| format!("unknown palette preset '{name}' ({})", PRESETS.join(", ")))?;
            install_doc(e, Doc::new(w, h, pal), None, true);
            Ok(info(e))
        },
    );
    b.add(
        "document",
        "open_document",
        Edit,
        "path:str A .jmn project or a .png image; force:bool? Discard unsaved changes",
        "Open a project file, or a PNG as a new document. An indexed PNG keeps its palette; a true-colour PNG is converted to a generated palette of up to 256 colours. Clears undo history.",
        |e, a| {
            check_discard(e, a)?;
            let p = PathBuf::from(a.s("path")?);
            if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("png")) {
                install_doc(e, doc_from_png(io::load_png(&p)?), None, false);
            } else {
                install_doc(e, io::load_doc(&p)?, Some(p), false);
            }
            Ok(info(e))
        },
    );
    b.add(
        "document",
        "save_document",
        Read,
        "path:str? Destination .jmn file (default: the current file)",
        "Save the project in the native .jmn format (layers, palette, ramps, tiles). Overwrites an existing file.",
        |e, a| {
            let p = match a.os("path")? {
                Some(_) => path_arg(a, "jmn")?,
                None => e.path.clone().ok_or("the document has no file yet; pass 'path'")?,
            };
            io::save_doc(&e.doc, &p)?;
            e.path = Some(p.clone());
            e.mark_saved();
            Ok(json!({"path": p.display().to_string()}))
        },
    );
    b.add(
        "document",
        "export_png",
        Read,
        "path:str Destination file; scale:int? Integer upscale 1-64 (default 1); layer:layer? Export a single layer",
        "Export a flattened PNG. Indexed documents export as indexed PNGs at scale 1. The scaled image may be at most 8192x8192 pixels in area.",
        |e, a| {
            let p = path_arg(a, "png")?;
            let layer = if a.get("layer").is_some() { Some(layer_arg(e, a)?) } else { None };
            let bytes = io::export_png(&e.doc, layer, a.oi("scale")?.unwrap_or(1).clamp(1, 64) as u32)?;
            write_file(&p, &bytes)?;
            Ok(json!({"path": p.display().to_string(), "bytes": bytes.len()}))
        },
    );
    b.add(
        "document",
        "import_png_layer",
        Edit,
        "path:str PNG file; x:int? Left offset (default 0); y:int? Top offset (default 0); name:str? Layer name",
        "Add a PNG as a new layer, mapped onto the document's palette.",
        |e, a| {
            let p = PathBuf::from(a.s("path")?);
            let src = doc_from_png(io::load_png(&p)?);
            let (ox, oy) = (a.oc("x")?.unwrap_or(0), a.oc("y")?.unwrap_or(0));
            let name = a.os("name")?.map(str::to_string).unwrap_or_else(|| p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or("Imported".into()));
            let mut l = e.doc.new_layer(&name);
            let (w, h) = (e.doc.w as i32, e.doc.h as i32);
            let data = Arc::make_mut(&mut l.data);
            for sy in 0..src.h as i32 {
                for sx in 0..src.w as i32 {
                    let (x, y) = (sx + ox, sy + oy);
                    if x >= 0 && y >= 0 && x < w && y < h {
                        let rgba = src.to_rgba(src.layers[0].data[(sy * src.w as i32 + sx) as usize]);
                        data[(y * w + x) as usize] = e.doc.from_rgba(rgba);
                    }
                }
            }
            let at = e.doc.active + 1;
            e.doc.layers.insert(at, l);
            e.doc.active = at;
            Ok(json!({"layer": at}))
        },
    );
    b.add(
        "document",
        "resize_canvas",
        Edit,
        "width:int New width; height:int New height; anchor:str? Where existing pixels stay: top-left (default), top, top-right, left, center, right, bottom-left, bottom, bottom-right",
        "Change the canvas size without scaling the pixels.",
        |e, a| {
            let (nw, nh) = (dim(a.i("width")?)?, dim(a.i("height")?)?);
            let anchor = a.os("anchor")?.unwrap_or("top-left").to_ascii_lowercase();
            let (dw, dh) = (nw as i32 - e.doc.w as i32, nh as i32 - e.doc.h as i32);
            let ox = if anchor.contains("left") { 0 } else if anchor.contains("right") { dw } else { dw / 2 };
            let oy = if anchor.contains("top") { 0 } else if anchor.contains("bottom") { dh } else { dh / 2 };
            transform(e, nw, nh, |x, y| (x - ox, y - oy));
            ok()
        },
    );
    b.add("document", "scale_image", Edit, "width:int New width; height:int New height", "Scale the whole image with nearest-neighbour sampling.", |e, a| {
        let (nw, nh) = (dim(a.i("width")?)?, dim(a.i("height")?)?);
        let (ow, oh) = (e.doc.w as i64, e.doc.h as i64);
        transform(e, nw, nh, |x, y| ((x as i64 * ow / nw as i64) as i32, (y as i64 * oh / nh as i64) as i32));
        ok()
    });
    b.add("document", "crop_to_selection", Edit, "", "Crop the canvas to the selection bounds.", |e, _| {
        let (x0, y0, x1, y1) = e.doc.sel_bounds().ok_or("nothing is selected")?;
        transform(e, (x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32, |x, y| (x + x0, y + y0));
        ok()
    });
    b.add(
        "document",
        "flip",
        Edit,
        "axis:str h (mirror left-right) or v (mirror top-bottom); scope:str? document, layer or selection (default: floating pixels, else selection, else document)",
        "Mirror the document, the active layer, or the selected area.",
        |e, a| {
            let horiz = match a.s("axis")?.to_ascii_lowercase().as_str() {
                "h" | "horizontal" | "x" => true,
                "v" | "vertical" | "y" => false,
                o => return Err(format!("unknown axis '{o}' (h or v)")),
            };
            if let Some(f) = e.doc.float.as_mut() {
                let (fw, fh) = (f.w as usize, f.h as usize);
                let at = |x: usize, y: usize| if horiz { y * fw + (fw - 1 - x) } else { (fh - 1 - y) * fw + x };
                let data: Vec<Cell> = (0..fw * fh).map(|i| f.data[at(i % fw, i / fw)]).collect();
                f.data = Arc::new(data);
                if let Some(s) = f.sel.clone() {
                    f.sel = Some(Arc::new((0..fw * fh).map(|i| s[at(i % fw, i / fw)]).collect()));
                }
                return ok();
            }
            let scope = a.os("scope")?.unwrap_or(if e.doc.selection.is_some() { "selection" } else { "document" });
            let (w, h) = (e.doc.w as i32, e.doc.h as i32);
            match scope {
                "document" => transform(e, w as u32, h as u32, |x, y| if horiz { (w - 1 - x, y) } else { (x, h - 1 - y) }),
                "layer" | "selection" => {
                    let (x0, y0, x1, y1) = if scope == "selection" { e.doc.sel_bounds().ok_or("nothing is selected")? } else { (0, 0, w - 1, h - 1) };
                    let li = e.doc.active;
                    let old = e.doc.layers[li].data.clone();
                    let data = layer_data(e, li)?;
                    for y in y0..=y1 {
                        for x in x0..=x1 {
                            let (sx, sy) = if horiz { (x0 + x1 - x, y) } else { (x, y0 + y1 - y) };
                            data[(y * w + x) as usize] = old[(sy * w + sx) as usize];
                        }
                    }
                }
                o => return Err(format!("unknown scope '{o}'")),
            }
            ok()
        },
    );
    b.add("document", "rotate", Edit, "turns:int? Quarter turns clockwise: 1, 2 or 3 (default 1)", "Rotate the whole document in 90 degree steps.", |e, a| {
        let (w, h) = (e.doc.w as i32, e.doc.h as i32);
        match a.oi("turns")?.unwrap_or(1).rem_euclid(4) {
            1 => transform(e, h as u32, w as u32, |x, y| (y, h - 1 - x)),
            2 => transform(e, w as u32, h as u32, |x, y| (w - 1 - x, h - 1 - y)),
            3 => transform(e, h as u32, w as u32, |x, y| (w - 1 - y, x)),
            _ => {}
        }
        ok()
    });
    b.add(
        "document",
        "reduce_colors",
        Edit,
        "colors:int? Palette size when generating (default 256); dither:str? none (default), bayer or floyd; \
         palette:str? generate (default) builds a palette from the image, current remaps onto the existing palette",
        "Reduce the image to fewer colours, or remap it onto the current palette, with optional dithering.",
        |e, a| {
            let d = a.os("dither")?.unwrap_or("none");
            let dither = Dither::parse(d).ok_or_else(|| format!("unknown dither '{d}' (none, bayer, floyd)"))?;
            let generate = match a.os("palette")?.unwrap_or("generate") {
                "generate" => true,
                "current" => false,
                o => return Err(format!("unknown palette option '{o}' (generate or current)")),
            };
            reduce(e, a.oi("colors")?.unwrap_or(256).clamp(1, 256) as usize, dither, generate);
            Ok(json!({"palette_count": e.doc.palette.colors.len()}))
        },
    );
    b.add("document", "undo", Read, "steps:int? How many steps (default 1)", "Undo. Each command, batch, script or user stroke is one step.", |e, a| {
        let mut n = 0;
        for _ in 0..a.oi("steps")?.unwrap_or(1).max(1) {
            if !e.undo()? {
                break;
            }
            n += 1;
        }
        Ok(json!({"undone": n}))
    });
    b.add("document", "redo", Read, "steps:int? How many steps (default 1)", "Redo previously undone steps.", |e, a| {
        let mut n = 0;
        for _ in 0..a.oi("steps")?.unwrap_or(1).max(1) {
            if !e.redo()? {
                break;
            }
            n += 1;
        }
        Ok(json!({"redone": n}))
    });
}

// ------------------------------------------------------------------ layers

fn layer_cmds(b: &mut B) {
    use Kind::*;
    b.add("layer", "layer_add", Edit, "name:str? Layer name; index:int? Position, 0 = bottom (default: above the active layer)", "Add an empty layer and make it active.", |e, a| {
        let n = e.doc.layers.len();
        let name = a.os("name")?.map(str::to_string).unwrap_or_else(|| format!("Layer {}", e.doc.next_id));
        let at = a.oi("index")?.map(|i| i.clamp(0, n as i64) as usize).unwrap_or(e.doc.active + 1);
        let l = e.doc.new_layer(&name);
        e.doc.layers.insert(at, l);
        e.doc.active = at;
        Ok(json!({"layer": at}))
    });
    b.add("layer", "layer_delete", Edit, "layer:layer? Layer index or name (default: active)", "Delete a layer. The last remaining layer cannot be deleted.", |e, a| {
        let li = layer_arg(e, a)?;
        if e.doc.layers.len() < 2 {
            return Err("cannot delete the only layer".into());
        }
        e.doc.layers.remove(li);
        if e.doc.active > li || e.doc.active >= e.doc.layers.len() {
            e.doc.active = e.doc.active.saturating_sub(1);
        }
        ok()
    });
    b.add("layer", "layer_duplicate", Edit, "layer:layer? Layer index or name (default: active)", "Duplicate a layer above itself.", |e, a| {
        let li = layer_arg(e, a)?;
        let mut l = e.doc.layers[li].clone();
        l.id = e.doc.next_id;
        e.doc.next_id += 1;
        l.name = format!("{} copy", l.name);
        e.doc.layers.insert(li + 1, l);
        e.doc.active = li + 1;
        Ok(json!({"layer": li + 1}))
    });
    b.add("layer", "layer_select", Edit, "layer:layer Layer index or name", "Make a layer the active layer.", |e, a| {
        e.doc.active = layer_arg(e, a)?;
        ok()
    });
    b.add(
        "layer",
        "layer_set",
        Edit,
        "layer:layer? Layer index or name (default: active); name:str? New name; visible:bool? Show or hide; \
         locked:bool? Protect from edits; alpha_lock:bool? Only allow painting on existing pixels",
        "Change layer properties.",
        |e, a| {
            let li = layer_arg(e, a)?;
            let l = &mut e.doc.layers[li];
            if let Some(n) = a.os("name")? {
                l.name = n.to_string();
            }
            if let Some(v) = a.ob("visible")? {
                l.visible = v;
            }
            if let Some(v) = a.ob("locked")? {
                l.locked = v;
            }
            if let Some(v) = a.ob("alpha_lock")? {
                l.alpha_lock = v;
            }
            ok()
        },
    );
    b.add("layer", "layer_move", Edit, "layer:layer? Layer index or name (default: active); to:int New position, 0 = bottom", "Reorder a layer.", |e, a| {
        let li = layer_arg(e, a)?;
        let to = a.i("to")?.clamp(0, e.doc.layers.len() as i64 - 1) as usize;
        let was_active = e.doc.layers[e.doc.active].id;
        let l = e.doc.layers.remove(li);
        e.doc.layers.insert(to, l);
        e.doc.active = e.doc.layers.iter().position(|l| l.id == was_active).unwrap_or(0);
        ok()
    });
    b.add("layer", "layer_merge_down", Edit, "layer:layer? Upper layer (default: active)", "Merge a layer into the one below it.", |e, a| {
        let li = layer_arg(e, a)?;
        if li == 0 {
            return Err("there is no layer below to merge into".into());
        }
        let upper = e.doc.layers.remove(li);
        let lower = layer_data(e, li - 1)?;
        for (d, &s) in lower.iter_mut().zip(upper.data.iter()) {
            if s < 256 {
                *d = s;
            }
        }
        e.doc.active = li - 1;
        ok()
    });
    b.add("layer", "layer_flatten", Edit, "", "Merge all visible layers into one.", |e, _| {
        let cells = e.doc.flat_cells();
        let mut l = e.doc.new_layer("Flattened");
        l.data = Arc::new(cells);
        e.doc.layers = vec![l];
        e.doc.active = 0;
        ok()
    });
    b.add("layer", "layer_clear", Paint, "layer:layer? Layer index or name (default: active)", "Erase every pixel on a layer.", |e, a| {
        let li = layer_arg(e, a)?;
        let empty = TRANSP;
        layer_data(e, li)?.fill(empty);
        ok()
    });
}

// ------------------------------------------------------------------ drawing

fn draw_cmds(b: &mut B) {
    use Kind::*;
    b.add(
        "draw",
        "get_pixel",
        Read,
        "x:int X; y:int Y; layer:layer? Layer to read (default: active); merged:bool? Read the topmost visible pixel across all layers",
        "Read one pixel: its palette index (-1 = transparent) and its colour as '#rrggbbaa'.",
        |e, a| {
            let (x, y) = (a.i("x")?, a.i("y")?);
            if x < 0 || y < 0 || x >= e.doc.w as i64 || y >= e.doc.h as i64 {
                return Err(format!("({x}, {y}) is outside the {}x{} canvas", e.doc.w, e.doc.h));
            }
            let i = (y * e.doc.w as i64 + x) as usize;
            let c = if a.b("merged")? { e.doc.flat_cells()[i] } else { e.doc.layers[layer_arg(e, a)?].data[i] };
            Ok(json!({"color": e.cell_json(c), "rgba": hex8(e.doc.to_rgba(c))}))
        },
    );
    b.add(
        "draw",
        "get_pixels",
        Read,
        "x:int? Left (default 0); y:int? Top (default 0); w:int? Width (default: to the right edge); h:int? Height (default: to the bottom edge); \
         layer:layer? Layer to read (default: active); merged:bool? Read all visible layers flattened",
        "Read a block of pixels as rows of colours (max 65536 pixels per call).",
        |e, a| {
            let (dw, dh) = (e.doc.w as i64, e.doc.h as i64);
            let (x, y) = (a.oi("x")?.unwrap_or(0).clamp(0, dw - 1), a.oi("y")?.unwrap_or(0).clamp(0, dh - 1));
            let (w, h) = (a.oi("w")?.unwrap_or(dw - x).clamp(1, dw - x), a.oi("h")?.unwrap_or(dh - y).clamp(1, dh - y));
            if w * h > 65536 {
                return Err(format!("{w}x{h} is too many pixels for one call (max 65536); read a smaller block"));
            }
            let flat;
            let src: &[Cell] = if a.b("merged")? {
                flat = e.doc.flat_cells();
                &flat
            } else {
                &e.doc.layers[layer_arg(e, a)?].data
            };
            let mut cells = Vec::with_capacity((w * h) as usize);
            for yy in y..y + h {
                cells.extend_from_slice(&src[(yy * dw + x) as usize..(yy * dw + x + w) as usize]);
            }
            Ok(json!({"x": x, "y": y, "w": w, "h": h, "rows": rows_json(e, &cells, w as usize)}))
        },
    );
    b.add(
        "draw",
        "set_pixels",
        Paint,
        "pixels:array List of [x, y, color]; layer:layer? Target layer (default: active)",
        "Write individual pixels directly (ignores brush, paint mode and selection).",
        |e, a| {
            let li = layer_arg(e, a)?;
            let (w, h) = (e.doc.w as i64, e.doc.h as i64);
            let mut writes = vec![];
            for p in a.arr("pixels")? {
                let p = p.as_array().filter(|p| p.len() >= 3).ok_or("each pixel must be [x, y, color]")?;
                let (x, y) = (p[0].as_f64().ok_or("bad x")?.floor() as i64, p[1].as_f64().ok_or("bad y")?.floor() as i64);
                if x >= 0 && y >= 0 && x < w && y < h {
                    writes.push(((y * w + x) as usize, e.parse_color(&p[2])?));
                }
            }
            let data = layer_data(e, li)?;
            let n = writes.len();
            for (i, c) in writes {
                data[i] = c;
            }
            Ok(json!({"written": n}))
        },
    );
    b.add(
        "draw",
        "put_pixels",
        Paint,
        "x:int Left; y:int Top; rows:array Rows of colours, top to bottom - null leaves a pixel unchanged, -1 or 'transparent' erases; layer:layer? Target layer (default: active)",
        "Write a block of pixels directly - the fastest way to place a sprite.",
        |e, a| {
            let li = layer_arg(e, a)?;
            let rows = rows_arg(e, a)?;
            let (ox, oy) = (a.c("x")? as i64, a.c("y")? as i64);
            let (w, h) = (e.doc.w as i64, e.doc.h as i64);
            let data = layer_data(e, li)?;
            let mut n = 0;
            for (ry, row) in rows.iter().enumerate() {
                for (rx, c) in row.iter().enumerate() {
                    let (x, y) = (ox + rx as i64, oy + ry as i64);
                    if let (Some(c), true) = (c, x >= 0 && y >= 0 && x < w && y < h) {
                        data[(y * w + x) as usize] = *c;
                        n += 1;
                    }
                }
            }
            Ok(json!({"written": n}))
        },
    );
    b.paint("draw_stroke", "points:array Path as [[x,y],...] or a flat [x,y,x,y,...] list", "Freehand brush stroke through the points.", |e, a| {
        let pts = points_arg(a, "points")?;
        let mut o = paint_args(e, a)?;
        o.pixel_perfect = false;
        draw(e, a, o, |e| {
            for (x, y) in pts {
                e.stroke_to(x, y);
            }
        })
    });
    b.paint("draw_line", "x0:int Start X; y0:int Start Y; x1:int End X; y1:int End Y", "Draw a straight line with the brush.", |e, a| {
        let (x0, y0, x1, y1) = (a.c("x0")?, a.c("y0")?, a.c("x1")?, a.c("y1")?);
        let o = paint_args(e, a)?;
        draw(e, a, o, |e| e.stroke_shape(ShapeKind::Line, x0, y0, x1, y1, false))
    });
    b.paint("draw_rect", "x:int Left; y:int Top; w:int Width; h:int Height; filled:bool? Fill the interior (default false)", "Draw a rectangle outline or filled rectangle.", |e, a| {
        let (x, y, w, h) = (a.c("x")?, a.c("y")?, a.len("w")?, a.len("h")?);
        let filled = a.b("filled")?;
        let o = paint_args(e, a)?;
        draw(e, a, o, |e| e.stroke_shape(ShapeKind::Rect, x, y, x + w - 1, y + h - 1, filled))
    });
    b.paint(
        "draw_ellipse",
        "x:int Left of bounding box; y:int Top of bounding box; w:int Width; h:int Height; filled:bool? Fill the interior (default false)",
        "Draw an ellipse inscribed in the bounding box.",
        |e, a| {
            let (x, y, w, h) = (a.c("x")?, a.c("y")?, a.len("w")?, a.len("h")?);
            let filled = a.b("filled")?;
            let o = paint_args(e, a)?;
            draw(e, a, o, |e| e.stroke_shape(ShapeKind::Ellipse, x, y, x + w - 1, y + h - 1, filled))
        },
    );
    b.paint(
        "fill",
        "x:int Seed X; y:int Seed Y; contiguous:bool? Only the connected area (default true)",
        "Flood fill from a point. With mode=dither it fills with a dither pattern.",
        |e, a| {
            let li = layer_arg(e, a)?;
            let (x, y) = (a.c("x")?, a.c("y")?);
            let d = &e.doc;
            let mask = flood_mask(&d.layers[li].data, d.w as i32, d.h as i32, x, y, a.ob("contiguous")?.unwrap_or(true));
            let o = paint_args(e, a)?;
            draw(e, a, o, |e| e.stroke_mask(&mask))
        },
    );
    b.add(
        "draw",
        "gradient",
        Paint,
        "x0:int Start X; y0:int Start Y; x1:int End X; y1:int End Y; colors:array? Colour stops in order (default: foreground to background); \
         ramp:int? Use a palette ramp's colours as the stops; radial:bool? Radial instead of linear; dither:bool? Ordered dithering between stops (default true); \
         layer:layer? Target layer (default: active)",
        "Fill the selection (or the whole layer) with a gradient. Indexed documents step through the stops with optional dithering.",
        |e, a| {
            let (x0, y0, x1, y1) = (a.c("x0")?, a.c("y0")?, a.c("x1")?, a.c("y1")?);
            let stops: Vec<Cell> = if let Some(c) = a.get("colors") {
                c.as_array().ok_or("'colors' must be an array")?.iter().map(|v| e.parse_color(v)).collect::<Result<_, _>>()?
            } else if let Some(r) = a.oi("ramp")? {
                let ramp = e.doc.palette.ramps.get(r.max(0) as usize).ok_or_else(|| format!("no ramp {r}"))?;
                ramp.indices.iter().map(|&i| e.parse_color(&json!(i))).collect::<Result<_, _>>()?
            } else {
                vec![e.o.fg, e.o.bg]
            };
            if stops.is_empty() {
                return Err("gradient needs at least one colour".into());
            }
            let (radial, dither) = (a.b("radial")?, a.ob("dither")?.unwrap_or(true));
            let none = Map::new();
            let o = paint_args(e, &Args(&none))?;
            draw(e, a, o, |e| e.stroke_gradient(x0, y0, x1, y1, &stops, radial, dither))
        },
    );
    b.add(
        "draw",
        "replace_color",
        Paint,
        "from:color Colour to find; to:color Replacement; scope:str? layer (default), document or selection",
        "Replace every pixel of one colour with another.",
        |e, a| {
            let (from, to) = (e.parse_color(a.get("from").ok_or("missing 'from'")?)?, e.parse_color(a.get("to").ok_or("missing 'to'")?)?);
            let scope = a.os("scope")?.unwrap_or("layer");
            let sel = if scope == "selection" { Some(e.doc.selection.clone().ok_or("nothing is selected")?) } else { None };
            let layers: Vec<usize> = if scope == "document" { (0..e.doc.layers.len()).collect() } else { vec![e.doc.active] };
            let mut n = 0;
            for li in layers {
                if !e.doc.layers[li].data.contains(&from) {
                    continue;
                }
                for (i, c) in layer_data(e, li)?.iter_mut().enumerate() {
                    if *c == from && sel.as_ref().is_none_or(|s| s[i] != 0) {
                        *c = to;
                        n += 1;
                    }
                }
            }
            Ok(json!({"replaced": n}))
        },
    );
    b.add(
        "draw",
        "pick_color",
        Read,
        "x:int X; y:int Y; target:str? fg (default) or bg; merged:bool? Sample the topmost visible pixel across layers (default true)",
        "Eyedropper: set the foreground or background colour from a pixel.",
        |e, a| {
            let (x, y) = (a.i("x")?, a.i("y")?);
            if x < 0 || y < 0 || x >= e.doc.w as i64 || y >= e.doc.h as i64 {
                return Err("outside the canvas".into());
            }
            let i = (y * e.doc.w as i64 + x) as usize;
            let c = if a.ob("merged")?.unwrap_or(true) { e.doc.flat_cells()[i] } else { e.doc.layers[e.doc.active].data[i] };
            if !cell_empty(c) {
                match a.os("target")?.unwrap_or("fg") {
                    "bg" => e.o.bg = c,
                    _ => e.o.fg = c,
                }
            }
            Ok(json!({"color": e.cell_json(c)}))
        },
    );
}

// ------------------------------------------------------------------ selection

fn selection_cmds(b: &mut B) {
    use Kind::*;
    const OP: &str = "op:str? replace (default), add, subtract or intersect";
    b.add("selection", "select_rect", Edit, &format!("x:int Left; y:int Top; w:int Width; h:int Height; {OP}"), "Select a rectangle. Drawing is confined to the selection.", |e, a| {
        let (x, y, w, h) = (a.c("x")? as i64, a.c("y")? as i64, a.c("w")? as i64, a.c("h")? as i64);
        let (dw, dh) = (e.doc.w as i64, e.doc.h as i64);
        let mut mask = vec![false; e.doc.len()];
        for yy in y.max(0)..(y + h).min(dh) {
            for xx in x.max(0)..(x + w).min(dw) {
                mask[(yy * dw + xx) as usize] = true;
            }
        }
        sel_op(e, mask, a.os("op")?)?;
        ok()
    });
    b.add(
        "selection",
        "select_wand",
        Edit,
        &format!("x:int Seed X; y:int Seed Y; contiguous:bool? Only the connected area (default true); layer:layer? Layer to sample (default: active); {OP}"),
        "Select pixels matching the colour at a point (magic wand). contiguous=false selects that colour everywhere.",
        |e, a| {
            let li = layer_arg(e, a)?;
            let d = &e.doc;
            let mask = flood_mask(&d.layers[li].data, d.w as i32, d.h as i32, a.c("x")?, a.c("y")?, a.ob("contiguous")?.unwrap_or(true));
            sel_op(e, mask, a.os("op")?)?;
            ok()
        },
    );
    b.add("selection", "select_all", Edit, "", "Select the whole canvas.", |e, _| {
        e.doc.selection = Some(Arc::new(vec![1; e.doc.len()]));
        ok()
    });
    b.add("selection", "select_none", Edit, "", "Clear the selection.", |e, _| {
        e.doc.selection = None;
        ok()
    });
    b.add("selection", "select_invert", Edit, "", "Invert the selection.", |e, _| {
        let n = e.doc.len();
        let old = e.doc.selection.clone();
        let mask = (0..n).map(|i| old.as_ref().is_none_or(|s| s[i] == 0)).collect();
        sel_op(e, mask, None)?;
        ok()
    });
    b.add("selection", "selection_delete", Paint, "", "Erase the selected pixels on the active layer (the whole layer if nothing is selected).", |e, _| {
        let (sel, empty, li) = (e.doc.selection.clone(), TRANSP, e.doc.active);
        for (i, c) in layer_data(e, li)?.iter_mut().enumerate() {
            if sel.as_ref().is_none_or(|s| s[i] != 0) {
                *c = empty;
            }
        }
        ok()
    });
    b.add("selection", "selection_fill", Paint, "color:color? Fill colour (default: foreground)", "Fill the selection (or whole layer) with a flat colour.", |e, a| {
        let color = match a.get("color") {
            Some(v) => e.parse_color(v)?,
            None => e.o.fg,
        };
        let (sel, li) = (e.doc.selection.clone(), e.doc.active);
        for (i, c) in layer_data(e, li)?.iter_mut().enumerate() {
            if sel.as_ref().is_none_or(|s| s[i] != 0) {
                *c = color;
            }
        }
        painted(e)
    });
    b.add("selection", "copy", Read, "", "Copy the selection (or whole active layer) to the internal clipboard.", |e, _| {
        e.copy()?;
        let c = e.clipboard.as_ref().unwrap();
        Ok(json!({"w": c.w, "h": c.h}))
    });
    b.add("selection", "cut", Paint, "", "Copy the selection to the clipboard and erase it.", |e, _| {
        e.copy()?;
        let (sel, empty, li) = (e.doc.selection.clone(), TRANSP, e.doc.active);
        for (i, c) in layer_data(e, li)?.iter_mut().enumerate() {
            if sel.as_ref().is_none_or(|s| s[i] != 0) {
                *c = empty;
            }
        }
        ok()
    });
    b.add(
        "selection",
        "paste",
        Paint,
        "x:int? Left (default: where it was copied from); y:int? Top; float:bool? Leave the pixels floating so they can be moved before committing (default false)",
        "Paste the clipboard onto the active layer. Transparent pixels do not overwrite.",
        |e, a| {
            let data = e.clip_cells().ok_or("the clipboard is empty; use copy first")?;
            let c = e.clipboard.as_ref().unwrap();
            let (x, y) = (a.oc("x")?.unwrap_or(c.x), a.oc("y")?.unwrap_or(c.y));
            if e.doc.layers[e.doc.active].locked {
                return Err("the active layer is locked".into());
            }
            e.doc.float = Some(Float { x, y, w: c.w, h: c.h, data: Arc::new(data), sel: None, restore: None });
            if !a.b("float")? {
                e.doc.float_commit();
            }
            ok()
        },
    );
    b.add("selection", "float_lift", Paint, "", "Lift the selected pixels (or the whole layer's content) so they float and can be moved.", |e, _| {
        if e.doc.layers[e.doc.active].locked {
            return Err("the active layer is locked".into());
        }
        e.doc.float_lift()?;
        ok()
    });
    b.add("selection", "float_move", Edit, "dx:int Horizontal offset; dy:int Vertical offset", "Move floating pixels.", |e, a| {
        let (dx, dy) = (a.c("dx")?, a.c("dy")?);
        let f = e.doc.float.as_mut().ok_or("nothing is floating; use float_lift or paste with float=true")?;
        f.x = (f.x + dx).clamp(-COORD, COORD);
        f.y = (f.y + dy).clamp(-COORD, COORD);
        ok()
    });
    b.add("selection", "float_commit", Paint, "", "Stamp floating pixels into the active layer.", |e, _| Ok(json!({"committed": e.doc.float_commit()})));
    b.add("selection", "float_cancel", Edit, "", "Discard floating pixels (lifted pixels return to where they were).", |e, _| Ok(json!({"cancelled": e.doc.float_cancel()})));
    b.add("selection", "selection_move", Paint, "dx:int Horizontal offset; dy:int Vertical offset", "Move the selected pixels (or the whole layer's content) in one step.", |e, a| {
        let (dx, dy) = (a.c("dx")?, a.c("dy")?);
        if e.doc.layers[e.doc.active].locked {
            return Err("the active layer is locked".into());
        }
        e.doc.float_lift()?;
        if let Some(f) = e.doc.float.as_mut() {
            f.x += dx;
            f.y += dy;
        }
        e.doc.float_commit();
        ok()
    });
    b.add("selection", "grab_brush", Read, "", "Turn the selection (or whole active layer) into a custom brush for the drawing tools.", |e, _| {
        // Grabbing a brush leaves the clipboard alone.
        let c = e.grab()?;
        if c.w > 256 || c.h > 256 {
            return Err("custom brushes are limited to 256x256".into());
        }
        let mask: Vec<bool> = c.data.iter().map(|&v| !cell_empty(v)).collect();
        if !mask.iter().any(|m| *m) {
            return Err("the selection has no pixels to grab".into());
        }
        let (w, h) = (c.w, c.h);
        e.o.custom_brush = Some(Brush { w: w as i32, h: h as i32, mask, cells: Some(c.data) });
        Ok(json!({"w": w, "h": h}))
    });
    b.add("selection", "clear_brush", Read, "", "Drop the custom brush and return to the standard round/square brush.", |e, _| {
        e.o.custom_brush = None;
        ok()
    });
}

// ------------------------------------------------------------------ palette

fn palette_cmds(b: &mut B) {
    use Kind::*;
    b.add("palette", "palette_get", Read, "start:int? First index (default 0); count:int? How many (default: all)", "List palette colours as '#rrggbb'.", |e, a| {
        let n = e.doc.palette.colors.len();
        let start = a.oi("start")?.unwrap_or(0).clamp(0, n as i64) as usize;
        let count = a.oi("count")?.unwrap_or(n as i64).clamp(0, (n - start) as i64) as usize;
        Ok(json!({"count": n, "start": start, "colors": e.doc.palette.colors[start..start + count].iter().map(|c| hex6(*c)).collect::<Vec<_>>()}))
    });
    b.add("palette", "palette_set", Edit, "index:int Palette index (index = palette size appends); color:color '#rrggbb' or [r,g,b]", "Change one palette colour.", |e, a| {
        let i = a.i("index")?;
        let c = rgb_arg(a.get("color").ok_or("missing 'color'")?)?;
        let n = e.doc.palette.colors.len() as i64;
        if i < 0 || i > n || i > 255 {
            return Err(format!("index {i} out of range (0..{})", n.min(255)));
        }
        if i == n {
            e.doc.palette.colors.push(c);
        } else {
            e.doc.palette.colors[i as usize] = c;
        }
        ok()
    });
    b.add("palette", "palette_set_many", Edit, "colors:array Colours as '#rrggbb' or [r,g,b]; start:int? First index (default 0)", "Set a run of palette colours, growing the palette if needed.", |e, a| {
        let start = a.oi("start")?.unwrap_or(0).max(0) as usize;
        let cols: Vec<[u8; 3]> = a.arr("colors")?.iter().map(rgb_arg).collect::<Result<_, _>>()?;
        if start > e.doc.palette.colors.len() || start + cols.len() > 256 {
            return Err("colours would fall outside the 256-entry palette".into());
        }
        for (k, c) in cols.into_iter().enumerate() {
            match e.doc.palette.colors.get_mut(start + k) {
                Some(slot) => *slot = c,
                None => e.doc.palette.colors.push(c),
            }
        }
        Ok(json!({"count": e.doc.palette.colors.len()}))
    });
    b.add("palette", "palette_resize", Edit, "count:int New number of colours, 1-256", "Grow or shrink the palette. Pixels using removed colours move to the nearest remaining colour.", |e, a| {
        let n = a.i("count")?.clamp(1, 256) as usize;
        let old = e.doc.palette.colors.clone();
        e.doc.palette.colors.resize(n, [0, 0, 0]);
        if n < old.len() {
            let map: Vec<Cell> = old.iter().enumerate().map(|(i, c)| if i < n { i as Cell } else { e.doc.palette.nearest(*c) as Cell }).collect();
            remap_indices(e, &map);
        }
        ok()
    });
    b.add("palette", "palette_presets", Read, "", "List the built-in palette presets.", |_, _| Ok(json!(PRESETS)));
    b.add(
        "palette",
        "palette_load_preset",
        Edit,
        "name:str Preset name (see palette_presets); remap:bool? Move existing pixels to the nearest new colour so the image keeps its look (default true)",
        "Replace the palette with a built-in preset.",
        |e, a| {
            let name = a.s("name")?;
            let new = Palette::preset(name).ok_or_else(|| format!("unknown preset '{name}' ({})", PRESETS.join(", ")))?;
            let remap = a.ob("remap")?.unwrap_or(true);
            let old = std::mem::replace(&mut e.doc.palette, new);
            let n = e.doc.palette.colors.len();
            let map: Vec<Cell> = old.colors.iter().enumerate().map(|(i, c)| if remap || i >= n { e.doc.palette.nearest(*c) as Cell } else { i as Cell }).collect();
            let ramps = std::mem::take(&mut e.doc.palette.ramps);
            remap_indices(e, &map);
            e.doc.palette.ramps = ramps;
            Ok(json!({"count": n}))
        },
    );
    b.add("palette", "palette_gradient", Edit, "from:int First index; to:int Last index; space:str? rgb (default) or hsv", "Blend the palette entries between two indices into a smooth run of shades.", |e, a| {
        let (i0, i1) = (idx_arg(e, a, "from")?, idx_arg(e, a, "to")?);
        let (lo, hi) = (i0.min(i1), i0.max(i1));
        let (ca, cb) = (e.doc.palette.colors[lo], e.doc.palette.colors[hi]);
        let hsv = a.os("space")? == Some("hsv");
        for i in lo + 1..hi {
            let t = (i - lo) as f32 / (hi - lo) as f32;
            e.doc.palette.colors[i] = if hsv {
                let ((h0, s0, v0), (h1, s1, v1)) = (rgb_to_hsv(ca), rgb_to_hsv(cb));
                let dh = (h1 - h0 + 540.0).rem_euclid(360.0) - 180.0;
                hsv_to_rgb(h0 + dh * t, s0 + (s1 - s0) * t, v0 + (v1 - v0) * t)
            } else {
                let l = |k: usize| (ca[k] as f32 + (cb[k] as f32 - ca[k] as f32) * t).round() as u8;
                [l(0), l(1), l(2)]
            };
        }
        ok()
    });
    b.add("palette", "palette_swap", Edit, "a:int First index; b:int Second index; remap:bool? Also swap the pixels so the image looks the same (default true)", "Swap two palette entries.", |e, a| {
        let (x, y) = (idx_arg(e, a, "a")?, idx_arg(e, a, "b")?);
        e.doc.palette.colors.swap(x, y);
        if a.ob("remap")?.unwrap_or(true) {
            let mut map: Vec<Cell> = (0..e.doc.palette.colors.len() as Cell).collect();
            map.swap(x, y);
            remap_indices(e, &map);
        }
        ok()
    });
    b.add("palette", "palette_sort", Edit, "from:int? First index (default 0); to:int? Last index (default: last colour); by:str? luma (default), hue or sat", "Sort a palette range. Pixels are renumbered so the image looks the same.", |e, a| {
        let n = e.doc.palette.colors.len();
        let lo = a.oi("from")?.unwrap_or(0).clamp(0, n as i64 - 1) as usize;
        let hi = a.oi("to")?.unwrap_or(n as i64 - 1).clamp(lo as i64, n as i64 - 1) as usize;
        let by = a.os("by")?.unwrap_or("luma");
        let cols = e.doc.palette.colors.clone();
        let key = |i: usize| -> i64 {
            let (h, s, v) = rgb_to_hsv(cols[i]);
            match by {
                "hue" => (h * 1000.0) as i64 * 1000 + (v * 999.0) as i64,
                "sat" => (s * 1000.0) as i64 * 1000 + (v * 999.0) as i64,
                _ => luma(cols[i]) as i64,
            }
        };
        let mut order: Vec<usize> = (lo..=hi).collect();
        order.sort_by_key(|&i| key(i));
        let mut map: Vec<Cell> = (0..n as Cell).collect();
        for (k, &old) in order.iter().enumerate() {
            e.doc.palette.colors[lo + k] = cols[old];
            map[old] = (lo + k) as Cell;
        }
        remap_indices(e, &map);
        ok()
    });
    b.add("palette", "palette_cleanup", Edit, "duplicates:bool? Merge identical colours (default true); unused:bool? Remove colours no pixel uses (default false)", "Tidy the palette, renumbering pixels to match.", |e, a| {
        let before = e.doc.palette.colors.len();
        if a.ob("duplicates")?.unwrap_or(true) {
            let cols = &e.doc.palette.colors;
            let map: Vec<Cell> = (0..cols.len()).map(|i| cols.iter().position(|c| *c == cols[i]).unwrap_or(i) as Cell).collect();
            remap_indices(e, &map);
        }
        if a.b("unused")? {
            let u = usage(e);
            let mut map = vec![0 as Cell; u.len()];
            let mut cols = vec![];
            for (i, n) in u.iter().enumerate() {
                if *n > 0 {
                    map[i] = cols.len() as Cell;
                    cols.push(e.doc.palette.colors[i]);
                }
            }
            if cols.is_empty() {
                cols.push(e.doc.palette.colors[0]);
            }
            // Ramp entries pointing at removed colours are dropped rather than renumbered.
            for r in &mut e.doc.palette.ramps {
                r.indices.retain(|&i| u.get(i as usize).is_some_and(|n| *n > 0));
            }
            e.doc.palette.colors = cols;
            remap_indices(e, &map);
        }
        Ok(json!({"before": before, "after": e.doc.palette.colors.len()}))
    });
    b.add("palette", "palette_usage", Read, "", "Count how many pixels use each palette index.", |e, _| {
        let u = usage(e);
        Ok(json!({"counts": u, "used": u.iter().filter(|n| **n > 0).count()}))
    });
    b.add("palette", "ramp_list", Read, "", "List colour ramps (ordered runs of palette indices used for shading, gradients and colour cycling).", |e, _| {
        Ok(Value::Array(e.doc.palette.ramps.iter().enumerate().map(|(i, r)| json!({"ramp": i, "name": r.name, "indices": r.indices, "cycle": r.cycle, "speed": r.speed})).collect()))
    });
    b.add(
        "palette",
        "ramp_add",
        Edit,
        "name:str? Ramp name; indices:array? Palette indices in order, dark to light; from:int? Or a contiguous range start; to:int? Range end; \
         cycle:bool? Animate by colour cycling; speed:num? Cycle steps per second (default 8)",
        "Define a colour ramp. Lighten/darken paint modes step along ramps; gradients can run through them.",
        |e, a| {
            let n = e.doc.palette.colors.len() as i64;
            let indices: Vec<u8> = if let Some(v) = a.get("indices") {
                v.as_array().ok_or("'indices' must be an array")?.iter().map(|v| v.as_i64().filter(|i| (0..n).contains(i)).map(|i| i as u8).ok_or("ramp indices must be valid palette indices")).collect::<Result<_, _>>()?
            } else {
                let (lo, hi) = (idx_arg(e, a, "from")?, idx_arg(e, a, "to")?);
                if lo <= hi { (lo..=hi).map(|i| i as u8).collect() } else { (hi..=lo).rev().map(|i| i as u8).collect() }
            };
            if indices.len() < 2 {
                return Err("a ramp needs at least two colours".into());
            }
            let name = a.os("name")?.map(str::to_string).unwrap_or_else(|| format!("Ramp {}", e.doc.palette.ramps.len() + 1));
            e.doc.palette.ramps.push(Ramp { name, indices, cycle: a.b("cycle")?, speed: a.of("speed")?.unwrap_or(8.0) as f32 });
            Ok(json!({"ramp": e.doc.palette.ramps.len() - 1}))
        },
    );
    b.add("palette", "ramp_set", Edit, "ramp:int Ramp number; name:str? New name; indices:array? New palette indices; cycle:bool? Colour cycling on/off; speed:num? Cycle steps per second", "Change a ramp.", |e, a| {
        let n = e.doc.palette.colors.len() as i64;
        let ri = a.i("ramp")?;
        let r = e.doc.palette.ramps.get_mut(ri.max(0) as usize).ok_or_else(|| format!("no ramp {ri}"))?;
        if let Some(s) = a.os("name")? {
            r.name = s.to_string();
        }
        if let Some(v) = a.get("indices") {
            r.indices = v.as_array().ok_or("'indices' must be an array")?.iter().map(|v| v.as_i64().filter(|i| (0..n).contains(i)).map(|i| i as u8).ok_or("ramp indices must be valid palette indices")).collect::<Result<_, _>>()?;
        }
        if let Some(c) = a.ob("cycle")? {
            r.cycle = c;
        }
        if let Some(s) = a.of("speed")? {
            r.speed = s as f32;
        }
        ok()
    });
    b.add("palette", "ramp_delete", Edit, "ramp:int Ramp number", "Delete a ramp (the colours stay in the palette).", |e, a| {
        let ri = a.i("ramp")?;
        if ri < 0 || ri as usize >= e.doc.palette.ramps.len() {
            return Err(format!("no ramp {ri}"));
        }
        e.doc.palette.ramps.remove(ri as usize);
        e.o.active_ramp = None;
        ok()
    });
}

// ------------------------------------------------------------------ tiles

fn tile_cmds(b: &mut B) {
    use Kind::*;
    const LAYER: &str = "layer:layer? Tile-mapped layer (default: the active layer if mapped, else the first mapped layer)";
    b.add("tiles", "tileset_create", Edit, "tile_w:int Tile width in pixels; tile_h:int Tile height in pixels", "Create the document's tile set. Tile 0 is always the blank tile.", |e, a| {
        if e.doc.tileset.is_some() {
            return Err("the document already has a tile set; call tileset_delete first to change the tile size".into());
        }
        let (tw, th) = (a.i("tile_w")?, a.i("tile_h")?);
        if tw < 2 || th < 2 || tw > e.doc.w as i64 || th > e.doc.h as i64 || tw > 256 || th > 256 {
            return Err(format!("tile size {tw}x{th} must be between 2x2 and the canvas size (max 256)"));
        }
        e.doc.tileset = Some(TileSet { tw: tw as u32, th: th as u32, tiles: vec![Arc::new(vec![TRANSP; (tw * th) as usize])] });
        e.view.grid_w = tw as u32;
        e.view.grid_h = th as u32;
        ok()
    });
    b.add("tiles", "tileset_delete", Edit, "", "Remove the tile set. Layers keep their pixels and become ordinary layers.", |e, _| {
        e.doc.tileset = None;
        e.doc.layers.iter_mut().for_each(|l| l.map = None);
        ok()
    });
    b.add("tiles", "tiles_info", Read, "", "Tile set size, tile count, map dimensions and which layers are tile mapped.", |e, _| {
        let ts = tileset(e)?;
        Ok(json!({
            "tile_w": ts.tw, "tile_h": ts.th, "count": ts.tiles.len(), "cols": e.doc.w / ts.tw, "rows": e.doc.h / ts.th,
            "mapped_layers": e.doc.layers.iter().enumerate().filter(|(_, l)| l.map.is_some()).map(|(i, _)| i).collect::<Vec<_>>(),
            "tile_mode": if e.o.tile_mode == TileMode::Sync { "sync" } else { "auto" }, "active_tile": e.o.active_tile,
        }))
    });
    b.add("tiles", "layer_to_tiles", Edit, "layer:layer? Layer index or name (default: active)", "Make a layer tile mapped: its pixels are cut into cells, identical cells share one tile.", |e, a| {
        let li = layer_arg(e, a)?;
        let ts = tileset(e)?;
        let n = ((e.doc.w / ts.tw) * (e.doc.h / ts.th)) as usize;
        e.doc.layers[li].map = Some(Arc::new(vec![u16::MAX; n]));
        e.match_cells(li);
        Ok(json!({"tiles": tileset(e)?.tiles.len()}))
    });
    b.add("tiles", "layer_detach_tiles", Edit, "layer:layer? Layer index or name (default: active)", "Turn a tile-mapped layer back into an ordinary layer (pixels are kept).", |e, a| {
        let li = layer_arg(e, a)?;
        e.doc.layers[li].map = None;
        ok()
    });
    b.add("tiles", "tile_add", Edit, "copy_of:int? Tile to duplicate (default: a blank tile)", "Add a tile to the tile set and make it the active tile.", |e, a| {
        let src = if a.get("copy_of").is_some() { Some(tile_arg(e, a, "copy_of")?) } else { None };
        let empty = TRANSP;
        let ts = e.doc.tileset.as_mut().ok_or("this document has no tile set; call tileset_create first")?;
        if ts.tiles.len() >= u16::MAX as usize {
            return Err("the tile set is full".into());
        }
        let t = match src {
            Some(s) => ts.tiles[s].clone(),
            None => Arc::new(vec![empty; (ts.tw * ts.th) as usize]),
        };
        ts.tiles.push(t);
        e.o.active_tile = (ts.tiles.len() - 1) as u16;
        Ok(json!({"tile": e.o.active_tile}))
    });
    b.add("tiles", "tile_delete", Edit, "tile:int Tile id (not 0)", "Delete a tile. Map cells using it become the blank tile.", |e, a| {
        let t = tile_arg(e, a, "tile")?;
        if t == 0 {
            return Err("tile 0 is the blank tile and cannot be deleted".into());
        }
        let n = tileset(e)?.tiles.len();
        let remap: Vec<u16> = (0..n).map(|i| if i == t { 0 } else if i > t { (i - 1) as u16 } else { i as u16 }).collect();
        e.remap_tiles(&remap, n - 1);
        e.reblit();
        ok()
    });
    b.add("tiles", "tile_get_pixels", Read, "tile:int? Tile id (default: active tile)", "Read a tile's pixels as rows of colours.", |e, a| {
        let t = tile_arg(e, a, "tile")?;
        let ts = tileset(e)?;
        Ok(json!({"tile": t, "w": ts.tw, "h": ts.th, "rows": rows_json(e, &ts.tiles[t], ts.tw as usize)}))
    });
    b.add("tiles", "tile_put_pixels", Edit, "tile:int? Tile id (default: active tile); rows:array Rows of colours - null leaves a pixel unchanged", "Write a tile's pixels. Every map cell using the tile updates.", |e, a| {
        let t = tile_arg(e, a, "tile")?;
        let rows = rows_arg(e, a)?;
        let ts = e.doc.tileset.as_mut().unwrap();
        let (tw, th) = (ts.tw as usize, ts.th as usize);
        let tile = Arc::make_mut(&mut ts.tiles[t]);
        for (y, row) in rows.iter().enumerate().take(th) {
            for (x, c) in row.iter().enumerate().take(tw) {
                if let Some(c) = c {
                    tile[y * tw + x] = *c;
                }
            }
        }
        e.reblit();
        ok()
    });
    b.add("tiles", "tile_select", Read, "tile:int Tile id", "Choose the active tile used by the tile stamp tool.", |e, a| {
        e.o.active_tile = tile_arg(e, a, "tile")? as u16;
        ok()
    });
    b.add("tiles", "tile_get_cell", Read, &format!("col:int Map column; row:int Map row; {LAYER}"), "Read which tile a map cell uses.", |e, a| {
        let (li, cols, rows) = map_layer(e, a)?;
        let i = cell_arg(a, cols, rows)?;
        Ok(json!({"tile": e.doc.layers[li].map.as_ref().unwrap()[i]}))
    });
    b.add("tiles", "tile_set_cell", Edit, &format!("col:int Map column; row:int Map row; tile:int? Tile id (default: active tile); {LAYER}"), "Place a tile in a map cell.", |e, a| {
        let (li, cols, rows) = map_layer(e, a)?;
        let i = cell_arg(a, cols, rows)?;
        let t = tile_arg(e, a, "tile")? as u16;
        if e.doc.layers[li].locked {
            return Err("the layer is locked".into());
        }
        let m = e.doc.layers[li].map.as_mut().unwrap();
        if m[i] != t {
            Arc::make_mut(m)[i] = t;
            e.reblit();
        }
        ok()
    });
    b.add("tiles", "tile_make_unique", Edit, &format!("col:int Map column; row:int Map row; {LAYER}"), "Give a map cell its own private copy of its tile, so it can be edited without changing other cells.", |e, a| {
        let (li, cols, rows) = map_layer(e, a)?;
        let i = cell_arg(a, cols, rows)?;
        let old = e.doc.layers[li].map.as_ref().unwrap()[i] as usize;
        let ts = e.doc.tileset.as_mut().unwrap();
        if ts.tiles.len() >= u16::MAX as usize {
            return Err("the tile set is full".into());
        }
        let copy = ts.tiles.get(old).ok_or("that cell has no valid tile")?.clone();
        ts.tiles.push(copy);
        let id = (ts.tiles.len() - 1) as u16;
        Arc::make_mut(e.doc.layers[li].map.as_mut().unwrap())[i] = id;
        Ok(json!({"tile": id}))
    });
    b.add("tiles", "tilemap_get", Read, LAYER, "Read the whole tile map as rows of tile ids.", |e, a| {
        let (li, cols, rows) = map_layer(e, a)?;
        let m = e.doc.layers[li].map.as_ref().unwrap();
        Ok(json!({"cols": cols, "rows": rows, "cells": m.chunks(cols.max(1)).map(|r| r.to_vec()).collect::<Vec<_>>()}))
    });
    b.add("tiles", "tilemap_set", Edit, &format!("cells:array Rows of tile ids - null leaves a cell unchanged; col:int? Left column of the block (default 0); row:int? Top row (default 0); {LAYER}"), "Write a block of the tile map.", |e, a| {
        let (li, cols, rows) = map_layer(e, a)?;
        let n = tileset(e)?.tiles.len();
        let (oc, or) = (a.oc("col")?.unwrap_or(0) as i64, a.oc("row")?.unwrap_or(0) as i64);
        let mut m = e.doc.layers[li].map.as_ref().unwrap().to_vec();
        for (ry, row) in a.arr("cells")?.iter().enumerate() {
            for (rx, v) in row.as_array().ok_or("'cells' must be an array of arrays")?.iter().enumerate() {
                if v.is_null() {
                    continue;
                }
                let t = v.as_i64().filter(|t| (0..n as i64).contains(t)).ok_or_else(|| format!("bad tile id {v} (tile set has {n} tiles)"))?;
                let (c, r) = (oc + rx as i64, or + ry as i64);
                if c >= 0 && r >= 0 && (c as usize) < cols && (r as usize) < rows {
                    m[r as usize * cols + c as usize] = t as u16;
                }
            }
        }
        e.doc.layers[li].map = Some(Arc::new(m));
        e.reblit();
        ok()
    });
    b.add("tiles", "tiles_remove_unused", Edit, "", "Delete tiles that no map cell uses.", |e, _| {
        let n = tileset(e)?.tiles.len();
        let mut used = vec![false; n];
        used[0] = true;
        for m in e.doc.layers.iter().filter_map(|l| l.map.as_ref()) {
            for &c in m.iter() {
                if let Some(u) = used.get_mut(c as usize) {
                    *u = true;
                }
            }
        }
        let mut next = 0u16;
        let remap: Vec<u16> = used
            .iter()
            .map(|&u| {
                if u {
                    next += 1;
                    next - 1
                } else {
                    0
                }
            })
            .collect();
        e.remap_tiles(&remap, next as usize);
        Ok(json!({"removed": n - next as usize, "count": next}))
    });
    b.add("tiles", "tiles_dedupe", Edit, "", "Merge tiles with identical pixels.", |e, _| {
        let ts = tileset(e)?;
        let n = ts.tiles.len();
        let mut seen: HashMap<Arc<Vec<Cell>>, u16> = HashMap::new();
        let mut next = 0u16;
        let remap: Vec<u16> = ts
            .tiles
            .iter()
            .map(|t| {
                *seen.entry(t.clone()).or_insert_with(|| {
                    next += 1;
                    next - 1
                })
            })
            .collect();
        e.remap_tiles(&remap, next as usize);
        Ok(json!({"removed": n - next as usize, "count": next}))
    });
    b.add("tiles", "export_tileset_png", Read, "path:str Destination file; columns:int? Tiles per row (default 16)", "Export the tile set as a PNG sheet.", |e, a| {
        let p = path_arg(a, "png")?;
        let ts = tileset(e)?;
        let cols = a.oi("columns")?.unwrap_or(16).clamp(1, 256) as usize;
        let (tw, th) = (ts.tw as usize, ts.th as usize);
        let rows = ts.tiles.len().div_ceil(cols);
        let (w, h) = (cols.min(ts.tiles.len()) * tw, rows * th);
        let mut cells = vec![TRANSP; w * h];
        for (i, t) in ts.tiles.iter().enumerate() {
            let (ox, oy) = ((i % cols) * tw, (i / cols) * th);
            for y in 0..th {
                cells[(oy + y) * w + ox..][..tw].copy_from_slice(&t[y * tw..][..tw]);
            }
        }
        let bytes = io::encode_cells(&e.doc, w as u32, h as u32, &cells)?;
        write_file(&p, &bytes)?;
        Ok(json!({"path": p.display().to_string(), "tiles": ts.tiles.len(), "width": w, "height": h}))
    });
    b.add("tiles", "export_tilemap_csv", Read, &format!("path:str Destination file; {LAYER}"), "Export a layer's tile map as CSV rows of tile ids.", |e, a| {
        let p = path_arg(a, "csv")?;
        let (li, cols, _) = map_layer(e, a)?;
        let m = e.doc.layers[li].map.as_ref().unwrap();
        let text: String = m.chunks(cols.max(1)).map(|r| r.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(",") + "\n").collect();
        write_file(&p, text.as_bytes())?;
        Ok(json!({"path": p.display().to_string()}))
    });
}

// ------------------------------------------------------------------ editor state

fn options_json(e: &Engine) -> Value {
    let o = &e.o;
    json!({
        "tool": o.tool.name(), "fg": e.cell_json(o.fg), "bg": e.cell_json(o.bg),
        "brush_size": o.brush_size, "brush_shape": o.brush_shape.name(), "custom_brush": o.custom_brush.is_some(),
        "mode": o.mode.name(), "dither": o.dither, "dither_bg": o.dither_bg, "filled": o.filled,
        "pixel_perfect": o.pixel_perfect,
        "symmetry": format!("{}{}", if o.sym_x { "x" } else { "" }, if o.sym_y { "y" } else { "" }),
        "contiguous": o.contiguous,
        "tile_mode": if o.tile_mode == TileMode::Sync { "sync" } else { "auto" }, "active_tile": o.active_tile,
        "active_ramp": o.active_ramp.map(|r| r as i64).unwrap_or(-1),
        "gradient_radial": o.gradient_radial, "gradient_dither": o.gradient_dither,
    })
}

fn editor_cmds(b: &mut B) {
    use Kind::*;
    b.add("editor", "get_options", Read, "", "The interactive tool settings shown in the app's toolbar.", |e, _| Ok(options_json(e)));
    b.add(
        "editor",
        "set_options",
        Read,
        "tool:str? pencil, eraser, line, rect, ellipse, fill, gradient, picker, select, wand, move, tile_stamp, pan; \
         brush_size:int? 1-128; brush_shape:str? round or square; mode:str? Paint mode; dither:int? 0-16; dither_bg:bool? Paint background in dither gaps; \
         filled:bool? Filled shapes; pixel_perfect:bool? Remove corner pixels from 1px freehand strokes; symmetry:str? none, x, y or xy; \
         contiguous:bool? Fill/wand connected area only; \
         tile_mode:str? auto (edited cells become new/matching tiles) or sync (editing a cell edits its tile everywhere); \
         active_tile:int? Tile used by the tile stamp; active_ramp:int? Ramp used by gradient and shading, -1 for none; \
         gradient_radial:bool? Radial gradients; gradient_dither:bool? Dithered gradients",
        "Change the interactive tool settings the user sees. Scripted drawing commands take their own parameters and do not depend on these, except tile_mode.",
        |e, a| {
            if let Some(t) = a.os("tool")? {
                e.o.tool = Tool::parse(t).ok_or_else(|| format!("unknown tool '{t}'"))?;
            }
            if let Some(s) = a.oi("brush_size")? {
                e.o.brush_size = s.clamp(1, 128) as u32;
                e.o.custom_brush = None;
            }
            if let Some(s) = a.os("brush_shape")? {
                e.o.brush_shape = BrushShape::parse(s).ok_or_else(|| format!("unknown brush shape '{s}'"))?;
                e.o.custom_brush = None;
            }
            if let Some(m) = a.os("mode")? {
                e.o.mode = PaintMode::parse(m).ok_or_else(|| format!("unknown paint mode '{m}'"))?;
            }
            if let Some(d) = a.oi("dither")? {
                e.o.dither = d.clamp(0, 16) as u8;
            }
            if let Some(v) = a.ob("dither_bg")? {
                e.o.dither_bg = v;
            }
            if let Some(v) = a.ob("filled")? {
                e.o.filled = v;
            }
            if let Some(v) = a.ob("pixel_perfect")? {
                e.o.pixel_perfect = v;
            }
            if let Some(s) = a.os("symmetry")? {
                e.o.sym_x = s.contains('x');
                e.o.sym_y = s.contains('y');
            }
            if let Some(v) = a.ob("contiguous")? {
                e.o.contiguous = v;
            }
            if let Some(m) = a.os("tile_mode")? {
                e.o.tile_mode = match m {
                    "auto" => TileMode::Auto,
                    "sync" => TileMode::Sync,
                    o => return Err(format!("unknown tile_mode '{o}' (auto or sync)")),
                };
            }
            if a.get("active_tile").is_some() {
                e.o.active_tile = tile_arg(e, a, "active_tile")? as u16;
            }
            if let Some(r) = a.oi("active_ramp")? {
                e.o.active_ramp = (r >= 0 && (r as usize) < e.doc.palette.ramps.len()).then_some(r as usize);
            }
            if let Some(v) = a.ob("gradient_radial")? {
                e.o.gradient_radial = v;
            }
            if let Some(v) = a.ob("gradient_dither")? {
                e.o.gradient_dither = v;
            }
            Ok(options_json(e))
        },
    );
    b.add("editor", "set_colors", Read, "fg:color? Foreground colour; bg:color? Background colour", "Set the current foreground and/or background colour.", |e, a| {
        if let Some(v) = a.get("fg") {
            e.o.fg = e.parse_color(v)?;
        }
        if let Some(v) = a.get("bg") {
            e.o.bg = e.parse_color(v)?;
        }
        Ok(json!({"fg": e.cell_json(e.o.fg), "bg": e.cell_json(e.o.bg)}))
    });
    b.add("editor", "swap_colors", Read, "", "Swap foreground and background colours.", |e, _| {
        std::mem::swap(&mut e.o.fg, &mut e.o.bg);
        ok()
    });
    b.add(
        "editor",
        "set_view",
        Read,
        "zoom:num? Screen pixels per image pixel; center_x:num? Image X at the centre of the view; center_y:num? Image Y at the centre; fit:bool? Zoom to fit the window; \
         pixel_grid:bool? Pixel grid at high zoom; grid:bool? Custom grid; grid_w:int? Grid cell width; grid_h:int? Grid cell height; \
         tile_grid:bool? Tile grid; tile_ids:bool? Show tile ids on the canvas; cycling:bool? Preview colour cycling; \
         anim_guides:bool? Show the animation's frame cuts on the canvas; \
         keymap:str? Single-key shortcuts in the window: standard or dpaint (Deluxe Paint keys)",
        "Change what the app window shows. Has no effect on the document.",
        |e, a| {
            let v = &mut e.view;
            if let Some(z) = a.of("zoom")? {
                v.zoom = (z as f32).clamp(0.125, 64.0);
                v.fit = false;
            }
            if let Some(x) = a.of("center_x")? {
                v.cx = x as f32;
                v.fit = false;
            }
            if let Some(y) = a.of("center_y")? {
                v.cy = y as f32;
                v.fit = false;
            }
            if a.b("fit")? {
                v.fit = true;
            }
            if let Some(b) = a.ob("pixel_grid")? {
                v.pixel_grid = b;
            }
            if let Some(b) = a.ob("grid")? {
                v.grid = b;
            }
            if let Some(n) = a.oi("grid_w")? {
                v.grid_w = n.clamp(1, 1024) as u32;
            }
            if let Some(n) = a.oi("grid_h")? {
                v.grid_h = n.clamp(1, 1024) as u32;
            }
            if let Some(b) = a.ob("tile_grid")? {
                v.tile_grid = b;
            }
            if let Some(b) = a.ob("tile_ids")? {
                v.tile_ids = b;
            }
            if let Some(b) = a.ob("cycling")? {
                v.cycling = b;
            }
            if let Some(b) = a.ob("anim_guides")? {
                v.anim_guides = b;
            }
            if let Some(k) = a.os("keymap")? {
                v.keymap = Keymap::parse(k).ok_or_else(|| format!("unknown keymap '{k}' (standard or dpaint)"))?;
            }
            ok()
        },
    );
    b.add("editor", "get_view", Read, "", "Current zoom, view centre and overlay settings.", |e, _| {
        let v = &e.view;
        Ok(json!({"zoom": v.zoom, "center_x": v.cx, "center_y": v.cy, "pixel_grid": v.pixel_grid, "grid": v.grid, "grid_w": v.grid_w,
                  "grid_h": v.grid_h, "tile_grid": v.tile_grid, "tile_ids": v.tile_ids, "cycling": v.cycling, "anim_guides": v.anim_guides, "keymap": v.keymap.name()}))
    });
}

// ------------------------------------------------------------------ animation

/// What the commands report about animation `i`: its setup, and the playback position when
/// it is the one on show.
fn anim_json(e: &Engine, i: usize) -> Value {
    let Some(a) = e.doc.anims.get(i) else { return json!({"active": false, "count": e.doc.anims.len()}) };
    let current = i == e.view.anim_index;
    let frame = if current { e.view.anim_frame.min(a.count().saturating_sub(1)) } else { 0 };
    let f = a.frame(frame).unwrap_or(Frame { x: 0, y: 0, w: 1, h: 1, ox: 0, oy: 0 });
    let (_, _, bw, bh) = a.bounds();
    let mut v = json!({"active": true, "name": a.name, "index": i, "count": e.doc.anims.len(), "current": current,
        "frames": a.count(), "fps": io::fps_json(a.fps), "frame": frame, "frame_x": f.x, "frame_y": f.y, "frame_w": f.w, "frame_h": f.h,
        "offset_x": f.ox, "offset_y": f.oy, "box_w": bw, "box_h": bh, "playing": current && e.view.anim_playing});
    match a.grid() {
        Some(g) => v.as_object_mut().unwrap().extend(json!({"kind": "grid", "x": g.x, "y": g.y, "w": g.w, "h": g.h, "cols": g.cols(), "rows": g.rows()}).as_object().unwrap().clone()),
        None => v["kind"] = json!("frames"),
    }
    v
}

/// The animation a command works on: the named one, or the one on show. `None` when there is
/// no such animation.
fn anim_target(e: &Engine, a: &Args) -> Result<Option<usize>, String> {
    Ok(match a.os("name")? {
        Some(n) => e.doc.anims.iter().position(|x| x.name == n),
        None => (e.view.anim_index < e.doc.anims.len()).then_some(e.view.anim_index),
    })
}

/// As `anim_target`, for commands that need the animation to exist.
fn anim_need(e: &Engine, a: &Args) -> Result<usize, String> {
    anim_target(e, a)?.ok_or_else(|| match a.os("name") {
        Ok(Some(n)) => format!("there is no animation named '{n}' (see anim_list)"),
        _ => "no animation is set up (see anim_set)".into(),
    })
}

/// A name no animation of the document has: "Animation", "Animation 2", ...
pub fn fresh_anim_name(d: &Doc) -> String {
    (1..).map(|i| if i == 1 { "Animation".to_string() } else { format!("Animation {i}") }).find(|n| !d.anims.iter().any(|a| a.name == *n)).unwrap_or_default()
}

fn anim_name_check(d: &Doc, name: &str, except: Option<usize>) -> Result<(), String> {
    if !Anim::name_ok(name) {
        return Err(format!("an animation name must be 1 to {} characters, with no control characters or spaces at the ends", Anim::NAME_MAX));
    }
    if d.anims.iter().enumerate().any(|(i, a)| a.name == name && Some(i) != except) {
        return Err(format!("there is already an animation named '{name}'"));
    }
    Ok(())
}

/// Store an animation at `at`, or add it as a new one and put it on show.
fn anim_put(e: &mut Engine, at: Option<usize>, anim: Anim) -> Result<usize, String> {
    let others: usize = e.doc.anims.iter().enumerate().filter(|(i, _)| Some(*i) != at).map(|(_, a)| Anim::list_frames(std::slice::from_ref(a))).sum();
    if others + Anim::list_frames(std::slice::from_ref(&anim)) > Anim::MAX_LIST_FRAMES {
        return Err(format!("a document holds at most {} frame-list frames in all", Anim::MAX_LIST_FRAMES));
    }
    match at {
        Some(i) => {
            e.doc.anims[i] = anim;
            if i == e.view.anim_index {
                e.view.anim_frame = e.view.anim_frame.min(e.doc.anims[i].count().saturating_sub(1));
            }
            Ok(i)
        }
        None => {
            anim_name_check(&e.doc, &anim.name, None)?;
            if e.doc.anims.len() >= Anim::MAX {
                return Err(format!("a document holds at most {} animations", Anim::MAX));
            }
            e.doc.anims.push(anim);
            e.show_anim(e.doc.anims.len() - 1);
            e.view.anim_frame = 0;
            e.view.anim_playing = true;
            Ok(e.view.anim_index)
        }
    }
}

/// The name for the animation a setup command is about to write: the existing one's, the
/// name asked for, or a fresh one.
fn anim_name_for(e: &Engine, a: &Args, at: Option<usize>) -> Result<String, String> {
    Ok(match (at, a.os("name")?) {
        (Some(i), _) => e.doc.anims[i].name.clone(),
        (None, Some(n)) => n.to_string(),
        (None, None) => fresh_anim_name(&e.doc),
    })
}

fn fps_arg(a: &Args, cur: Option<f32>) -> Result<f32, String> {
    let fps = a.of("fps")?.map(|v| v as f32).or(cur).unwrap_or(8.0);
    if !Anim::FPS.contains(&fps) {
        return Err("fps must be between 1 and 60".into());
    }
    Ok(fps)
}

fn animation_cmds(b: &mut B) {
    use Kind::*;
    b.add(
        "animation",
        "anim_set",
        Edit,
        "name:str? Which animation (default: the one on show) - a name that is not in use makes a new animation; \
         x:int? Left of the region; y:int? Top of the region; w:int? Region width; h:int? Region height; \
         frame_w:int? Width of one frame (default: the grid width); frame_h:int? Height of one frame (default: the grid height); \
         frames:int? How many frames play, 0 for every frame in the region (default 0); fps:num? Frames per second, 1-60 (default 8)",
        "Set up a grid animation: a region of the image is cut into frames of frame_w x frame_h, read left to right, then top to bottom \
         (part frames at the right and bottom edges are left out). For a new animation the region defaults to the selection, or the whole image \
         when nothing is selected; for an existing one anything omitted keeps its value. A document can hold many named animations; a new one \
         is put on show. A new name needs at least one region or frame value, so a mistyped name is an error rather than a new animation. \
         On a frame-list animation (see anim_frames) only fps can be changed here. Animations are saved in the project; no pixels change.",
        |e, a| {
            let at = anim_target(e, a)?;
            let cur = at.map(|i| e.doc.anims[i].clone());
            let fps = fps_arg(a, cur.as_ref().map(|c| c.fps))?;
            let geometry = ["x", "y", "w", "h", "frame_w", "frame_h", "frames"].iter().any(|k| a.get(k).is_some());
            if let (Some(i), Some(c)) = (at, cur.as_ref().filter(|c| c.grid().is_none())) {
                if geometry {
                    return Err(format!("'{}' is a frame list, so only its fps can be set here; use anim_frames to change its frames, or anim_clear it first to make a grid", c.name));
                }
                e.doc.anims[i].fps = fps;
                return Ok(anim_json(e, i));
            }
            if let (None, Some(n), false) = (at, a.os("name")?, geometry) {
                return Err(format!("there is no animation named '{n}' (to make one, give its region or frame size)"));
            }
            let grid = cur.as_ref().and_then(|c| c.grid());
            let (dw, dh) = (e.doc.w as i64, e.doc.h as i64);
            let base = match (grid, e.doc.sel_bounds()) {
                (Some(c), _) => (c.x as i64, c.y as i64, c.w as i64, c.h as i64),
                (None, Some((x0, y0, x1, y1))) => (x0 as i64, y0 as i64, (x1 - x0 + 1) as i64, (y1 - y0 + 1) as i64),
                (None, None) => (0, 0, dw, dh),
            };
            let (x, y) = (a.oi("x")?.unwrap_or(base.0), a.oi("y")?.unwrap_or(base.1));
            let (w, h) = (a.oi("w")?.unwrap_or(base.2), a.oi("h")?.unwrap_or(base.3));
            // The part of the region that lies on the image.
            let (x0, y0, x1, y1) = (x.clamp(0, dw), y.clamp(0, dh), (x + w).clamp(0, dw), (y + h).clamp(0, dh));
            if x1 <= x0 || y1 <= y0 {
                return Err("the region is empty or outside the image".into());
            }
            let (w, h) = (x1 - x0, y1 - y0);
            let fw = a.oi("frame_w")?.or(grid.map(|c| c.fw as i64)).unwrap_or(e.view.grid_w as i64);
            let fh = a.oi("frame_h")?.or(grid.map(|c| c.fh as i64)).unwrap_or(e.view.grid_h as i64);
            if fw < 1 || fh < 1 {
                return Err("frame_w and frame_h must be at least 1".into());
            }
            if fw > w || fh > h {
                return Err(format!("a {fw}x{fh} frame does not fit in the {w}x{h} region"));
            }
            let total = (w / fw) * (h / fh);
            let limit = a.oi("frames")?.or(grid.map(|c| c.limit as i64)).unwrap_or(0);
            if limit < 0 {
                return Err("frames must be 0 (all) or more".into());
            }
            let g = Grid { x: x0 as u32, y: y0 as u32, w: w as u32, h: h as u32, fw: fw as u32, fh: fh as u32, limit: if limit >= total { 0 } else { limit as u32 } };
            let anim = Anim { name: anim_name_for(e, a, at)?, fps, frames: Frames::Grid(g) };
            let i = anim_put(e, at, anim)?;
            Ok(anim_json(e, i))
        },
    );
    b.add(
        "animation",
        "anim_frames",
        Edit,
        "frames:array List of frames, each [x, y, w, h] or [x, y, w, h, offset_x, offset_y] (or an object with those names); \
         name:str? Which animation (default: the one on show) - a name that is not in use makes a new animation; fps:num? Frames per second, 1-60 (default 8)",
        "Set up a frame-list animation: every frame is its own rectangle of the image, so frames can differ in size and sit anywhere. \
         offset_x / offset_y say where a frame's top-left corner is drawn relative to the animation's origin, which keeps trimmed frames \
         of different sizes lined up (default 0, 0). Up to 4096 frames, each inside the image. Replaces the frames of an existing animation.",
        |e, a| {
            let at = anim_target(e, a)?;
            let fps = fps_arg(a, at.map(|i| e.doc.anims[i].fps))?;
            let frames = io::frames_from_json(a.arr("frames")?, e.doc.w, e.doc.h)?;
            let anim = Anim { name: anim_name_for(e, a, at)?, fps, frames };
            let i = anim_put(e, at, anim)?;
            Ok(anim_json(e, i))
        },
    );
    b.add(
        "animation",
        "anim_rename",
        Edit,
        "to:str The new name (1-64 characters, not already in use); name:str? Which animation (default: the one on show)",
        "Rename an animation.",
        |e, a| {
            let i = anim_need(e, a)?;
            let to = a.s("to")?;
            anim_name_check(&e.doc, to, Some(i))?;
            e.doc.anims[i].name = to.to_string();
            if i == e.view.anim_index {
                e.show_anim(i);
            }
            Ok(anim_json(e, i))
        },
    );
    b.add(
        "animation",
        "anim_clear",
        Edit,
        "name:str? Which animation (default: the one on show); all:bool? Remove every animation",
        "Remove an animation's setup (or all of them). No pixels change.",
        |e, a| {
            if a.b("all")? {
                e.doc.anims.clear();
            } else if a.os("name")?.is_some() || !e.doc.anims.is_empty() {
                let i = anim_need(e, a)?;
                e.doc.anims.remove(i);
                if i < e.view.anim_index {
                    e.show_anim(e.view.anim_index - 1);
                } else if i == e.view.anim_index {
                    e.show_anim(i.min(e.doc.anims.len().saturating_sub(1)));
                    e.view.anim_frame = 0;
                    e.view.anim_playing = false;
                }
            }
            if e.doc.anims.is_empty() {
                e.view.anim_playing = false;
            }
            Ok(json!({"ok": true, "count": e.doc.anims.len()}))
        },
    );
    b.add(
        "animation",
        "anim_control",
        Read,
        "name:str? Put this animation on show (it starts at its first frame); frame:int? Show this frame (0 is the first); \
         step:int? Move this many frames forward, or back when negative, wrapping round; playing:bool? Start or stop playback in the app window",
        "Choose which animation is on show, and step or play it. This is the playback position only: it is not saved and does not change the document.",
        |e, a| {
            let i = anim_need(e, a)?;
            if i != e.view.anim_index {
                e.show_anim(i);
                e.view.anim_frame = 0;
            }
            let n = e.doc.anims[i].count() as i64;
            if let Some(f) = a.oi("frame")? {
                if f < 0 || f >= n {
                    return Err(format!("frame {f} is out of range (0-{})", n - 1));
                }
                e.view.anim_frame = f as u32;
            }
            if let Some(d) = a.oi("step")? {
                e.view.anim_frame = (e.view.anim_frame as i64 + d).rem_euclid(n.max(1)) as u32;
            }
            if let Some(p) = a.ob("playing")? {
                e.view.anim_playing = p;
            }
            Ok(anim_json(e, i))
        },
    );
    b.add(
        "animation",
        "anim_info",
        Read,
        "name:str? Which animation (default: the one on show)",
        "One animation's setup and playback position: name, kind (grid or frames), frames, fps, the current frame and where it is in the image \
         (frame_x, frame_y, frame_w, frame_h, offset_x, offset_y), the box that holds every frame (box_w, box_h), and for a grid its region, cols and rows. \
         active is false when there is none.",
        |e, a| {
            if a.os("name")?.is_some() {
                return Ok(anim_json(e, anim_need(e, a)?));
            }
            Ok(anim_json(e, e.view.anim_index))
        },
    );
    b.add("animation", "anim_list", Read, "", "Every animation in the document, in order, and which one is on show (current).", |e, _| {
        Ok(json!({"current": if e.doc.anims.is_empty() { Value::Null } else { json!(e.view.anim_index) },
                  "animations": (0..e.doc.anims.len()).map(|i| anim_json(e, i)).collect::<Vec<_>>()}))
    });
    b.add(
        "animation",
        "anim_export_json",
        Read,
        "path:str Destination .json file",
        "Write every animation to a JSON file: {\"format\": \"jon-motion-animations\", \"version\": 1, \"image\": {width, height}, \"animations\": [...]}, \
         where each animation is {name, fps, grid: {x, y, w, h, frame_w, frame_h, frames}} or {name, fps, frames: [[x, y, w, h] or [x, y, w, h, offset_x, offset_y], ...]}. \
         An existing file is only overwritten if it is itself an animations file and nothing else, and nothing is written in the app's own settings folder.",
        |e, a| {
            let p = path_arg(a, "json")?;
            if e.doc.anims.is_empty() {
                return Err("there are no animations to export".into());
            }
            anim_export_allowed(&p, crate::remote::settings_file().as_deref().and_then(|f| f.parent()))?;
            write_file(&p, io::anims_to_text(&e.doc).as_bytes())?;
            Ok(json!({"ok": true, "animations": e.doc.anims.len()}))
        },
    );
    b.add(
        "animation",
        "anim_import_json",
        Edit,
        "path:str A .json file written by anim_export_json (or by hand in the same form); replace:bool? Remove the document's own animations first (default false)",
        "Read animations from a JSON file. Each one is checked against this image (frames inside it, 1-60 fps, sound names); if any is refused \
         nothing is imported. An animation with the name of an existing one replaces it; the others are added.",
        |e, a| {
            let p = PathBuf::from(a.s("path")?);
            let list = io::anims_from_json(&anim_file(&p)?, e.doc.w, e.doc.h)?.into_iter().collect::<Result<Vec<Anim>, String>>().map_err(|err| format!("{}: {err}", p.display()))?;
            if let Some(n) = list.iter().enumerate().find_map(|(i, x)| list[..i].iter().any(|y| y.name == x.name).then_some(&x.name)) {
                return Err(format!("{}: two animations are named '{n}'", p.display()));
            }
            let mut all = if a.b("replace")? { vec![] } else { e.doc.anims.clone() };
            for x in list.iter().cloned() {
                match all.iter().position(|y| y.name == x.name) {
                    Some(i) => all[i] = x,
                    None => all.push(x),
                }
            }
            if all.len() > Anim::MAX {
                return Err(format!("that would make {} animations; a document holds at most {}", all.len(), Anim::MAX));
            }
            if Anim::list_frames(&all) > Anim::MAX_LIST_FRAMES {
                return Err(format!("that would make {} frame-list frames; a document holds at most {}", Anim::list_frames(&all), Anim::MAX_LIST_FRAMES));
            }
            e.doc.anims = all;
            // Show the first one that came in.
            if let Some(i) = list.first().and_then(|x| e.doc.anims.iter().position(|y| y.name == x.name)) {
                e.show_anim(i);
                e.view.anim_frame = 0;
            }
            Ok(json!({"ok": true, "imported": list.len(), "count": e.doc.anims.len()}))
        },
    );
}

fn tab_cmds(b: &mut B) {
    use Kind::*;
    b.add(
        "tabs",
        "tab_list",
        Read,
        "",
        "The open documents (tabs) in order: id, name, path, modified, width, height and which one is active. Every other command works on the active document.",
        |e, _| Ok(json!({"active": e.id, "tabs": e.tab_list()})),
    );
    b.add(
        "tabs",
        "tab_new",
        Tab,
        "width:int? Width in pixels (default: the active document's); height:int? Height in pixels (default: the active document's); \
         palette:str? Palette preset name (default 'default'); reuse_blank:bool? Use the active tab instead when it is a blank document nobody has touched",
        "Open a blank document in a new tab and make it active. Nothing is replaced, so nothing is lost. Up to 32 documents can be open. \
         Tab commands cannot be used inside a script or batch, which work on one document from start to finish.",
        |e, a| {
            let p = json!({"width": a.oi("width")?.unwrap_or(e.doc.w as i64), "height": a.oi("height")?.unwrap_or(e.doc.h as i64),
                           "palette": a.os("palette")?.unwrap_or("default"), "force": true});
            if a.b("reuse_blank")? && e.pristine() {
                e.exec("new_document", &p)?;
                return Ok(json!({"id": e.id, "reused": true}));
            }
            let mut t = Engine::new(e.host);
            t.exec("new_document", &p)?;
            Ok(json!({"id": e.tab_add(t)?}))
        },
    );
    b.add(
        "tabs",
        "tab_open",
        Tab,
        "path:str A .jmn project or a .png image; reuse_blank:bool? Use the active tab instead when it is a blank document nobody has touched",
        "Open a file in a new tab and make it active. If that project is already open, its tab is made active instead (already_open is true) and it is not reloaded.",
        |e, a| {
            let p = PathBuf::from(a.s("path")?);
            if let Some(id) = e.tab_with_path(&p) {
                e.tab_select(id)?;
                return Ok(json!({"id": id, "already_open": true}));
            }
            let open = json!({"path": a.s("path")?, "force": true});
            if a.b("reuse_blank")? && e.pristine() {
                e.exec("open_document", &open)?;
                return Ok(json!({"id": e.id, "reused": true}));
            }
            if e.order.len() >= MAX_TABS {
                return Err(format!("{MAX_TABS} documents are open, which is the limit; close one first"));
            }
            let mut t = Engine::new(e.host);
            t.exec("open_document", &open)?;
            Ok(json!({"id": e.tab_add(t)?}))
        },
    );
    b.add("tabs", "tab_select", Tab, "id:int The document's id (see tab_list)", "Make another open document the active one. Its own undo history, selection, view and colours come with it.", |e, a| {
        e.tab_select(a.i("id")?.clamp(0, u32::MAX as i64) as u32)?;
        Ok(json!({"active": e.id}))
    });
    b.add(
        "tabs",
        "tab_close",
        Tab,
        "id:int? The document's id (default: the active one); force:bool? Discard unsaved changes",
        "Close a document. One with unsaved changes is refused unless force is true. Closing the last one leaves a blank document.",
        |e, a| {
            let id = a.oi("id")?.map(|i| i.clamp(0, u32::MAX as i64) as u32).unwrap_or(e.id);
            let t = e.tab(id).ok_or_else(|| format!("no open document has id {id} (see tab_list)"))?;
            if t.modified && !a.b("force")? {
                return Err(format!("document {id} ({}) has unsaved changes; save it first, or pass force=true to discard them", t.title()));
            }
            e.tab_close(id)?;
            Ok(json!({"active": e.id, "open_documents": e.order.len()}))
        },
    );
}

/// Where an export may be written. Never over some other program's .json: an existing file
/// must be an animations file with nothing else in it. Nor into the app's own folder, where
/// the settings and the access token live.
pub fn anim_export_allowed(p: &Path, app_dir: Option<&Path>) -> Result<(), String> {
    let ours = |v: Value| v.as_object().is_some_and(|o| o.keys().all(|k| matches!(k.as_str(), "format" | "version" | "image" | "animations")));
    if std::fs::symlink_metadata(p).is_ok() && !anim_file(p).is_ok_and(ours) {
        return Err(format!("{} already exists and is not a Jon Motion animations file; choose another name", p.display()));
    }
    let folder = |d: &Path| std::fs::canonicalize(d).unwrap_or_else(|_| d.to_path_buf());
    let parent = p.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    if app_dir.is_some_and(|d| folder(d) == folder(parent)) {
        return Err("animations cannot be exported into the app's settings folder; choose another place".into());
    }
    Ok(())
}

/// Read an animations file: a regular .json file of a sensible size holding our format.
fn anim_file(p: &Path) -> Result<Value, String> {
    let err = |why: &str| format!("{}: {why}", p.display());
    if !p.extension().is_some_and(|x| x.eq_ignore_ascii_case("json")) {
        return Err(err("an animations file must end in .json"));
    }
    let m = std::fs::metadata(p).map_err(|e| err(&e.to_string()))?;
    if !m.is_file() || m.len() > io::ANIM_JSON_MAX {
        return Err(err("not a file, or larger than 8 MB"));
    }
    let v: Value = serde_json::from_slice(&std::fs::read(p).map_err(|e| err(&e.to_string()))?).map_err(|_| err("not valid JSON"))?;
    if v.get("format").and_then(|f| f.as_str()) != Some(io::ANIM_FORMAT) {
        return Err(err(&format!("not a Jon Motion animations file (it needs \"format\": \"{}\")", io::ANIM_FORMAT)));
    }
    Ok(v)
}
