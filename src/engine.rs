//! The editor engine: document, undo history, strokes and tile synchronisation.
//! Every user-facing feature is reached through `Engine::exec`, so the GUI,
//! Lua scripts and MCP clients all drive the same code.

use crate::commands::{self, Args, Kind};
use crate::doc::*;
use crate::paint::*;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

const MAX_UNDO: usize = 100;
const MIN_UNDO: usize = 8;
/// Rough memory budget for undo history (each step can hold a whole layer).
const UNDO_BYTES: usize = 1 << 30;

fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "panic".into())
}

/// Turn a panic inside `f` into an error, so one bad command cannot take the editor down.
pub fn guarded<R>(f: impl FnOnce() -> Result<R, String>) -> Result<R, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|p| Err(format!("internal error: {}", panic_text(&*p))))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Pencil,
    Eraser,
    Line,
    Rect,
    Ellipse,
    Fill,
    Gradient,
    Picker,
    Select,
    Wand,
    Move,
    TileStamp,
    Pan,
}

impl Tool {
    pub const ALL: [Tool; 13] = [
        Tool::Pencil,
        Tool::Eraser,
        Tool::Line,
        Tool::Rect,
        Tool::Ellipse,
        Tool::Fill,
        Tool::Gradient,
        Tool::Picker,
        Tool::Select,
        Tool::Wand,
        Tool::Move,
        Tool::TileStamp,
        Tool::Pan,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Tool::Pencil => "pencil",
            Tool::Eraser => "eraser",
            Tool::Line => "line",
            Tool::Rect => "rect",
            Tool::Ellipse => "ellipse",
            Tool::Fill => "fill",
            Tool::Gradient => "gradient",
            Tool::Picker => "picker",
            Tool::Select => "select",
            Tool::Wand => "wand",
            Tool::Move => "move",
            Tool::TileStamp => "tile_stamp",
            Tool::Pan => "pan",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Tool::Pencil => "Pencil",
            Tool::Eraser => "Eraser",
            Tool::Line => "Line",
            Tool::Rect => "Rectangle",
            Tool::Ellipse => "Ellipse",
            Tool::Fill => "Fill",
            Tool::Gradient => "Gradient",
            Tool::Picker => "Colour picker",
            Tool::Select => "Select",
            Tool::Wand => "Magic wand",
            Tool::Move => "Move",
            Tool::TileStamp => "Tile stamp",
            Tool::Pan => "Pan",
        }
    }
    pub fn key(self) -> char {
        match self {
            Tool::Pencil => 'B',
            Tool::Eraser => 'E',
            Tool::Line => 'L',
            Tool::Rect => 'R',
            Tool::Ellipse => 'O',
            Tool::Fill => 'F',
            Tool::Gradient => 'G',
            Tool::Picker => 'I',
            Tool::Select => 'M',
            Tool::Wand => 'W',
            Tool::Move => 'V',
            Tool::TileStamp => 'T',
            Tool::Pan => 'H',
        }
    }
    pub fn parse(s: &str) -> Option<Tool> {
        Tool::ALL.into_iter().find(|t| t.name() == s.to_ascii_lowercase())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TileMode {
    /// Edited cells are matched to an existing tile or become a new tile.
    Auto,
    /// Editing a cell edits its tile, so every instance changes together.
    Sync,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShapeKind {
    Line,
    Rect,
    Ellipse,
}

/// Interactive tool settings. Scripted drawing commands take explicit
/// parameters instead, so they never depend on what the user has selected.
#[derive(Clone)]
pub struct Opts {
    pub tool: Tool,
    pub fg: Cell,
    pub bg: Cell,
    pub brush_size: u32,
    pub brush_shape: BrushShape,
    pub custom_brush: Option<Brush>,
    pub mode: PaintMode,
    pub dither: u8,
    pub dither_bg: bool,
    pub filled: bool,
    pub pixel_perfect: bool,
    pub sym_x: bool,
    pub sym_y: bool,
    pub contiguous: bool,
    pub tile_mode: TileMode,
    pub active_tile: u16,
    pub active_ramp: Option<usize>,
    pub gradient_radial: bool,
    pub gradient_dither: bool,
}

/// Which set of single-key shortcuts the window uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Keymap {
    Standard,
    /// Deluxe Paint's keys (see `keys::DPAINT`).
    DPaint,
}

impl Keymap {
    pub const ALL: [Keymap; 2] = [Keymap::Standard, Keymap::DPaint];
    pub fn name(self) -> &'static str {
        match self {
            Keymap::Standard => "standard",
            Keymap::DPaint => "dpaint",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Keymap::Standard => "Standard",
            Keymap::DPaint => "Deluxe Paint",
        }
    }
    pub fn parse(s: &str) -> Option<Keymap> {
        Keymap::ALL.into_iter().find(|k| k.name() == s.to_ascii_lowercase())
    }
}

#[derive(Clone)]
pub struct View {
    pub keymap: Keymap,
    pub zoom: f32,
    pub cx: f32,
    pub cy: f32,
    pub fit: bool,
    pub pixel_grid: bool,
    pub grid: bool,
    pub grid_w: u32,
    pub grid_h: u32,
    pub tile_grid: bool,
    pub tile_ids: bool,
    pub cycling: bool,
    /// Which animation frame the window shows, and whether it is playing. The animation's
    /// setup lives in the document (`Doc::anim`); this playback position does not.
    pub anim_frame: u32,
    pub anim_playing: bool,
    /// Which of the document's animations is on show.
    pub anim_index: usize,
    /// Draw the animation's frame cuts on the canvas.
    pub anim_guides: bool,
}

#[derive(Clone)]
pub struct Clip {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub data: Vec<Cell>,
    /// The palette the cells were copied under, so a paste into another palette keeps its look.
    pub pal: Vec<[u8; 3]>,
}

pub struct Stroke {
    pub base: Doc,
    pub layer: usize,
    pub touched: Vec<bool>,
    pub last: Option<(i32, i32)>,
    pub opts: PaintOpts,
    pp: Vec<(i32, i32)>,
}

pub struct Snapshot {
    doc: Doc,
    path: Option<PathBuf>,
    modified: bool,
    fg: Cell,
    bg: Cell,
    tiles_dirty: Option<bool>,
    reset_history: bool,
}

pub struct Engine {
    pub doc: Doc,
    pub undo: Vec<Doc>,
    pub redo: Vec<Doc>,
    pub o: Opts,
    pub view: View,
    pub clipboard: Option<Clip>,
    pub path: Option<PathBuf>,
    pub modified: bool,
    /// Bumped on every change; the GUI redraws when it moves.
    pub rev: u64,
    pub stroke: Option<Stroke>,
    /// Undo snapshot held while the user is mid-gesture.
    pending: Option<Doc>,
    /// Depth of command/script nesting; only the outermost level records undo.
    pub group: u32,
    /// Some(painted) when tile-mapped layers may be out of step with the tile set.
    tiles_dirty: Option<bool>,
    pub reset_history: bool,
    coalesce: Option<(String, u64)>,
    /// Bumped whenever undo history changes; lets coalescing spot unrelated edits.
    undo_seq: u64,
    /// Set while a coalesced edit continues: the earlier undo step already covers it.
    merge_undo: bool,
    /// Count of document edits, and its value at the last save, so a save made
    /// part-way through a script still leaves the document marked clean.
    edits: u64,
    pub saved_at: Option<u64>,
    /// Where the current command, script or batch last saved the document.
    saved_to: Option<PathBuf>,
    /// True once a script on this engine replaced the document (new/open).
    was_reset: bool,
    /// Set on a script's working copy when the script is abandoned: it then refuses every command.
    halt: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// "gui" or "headless"; reported by get_info.
    pub host: &'static str,
}

fn tile_dims(doc: &Doc) -> Option<(usize, usize, usize, usize)> {
    let ts = doc.tileset.as_ref()?;
    let (tw, th) = (ts.tw as usize, ts.th as usize);
    Some((tw, th, doc.w as usize / tw, doc.h as usize / th))
}

fn cell_eq(data: &[Cell], w: usize, cx: usize, cy: usize, tw: usize, th: usize, tile: &[Cell]) -> bool {
    (0..th).all(|py| data[(cy * th + py) * w + cx * tw..][..tw] == tile[py * tw..][..tw])
}

fn cell_pixels(data: &[Cell], w: usize, cx: usize, cy: usize, tw: usize, th: usize) -> Vec<Cell> {
    let mut out = Vec::with_capacity(tw * th);
    for py in 0..th {
        out.extend_from_slice(&data[(cy * th + py) * w + cx * tw..][..tw]);
    }
    out
}

fn blit_tile(data: &mut [Cell], w: usize, cx: usize, cy: usize, tw: usize, th: usize, tile: &[Cell]) {
    for py in 0..th {
        data[(cy * th + py) * w + cx * tw..][..tw].copy_from_slice(&tile[py * tw..][..tw]);
    }
}

impl Engine {
    pub fn new(host: &'static str) -> Engine {
        Engine {
            doc: Doc::new(320, 240, Palette::default_256()),
            undo: vec![],
            redo: vec![],
            o: Opts {
                tool: Tool::Pencil,
                fg: 15,
                bg: 0,
                brush_size: 1,
                brush_shape: BrushShape::Round,
                custom_brush: None,
                mode: PaintMode::Normal,
                dither: 8,
                dither_bg: false,
                filled: false,
                pixel_perfect: false,
                sym_x: false,
                sym_y: false,
                contiguous: true,
                tile_mode: TileMode::Auto,
                active_tile: 0,
                active_ramp: None,
                gradient_radial: false,
                gradient_dither: true,
            },
            view: View {
                keymap: Keymap::Standard,
                zoom: 2.0,
                cx: 160.0,
                cy: 120.0,
                fit: true,
                pixel_grid: true,
                grid: false,
                grid_w: 16,
                grid_h: 16,
                tile_grid: true,
                tile_ids: false,
                cycling: false,
                anim_frame: 0,
                anim_playing: false,
                anim_index: 0,
                anim_guides: true,
            },
            clipboard: None,
            path: None,
            modified: false,
            rev: 1,
            stroke: None,
            pending: None,
            group: 0,
            tiles_dirty: None,
            reset_history: false,
            coalesce: None,
            undo_seq: 0,
            merge_undo: false,
            edits: 0,
            saved_at: None,
            saved_to: None,
            was_reset: false,
            halt: None,
            host,
        }
    }

    /// A working copy for a script to run on. The script's result is taken over with
    /// `adopt`; if the script fails or never finishes the copy is simply dropped, so the
    /// live document cannot be left half-changed or held hostage by a runaway script.
    pub fn fork(&mut self, halt: Arc<std::sync::atomic::AtomicBool>) -> Engine {
        self.tiles_flush();
        let mut e = Engine::new(self.host);
        e.doc = self.doc.clone();
        e.o = self.o.clone();
        e.view = self.view.clone();
        e.clipboard = self.clipboard.clone();
        e.path = self.path.clone();
        e.modified = self.modified;
        // History is only there so get_info reports the same counts; a script cannot undo.
        e.undo = self.undo.clone();
        e.redo = self.redo.clone();
        e.halt = Some(halt);
        e
    }

    /// Take over the state a script left in its working copy, as one undo step.
    pub fn adopt(&mut self, s: Engine) {
        let (modified, reset) = (s.modified, s.was_reset);
        let _ = self.group(|e| {
            e.doc = s.doc;
            e.o = s.o;
            e.view = s.view;
            e.clipboard = s.clipboard;
            e.path = s.path;
            e.reset_history = reset;
            Ok(())
        });
        self.modified = modified;
    }

    /// True while the user is mid-gesture; remote commands wait for it to finish.
    pub fn busy(&self) -> bool {
        self.pending.is_some() || self.stroke.is_some()
    }

    // ------------------------------------------------------------ colours

    pub fn parse_color(&self, v: &Value) -> Result<Cell, String> {
        match v {
            Value::Number(n) => {
                let i = n.as_f64().unwrap_or(0.0).floor() as i64;
                if i < 0 {
                    return Ok(TRANSP);
                }
                if i as usize >= self.doc.palette.colors.len() {
                    return Err(format!("palette index {i} out of range (palette has {} colours)", self.doc.palette.colors.len()));
                }
                Ok(i as Cell)
            }
            Value::String(s) => match s.as_str() {
                "transparent" | "none" | "clear" => Ok(TRANSP),
                "fg" => Ok(self.o.fg),
                "bg" => Ok(self.o.bg),
                _ => parse_hex(s).map(|c| self.doc.from_rgba(c)).ok_or_else(|| format!("bad colour '{s}' (use a palette index, '#rrggbb' for the nearest palette colour, 'transparent', 'fg' or 'bg')")),
            },
            Value::Array(a) if a.len() == 3 || a.len() == 4 => {
                let ch = |i: usize| a.get(i).and_then(|v| v.as_f64()).map(|v| v.clamp(0.0, 255.0) as u8);
                let c = [ch(0).ok_or("bad colour array")?, ch(1).ok_or("bad colour array")?, ch(2).ok_or("bad colour array")?, ch(3).unwrap_or(255)];
                Ok(self.doc.from_rgba(c))
            }
            _ => Err("colour must be a palette index, a hex string or [r,g,b,a]".into()),
        }
    }

    pub fn cell_json(&self, c: Cell) -> Value {
        if c >= 256 {
            Value::from(-1)
        } else {
            Value::from(c)
        }
    }

    /// Swap in a different document, carrying FG/BG across to its palette.
    pub fn replace_doc(&mut self, new: Doc) -> Doc {
        let (fg, bg) = (self.doc.to_rgba(self.o.fg), self.doc.to_rgba(self.o.bg));
        let old = std::mem::replace(&mut self.doc, new);
        if old.palette.colors != self.doc.palette.colors {
            self.o.fg = self.doc.from_rgba(fg);
            self.o.bg = self.doc.from_rgba(bg);
            // The custom brush keeps its look too (undoing a palette sort or swap renumbers it).
            if let Some(cells) = self.o.custom_brush.as_mut().and_then(|b| b.cells.as_mut()) {
                let map: Vec<Cell> = (0..old.palette.colors.len()).map(|i| self.doc.from_rgba(old.palette.rgba(i))).collect();
                cells.iter_mut().for_each(|c| *c = map.get(*c as usize).copied().unwrap_or(*c));
            }
        }
        self.fix_state();
        old
    }

    /// The animation on show, if the document has any.
    pub fn anim(&self) -> Option<&Anim> {
        self.doc.anims.get(self.view.anim_index)
    }

    /// Clamp editor state that refers into the document.
    pub fn fix_state(&mut self) {
        let d = &self.doc;
        let n = d.palette.colors.len() as Cell;
        if self.o.fg != TRANSP && self.o.fg >= n {
            self.o.fg = n - 1;
        }
        if self.o.bg != TRANSP && self.o.bg >= n {
            self.o.bg = 0;
        }
        if self.o.active_ramp.is_some_and(|r| r >= d.palette.ramps.len()) {
            self.o.active_ramp = None;
        }
        let tiles = d.tileset.as_ref().map(|t| t.tiles.len()).unwrap_or(0);
        if self.o.active_tile as usize >= tiles {
            self.o.active_tile = 0;
        }
        if self.doc.active >= self.doc.layers.len() {
            self.doc.active = self.doc.layers.len() - 1;
        }
        // Animations whose frames no longer lie inside the image (after a crop or resize) are dropped.
        let (w, h) = (self.doc.w, self.doc.h);
        if self.doc.anims.iter().any(|a| !a.fits(w, h)) {
            self.doc.anims.retain(|a| a.fits(w, h));
        }
        self.view.anim_index = self.view.anim_index.min(self.doc.anims.len().saturating_sub(1));
        let frames = self.anim().map(|a| a.count()).unwrap_or(0);
        self.view.anim_frame = self.view.anim_frame.min(frames.saturating_sub(1));
        if frames == 0 {
            self.view.anim_playing = false;
        }
    }

    // ------------------------------------------------------------ undo

    fn push_undo(&mut self, before: Doc) {
        if before != self.doc {
            if !self.merge_undo {
                self.undo.push(before);
                self.undo_seq += 1;
                let max = (UNDO_BYTES / (self.doc.len() * 4).max(1)).clamp(MIN_UNDO, MAX_UNDO);
                while self.undo.len() > max {
                    self.undo.remove(0);
                }
            }
            self.redo.clear();
            self.modified = true;
        }
    }

    pub fn undo(&mut self) -> Result<bool, String> {
        if self.busy() || self.group > 0 {
            return Err("undo is not available inside a script, batch or stroke".into());
        }
        let Some(d) = self.undo.pop() else { return Ok(false) };
        let cur = self.replace_doc(d);
        self.redo.push(cur);
        self.undo_seq += 1;
        self.modified = true;
        self.rev += 1;
        Ok(true)
    }

    pub fn redo(&mut self) -> Result<bool, String> {
        if self.busy() || self.group > 0 {
            return Err("redo is not available inside a script, batch or stroke".into());
        }
        let Some(d) = self.redo.pop() else { return Ok(false) };
        let cur = self.replace_doc(d);
        self.undo.push(cur);
        self.undo_seq += 1;
        self.modified = true;
        self.rev += 1;
        Ok(true)
    }

    /// Start a user gesture (stroke, drag); one undo step is recorded when it ends.
    pub fn begin_interaction(&mut self) {
        if self.group == 0 && self.pending.is_none() {
            self.pending = Some(self.doc.clone());
        }
    }

    pub fn end_interaction(&mut self) {
        if self.group == 0 {
            if let Some(before) = self.pending.take() {
                self.push_undo(before);
            }
        }
        self.rev += 1;
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            doc: self.doc.clone(),
            path: self.path.clone(),
            modified: self.modified,
            fg: self.o.fg,
            bg: self.o.bg,
            tiles_dirty: self.tiles_dirty,
            reset_history: self.reset_history,
        }
    }

    fn restore(&mut self, s: Snapshot) {
        self.doc = s.doc;
        self.path = s.path;
        self.modified = s.modified;
        self.o.fg = s.fg;
        self.o.bg = s.bg;
        self.tiles_dirty = s.tiles_dirty;
        self.reset_history = s.reset_history;
    }

    fn begin_top(&mut self) -> Option<Snapshot> {
        if self.group == 0 {
            self.saved_to = None;
        }
        (self.group == 0).then(|| self.snapshot())
    }

    fn end_top(&mut self, snap: Option<Snapshot>, ok: bool) {
        let Some(s) = snap else { return };
        if ok {
            self.tiles_flush();
            if self.reset_history {
                self.was_reset = true;
                self.undo.clear();
                self.redo.clear();
                self.pending = None;
            } else if self.pending.is_none() {
                self.push_undo(s.doc);
            }
            if self.saved_at == Some(self.edits) {
                self.modified = false;
            }
        } else {
            self.restore(s);
            self.tiles_dirty = None;
            // A save made before the failure wrote changes that have now been rolled
            // back, so the file no longer matches the document.
            if self.saved_to.is_some() && self.saved_to == self.path {
                self.modified = true;
            }
        }
        self.saved_to = None;
        self.reset_history = false;
        self.saved_at = None;
        self.fix_state();
        self.rev += 1;
    }

    /// Run several operations as one atomic, single-undo-step change.
    /// On error the document is rolled back to how it was.
    pub fn group<R>(&mut self, f: impl FnOnce(&mut Engine) -> Result<R, String>) -> Result<R, String> {
        let snap = self.begin_top();
        let (depth, had_stroke) = (self.group, self.stroke.is_some());
        self.group += 1;
        let r = guarded(|| f(self));
        // A panic can leave these behind; put them back so the editor stays usable.
        self.group = depth;
        if !had_stroke {
            self.stroke = None;
        }
        self.end_top(snap, r.is_ok());
        r
    }

    /// Execute a named command with JSON parameters.
    pub fn exec(&mut self, name: &str, params: &Value) -> Result<Value, String> {
        if self.halt.as_ref().is_some_and(|h| h.load(std::sync::atomic::Ordering::Relaxed)) {
            return Err("the script was stopped".into());
        }
        let cmd = commands::find(name).ok_or_else(|| format!("unknown command '{name}' (see list_commands)"))?;
        let empty = serde_json::Map::new();
        let map = match params {
            Value::Object(m) => m,
            Value::Null => &empty,
            Value::Array(a) if a.is_empty() => &empty,
            _ => return Err(format!("{name}: parameters must be an object")),
        };
        cmd.validate(map)?;
        let args = Args(map);
        if let Some(painted) = self.tiles_dirty {
            let kind_changed = cmd.kind != Kind::Read && (cmd.kind == Kind::Paint) != painted;
            if kind_changed || cmd.cat == "tiles" || matches!(cmd.name, "get_info" | "save_document") {
                self.tiles_flush();
            }
        }
        if cmd.kind == Kind::Read {
            let r = guarded(|| (cmd.run)(self, &args));
            self.rev += 1;
            return r.map_err(|e| format!("{name}: {e}"));
        }
        if self.stroke.is_some() {
            return Err("busy: a stroke is in progress".into());
        }
        // Inside a script or batch each command still fails cleanly on its own,
        // so a caught error (pcall) never leaves half a command behind.
        let nested = (self.group > 0).then(|| self.snapshot());
        let r = self.group(|e| {
            if !cmd.float_ok {
                e.doc.float_commit();
            }
            let r = (cmd.run)(e, &args);
            e.touch(cmd.kind == Kind::Paint, matches!(cmd.name, "new_document" | "open_document"));
            r
        });
        if let (Err(_), Some(s)) = (&r, nested) {
            self.restore(s);
        }
        r.map_err(|e| format!("{name}: {e}"))
    }

    /// Like `exec`, but consecutive calls with the same key share one undo step
    /// (used for continuous edits such as dragging a colour slider).
    pub fn exec_coalesced(&mut self, name: &str, params: &Value, key: &str) -> Result<Value, String> {
        let before = self.undo_seq;
        let cont = self.coalesce.as_ref().is_some_and(|(k, n)| k == key && *n == before);
        self.merge_undo = cont;
        let r = self.exec(name, params);
        self.merge_undo = false;
        let r = r?;
        if !cont && self.undo_seq == before + 1 {
            self.coalesce = Some((key.to_string(), self.undo_seq));
        }
        Ok(r)
    }

    /// Note a document change made inside a command, script or batch.
    pub fn touch(&mut self, painted: bool, replaced_doc: bool) {
        self.edits += 1;
        // Edits after new/open in the same script leave the fresh document unsaved.
        if self.reset_history && !replaced_doc {
            self.modified = true;
        }
        self.mark_dirty(painted);
    }

    /// Record that the document was just written to disk.
    pub fn mark_saved(&mut self) {
        self.modified = false;
        self.saved_at = Some(self.edits);
        self.saved_to = self.path.clone();
    }

    /// Direct pixel read for scripts.
    pub fn raw_get(&self, x: i64, y: i64) -> Option<Cell> {
        let d = &self.doc;
        (x >= 0 && y >= 0 && x < d.w as i64 && y < d.h as i64).then(|| d.layers[d.active].data[(y * d.w as i64 + x) as usize])
    }

    /// Direct pixel write for scripts (active layer; ignores selection and paint modes).
    pub fn raw_set(&mut self, x: i64, y: i64, c: Cell) -> Result<(), String> {
        let (w, h, a) = (self.doc.w as i64, self.doc.h as i64, self.doc.active);
        if x < 0 || y < 0 || x >= w || y >= h {
            return Ok(());
        }
        if self.doc.layers[a].locked {
            return Err("the active layer is locked".into());
        }
        if self.doc.float.is_some() {
            self.doc.float_commit();
        }
        Arc::make_mut(&mut self.doc.layers[a].data)[(y * w + x) as usize] = c;
        if self.tiles_dirty != Some(true) {
            self.touch(true, false);
        } else {
            self.edits += 1;
        }
        Ok(())
    }

    pub fn mark_dirty(&mut self, painted: bool) {
        if self.tiles_dirty.is_some_and(|p| p != painted) {
            self.tiles_flush();
        }
        self.tiles_dirty = Some(painted);
    }

    // ------------------------------------------------------------ strokes

    /// Paint options from the interactive tool settings.
    pub fn paint_opts(&self, secondary: bool, erase: bool) -> PaintOpts {
        let (mut color, bg) = if secondary { (self.o.bg, self.o.fg) } else { (self.o.fg, self.o.bg) };
        let mut brush = self.o.custom_brush.clone().unwrap_or_else(|| Brush::new(self.o.brush_size, self.o.brush_shape));
        let mut mode = self.o.mode;
        if erase {
            color = TRANSP;
            brush.cells = None;
            mode = PaintMode::Normal;
        }
        PaintOpts {
            color,
            bg,
            mode,
            dither: self.o.dither,
            dither_bg: self.o.dither_bg,
            brush,
            sym_x: self.o.sym_x,
            sym_y: self.o.sym_y,
            pixel_perfect: self.o.pixel_perfect,
            ramp: self.o.active_ramp,
        }
    }

    pub fn stroke_begin(&mut self, opts: PaintOpts) -> Result<(), String> {
        if self.stroke.is_some() {
            self.stroke_end();
        }
        let l = &self.doc.layers[self.doc.active];
        if l.locked {
            return Err(format!("layer '{}' is locked", l.name));
        }
        if !l.visible {
            return Err(format!("layer '{}' is hidden", l.name));
        }
        self.begin_interaction();
        self.doc.float_commit();
        self.stroke = Some(Stroke {
            base: self.doc.clone(),
            layer: self.doc.active,
            touched: vec![false; self.doc.len()],
            last: None,
            opts,
            pp: vec![],
        });
        Ok(())
    }

    fn paint(&mut self, f: impl FnOnce(&mut Painter)) {
        let Some(st) = self.stroke.as_mut() else { return };
        let doc = &mut self.doc;
        let li = st.layer;
        let alpha_lock = doc.layers[li].alpha_lock;
        let data = Arc::make_mut(&mut doc.layers[li].data);
        let mut p = Painter {
            w: doc.w as i32,
            h: doc.h as i32,
            pal: &doc.palette,
            data,
            base: &st.base.layers[li].data,
            sel: doc.selection.as_deref().map(|v| v.as_slice()),
            touched: &mut st.touched,
            alpha_lock,
            o: &st.opts,
        };
        f(&mut p);
    }

    fn step_done(&mut self) {
        if self.o.tile_mode == TileMode::Sync {
            self.tiles_sync_pass();
        }
        self.rev += 1;
    }

    /// Extend a freehand stroke to (x, y).
    pub fn stroke_to(&mut self, x: i32, y: i32) {
        let Some(st) = self.stroke.as_mut() else { return };
        let last = st.last;
        if last == Some((x, y)) {
            return;
        }
        st.last = Some((x, y));
        let o = &st.opts;
        let pixel_perfect = o.pixel_perfect && o.brush.w == 1 && o.brush.h == 1 && o.mode == PaintMode::Normal && !o.sym_x && !o.sym_y;
        if o.mode == PaintMode::Smear {
            if let Some((lx, ly)) = last {
                self.paint(|p| p.smear(lx, ly, x, y));
            }
        } else if pixel_perfect {
            let mut pp = std::mem::take(&mut st.pp);
            let pts = match last {
                Some((lx, ly)) => line_points(lx, ly, x, y).split_off(1),
                None => vec![(x, y)],
            };
            self.paint(|p| {
                for pt in pts {
                    pp.push(pt);
                    p.plot(pt.0, pt.1, None);
                    let n = pp.len();
                    if n >= 3 {
                        let (a, b, c) = (pp[n - 3], pp[n - 2], pp[n - 1]);
                        // b is the corner of an L: drop it so lines stay one pixel wide.
                        if (a.0 == b.0 || a.1 == b.1) && (b.0 == c.0 || b.1 == c.1) && a.0 != c.0 && a.1 != c.1 {
                            if b.0 >= 0 && b.1 >= 0 && b.0 < p.w && b.1 < p.h {
                                let i = (b.1 * p.w + b.0) as usize;
                                p.data[i] = p.base[i];
                                p.touched[i] = false;
                            }
                            pp.remove(n - 2);
                        }
                    }
                }
            });
            if let Some(st) = self.stroke.as_mut() {
                st.pp = pp;
            }
        } else {
            self.paint(|p| match last {
                Some((lx, ly)) => p.line(lx, ly, x, y, true),
                None => p.stamp(x, y),
            });
        }
        self.step_done();
    }

    fn stroke_reset(&mut self) {
        if let Some(st) = self.stroke.as_mut() {
            self.doc.layers = st.base.layers.clone();
            self.doc.tileset = st.base.tileset.clone();
            st.touched.fill(false);
        }
    }

    /// Redraw the stroke as a single shape (used for live previews and shape commands).
    pub fn stroke_shape(&mut self, kind: ShapeKind, x0: i32, y0: i32, x1: i32, y1: i32, filled: bool) {
        self.stroke_reset();
        self.paint(|p| match kind {
            ShapeKind::Line => p.line(x0, y0, x1, y1, false),
            ShapeKind::Rect => p.rect(x0, y0, x1, y1, filled),
            ShapeKind::Ellipse => p.ellipse(x0, y0, x1, y1, filled),
        });
        self.step_done();
    }

    pub fn stroke_gradient(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, stops: &[Cell], radial: bool, dither: bool) {
        self.stroke_reset();
        self.paint(|p| p.gradient(x0 as f32, y0 as f32, x1 as f32, y1 as f32, stops, radial, dither));
        self.step_done();
    }

    pub fn stroke_mask(&mut self, mask: &[bool]) {
        self.paint(|p| p.fill_mask(mask));
        self.step_done();
    }

    pub fn stroke_end(&mut self) {
        if self.stroke.take().is_none() {
            return;
        }
        if self.group == 0 {
            self.tiles_post(true);
        } else {
            self.mark_dirty(true);
        }
        self.end_interaction();
    }

    /// Colours the gradient tool runs through with the current settings.
    pub fn gradient_stops(&self, secondary: bool) -> Vec<Cell> {
        if let Some(r) = self.o.active_ramp.and_then(|r| self.doc.palette.ramps.get(r)) {
            let mut v: Vec<Cell> = r.indices.iter().map(|&i| i as Cell).collect();
            if secondary {
                v.reverse();
            }
            return v;
        }
        if secondary {
            vec![self.o.bg, self.o.fg]
        } else {
            vec![self.o.fg, self.o.bg]
        }
    }

    // ------------------------------------------------------------ clipboard

    pub fn copy(&mut self) -> Result<(), String> {
        self.clipboard = Some(self.grab()?);
        Ok(())
    }

    /// The selected pixels of the active layer (or floating pixels, or the whole layer).
    pub fn grab(&self) -> Result<Clip, String> {
        let d = &self.doc;
        if let Some(f) = &d.float {
            return Ok(Clip { x: f.x, y: f.y, w: f.w, h: f.h, data: f.data.to_vec(), pal: d.palette.colors.clone() });
        }
        let li = d.active;
        let (x0, y0, x1, y1) = match d.sel_bounds() {
            Some(b) => b,
            None if d.selection.is_some() => return Err("selection is empty".into()),
            None => (0, 0, d.w as i32 - 1, d.h as i32 - 1),
        };
        let (cw, ch) = ((x1 - x0 + 1) as usize, (y1 - y0 + 1) as usize);
        let mut data = vec![TRANSP; cw * ch];
        for y in 0..ch {
            for x in 0..cw {
                let (sx, sy) = (x0 + x as i32, y0 + y as i32);
                if d.sel_at(sx, sy) {
                    data[y * cw + x] = d.layers[li].data[(sy * d.w as i32 + sx) as usize];
                }
            }
        }
        Ok(Clip { x: x0, y: y0, w: cw as u32, h: ch as u32, data, pal: d.palette.colors.clone() })
    }

    /// Clipboard cells converted to this document's palette.
    pub fn clip_cells(&self) -> Option<Vec<Cell>> {
        let c = self.clipboard.as_ref()?;
        let d = &self.doc;
        if c.pal == d.palette.colors {
            return Some(c.data.clone());
        }
        let map: Vec<Cell> = c.pal.iter().map(|p| d.from_rgba([p[0], p[1], p[2], 255])).collect();
        Some(c.data.iter().map(|&cell| map.get(cell as usize).copied().unwrap_or(TRANSP)).collect())
    }

    // ------------------------------------------------------------ tiles

    pub fn tiles_flush(&mut self) {
        if let Some(painted) = self.tiles_dirty.take() {
            self.tiles_post(painted);
        }
    }

    /// Bring tile-mapped layers and the tile set back into agreement after a change.
    pub fn tiles_post(&mut self, painted: bool) {
        self.tiles_dirty = None;
        let Some((_, _, cols, rows)) = tile_dims(&self.doc) else {
            for l in &mut self.doc.layers {
                l.map = None;
            }
            return;
        };
        for li in 0..self.doc.layers.len() {
            if self.doc.layers[li].map.as_ref().is_some_and(|m| m.len() != cols * rows) {
                self.doc.layers[li].map = Some(Arc::new(vec![0; cols * rows]));
                self.match_cells(li);
            }
        }
        if painted && self.o.tile_mode == TileMode::Sync {
            self.tiles_sync_pass();
        } else {
            for li in 0..self.doc.layers.len() {
                self.match_cells(li);
            }
        }
        self.fix_state();
    }

    /// Sync mode: fold pixel edits back into their tiles, then refresh every instance.
    fn tiles_sync_pass(&mut self) {
        let Some((tw, th, cols, rows)) = tile_dims(&self.doc) else { return };
        let w = self.doc.w as usize;
        let Some(ts) = self.doc.tileset.as_mut() else { return };
        let old = ts.tiles.clone();
        let mut changed = vec![false; old.len()];
        let mut blank_edited = false;
        for l in &self.doc.layers {
            let Some(map) = &l.map else { continue };
            if map.len() != cols * rows {
                continue;
            }
            for cy in 0..rows {
                for cx in 0..cols {
                    let t = map[cy * cols + cx] as usize;
                    if t >= old.len() || cell_eq(&l.data, w, cx, cy, tw, th, &old[t]) {
                        continue;
                    }
                    if t == 0 {
                        // Tile 0 stays blank: a cell drawn on gets a tile of its own instead.
                        blank_edited = true;
                        continue;
                    }
                    let nt = Arc::make_mut(&mut ts.tiles[t]);
                    for py in 0..th {
                        for px in 0..tw {
                            let v = l.data[(cy * th + py) * w + cx * tw + px];
                            if v != old[t][py * tw + px] {
                                nt[py * tw + px] = v;
                            }
                        }
                    }
                    changed[t] = true;
                }
            }
        }
        if changed.iter().any(|c| *c) {
            for l in &mut self.doc.layers {
                let Some(map) = l.map.clone() else { continue };
                if map.len() != cols * rows {
                    continue;
                }
                for cy in 0..rows {
                    for cx in 0..cols {
                        let t = map[cy * cols + cx] as usize;
                        if t < changed.len() && changed[t] && !cell_eq(&l.data, w, cx, cy, tw, th, &ts.tiles[t]) {
                            blit_tile(Arc::make_mut(&mut l.data).as_mut_slice(), w, cx, cy, tw, th, &ts.tiles[t]);
                        }
                    }
                }
            }
        }
        if blank_edited {
            for li in 0..self.doc.layers.len() {
                self.match_cells(li);
            }
        }
    }

    /// Auto mode: re-point any cell whose pixels no longer match its tile, creating tiles as needed.
    pub fn match_cells(&mut self, li: usize) {
        let Some((tw, th, cols, rows)) = tile_dims(&self.doc) else { return };
        let w = self.doc.w as usize;
        let Some(ts) = self.doc.tileset.as_mut() else { return };
        let l = &mut self.doc.layers[li];
        let Some(map_arc) = l.map.as_mut() else { return };
        if map_arc.len() != cols * rows {
            return;
        }
        let mut index: Option<HashMap<Arc<Vec<Cell>>, u16>> = None;
        for cy in 0..rows {
            for cx in 0..cols {
                let t = map_arc[cy * cols + cx] as usize;
                if t < ts.tiles.len() && cell_eq(&l.data, w, cx, cy, tw, th, &ts.tiles[t]) {
                    continue;
                }
                let idx = index.get_or_insert_with(|| {
                    let mut m = HashMap::new();
                    for (i, t) in ts.tiles.iter().enumerate() {
                        m.entry(t.clone()).or_insert(i as u16);
                    }
                    m
                });
                let px = cell_pixels(&l.data, w, cx, cy, tw, th);
                let id = match idx.get(&px) {
                    Some(id) => *id,
                    None if ts.tiles.len() < u16::MAX as usize => {
                        let a = Arc::new(px);
                        ts.tiles.push(a.clone());
                        idx.insert(a, (ts.tiles.len() - 1) as u16);
                        (ts.tiles.len() - 1) as u16
                    }
                    None => 0,
                };
                Arc::make_mut(map_arc)[cy * cols + cx] = id;
            }
        }
    }

    /// Redraw tile-mapped layers from the tile set (after tiles or maps were edited directly).
    pub fn reblit(&mut self) {
        let Some((tw, th, cols, rows)) = tile_dims(&self.doc) else { return };
        let w = self.doc.w as usize;
        let Some(ts) = self.doc.tileset.as_ref() else { return };
        for l in &mut self.doc.layers {
            let Some(map) = l.map.clone() else { continue };
            if map.len() != cols * rows {
                continue;
            }
            for cy in 0..rows {
                for cx in 0..cols {
                    let t = (map[cy * cols + cx] as usize).min(ts.tiles.len() - 1);
                    if !cell_eq(&l.data, w, cx, cy, tw, th, &ts.tiles[t]) {
                        blit_tile(Arc::make_mut(&mut l.data).as_mut_slice(), w, cx, cy, tw, th, &ts.tiles[t]);
                    }
                }
            }
        }
    }

    /// Renumber tiles: `remap[old] = new`. Tiles mapped to the same id are merged.
    pub fn remap_tiles(&mut self, remap: &[u16], new_count: usize) {
        let empty = TRANSP;
        let Some(ts) = self.doc.tileset.as_mut() else { return };
        let mut tiles: Vec<Option<Arc<Vec<Cell>>>> = vec![None; new_count];
        for (old, t) in ts.tiles.iter().enumerate() {
            let n = remap[old] as usize;
            if n < new_count && tiles[n].is_none() {
                tiles[n] = Some(t.clone());
            }
        }
        let blank = Arc::new(vec![empty; (ts.tw * ts.th) as usize]);
        ts.tiles = tiles.into_iter().map(|t| t.unwrap_or_else(|| blank.clone())).collect();
        for l in &mut self.doc.layers {
            if let Some(m) = l.map.as_mut() {
                for c in Arc::make_mut(m).iter_mut() {
                    *c = remap.get(*c as usize).copied().unwrap_or(0);
                }
            }
        }
        self.o.active_tile = remap.get(self.o.active_tile as usize).copied().unwrap_or(0);
    }
}
