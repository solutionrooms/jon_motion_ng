//! A view-only reader for sprite atlases: an `atlas.json` beside its page images, as written
//! by a game's art baker. The pages are decoded to true colour and only ever displayed -
//! nothing here is a document, so no tool, command or script can paint, convert or save them.
//!
//! The file is `{prefix, pages, ext, objects: [{name, labels: [[name, frame], ...],
//! frames: [[page, x, y, w, h, source_w, source_h, x_offset, y_offset], ...]}]}`; page -1 is
//! an empty frame, and page `i` is the image `<prefix><i><ext>` in the same folder.

use crate::doc::MAX_DIM;
use crate::engine::guarded;
use crate::io;
use eframe::egui::{self, pos2, vec2, Color32, Rect, Sense, Stroke, StrokeKind};
use serde_json::Value;
use std::path::Path;

/// Largest description file read.
pub const JSON_MAX: u64 = 16 << 20;
/// Largest page file read, and the most pages.
pub const PAGE_FILE_MAX: u64 = 64 << 20;
pub const PAGES_MAX: usize = 16;
/// Largest page side, and the most pixels in all pages together (256 MB as true colour).
pub const PAGE_DIM: u32 = 8192;
pub const PIXELS_MAX: u64 = 64 << 20;
pub const OBJECTS_MAX: usize = 4096;
pub const FRAMES_MAX: usize = 8192;
pub const TOTAL_FRAMES_MAX: usize = 131_072;
pub const LABELS_MAX: usize = 1024;
const NAME_MAX: usize = 64;

pub struct Page {
    pub w: u32,
    pub h: u32,
    /// Straight (not premultiplied) RGBA, taken by the window when it makes its texture.
    pub rgba: Vec<u8>,
}

/// A frame: a rectangle of one page, and where its top-left corner is drawn relative to the
/// object's origin.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct AFrame {
    pub page: usize,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub ox: i32,
    pub oy: i32,
}

/// A labelled run of frames, first to last inclusive.
#[derive(Clone, PartialEq, Debug)]
pub struct Seq {
    pub name: String,
    pub start: u32,
    pub end: u32,
}

pub struct Object {
    pub name: String,
    /// `None` is an empty frame: nothing is drawn for it.
    pub frames: Vec<Option<AFrame>>,
    pub seqs: Vec<Seq>,
    /// The box that holds every frame at its offset: left, top, width, height.
    pub bounds: (i32, i32, u32, u32),
}

pub struct Atlas {
    /// The folder's name, for the window title.
    pub name: String,
    pub pages: Vec<Page>,
    pub objects: Vec<Object>,
}

/// Text from the file made safe to show: no control or text-reordering characters, a sensible length.
fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control() && !matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')).take(NAME_MAX).collect::<String>().trim().to_string()
}

/// A whole number in a range.
fn whole(v: Option<&Value>, lo: i64, hi: i64) -> Option<i64> {
    v.and_then(|v| v.as_f64()).filter(|n| n.fract() == 0.0 && *n >= lo as f64 && *n <= hi as f64).map(|n| n as i64)
}

/// The page files the description names: prefix, extension and how many. The prefix and
/// extension come from the file, so they may only ever name files in the atlas's own folder.
pub fn header(v: &Value) -> Result<(String, String, usize), String> {
    let pages = whole(v.get("pages"), 1, PAGES_MAX as i64).ok_or(format!("\"pages\" must be a whole number from 1 to {PAGES_MAX}"))? as usize;
    let prefix = v.get("prefix").and_then(|p| p.as_str()).ok_or("\"prefix\" must be text")?;
    if prefix.is_empty() || prefix.len() > NAME_MAX || !prefix.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err("\"prefix\" must be 1 to 64 letters, digits, _ or -".into());
    }
    let ext = v.get("ext").and_then(|p| p.as_str()).unwrap_or(".png").to_ascii_lowercase();
    if ext != ".webp" && ext != ".png" {
        return Err("\"ext\" must be .webp or .png".into());
    }
    Ok((prefix.to_string(), ext, pages))
}

/// The objects in the description, every frame checked against the size of its page.
pub fn parse(v: &Value, pages: &[(u32, u32)]) -> Result<Vec<Object>, String> {
    let list = v.get("objects").and_then(|o| o.as_array()).ok_or("the file has no \"objects\" list")?;
    if list.len() > OBJECTS_MAX {
        return Err(format!("the file has {} objects; the limit is {OBJECTS_MAX}", list.len()));
    }
    let mut total = 0usize;
    let mut out = Vec::with_capacity(list.len());
    for (n, o) in list.iter().enumerate() {
        let name = Some(clean(o.get("name").and_then(|s| s.as_str()).unwrap_or(""))).filter(|s| !s.is_empty()).unwrap_or_else(|| format!("object {n}"));
        let err = |what: String| format!("\"{name}\": {what}");
        let frames = o.get("frames").and_then(|f| f.as_array()).ok_or_else(|| err("it has no \"frames\" list".into()))?;
        total += frames.len();
        if frames.len() > FRAMES_MAX || total > TOTAL_FRAMES_MAX {
            return Err(err(format!("too many frames (at most {FRAMES_MAX} in an object and {TOTAL_FRAMES_MAX} in a file)")));
        }
        let mut fs = Vec::with_capacity(frames.len());
        for (i, f) in frames.iter().enumerate() {
            let bad = |what: &str| err(format!("frame {i}: {what}"));
            let f = f.as_array().filter(|f| f.len() >= 5).ok_or_else(|| bad("it must be [page, x, y, w, h, ...]"))?;
            let page = whole(f.first(), -1, pages.len() as i64 - 1).ok_or_else(|| bad("its page is not one of the atlas's pages"))?;
            // Offsets are whole pixels in practice; anything else is rounded, and kept in a sane range.
            let off = |i: usize| f.get(i).and_then(|v| v.as_f64()).filter(|n| n.is_finite()).map_or(0, |n| n.round().clamp(-32768.0, 32768.0) as i32);
            let (ox, oy) = (off(7), off(8));
            if page < 0 {
                fs.push(None);
                continue;
            }
            let (pw, ph) = pages[page as usize];
            let n = |i: usize, hi: u32| whole(f.get(i), 0, hi as i64).map(|v| v as u32).ok_or_else(|| bad("its rectangle is not whole numbers inside the page"));
            let (x, y, w, h) = (n(1, pw)?, n(2, ph)?, n(3, pw)?, n(4, ph)?);
            if x as u64 + w as u64 > pw as u64 || y as u64 + h as u64 > ph as u64 {
                return Err(bad("its rectangle is not inside the page"));
            }
            fs.push((w > 0 && h > 0).then_some(AFrame { page: page as usize, x, y, w, h, ox, oy }));
        }
        // Labels name frames; "x" and "x_end" bracket a sequence. Ones that make no sense are left out.
        let mut labels: Vec<(String, u32)> = o
            .get("labels")
            .and_then(|l| l.as_array())
            .map(|l| l.as_slice())
            .unwrap_or(&[])
            .iter()
            .take(LABELS_MAX)
            .filter_map(|l| Some((clean(l.get(0)?.as_str()?), whole(l.get(1), 0, fs.len() as i64 - 1)? as u32)))
            .filter(|(s, _)| !s.is_empty())
            .collect();
        labels.sort_by_key(|l| l.1);
        let mut seqs = vec![];
        for (i, (label, start)) in labels.iter().enumerate() {
            // An "_end" closes the sequence it is named after; one with nothing to close is left out.
            if label.ends_with("_end") {
                continue;
            }
            let closing = labels.iter().find(|(s, at)| s.strip_suffix("_end") == Some(label.as_str()) && at >= start).map(|l| l.1);
            let next = labels[i + 1..].iter().find(|(_, at)| at > start).map(|l| l.1 - 1);
            seqs.push(Seq { name: label.clone(), start: *start, end: closing.or(next).unwrap_or(fs.len() as u32 - 1) });
        }
        let (mut x0, mut y0, mut x1, mut y1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
        for f in fs.iter().flatten() {
            (x0, y0) = (x0.min(f.ox as i64), y0.min(f.oy as i64));
            (x1, y1) = (x1.max(f.ox as i64 + f.w as i64), y1.max(f.oy as i64 + f.h as i64));
        }
        let bounds = if x1 > x0 && y1 > y0 { (x0 as i32, y0 as i32, (x1 - x0) as u32, (y1 - y0) as u32) } else { (0, 0, 1, 1) };
        out.push(Object { name, frames: fs, seqs, bounds });
    }
    Ok(out)
}

/// Decode one page to RGBA. `budget` is the number of pixels still allowed; the size is
/// read from the file's header and checked against it before the pixels are decoded.
pub fn decode_page(bytes: &[u8], webp: bool, budget: &mut u64) -> Result<Page, String> {
    let mut claim = |w: u32, h: u32| {
        if w == 0 || h == 0 || w > PAGE_DIM || h > PAGE_DIM {
            return Err(format!("it is {w}x{h}; pages can be up to {PAGE_DIM}x{PAGE_DIM}"));
        }
        let px = w as u64 * h as u64;
        if px > *budget {
            return Err("the pages together are too large (more than 64 million pixels)".to_string());
        }
        *budget -= px;
        Ok(())
    };
    if !webp {
        let bad = || "not a readable PNG image".to_string();
        let (w, h) = png::Decoder::new(bytes).read_info().map(|r| (r.info().width, r.info().height)).map_err(|_| bad())?;
        // PNG pages go through the document's PNG reader, which stops at the document size.
        if w > MAX_DIM || h > MAX_DIM {
            return Err(format!("it is {w}x{h}; PNG pages can be up to {MAX_DIM}x{MAX_DIM} (WebP pages up to {PAGE_DIM}x{PAGE_DIM})"));
        }
        claim(w, h)?;
        let img = io::decode_png(bytes).map_err(|_| bad())?;
        let rgba = match img.pixels {
            io::PngPixels::Rgba(px) => px.into_iter().flatten().collect(),
            io::PngPixels::Indexed { data, palette, trns } => data
                .iter()
                .flat_map(|&i| {
                    let c = palette.get(i as usize).copied().unwrap_or([0, 0, 0]);
                    [c[0], c[1], c[2], trns.get(i as usize).copied().unwrap_or(255)]
                })
                .collect(),
        };
        return Ok(Page { w: img.w, h: img.h, rgba });
    }
    // A decoder for a format this program does not write: any panic inside it is an error here.
    guarded(|| {
        let es = |e: image_webp::DecodingError| format!("not a readable WebP image ({e})");
        let mut d = image_webp::WebPDecoder::new(std::io::Cursor::new(bytes)).map_err(es)?;
        let (w, h) = d.dimensions();
        claim(w, h)?;
        if d.is_animated() {
            return Err("animated WebP pages are not supported".into());
        }
        let px = w as usize * h as usize;
        d.set_memory_limit(px * 16 + (16 << 20));
        let alpha = d.has_alpha();
        let mut buf = vec![0u8; px * if alpha { 4 } else { 3 }];
        if d.output_buffer_size() != Some(buf.len()) {
            return Err("not a readable WebP image (unexpected size)".into());
        }
        d.read_image(&mut buf).map_err(es)?;
        let rgba = if alpha { buf } else { buf.chunks_exact(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect() };
        Ok(Page { w, h, rgba })
    })
}

/// A regular file of at most `max` bytes; never a link, so a page cannot be made to point
/// somewhere outside the atlas's folder.
fn read_plain(p: &Path, max: u64) -> Result<Vec<u8>, String> {
    let err = |why: &str| format!("{}: {why}", p.display());
    let m = std::fs::symlink_metadata(p).map_err(|e| err(&e.to_string()))?;
    if !m.is_file() {
        return Err(err("not a plain file"));
    }
    if m.len() > max {
        return Err(err(&format!("larger than {} MB", max >> 20)));
    }
    std::fs::read(p).map_err(|e| err(&e.to_string()))
}

/// Read an atlas: the description, then each page from the same folder. Read-only throughout.
pub fn load(json: &Path) -> Result<Atlas, String> {
    if !json.extension().is_some_and(|x| x.eq_ignore_ascii_case("json")) {
        return Err(format!("{}: an atlas is opened by its .json file", json.display()));
    }
    let text = read_plain(json, JSON_MAX)?;
    // Some editors put a byte-order mark in front of the text.
    let text = text.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&text);
    let v: Value = serde_json::from_slice(text).map_err(|_| format!("{}: not valid JSON", json.display()))?;
    if v.get("format").and_then(|f| f.as_str()) == Some(io::ANIM_FORMAT) {
        return Err(format!("{}: this is a Jon Motion animations file, not a sprite atlas; use Import... on the Animation tab", json.display()));
    }
    if !v.is_object() || v.get("pages").is_none() || v.get("objects").is_none() {
        return Err(format!("{}: not a sprite atlas description (it has no \"pages\" and \"objects\")", json.display()));
    }
    let (prefix, ext, count) = header(&v).map_err(|e| format!("{}: {e}", json.display()))?;
    let dir = json.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut budget = PIXELS_MAX;
    let mut pages = Vec::with_capacity(count);
    for i in 0..count {
        let p = dir.join(format!("{prefix}{i}{ext}"));
        let page = decode_page(&read_plain(&p, PAGE_FILE_MAX)?, ext == ".webp", &mut budget).map_err(|e| format!("{}: {e}", p.display()))?;
        pages.push(page);
    }
    let sizes: Vec<(u32, u32)> = pages.iter().map(|p| (p.w, p.h)).collect();
    let objects = parse(&v, &sizes).map_err(|e| format!("{}: {e}", json.display()))?;
    let name = clean(&dir.canonicalize().ok().as_deref().unwrap_or(dir).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
    Ok(Atlas { name, pages, objects })
}

// ------------------------------------------------------------------ the viewer window

/// The open atlas and what the viewer is showing. Window state only: it is not part of any
/// document, and no command reads or changes it.
pub struct View {
    pub atlas: Atlas,
    tex: Vec<egui::TextureHandle>,
    pub object: usize,
    /// Index into the object's frames.
    pub frame: u32,
    pub playing: bool,
    pub fps: f32,
    /// The labelled sequence being played; `None` plays every frame.
    pub seq: Option<usize>,
    filter: String,
    animated_only: bool,
    clock: f64,
    /// Something the user should know about how the atlas is shown.
    note: String,
    /// Where the window was last drawn, for the Left / Right / Space keys.
    pub rect: Rect,
}

impl View {
    pub fn new(atlas: Atlas) -> View {
        let object = atlas.objects.iter().position(|o| o.frames.len() > 1).unwrap_or(0);
        View { atlas, tex: vec![], object, frame: 0, playing: true, fps: 30.0, seq: None, filter: String::new(), animated_only: false, clock: 0.0, note: String::new(), rect: Rect::NOTHING }
    }

    /// First and last frame of what is playing.
    pub fn range(&self) -> (u32, u32) {
        let Some(o) = self.atlas.objects.get(self.object) else { return (0, 0) };
        match self.seq.and_then(|i| o.seqs.get(i)) {
            Some(s) => (s.start, s.end.max(s.start)),
            None => (0, (o.frames.len() as u32).saturating_sub(1)),
        }
    }

    /// Move by `by` frames inside the current range, wrapping round.
    pub fn step(&mut self, by: i64) {
        let (a, b) = self.range();
        let n = (b - a + 1) as i64;
        self.frame = a + (self.frame.clamp(a, b) as i64 - a as i64 + by).rem_euclid(n) as u32;
    }

    pub fn select(&mut self, object: usize) {
        if object < self.atlas.objects.len() && object != self.object {
            self.object = object;
            self.seq = None;
            self.frame = 0;
        }
    }

    pub fn play_seq(&mut self, seq: Option<usize>) {
        self.seq = seq;
        self.frame = self.range().0;
    }
}

/// Draw the viewer, if an atlas is open. Returns false once the user has closed it.
pub fn window(ctx: &egui::Context, v: &mut View) -> bool {
    // Each page becomes one texture, once; the decoded pixels are handed over, not kept twice.
    if v.tex.len() < v.atlas.pages.len() {
        let side = ctx.input(|i| i.max_texture_side) as u32;
        for (i, p) in v.atlas.pages.iter_mut().enumerate().skip(v.tex.len()) {
            let img = if p.w <= side && p.h <= side {
                egui::ColorImage::from_rgba_unmultiplied([p.w as usize, p.h as usize], &std::mem::take(&mut p.rgba))
            } else {
                // Larger than this machine can draw: shown as a blank page, and said so.
                v.note = format!("Page {i} is {}x{}, larger than this computer can draw ({side}x{side}); its frames are blank.", p.w, p.h);
                egui::ColorImage::new([1, 1], Color32::TRANSPARENT)
            };
            let opts = egui::TextureOptions { magnification: egui::TextureFilter::Nearest, minification: egui::TextureFilter::Linear, ..Default::default() };
            v.tex.push(ctx.load_texture(format!("atlas{i}"), img, opts));
        }
    }
    let mut open = true;
    let title = format!("Sprite Atlas - {} (view only)", v.atlas.name);
    let shown = egui::Window::new(title).id(egui::Id::new("atlas_viewer")).open(&mut open).default_size([760.0, 520.0]).min_width(420.0).min_height(260.0).show(ctx, |ui| {
        egui::SidePanel::left("atlas_list").resizable(true).default_width(210.0).show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut v.filter).hint_text("Find").desired_width(110.0));
                ui.checkbox(&mut v.animated_only, "Animated");
            });
            let want = v.filter.to_lowercase();
            let mut pick = None;
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                for (i, o) in v.atlas.objects.iter().enumerate() {
                    if (v.animated_only && o.frames.len() < 2) || !o.name.to_lowercase().contains(&want) {
                        continue;
                    }
                    let label = if o.frames.len() > 1 { format!("{}  ({})", o.name, o.frames.len()) } else { o.name.clone() };
                    if ui.selectable_label(i == v.object, label).clicked() {
                        pick = Some(i);
                    }
                }
            });
            if let Some(i) = pick {
                v.select(i);
            }
        });
        let Some(o) = v.atlas.objects.get(v.object) else {
            ui.label("This atlas has no objects.");
            return;
        };
        if !v.note.is_empty() {
            ui.colored_label(Color32::from_rgb(255, 170, 90), &v.note);
        }
        let (first, last) = v.range();
        v.frame = v.frame.clamp(first, last);
        // Playback: move on by however many frames are due.
        let now = ui.input(|i| i.time);
        let (n, fps) = (last - first + 1, if v.fps.is_finite() { v.fps.clamp(1.0, 60.0) as f64 } else { 30.0 });
        if v.playing && n > 1 {
            let due = ((now - v.clock) * fps).floor();
            if due >= 1.0 {
                v.frame = first + ((v.frame - first) as f64 + due).rem_euclid(n as f64) as u32;
                v.clock = if due > fps { now } else { v.clock + due / fps };
            }
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64((v.clock + 1.0 / fps - now).max(0.0)));
        } else {
            v.clock = now;
        }
        let (mut step, mut toggle, mut seq) = (0i64, false, None);
        ui.horizontal_wrapped(|ui| {
            if ui.button("<").on_hover_text("Previous frame (Left)").clicked() {
                step = -1;
            }
            toggle = ui.add_sized([52.0, 20.0], egui::Button::new(if v.playing { "Pause" } else { "Play" })).on_hover_text("Space").clicked();
            if ui.button(">").on_hover_text("Next frame (Right)").clicked() {
                step = 1;
            }
            ui.label(format!("Frame {} of {}", v.frame + 1, o.frames.len()));
            ui.add(egui::DragValue::new(&mut v.fps).range(1.0..=60.0).speed(0.2).max_decimals(0).suffix(" fps"));
            if !o.seqs.is_empty() {
                let current = v.seq.and_then(|i| o.seqs.get(i)).map_or("All frames".to_string(), |s| s.name.clone());
                egui::ComboBox::from_id_salt("atlas_seq").selected_text(current).show_ui(ui, |ui| {
                    if ui.selectable_label(v.seq.is_none(), "All frames").clicked() {
                        seq = Some(None);
                    }
                    for (i, s) in o.seqs.iter().enumerate() {
                        if ui.selectable_label(v.seq == Some(i), format!("{}  ({}-{})", s.name, s.start + 1, s.end + 1)).clicked() {
                            seq = Some(Some(i));
                        }
                    }
                });
            }
        });
        let f = o.frames.get(v.frame as usize).copied().flatten();
        ui.weak(match f {
            Some(f) => format!("{}x{} at {},{} on page {}, offset {},{}.  Left / Right step and Space plays while the pointer is here.", f.w, f.h, f.x, f.y, f.page, f.ox, f.oy),
            None => "An empty frame.  Left / Right step and Space plays while the pointer is here.".to_string(),
        });
        let avail = ui.available_size();
        if avail.x >= 16.0 && avail.y >= 16.0 {
            let (resp, p) = ui.allocate_painter(avail, Sense::hover());
            p.rect_filled(resp.rect, 2.0, Color32::from_gray(38));
            // The box holds every frame of the object at its offset, so nothing jumps about.
            let (bx, by, bw, bh) = o.bounds;
            let fit = ((avail.x - 16.0) / bw as f32).min((avail.y - 16.0) / bh as f32);
            let scale = if fit >= 1.0 { fit.floor().min(8.0) } else { fit.max(0.01) };
            let r = Rect::from_center_size(resp.rect.center(), vec2(bw as f32, bh as f32) * scale);
            let r = Rect::from_min_size(pos2(r.min.x.round(), r.min.y.round()), r.size());
            if let (Some(f), Some(t)) = (f, f.and_then(|f| v.tex.get(f.page))) {
                let (pw, ph) = (v.atlas.pages[f.page].w as f32, v.atlas.pages[f.page].h as f32);
                let at = r.min + vec2((f.ox - bx) as f32, (f.oy - by) as f32) * scale;
                let uv = Rect::from_min_max(pos2(f.x as f32 / pw, f.y as f32 / ph), pos2((f.x + f.w) as f32 / pw, (f.y + f.h) as f32 / ph));
                p.with_clip_rect(resp.rect).image(t.id(), Rect::from_min_size(at, vec2(f.w as f32, f.h as f32) * scale), uv, Color32::WHITE);
            }
            p.rect_stroke(r, 0.0, Stroke::new(1.0_f32, Color32::from_white_alpha(28)), StrokeKind::Outside);
        }
        if step != 0 {
            v.playing = false;
            v.step(step);
        }
        if toggle {
            v.playing = !v.playing;
        }
        if let Some(s) = seq {
            v.play_seq(s);
        }
    });
    v.rect = shown.map_or(Rect::NOTHING, |s| s.response.rect);
    open
}
