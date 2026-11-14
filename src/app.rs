//! The desktop app: window, menus, shortcuts, dialogs and docked panels.
//! All document changes go through `Engine::exec`, the same path scripts use.

use crate::canvas;
use crate::doc::*;
use crate::engine::*;
use crate::keys;
use crate::panels;
use crate::remote;
use crate::script;
use eframe::egui::{self, Color32, CornerRadius, Key, KeyboardShortcut, Modifiers, Stroke, ViewportCommand};
use egui_dock::{DockArea, DockState, NodeIndex};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

pub const ACCENT: Color32 = Color32::from_rgb(92, 148, 252);
pub const PANEL: Color32 = Color32::from_rgb(30, 31, 36);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Canvas,
    Script,
    Palette,
    Layers,
    Tiles,
    Preview,
    Anim,
}

pub enum Dialog {
    None,
    New { w: u32, h: u32, preset: usize },
    Resize { w: u32, h: u32, anchor: usize },
    Scale { w: u32, h: u32 },
    Reduce { colors: u32, dither: usize, generate: bool },
    Shortcuts,
    Mcp,
}

pub enum Pending {
    New,
    Open,
    /// Open this file (from File > Open Recent).
    OpenFile(PathBuf),
    Quit,
}

/// How many files File > Open Recent keeps.
pub const RECENT_MAX: usize = 10;

pub enum Drag {
    Pan { last: egui::Pos2, button: usize },
    Stroke { button: usize },
    Shape { tool: Tool, start: (i32, i32), button: usize },
    Select { start: (i32, i32), cur: (i32, i32), button: usize },
    Move { last: (i32, i32), button: usize },
    Tiles { button: usize },
}

/// GUI-only state (nothing here is part of the document).
pub struct Gui {
    pub tex: Option<egui::TextureHandle>,
    pub tex_key: (u64, u64, i64),
    pub tile_tex: Option<egui::TextureHandle>,
    pub tile_key: u64,
    pub drag: Option<Drag>,
    pub hover: Option<(i32, i32)>,
    /// The keymap last written to the settings file; `None` (tests) never touches the file.
    pub saved_keymap: Option<Keymap>,
    pub last_pos: Option<egui::Pos2>,
    pub zoom_accum: f32,
    pub canvas_rect: egui::Rect,
    pub status: String,
    pub status_err: bool,
    pub pal_sel: Option<(usize, usize)>,
    pub flash: Option<usize>,
    pub flash_now: Option<usize>,
    pub usage: Vec<u64>,
    pub usage_rev: u64,
    pub ants: Vec<[i32; 4]>,
    pub ants_key: (usize, usize, i32, i32),
    /// Keeps the buffers `ants_key` points at alive, so their addresses cannot be reused.
    pub ants_hold: (Option<Arc<Vec<Cell>>>, Option<Arc<Vec<u8>>>),
    pub script: String,
    pub script_out: String,
    /// Set by the Script tab's Run button; the frame loop starts the script on a worker thread.
    pub script_req: bool,
    pub script_job: Option<mpsc::Receiver<script::ScriptOut>>,
    pub cmd_filter: String,
    pub rename: Option<(u32, String)>,
    pub dialog: Dialog,
    pub pending: Option<Pending>,
    pub allow_close: bool,
    /// The user asked to close the window while a script or AI request had the document.
    pub busy_quit: bool,
    pub mcp_port: Option<u16>,
    pub title: String,
    pub tileset_size: (u32, u32),
    /// Where the Animation tab was drawn this frame; its keys apply while the pointer is over it.
    pub anim_rect: egui::Rect,
    /// When the playing animation last advanced.
    pub anim_clock: f64,
    /// Frame size typed in before an animation exists; `None` follows the grid size.
    pub anim_size: Option<(u32, u32)>,
    /// A tab to bring to the front.
    pub show_tab: Option<Tab>,
    /// Files opened or saved from this window, newest first. Window state only: no command,
    /// script or AI client can read it.
    pub recent: Vec<PathBuf>,
    /// The list changed and is to be written to the settings file.
    pub recent_dirty: bool,
    /// The user cleared the list, so the copy on disk is replaced rather than merged with.
    pub recent_cleared: bool,
    /// Recent files found to be missing, a check still running, and when the last one started.
    pub recent_gone: Vec<PathBuf>,
    pub recent_check: Option<mpsc::Receiver<Vec<PathBuf>>>,
    pub recent_checked: Option<Instant>,
}

impl Gui {
    pub fn new() -> Gui {
        Gui {
            saved_keymap: None,
            tex: None,
            tex_key: (0, 0, -1),
            tile_tex: None,
            tile_key: 0,
            drag: None,
            hover: None,
            last_pos: None,
            zoom_accum: 0.0,
            canvas_rect: egui::Rect::NOTHING,
            status: String::new(),
            status_err: false,
            pal_sel: None,
            flash: None,
            flash_now: None,
            usage: vec![],
            usage_rev: 0,
            ants: vec![],
            ants_key: (0, 0, 0, 0),
            ants_hold: (None, None),
            script_req: false,
            script_job: None,
            script: "-- Lua: every editor command is jm.<name>{ params }\n-- e.g. checkerboard on the active layer:\nlocal w, h = jm.size()\nfor y = 0, h - 1 do\n  for x = 0, w - 1 do\n    if (x // 8 + y // 8) % 2 == 0 then jm.set(x, y, 4) end\n  end\nend\n".into(),
            script_out: String::new(),
            cmd_filter: String::new(),
            rename: None,
            dialog: Dialog::None,
            pending: None,
            allow_close: false,
            busy_quit: false,
            mcp_port: None,
            title: String::new(),
            tileset_size: (16, 16),
            anim_rect: egui::Rect::NOTHING,
            anim_clock: 0.0,
            anim_size: None,
            show_tab: None,
            recent: vec![],
            recent_dirty: false,
            recent_cleared: false,
            recent_gone: vec![],
            recent_check: None,
            recent_checked: None,
        }
    }

    pub fn info(&mut self, s: impl Into<String>) {
        self.status = s.into();
        self.status_err = false;
    }
    pub fn error(&mut self, s: impl Into<String>) {
        self.status = s.into();
        self.status_err = true;
    }
    /// Run a command, reporting failures in the status bar.
    pub fn run(&mut self, e: &mut Engine, name: &str, params: Value) -> Option<Value> {
        match e.exec(name, &params) {
            Ok(v) => Some(v),
            Err(err) => {
                self.error(err);
                None
            }
        }
    }
    /// Run a selection command the way a hand-made selection should behave:
    /// a result with no pixels means nothing is selected.
    pub fn run_select(&mut self, e: &mut Engine, name: &str, params: Value) {
        let r = e.group(|e| {
            e.exec(name, &params)?;
            if e.doc.selection.is_some() && e.doc.sel_bounds().is_none() {
                e.exec("select_none", &Value::Null)?;
            }
            Ok(())
        });
        if let Err(err) = r {
            self.error(err);
        }
    }
    /// Put a file at the top of File > Open Recent.
    pub fn remember(&mut self, p: &Path) {
        // One spelling per file: symlinks, ".." and (on case-insensitive disks) letter case are
        // resolved. Windows keeps the plain absolute path, as its canonical form is the \\?\ kind.
        let full = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
        let p = if cfg!(unix) { std::fs::canonicalize(&full).unwrap_or(full) } else { full };
        // The list is stored as JSON text, which cannot hold a path that is not valid Unicode.
        if !p.to_str().is_some_and(listable) {
            return;
        }
        self.recent.retain(|q| *q != p);
        self.recent.insert(0, p);
        self.recent.truncate(RECENT_MAX);
        self.recent_dirty = true;
    }
    pub fn script_done(&mut self, out: script::ScriptOut) {
        let mut s = out.output;
        match out.error {
            Some(err) => {
                s.push_str(&format!("error (the document was rolled back): {err}\n"));
                self.error("Script failed");
            }
            None => {
                if !out.result.is_null() {
                    s.push_str(&format!("=> {}\n", out.result));
                }
                self.info("Script finished");
            }
        }
        self.script_out = s;
    }
}

pub struct App {
    engine: Arc<Mutex<Engine>>,
    gui: Gui,
    dock: DockState<Tab>,
}

fn theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = Color32::from_rgb(37, 38, 45);
    v.extreme_bg_color = Color32::from_rgb(20, 21, 25);
    v.faint_bg_color = Color32::from_rgb(40, 41, 48);
    v.selection.bg_fill = Color32::from_rgb(52, 92, 168);
    v.selection.stroke = Stroke::new(1.0_f32, Color32::WHITE);
    v.hyperlink_color = ACCENT;
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(7);
    v.window_stroke = Stroke::new(1.0_f32, Color32::from_rgb(58, 60, 70));
    let r = CornerRadius::same(5);
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = r;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(52, 54, 63));
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(188, 192, 204));
    v.widgets.inactive.bg_fill = Color32::from_rgb(50, 52, 61);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(46, 48, 56);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(206, 210, 220));
    v.widgets.hovered.bg_fill = Color32::from_rgb(64, 67, 79);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(60, 63, 74);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(98, 104, 122));
    v.widgets.active.bg_fill = Color32::from_rgb(72, 110, 190);
    v.widgets.active.weak_bg_fill = Color32::from_rgb(72, 110, 190);
    ctx.set_visuals(v);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(6.0, 5.0);
        s.spacing.button_padding = egui::vec2(8.0, 3.0);
        s.spacing.menu_margin = egui::Margin::same(6);
        s.interaction.tooltip_delay = 0.35;
    });
}

pub fn run(file: Option<PathBuf>) {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Jon Motion")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([900.0, 560.0])
            .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")).unwrap_or_default()),
        ..Default::default()
    };
    let result = eframe::run_native("Jon Motion", options, Box::new(move |cc| Ok(Box::new(App::new(cc, file)))));
    if let Err(e) = result {
        eprintln!("failed to start: {e}");
        std::process::exit(1);
    }
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, file: Option<PathBuf>) -> App {
        theme(&cc.egui_ctx);
        let mut engine = Engine::new("gui");
        let mut gui = Gui::new();
        if let Some(p) = remote::settings_file() {
            let (keymap, recent) = read_settings(&p);
            engine.view.keymap = keymap.unwrap_or(engine.view.keymap);
            gui.recent = recent;
        }
        gui.saved_keymap = Some(engine.view.keymap);
        if let Some(f) = file {
            match engine.exec("open_document", &json!({ "path": f.display().to_string() })) {
                Ok(_) => gui.remember(&f),
                Err(e) => gui.error(e),
            }
        }
        let engine = Arc::new(Mutex::new(engine));
        let ctx = cc.egui_ctx.clone();
        match remote::start_http(engine.clone(), move || ctx.request_repaint()) {
            Ok(port) => gui.mcp_port = Some(port),
            Err(e) => gui.error(e),
        }
        let mut dock = DockState::new(vec![Tab::Canvas, Tab::Script]);
        let surface = dock.main_surface_mut();
        let [_, right] = surface.split_right(NodeIndex::root(), 0.77, vec![Tab::Palette]);
        surface.split_below(right, 0.52, vec![Tab::Layers, Tab::Tiles, Tab::Preview, Tab::Anim]);
        App { engine, gui, dock }
    }
}

struct Viewer<'a> {
    e: &'a mut Engine,
    g: &'a mut Gui,
}

impl egui_dock::TabViewer for Viewer<'_> {
    type Tab = Tab;
    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        match tab {
            Tab::Canvas => "Canvas",
            Tab::Script => "Script",
            Tab::Palette => "Palette",
            Tab::Layers => "Layers",
            Tab::Tiles => "Tiles",
            Tab::Preview => "Preview",
            Tab::Anim => "Animation",
        }
        .into()
    }
    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        match tab {
            Tab::Canvas => canvas::show(ui, self.e, self.g),
            Tab::Script => panels::script(ui, self.e, self.g),
            Tab::Palette => panels::palette(ui, self.e, self.g),
            Tab::Layers => panels::layers(ui, self.e, self.g),
            Tab::Tiles => panels::tiles(ui, self.e, self.g),
            Tab::Preview => panels::preview(ui, self.e, self.g),
            Tab::Anim => panels::animation(ui, self.e, self.g),
        }
    }
    fn closeable(&mut self, _tab: &mut Tab) -> bool {
        false
    }
    fn scroll_bars(&self, tab: &Tab) -> [bool; 2] {
        match tab {
            Tab::Canvas | Tab::Preview | Tab::Script | Tab::Anim => [false, false],
            _ => [false, true],
        }
    }
}

// ------------------------------------------------------------------ file actions

fn pick_open() -> Option<PathBuf> {
    rfd::FileDialog::new().add_filter("Jon Motion project or PNG", &["jmn", "png"]).pick_file()
}

fn file_stem(e: &Engine) -> String {
    e.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "untitled".into())
}

pub fn save(e: &mut Engine, g: &mut Gui, save_as: bool) -> bool {
    let path = match (&e.path, save_as) {
        (Some(p), false) => Some(p.clone()),
        _ => rfd::FileDialog::new().add_filter("Jon Motion project", &["jmn"]).set_file_name(format!("{}.jmn", file_stem(e))).save_file(),
    };
    let Some(path) = path else { return false };
    let ok = g.run(e, "save_document", json!({ "path": path.display().to_string() })).is_some();
    if ok {
        g.info(format!("Saved {}", path.display()));
        g.remember(&path);
    }
    ok
}

fn open(e: &mut Engine, g: &mut Gui) {
    if let Some(p) = pick_open() {
        open_path(e, g, &p);
    }
}

/// Open a file the user chose; any unsaved changes have already been dealt with.
fn open_path(e: &mut Engine, g: &mut Gui, p: &Path) {
    if g.run(e, "open_document", json!({ "path": p.display().to_string(), "force": true })).is_some() {
        g.info(format!("Opened {}", p.display()));
        g.remember(p);
    }
}

/// A file dragged onto the window opens like File > Open, asking about unsaved changes first.
/// Anything that cannot be opened is refused with the reason in the status bar.
pub fn dropped(ctx: &egui::Context, e: &mut Engine, g: &mut Gui) {
    let files: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
    let [p] = files.as_slice() else {
        if files.len() > 1 {
            g.error(format!("Drop one file at a time ({} were dropped)", files.len()));
        }
        return;
    };
    if !matches!(g.dialog, Dialog::None) || g.pending.is_some() {
        return g.error("Finish this dialog first, then drop the file again");
    }
    // The name is shown in the status bar and the recent list, so it gets the same check as those.
    if !p.to_str().is_some_and(listable) {
        return g.error("That file's name has characters that cannot be shown; rename it and drop it again");
    }
    if !p.extension().is_some_and(|x| x.eq_ignore_ascii_case("jmn") || x.eq_ignore_ascii_case("png")) {
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        return g.error(format!("Only .jmn and .png files can be opened; {name} is neither"));
    }
    request(e, g, Pending::OpenFile(p.clone()));
}

/// Shown while a file is held over the window.
fn drop_hint(ctx: &egui::Context) {
    if ctx.input(|i| i.raw.hovered_files.is_empty()) {
        return;
    }
    let r = ctx.screen_rect();
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop_hint")));
    p.rect_filled(r, 0.0, Color32::from_black_alpha(160));
    p.text(r.center(), egui::Align2::CENTER_CENTER, "Drop a .jmn or .png file to open it", egui::FontId::proportional(22.0), Color32::WHITE);
}

fn export(e: &mut Engine, g: &mut Gui, command: &str, ext: &str, suffix: &str, extra: Value) {
    let Some(p) = rfd::FileDialog::new().add_filter(ext, &[ext]).set_file_name(format!("{}{suffix}.{ext}", file_stem(e))).save_file() else { return };
    let mut params = json!({ "path": p.display().to_string() });
    if let (Some(a), Some(b)) = (params.as_object_mut(), extra.as_object()) {
        a.extend(b.clone());
    }
    if g.run(e, command, params).is_some() {
        g.info(format!("Exported {}", p.display()));
    }
}

pub fn request(e: &mut Engine, g: &mut Gui, what: Pending) {
    if e.modified {
        g.pending = Some(what);
    } else {
        proceed(e, g, what);
    }
}

fn proceed(e: &mut Engine, g: &mut Gui, what: Pending) {
    match what {
        Pending::New => g.dialog = Dialog::New { w: e.doc.w, h: e.doc.h, preset: 0 },
        Pending::Open => open(e, g),
        Pending::OpenFile(p) => open_path(e, g, &p),
        Pending::Quit => g.allow_close = true,
    }
}

fn copy_to_system(e: &Engine, g: &mut Gui) {
    let d = &e.doc;
    let rgba = d.composite(&d.palette.colors, None, false);
    let img = arboard::ImageData { width: d.w as usize, height: d.h as usize, bytes: rgba.into() };
    match arboard::Clipboard::new().and_then(|mut c| c.set_image(img)) {
        Ok(()) => g.info("Image copied to the system clipboard"),
        Err(err) => g.error(format!("clipboard: {err}")),
    }
}

fn paste_from_system(e: &mut Engine, g: &mut Gui) {
    let img = match arboard::Clipboard::new().and_then(|mut c| c.get_image()) {
        Ok(i) => i,
        Err(err) => return g.error(format!("no image on the system clipboard ({err})")),
    };
    if img.width as u32 > MAX_DIM || img.height as u32 > MAX_DIM || img.width == 0 {
        return g.error("clipboard image is too large");
    }
    let data = img.bytes.chunks_exact(4).map(|c| e.doc.from_rgba([c[0], c[1], c[2], c[3]])).collect();
    e.clipboard = Some(Clip { x: 0, y: 0, w: img.width as u32, h: img.height as u32, data, pal: e.doc.palette.colors.clone() });
    if g.run(e, "paste", json!({ "float": true })).is_some() {
        e.o.tool = Tool::Move;
        g.info("Pasted from the system clipboard - drag to place, Enter to commit");
    }
}

fn paste(e: &mut Engine, g: &mut Gui) {
    let mut params = json!({ "float": true });
    if let (Some((hx, hy)), Some(c)) = (g.hover, e.clipboard.as_ref()) {
        params["x"] = json!(hx - c.w as i32 / 2);
        params["y"] = json!(hy - c.h as i32 / 2);
    }
    if g.run(e, "paste", params).is_some() {
        e.o.tool = Tool::Move;
    }
}

pub fn zoom(e: &mut Engine, dir: i32) {
    e.view.zoom = canvas::step_zoom(e.view.zoom, dir);
    e.view.fit = false;
}

// ------------------------------------------------------------------ shortcuts

/// A path that can be shown in a menu as it is: no control characters, and none of the
/// invisible characters that reorder text (which could make one name display as another).
fn listable(s: &str) -> bool {
    s.len() <= 4096 && !s.chars().any(|c| c.is_control() || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
}

/// The settings file as JSON, if it is there, a sensible size and well formed.
fn settings_json(p: &Path) -> Option<Value> {
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.len() <= 1 << 20).then(|| std::fs::read_to_string(p).ok()).flatten().and_then(|t| serde_json::from_str(&t).ok())
}

/// Read the preferences that outlive the window (~/.jon_motion/settings.json): the key preset
/// and the recent files. The file is the user's to edit, so anything malformed in it is ignored.
pub fn read_settings(p: &Path) -> (Option<Keymap>, Vec<PathBuf>) {
    let Some(v) = settings_json(p) else { return (None, vec![]) };
    let mut recent: Vec<PathBuf> = vec![];
    for s in v["recent"].as_array().into_iter().flatten().filter_map(|s| s.as_str()) {
        let f = PathBuf::from(s);
        if listable(s) && f.is_absolute() && !recent.contains(&f) && recent.len() < RECENT_MAX {
            recent.push(f);
        }
    }
    (v["keymap"].as_str().and_then(Keymap::parse), recent)
}

/// Write the preferences, keeping any keys in the file that this build does not know.
/// `recent: None` leaves the list in the file as it is.
pub fn write_settings(p: &Path, keymap: Keymap, recent: Option<&[PathBuf]>) -> std::io::Result<()> {
    use std::io::Write;
    // A settings file the user has made read-only is theirs to keep that way.
    if std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file() && m.permissions().readonly()) {
        return Ok(());
    }
    let mut v = settings_json(p).filter(Value::is_object).unwrap_or_else(|| json!({}));
    v["keymap"] = json!(keymap.name());
    if let Some(recent) = recent {
        v["recent"] = json!(recent.iter().filter_map(|f| f.to_str()).collect::<Vec<_>>());
    }
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Written beside the target and renamed over it, so a crash cannot leave half a file, and
    // whatever is at the path (a link, a directory) is never written through or re-moded.
    // The recent list is file names from the user's disk, so the file is created private.
    // The name carries the process id, so two windows saving at the same moment do not collide.
    let tmp = p.with_extension(format!("json.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    let done = o.open(&tmp).and_then(|mut f| f.write_all(serde_json::to_string_pretty(&v)?.as_bytes())).and_then(|()| std::fs::rename(&tmp, p));
    if done.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    done
}

/// Write the preferences when one changed (from the menu, the dialog, a script or an AI client).
fn save_settings(e: &Engine, g: &mut Gui) {
    if g.saved_keymap == Some(e.view.keymap) && !g.recent_dirty {
        return;
    }
    g.saved_keymap = Some(e.view.keymap);
    let Some(p) = remote::settings_file() else { return };
    // The list is only written when this window changed it, so a key preset change here
    // cannot drop files another window added.
    let dirty = std::mem::take(&mut g.recent_dirty);
    if dirty && !std::mem::take(&mut g.recent_cleared) {
        // Another window may have added files since this one started: keep those too.
        for f in read_settings(&p).1 {
            if !g.recent.contains(&f) && g.recent.len() < RECENT_MAX {
                g.recent.push(f);
            }
        }
    }
    if let Err(err) = write_settings(&p, e.view.keymap, dirty.then_some(g.recent.as_slice())) {
        g.error(format!("could not save settings to {}: {err}", p.display()));
    }
}

pub fn shortcuts(ctx: &egui::Context, e: &mut Engine, g: &mut Gui) {
    let cmd = Modifiers::COMMAND;
    let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
    let hit = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, k)));
    let anim_rect = std::mem::replace(&mut g.anim_rect, egui::Rect::NOTHING);
    // File shortcuts work even while typing.
    if hit(cmd_shift, Key::S) {
        save(e, g, true);
    } else if hit(cmd, Key::S) {
        save(e, g, false);
    }
    if hit(cmd, Key::O) {
        request(e, g, Pending::Open);
    }
    if hit(cmd, Key::N) {
        request(e, g, Pending::New);
    }
    if ctx.wants_keyboard_input() || e.busy() {
        return;
    }
    if hit(cmd_shift, Key::Z) || hit(cmd, Key::Y) {
        g.run(e, "redo", json!({}));
    } else if hit(cmd, Key::Z) {
        g.run(e, "undo", json!({}));
    }
    if hit(cmd, Key::A) {
        g.run(e, "select_all", json!({}));
    }
    if hit(cmd, Key::D) {
        g.run(e, "select_none", json!({}));
    }
    if hit(cmd_shift, Key::I) {
        g.run_select(e, "select_invert", json!({}));
    }
    if hit(cmd, Key::B) {
        if g.run(e, "grab_brush", json!({})).is_some() {
            e.o.tool = Tool::Pencil;
            g.info("Custom brush grabbed");
        }
    }
    if hit(cmd, Key::G) {
        e.view.grid = !e.view.grid;
    }
    if hit(cmd, Key::E) {
        export(e, g, "export_png", "png", "", json!({}));
    }
    if hit(cmd, Key::Num0) {
        e.view.fit = true;
    }
    if hit(cmd, Key::Num1) {
        e.view.zoom = 1.0;
    }
    for ev in ctx.input(|i| i.events.clone()) {
        match ev {
            egui::Event::Copy => {
                if g.run(e, "copy", json!({})).is_some() {
                    // Leaving text on the system clipboard is what makes the OS deliver the next paste key.
                    ctx.copy_text("Jon Motion pixels".into());
                    g.info("Copied");
                }
            }
            egui::Event::Cut => {
                if g.run(e, "cut", json!({})).is_some() {
                    ctx.copy_text("Jon Motion pixels".into());
                }
            }
            egui::Event::Paste(_) => paste(e, g),
            _ => {}
        }
    }
    let plain = |k: Key| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, k));
    // With the pointer over the Animation tab, Left / Right and Space drive the animation. Anywhere
    // else they keep their usual jobs: the arrows nudge the selection, and Space held pans the canvas.
    if e.doc.anim.is_some() && ctx.input(|i| i.pointer.latest_pos()).is_some_and(|p| anim_rect.contains(p)) {
        if plain(Key::Space) {
            g.run(e, "anim_control", json!({ "playing": !e.view.anim_playing }));
        }
        for (k, step) in [(Key::ArrowLeft, -1), (Key::ArrowRight, 1)] {
            // Shift is taken as well, so it cannot fall through to the 8 px selection nudge.
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, k) || i.consume_key(Modifiers::SHIFT, k)) {
                g.run(e, "anim_control", json!({ "step": step, "playing": false }));
            }
        }
    }
    match e.view.keymap {
        Keymap::Standard => {
            for t in Tool::ALL {
                if let Some(k) = Key::from_name(&t.key().to_string()) {
                    if plain(k) {
                        e.o.tool = t;
                    }
                }
            }
            if plain(Key::X) {
                g.run(e, "swap_colors", json!({}));
            }
            if plain(Key::OpenBracket) {
                e.o.brush_size = e.o.brush_size.saturating_sub(1).max(1);
                e.o.custom_brush = None;
            }
            if plain(Key::CloseBracket) {
                e.o.brush_size = (e.o.brush_size + 1).min(128);
                e.o.custom_brush = None;
            }
            if plain(Key::Plus) || plain(Key::Equals) {
                zoom(e, 1);
            }
            if plain(Key::Minus) {
                zoom(e, -1);
            }
            if plain(Key::Num0) {
                e.view.fit = true;
            }
        }
        Keymap::DPaint => {
            // Characters come from text events, so r and R (or [ and {) are told apart on any
            // keyboard layout. Nothing is typed while Cmd/Ctrl is held, so those shortcuts are unaffected.
            let mut acts: Vec<keys::Act> = ctx.input(|i| {
                i.events.iter().filter_map(|ev| if let egui::Event::Text(t) = ev { keys::find(t) } else { None }).collect()
            });
            for k in [Key::Tab, Key::F1, Key::F2, Key::F3, Key::F4, Key::F5, Key::F6, Key::F7, Key::F8] {
                if let Some(a) = keys::find(k.name()) {
                    if plain(k) {
                        acts.push(a);
                    }
                }
            }
            for a in acts {
                if let Some(cmd) = keys::apply(e, a, g.hover) {
                    g.run(e, cmd, json!({}));
                }
            }
        }
    }
    if plain(Key::Delete) || plain(Key::Backspace) {
        g.run(e, "selection_delete", json!({}));
    }
    if plain(Key::Enter) && e.doc.float.is_some() {
        g.run(e, "float_commit", json!({}));
    }
    if plain(Key::Escape) {
        if e.doc.float.is_some() {
            g.run(e, "float_cancel", json!({}));
        } else if e.doc.selection.is_some() {
            g.run(e, "select_none", json!({}));
        } else if e.o.custom_brush.is_some() {
            e.o.custom_brush = None;
        }
    }
    let step = if ctx.input(|i| i.modifiers.shift) { 8 } else { 1 };
    for (k, dx, dy) in [(Key::ArrowLeft, -1, 0), (Key::ArrowRight, 1, 0), (Key::ArrowUp, 0, -1), (Key::ArrowDown, 0, 1)] {
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, k) || i.consume_key(Modifiers::SHIFT, k)) {
            let p = json!({ "dx": dx * step, "dy": dy * step });
            if e.doc.float.is_some() {
                let _ = e.exec_coalesced("float_move", &p, "nudge");
            } else if e.o.tool == Tool::Move || e.doc.selection.is_some() {
                g.run(e, "selection_move", p);
            }
        }
    }
}

// ------------------------------------------------------------------ menus

fn item(ui: &mut egui::Ui, label: &str, shortcut: &str) -> bool {
    let cmd = if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" };
    let clicked = ui.add(egui::Button::new(label).shortcut_text(shortcut.replace("Cmd", cmd))).clicked();
    if clicked {
        ui.close_menu();
    }
    clicked
}

pub fn recent_menu(ui: &mut egui::Ui, e: &mut Engine, g: &mut Gui) {
    if g.recent.is_empty() {
        ui.weak("No recent files");
        return;
    }
    // Whether each file is still there is checked on another thread, at most every two seconds:
    // a path on an unreachable network drive can take a long time to answer, and must not
    // freeze the window while the menu is open.
    match g.recent_check.as_ref().map(|rx| rx.try_recv()) {
        Some(Ok(gone)) => (g.recent_gone, g.recent_check) = (gone, None),
        Some(Err(mpsc::TryRecvError::Disconnected)) => g.recent_check = None,
        _ => {}
    }
    if g.recent_check.is_none() && g.recent_checked.is_none_or(|t| t.elapsed() > Duration::from_secs(2)) {
        let (tx, rx) = mpsc::channel();
        let (list, ctx) = (g.recent.clone(), ui.ctx().clone());
        std::thread::spawn(move || {
            let _ = tx.send(list.into_iter().filter(|p| !p.exists()).collect());
            ctx.request_repaint();
        });
        (g.recent_check, g.recent_checked) = (Some(rx), Some(Instant::now()));
    }
    let short = |s: String| if s.chars().count() > 64 { format!("{}...{}", s.chars().take(30).collect::<String>(), s.chars().skip(s.chars().count() - 30).collect::<String>()) } else { s };
    let mut pick = None;
    for p in &g.recent {
        let name = short(p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| p.display().to_string()));
        // Two files with the same name are told apart by their folder.
        let twin = g.recent.iter().filter(|q| q.file_name() == p.file_name()).count() > 1;
        let label = match (twin, p.parent()) {
            (true, Some(dir)) => format!("{name}  -  {}", short(dir.display().to_string())),
            _ => name,
        };
        let there = !g.recent_gone.contains(p);
        let b = ui.add_enabled(there, egui::Button::new(label));
        let b = if there { b.on_hover_text(p.display().to_string()) } else { b.on_disabled_hover_text(format!("This file is no longer there: {}", p.display())) };
        if b.clicked() {
            pick = Some(p.clone());
            ui.close_menu();
        }
    }
    ui.separator();
    if item(ui, "Clear Recent", "") {
        g.recent.clear();
        (g.recent_dirty, g.recent_cleared) = (true, true);
    }
    if let Some(p) = pick {
        // The same unsaved-changes prompt as File > Open.
        request(e, g, Pending::OpenFile(p));
    }
}

fn menu_bar(ui: &mut egui::Ui, e: &mut Engine, g: &mut Gui) {
    egui::menu::bar(ui, |ui| {
        ui.menu_button("File", |ui| {
            if item(ui, "New...", "Cmd+N") {
                request(e, g, Pending::New);
            }
            if item(ui, "Open...", "Cmd+O") {
                request(e, g, Pending::Open);
            }
            ui.menu_button("Open Recent", |ui| recent_menu(ui, e, g));
            ui.separator();
            if item(ui, "Save", "Cmd+S") {
                save(e, g, false);
            }
            if item(ui, "Save As...", "Cmd+Shift+S") {
                save(e, g, true);
            }
            ui.separator();
            if item(ui, "Import PNG as Layer...", "") {
                if let Some(p) = rfd::FileDialog::new().add_filter("PNG", &["png"]).pick_file() {
                    g.run(e, "import_png_layer", json!({ "path": p.display().to_string() }));
                }
            }
            if item(ui, "Export PNG...", "Cmd+E") {
                export(e, g, "export_png", "png", "", json!({}));
            }
            ui.menu_button("Export PNG Scaled", |ui| {
                for s in [2, 3, 4, 8, 16] {
                    if item(ui, &format!("{s}x..."), "") {
                        export(e, g, "export_png", "png", &format!("_{s}x"), json!({ "scale": s }));
                    }
                }
            });
            ui.separator();
            if item(ui, "Quit", "Cmd+Q") {
                ui.ctx().send_viewport_cmd(ViewportCommand::Close);
            }
        });
        ui.menu_button("Edit", |ui| {
            if ui.add_enabled(!e.undo.is_empty(), egui::Button::new("Undo").shortcut_text("Cmd+Z")).clicked() {
                g.run(e, "undo", json!({}));
                ui.close_menu();
            }
            if ui.add_enabled(!e.redo.is_empty(), egui::Button::new("Redo").shortcut_text("Cmd+Shift+Z")).clicked() {
                g.run(e, "redo", json!({}));
                ui.close_menu();
            }
            ui.separator();
            if item(ui, "Cut", "Cmd+X") {
                g.run(e, "cut", json!({}));
            }
            if item(ui, "Copy", "Cmd+C") {
                g.run(e, "copy", json!({}));
            }
            if item(ui, "Paste", "Cmd+V") {
                paste(e, g);
            }
            if item(ui, "Delete", "Del") {
                g.run(e, "selection_delete", json!({}));
            }
            ui.separator();
            if item(ui, "Copy Image to System Clipboard", "") {
                copy_to_system(e, g);
            }
            if item(ui, "Paste Image from System Clipboard", "") {
                paste_from_system(e, g);
            }
            ui.separator();
            if item(ui, "Grab Selection as Brush", "Cmd+B") && g.run(e, "grab_brush", json!({})).is_some() {
                e.o.tool = Tool::Pencil;
            }
            if item(ui, "Fill Selection with Foreground", "") {
                g.run(e, "selection_fill", json!({}));
            }
        });
        ui.menu_button("Select", |ui| {
            if item(ui, "All", "Cmd+A") {
                g.run(e, "select_all", json!({}));
            }
            if item(ui, "None", "Cmd+D") {
                g.run(e, "select_none", json!({}));
            }
            if item(ui, "Invert", "Cmd+Shift+I") {
                g.run_select(e, "select_invert", json!({}));
            }
            ui.separator();
            if item(ui, "Lift Selection (Move)", "") && g.run(e, "float_lift", json!({})).is_some() {
                e.o.tool = Tool::Move;
            }
            if item(ui, "Commit Floating Pixels", "Enter") {
                g.run(e, "float_commit", json!({}));
            }
            if item(ui, "Crop Canvas to Selection", "") {
                g.run(e, "crop_to_selection", json!({}));
            }
            ui.separator();
            if item(ui, "Animate Selection", "") {
                panels::animate_selection(e, g);
            }
        });
        ui.menu_button("Image", |ui| {
            if item(ui, "Canvas Size...", "") {
                g.dialog = Dialog::Resize { w: e.doc.w, h: e.doc.h, anchor: 0 };
            }
            if item(ui, "Scale Image...", "") {
                g.dialog = Dialog::Scale { w: e.doc.w, h: e.doc.h };
            }
            if item(ui, "Reduce Colours...", "") {
                g.dialog = Dialog::Reduce { colors: e.doc.palette.colors.len().min(256) as u32, dither: 0, generate: true };
            }
            ui.separator();
            if item(ui, "Flip Horizontal", "") {
                g.run(e, "flip", json!({ "axis": "h" }));
            }
            if item(ui, "Flip Vertical", "") {
                g.run(e, "flip", json!({ "axis": "v" }));
            }
            if item(ui, "Rotate 90 Clockwise", "") {
                g.run(e, "rotate", json!({ "turns": 1 }));
            }
            if item(ui, "Rotate 90 Anticlockwise", "") {
                g.run(e, "rotate", json!({ "turns": 3 }));
            }
            if item(ui, "Rotate 180", "") {
                g.run(e, "rotate", json!({ "turns": 2 }));
            }
        });
        ui.menu_button("Layer", |ui| {
            if item(ui, "New Layer", "") {
                g.run(e, "layer_add", json!({}));
            }
            if item(ui, "Duplicate", "") {
                g.run(e, "layer_duplicate", json!({}));
            }
            if item(ui, "Delete", "") {
                g.run(e, "layer_delete", json!({}));
            }
            ui.separator();
            if item(ui, "Merge Down", "") {
                g.run(e, "layer_merge_down", json!({}));
            }
            if item(ui, "Flatten Image", "") {
                g.run(e, "layer_flatten", json!({}));
            }
            if item(ui, "Clear Layer", "") {
                g.run(e, "layer_clear", json!({}));
            }
        });
        ui.menu_button("Palette", |ui| {
            ui.menu_button("Load Preset", |ui| {
                for p in PRESETS {
                    if item(ui, p, "") {
                        g.run(e, "palette_load_preset", json!({ "name": p }));
                    }
                }
            });
            ui.separator();
            if item(ui, "Sort by Brightness", "") {
                g.run(e, "palette_sort", json!({ "by": "luma" }));
            }
            if item(ui, "Sort by Hue", "") {
                g.run(e, "palette_sort", json!({ "by": "hue" }));
            }
            if item(ui, "Merge Duplicate Colours", "") {
                g.run(e, "palette_cleanup", json!({}));
            }
            if item(ui, "Remove Unused Colours", "") {
                g.run(e, "palette_cleanup", json!({ "unused": true }));
            }
            ui.separator();
            let mut cyc = e.view.cycling;
            if ui.checkbox(&mut cyc, "Preview Colour Cycling").changed() {
                e.view.cycling = cyc;
                e.rev += 1;
            }
        });
        ui.menu_button("Tiles", |ui| {
            let has = e.doc.tileset.is_some();
            if ui.add_enabled(has, egui::Button::new("Make Layer Tile Mapped")).clicked() {
                g.run(e, "layer_to_tiles", json!({}));
                ui.close_menu();
            }
            if ui.add_enabled(has, egui::Button::new("Detach Layer from Tiles")).clicked() {
                g.run(e, "layer_detach_tiles", json!({}));
                ui.close_menu();
            }
            ui.separator();
            if ui.add_enabled(has, egui::Button::new("Remove Unused Tiles")).clicked() {
                g.run(e, "tiles_remove_unused", json!({}));
                ui.close_menu();
            }
            if ui.add_enabled(has, egui::Button::new("Merge Identical Tiles")).clicked() {
                g.run(e, "tiles_dedupe", json!({}));
                ui.close_menu();
            }
            ui.separator();
            if ui.add_enabled(has, egui::Button::new("Export Tile Set PNG...")).clicked() {
                export(e, g, "export_tileset_png", "png", "_tiles", json!({}));
                ui.close_menu();
            }
            if ui.add_enabled(has, egui::Button::new("Export Tile Map CSV...")).clicked() {
                export(e, g, "export_tilemap_csv", "csv", "_map", json!({}));
                ui.close_menu();
            }
            ui.separator();
            if ui.add_enabled(has, egui::Button::new("Delete Tile Set")).clicked() {
                g.run(e, "tileset_delete", json!({}));
                ui.close_menu();
            }
        });
        ui.menu_button("View", |ui| {
            if item(ui, "Zoom In", "+") {
                zoom(e, 1);
            }
            if item(ui, "Zoom Out", "-") {
                zoom(e, -1);
            }
            if item(ui, "Fit in Window", "0") {
                e.view.fit = true;
            }
            if item(ui, "Actual Size", "Cmd+1") {
                e.view.zoom = 1.0;
            }
            ui.separator();
            ui.checkbox(&mut e.view.pixel_grid, "Pixel Grid");
            ui.checkbox(&mut e.view.grid, "Grid");
            ui.horizontal(|ui| {
                ui.label("Grid size");
                ui.add(egui::DragValue::new(&mut e.view.grid_w).range(1..=1024));
                ui.label("x");
                ui.add(egui::DragValue::new(&mut e.view.grid_h).range(1..=1024));
            });
            ui.checkbox(&mut e.view.tile_grid, "Tile Grid");
            ui.checkbox(&mut e.view.tile_ids, "Tile Numbers");
            ui.checkbox(&mut e.view.anim_guides, "Animation Frames");
        });
        ui.menu_button("Help", |ui| {
            if item(ui, "Keyboard Shortcuts", "") {
                g.dialog = Dialog::Shortcuts;
            }
            ui.menu_button("Keyboard Preset", |ui| {
                for k in Keymap::ALL {
                    if ui.radio_value(&mut e.view.keymap, k, k.label()).clicked() {
                        ui.close_menu();
                    }
                }
            });
            if item(ui, "AI / MCP Setup", "") {
                g.dialog = Dialog::Mcp;
            }
        });
    });
}

// ------------------------------------------------------------------ dialogs

const ANCHORS: [&str; 9] = ["top-left", "top", "top-right", "left", "center", "right", "bottom-left", "bottom", "bottom-right"];
const DITHERS: [&str; 3] = ["none", "bayer", "floyd"];

fn size_fields(ui: &mut egui::Ui, w: &mut u32, h: &mut u32) {
    egui::Grid::new("size").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
        ui.label("Width");
        ui.add(egui::DragValue::new(w).range(1..=MAX_DIM).suffix(" px"));
        ui.end_row();
        ui.label("Height");
        ui.add(egui::DragValue::new(h).range(1..=MAX_DIM).suffix(" px"));
        ui.end_row();
    });
}

/// Returns (confirmed, cancelled).
fn buttons(ui: &mut egui::Ui, ok: &str) -> (bool, bool) {
    ui.add_space(8.0);
    let mut r = (false, false);
    ui.horizontal(|ui| {
        r.0 = ui.add(egui::Button::new(egui::RichText::new(ok).color(Color32::WHITE)).fill(Color32::from_rgb(58, 104, 196))).clicked();
        r.1 = ui.button("Cancel").clicked();
    });
    if ui.input(|i| i.key_pressed(Key::Escape)) {
        r.1 = true;
    }
    r
}

fn window(ctx: &egui::Context, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Window::new(title).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, -40.0]).show(ctx, |ui| {
        ui.add_space(4.0);
        add(ui);
    });
}

pub fn dialogs(ctx: &egui::Context, e: &mut Engine, g: &mut Gui) {
    let mut dialog = std::mem::replace(&mut g.dialog, Dialog::None);
    let mut close = false;
    match &mut dialog {
        Dialog::None => {}
        Dialog::New { w, h, preset } => window(ctx, "New Document", |ui| {
            size_fields(ui, w, h);
            ui.horizontal(|ui| {
                for (label, pw, ph) in [("16", 16, 16), ("32", 32, 32), ("64", 64, 64), ("128", 128, 128), ("256", 256, 256), ("320x240", 320, 240)] {
                    if ui.small_button(label).clicked() {
                        (*w, *h) = (pw, ph);
                    }
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Palette");
                egui::ComboBox::from_id_salt("preset").selected_text(PRESETS[*preset]).show_ui(ui, |ui| {
                    for (i, p) in PRESETS.iter().enumerate() {
                        ui.selectable_value(preset, i, *p);
                    }
                });
            });
            let (ok, cancel) = buttons(ui, "Create");
            if ok {
                let p = json!({ "width": *w, "height": *h, "palette": PRESETS[*preset], "force": true });
                close = g.run(e, "new_document", p).is_some();
            }
            close |= cancel;
        }),
        Dialog::Resize { w, h, anchor } => window(ctx, "Canvas Size", |ui| {
            size_fields(ui, w, h);
            ui.label("Keep existing pixels at");
            egui::Grid::new("anchor").spacing([4.0, 4.0]).show(ui, |ui| {
                for (i, a) in ANCHORS.iter().enumerate() {
                    if ui.add_sized([26.0, 22.0], egui::SelectableLabel::new(*anchor == i, "")).on_hover_text(*a).clicked() {
                        *anchor = i;
                    }
                    if i % 3 == 2 {
                        ui.end_row();
                    }
                }
            });
            let (ok, cancel) = buttons(ui, "Resize");
            if ok {
                close = g.run(e, "resize_canvas", json!({ "width": *w, "height": *h, "anchor": ANCHORS[*anchor] })).is_some();
            }
            close |= cancel;
        }),
        Dialog::Scale { w, h } => window(ctx, "Scale Image", |ui| {
            size_fields(ui, w, h);
            ui.horizontal(|ui| {
                for (label, n, d) in [("50%", 1, 2), ("200%", 2, 1), ("300%", 3, 1), ("400%", 4, 1)] {
                    if ui.small_button(label).clicked() {
                        *w = (e.doc.w * n / d).clamp(1, MAX_DIM);
                        *h = (e.doc.h * n / d).clamp(1, MAX_DIM);
                    }
                }
            });
            ui.weak("Nearest-neighbour, so pixels stay crisp.");
            let (ok, cancel) = buttons(ui, "Scale");
            if ok {
                close = g.run(e, "scale_image", json!({ "width": *w, "height": *h })).is_some();
            }
            close |= cancel;
        }),
        Dialog::Reduce { colors, dither, generate } => window(ctx, "Reduce Colours", |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(generate, true, "Build palette from image");
                ui.selectable_value(generate, false, "Use current palette");
            });
            ui.add_enabled(*generate, egui::Slider::new(colors, 2..=256).text("colours"));
            ui.horizontal(|ui| {
                ui.label("Dither");
                for (i, d) in ["None", "Ordered", "Floyd-Steinberg"].iter().enumerate() {
                    ui.selectable_value(dither, i, *d);
                }
            });
            let (ok, cancel) = buttons(ui, "Reduce");
            if ok {
                let p = json!({ "colors": *colors, "dither": DITHERS[*dither], "palette": if *generate { "generate" } else { "current" } });
                close = g.run(e, "reduce_colors", p).is_some();
            }
            close |= cancel;
        }),
        Dialog::Shortcuts => window(ctx, "Keyboard Shortcuts", |ui| {
            ui.horizontal(|ui| {
                ui.label("Preset");
                for k in Keymap::ALL {
                    ui.selectable_value(&mut e.view.keymap, k, k.label());
                }
            });
            ui.add_space(4.0);
            let dpaint = e.view.keymap == Keymap::DPaint;
            egui::ScrollArea::vertical().max_height(520.0).show(ui, |ui| {
            egui::Grid::new("keys").num_columns(2).spacing([24.0, 5.0]).striped(true).show(ui, |ui| {
                if dpaint {
                    for b in keys::DPAINT {
                        ui.label(b.what);
                        ui.monospace(b.key);
                        ui.end_row();
                    }
                } else {
                    for t in Tool::ALL {
                        ui.label(t.label());
                        ui.monospace(t.key().to_string());
                        ui.end_row();
                    }
                    for (a, b) in [("Brush size", "[ and ]"), ("Swap colours", "X"), ("Zoom", "+ / -, 0 to fit")] {
                        ui.label(a);
                        ui.monospace(b);
                        ui.end_row();
                    }
                }
                for (a, b) in [
                    ("Draw with background colour", "Right mouse button"),
                    ("Pick colour while drawing", "Alt + click"),
                    ("Pan", "Space + drag, or middle mouse"),
                    ("Zoom", "Scroll or pinch; Cmd/Ctrl + 0 fits, Cmd/Ctrl + 1 is 100%"),
                    ("Constrain shape / line angle", "Shift while dragging"),
                    ("Add / subtract selection", "Shift / Alt with Select or Wand"),
                    ("Grab selection as brush", "Cmd/Ctrl + B"),
                    ("Nudge selection", "Arrow keys (Shift = 8 px)"),
                    ("Animation: step / play", "Left and Right / Space, pointer over the Animation tab"),
                    ("Commit / cancel floating pixels", "Enter / Esc"),
                    ("Toggle grid", "Cmd/Ctrl + G"),
                    ("Run script", "Cmd/Ctrl + Enter in the Script tab"),
                    ("Flash a palette colour on the canvas", "Alt + hover the palette"),
                ] {
                    ui.label(a);
                    ui.monospace(b);
                    ui.end_row();
                }
            });
            if dpaint {
                ui.add_space(4.0);
                ui.weak("Tools without a Deluxe Paint key (eraser, gradient, wand, move, tile stamp, pan) are on the toolbar.");
                ui.weak(format!("Deluxe Paint keys with no equivalent here do nothing: {}.", keys::DPAINT_UNBOUND));
            }
            });
            close = buttons(ui, "Close") != (false, false);
        }),
        Dialog::Mcp => window(ctx, "AI / MCP", |ui| {
            ui.set_max_width(560.0);
            let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "jon_motion".into());
            ui.label("Any MCP client (Claude Code, Claude Desktop, ...) can drive this editor. AI edits land on the document you have open, between your own strokes, and each one is a single undo step.");
            ui.add_space(6.0);
            ui.strong("Recommended: stdio");
            ui.weak("Works whether or not the app is open - it attaches to this window when it is running.");
            let stdio = format!("claude mcp add jon-motion -- \"{exe}\" mcp");
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(egui::RichText::new(&stdio).monospace()).wrap());
            });
            if ui.button("Copy command").clicked() {
                ui.ctx().copy_text(stdio.clone());
            }
            if let Some(port) = g.mcp_port {
                ui.add_space(6.0);
                ui.strong("Or HTTP, direct to this window");
                let http = format!("claude mcp add --transport http jon-motion http://127.0.0.1:{port}/mcp --header \"Authorization: Bearer {}\"", remote::auth_token());
                ui.weak("The token keeps other users and web pages out; it is stored in ~/.jon_motion/token.");
                ui.add(egui::Label::new(egui::RichText::new(&http).monospace()).wrap());
                if ui.button("Copy command ").clicked() {
                    ui.ctx().copy_text(http);
                }
            }
            ui.add_space(6.0);
            ui.weak("Tools: get_info, list_commands, execute_command, batch, run_lua, screenshot.");
            close = buttons(ui, "Close") != (false, false);
        }),
    }
    if !close && matches!(g.dialog, Dialog::None) {
        g.dialog = dialog;
    }

    if let Some(what) = g.pending.take() {
        let mut choice = None;
        window(ctx, "Unsaved Changes", |ui| {
            ui.label(format!("Save changes to \"{}\" first?", file_stem(e)));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(egui::RichText::new("Save").color(Color32::WHITE)).fill(Color32::from_rgb(58, 104, 196))).clicked() {
                    choice = Some(0);
                }
                if ui.button("Don't Save").clicked() {
                    choice = Some(1);
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                    choice = Some(2);
                }
            });
        });
        match choice {
            None => g.pending = Some(what),
            Some(0) => {
                if save(e, g, false) {
                    proceed(e, g, what);
                }
            }
            Some(1) => proceed(e, g, what),
            _ => {}
        }
    }
}

// ------------------------------------------------------------------ frame

/// Lock the engine, giving a short-lived MCP call time to finish.
fn lock_soon(m: &Mutex<Engine>, wait: Duration) -> Option<MutexGuard<'_, Engine>> {
    let start = Instant::now();
    loop {
        match m.try_lock() {
            Ok(g) => return Some(g),
            Err(TryLockError::Poisoned(p)) => return Some(p.into_inner()),
            Err(TryLockError::WouldBlock) if start.elapsed() > wait => return None,
            Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(2)),
        }
    }
}

/// Shown while a script or a long AI request has the document: the window stays
/// responsive and the script can be stopped.
fn busy_screen(ctx: &egui::Context, g: &mut Gui) {
    if ctx.input(|i| i.viewport().close_requested()) {
        if g.allow_close {
            return;
        }
        // Stop the script if it can be stopped; if the window is still busy the user may quit anyway.
        ctx.send_viewport_cmd(ViewportCommand::CancelClose);
        script::cancel();
        g.busy_quit = true;
    }
    egui::CentralPanel::default().show(ctx, |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.4);
            ui.spinner();
            ui.add_space(8.0);
            ui.label("A script or AI request is working on the document...");
            ui.add_space(8.0);
            if ui.button("Stop script").on_hover_text("Stops a running Lua script; its changes are rolled back").clicked() {
                script::cancel();
            }
            if g.busy_quit {
                ui.add_space(16.0);
                ui.label("The document is still busy. Quitting now loses any unsaved changes.");
                ui.add_space(4.0);
                if ui.button("Quit anyway").clicked() {
                    g.allow_close = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
                if ui.button("Keep waiting").clicked() {
                    g.busy_quit = false;
                }
            }
        });
    });
    ctx.request_repaint_after(Duration::from_millis(50));
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(out) = self.gui.script_job.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.gui.script_job = None;
            self.gui.script_done(out);
        }
        if std::mem::take(&mut self.gui.script_req) && self.gui.script_job.is_none() {
            // Scripts run off the UI thread so a slow one cannot freeze the window.
            let (tx, rx) = mpsc::channel();
            let (engine, code, ctx2) = (self.engine.clone(), self.gui.script.clone(), ctx.clone());
            self.gui.script_job = Some(rx);
            std::thread::spawn(move || {
                let out = script::run_lua(&mut engine.lock().unwrap_or_else(|p| p.into_inner()), &code);
                let _ = tx.send(out);
                ctx2.request_repaint();
            });
            std::thread::sleep(Duration::from_millis(2));
        }
        // The engine is locked for the whole frame; MCP calls and scripts run between frames.
        let arc = self.engine.clone();
        let Some(mut guard) = lock_soon(&arc, Duration::from_millis(200)) else { return busy_screen(ctx, &mut self.gui) };
        let e = &mut *guard;
        let g = &mut self.gui;
        g.busy_quit = false;
        g.flash_now = g.flash.take();

        let title = format!("{}{} - Jon Motion", e.path.as_ref().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Untitled".into()), if e.modified { " *" } else { "" });
        if title != g.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            g.title = title;
        }
        if ctx.input(|i| i.viewport().close_requested()) && e.modified && !g.allow_close {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            g.pending = Some(Pending::Quit);
        }
        if g.allow_close {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }

        dropped(ctx, e, g);
        drop_hint(ctx);
        let modal = !matches!(g.dialog, Dialog::None) || g.pending.is_some();
        if !modal {
            shortcuts(ctx, e, g);
            if g.saved_keymap.is_some() {
                save_settings(e, g);
            }
        } else {
            g.anim_rect = egui::Rect::NOTHING;
        }
        egui::TopBottomPanel::top("menu").show(ctx, |ui| menu_bar(ui, e, g));
        egui::TopBottomPanel::top("options").exact_height(34.0).show(ctx, |ui| panels::options(ui, e, g));
        egui::TopBottomPanel::bottom("status").exact_height(24.0).show(ctx, |ui| panels::status(ui, e, g));
        egui::SidePanel::left("tools").exact_width(46.0).resizable(false).show(ctx, |ui| panels::toolbar(ui, e, g));
        if let Some(at) = g.show_tab.take().and_then(|t| self.dock.find_tab(&t)) {
            self.dock.set_active_tab(at);
        }
        let mut style = egui_dock::Style::from_egui(ctx.style().as_ref());
        style.tab_bar.fill_tab_bar = true;
        DockArea::new(&mut self.dock)
            .style(style)
            .show_close_buttons(false)
            .show_add_buttons(false)
            .show_leaf_close_all_buttons(false)
            .show_leaf_collapse_buttons(false)
            .show(ctx, &mut Viewer { e, g });
        dialogs(ctx, e, g);

        e.fix_state();
        let cycling = e.view.cycling && e.doc.palette.ramps.iter().any(|r| r.cycle);
        if cycling || g.flash.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        } else if e.doc.selection.is_some() || e.doc.float.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        remote::stop_http();
    }
}
