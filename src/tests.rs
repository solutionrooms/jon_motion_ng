//! End-to-end tests: the Lua command suite, the MCP protocol, and the
//! interactive canvas driven by synthetic pointer input (no window needed).

use crate::app::Gui;
use crate::canvas;
use crate::engine::*;
use crate::remote;
use crate::script;
use eframe::egui::{self, pos2, vec2, Event, Modifiers, PointerButton, Pos2, Rect};
use serde_json::{json, Value};

#[test]
fn lua_smoke_suite() {
    let mut e = Engine::new("headless");
    let out = script::run_lua(&mut e, include_str!("../tests/smoke.lua"));
    assert_eq!(out.error, None, "output so far:\n{}", out.output);
    assert!(out.output.contains("smoke ok"));
}

#[test]
fn script_errors_roll_back_and_scripts_are_one_undo_step() {
    let mut e = Engine::new("headless");
    let before = e.doc.clone();
    let out = script::run_lua(&mut e, "jm.draw_rect{x=0,y=0,w=9,h=9,filled=true} jm.layer_add{} error('boom')");
    assert!(out.error.unwrap().contains("boom"));
    assert!(e.doc == before && e.undo.is_empty() && !e.modified);

    let out = script::run_lua(&mut e, "for i = 0, 50 do jm.set(i, i, 12) end jm.layer_add{}");
    assert_eq!(out.error, None);
    assert_eq!(e.undo.len(), 1);
    e.exec("undo", &json!({})).unwrap();
    assert!(e.doc == before);
}

#[test]
fn sandbox_has_no_file_or_os_access() {
    let mut e = Engine::new("headless");
    let out = script::run_lua(&mut e, "return tostring(io) .. tostring(os) .. tostring(require) .. tostring(dofile)");
    assert_eq!(out.result, json!("nilnilnilnil"));
}

#[test]
fn save_and_reload_round_trips() {
    let mut e = Engine::new("headless");
    let dir = std::env::temp_dir().join(format!("jmn_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (jmn, png) = (dir.join("a.jmn"), dir.join("a.png"));
    let code = format!(
        "jm.draw_ellipse{{x=4,y=4,w=40,h=30,color=44,filled=true}} jm.layer_add{{name='L2'}} jm.set(1,1,7)
         jm.tileset_create{{tile_w=16,tile_h=16}} jm.layer_to_tiles{{layer=0}} jm.ramp_add{{from=1,to=5,name='R'}}
         jm.save_document{{path=[[{}]]}} jm.export_png{{path=[[{}]]}}",
        jmn.display(),
        png.display()
    );
    assert_eq!(script::run_lua(&mut e, &code).error, None);
    assert!(!e.modified, "a save at the end of a script leaves the document clean");
    let saved = e.doc.clone();
    e.exec("new_document", &json!({"width": 8, "height": 8})).unwrap();
    e.exec("open_document", &json!({"path": jmn.display().to_string()})).unwrap();
    assert!(e.doc == saved, "project file did not round-trip");
    // An indexed PNG comes back with the same pixels and palette indices.
    e.exec("open_document", &json!({"path": png.display().to_string()})).unwrap();
    assert_eq!(e.doc.mode, crate::doc::ColorMode::Indexed);
    assert_eq!(e.doc.layers[0].data[20 * 320 + 20], 44);
    assert_eq!(e.doc.layers[0].data[320 + 1], 7);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn unsaved_changes_are_protected_from_remote_replacement() {
    let mut e = Engine::new("gui");
    e.exec("draw_line", &json!({"x0": 0, "y0": 0, "x1": 5, "y1": 5})).unwrap();
    assert!(e.modified);
    assert!(e.exec("new_document", &json!({"width": 8, "height": 8})).unwrap_err().contains("unsaved"));
    assert_eq!(e.doc.w, 320);
    e.exec("new_document", &json!({"width": 8, "height": 8, "force": true})).unwrap();
    assert_eq!(e.doc.w, 8);
}

#[test]
fn mcp_protocol() {
    let mut e = Engine::new("headless");
    let mut call = |name: &str, args: &Value| remote::call_tool(&mut e, name, args);
    let init = remote::handle_rpc(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}}), &mut call).unwrap();
    assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
    assert!(remote::handle_rpc(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}), &mut call).is_none());
    let tools = remote::handle_rpc(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}), &mut call).unwrap();
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 6);
    let req = |name: &str, args: Value| json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": name, "arguments": args}});
    let r = remote::handle_rpc(&req("execute_command", json!({"command": "draw_rect", "params": {"x": 1, "y": 1, "w": 4, "h": 4, "color": 9}})), &mut call).unwrap();
    assert_eq!(r["result"]["isError"], false);
    // A failing batch applies nothing.
    let r = remote::handle_rpc(&req("batch", json!({"commands": [{"command": "layer_clear"}, {"command": "bogus"}]})), &mut call).unwrap();
    assert_eq!(r["result"]["isError"], true);
    let r = remote::handle_rpc(&req("run_lua", json!({"code": "return jm.get(1, 1)"})), &mut call).unwrap();
    assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains('9'));
    let r = remote::handle_rpc(&req("screenshot", json!({})), &mut call).unwrap();
    assert_eq!(r["result"]["content"][0]["mimeType"], "image/png");
    let r = remote::handle_rpc(&json!({"jsonrpc": "2.0", "id": 4, "method": "nope"}), &mut call).unwrap();
    assert_eq!(r["error"]["code"], -32601);
}

// ---------------------------------------------------------------- interactive canvas

struct Rig {
    ctx: egui::Context,
    e: Engine,
    g: Gui,
    time: f64,
    mods: Modifiers,
}

impl Rig {
    fn new() -> Rig {
        let mut r = Rig { ctx: egui::Context::default(), e: Engine::new("gui"), g: Gui::new(), time: 0.0, mods: Modifiers::NONE };
        r.frame(vec![]);
        r.frame(vec![]);
        r
    }
    fn frame(&mut self, events: Vec<Event>) {
        self.time += 1.0 / 60.0;
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 800.0))), time: Some(self.time), modifiers: self.mods, events, ..Default::default() };
        let (e, g) = (&mut self.e, &mut self.g);
        let _ = self.ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| canvas::show(ui, e, g));
        });
    }
    /// Screen position of the centre of an image pixel.
    fn at(&self, x: i32, y: i32) -> Pos2 {
        let v = &self.e.view;
        self.g.canvas_rect.center() + (vec2(x as f32 + 0.5, y as f32 + 0.5) - vec2(v.cx, v.cy)) * v.zoom
    }
    fn button(&mut self, p: Pos2, button: PointerButton, pressed: bool, modifiers: Modifiers) {
        self.mods = modifiers;
        self.frame(vec![Event::PointerButton { pos: p, button, pressed, modifiers }]);
    }
    fn drag_with(&mut self, from: (i32, i32), to: (i32, i32), button: PointerButton, mods: Modifiers) {
        let (a, b) = (self.at(from.0, from.1), self.at(to.0, to.1));
        self.frame(vec![Event::PointerMoved(a)]);
        self.button(a, button, true, mods);
        for i in 1..=4 {
            self.frame(vec![Event::PointerMoved(a + (b - a) * (i as f32 / 4.0))]);
        }
        self.button(b, button, false, mods);
        self.mods = Modifiers::NONE;
        self.frame(vec![]);
    }
    fn drag(&mut self, from: (i32, i32), to: (i32, i32)) {
        self.drag_with(from, to, PointerButton::Primary, Modifiers::NONE);
    }
    fn px(&self, x: i32, y: i32) -> u32 {
        self.e.doc.layers[self.e.doc.active].data[(y * self.e.doc.w as i32 + x) as usize]
    }
}

#[test]
fn canvas_pencil_stroke_is_one_undo_step() {
    let mut r = Rig::new();
    assert!(r.e.view.zoom >= 1.0 && !r.e.view.fit, "view should have been fitted");
    r.drag((10, 10), (40, 10));
    for x in 10..=40 {
        assert_eq!(r.px(x, 10), 15, "pixel {x} of the stroke");
    }
    assert_eq!(r.px(41, 10), 256);
    assert!(!r.e.busy() && r.g.drag.is_none(), "gesture must end on release");
    assert_eq!(r.e.undo.len(), 1);
    // Right button paints the background colour.
    r.drag_with((10, 20), (20, 20), PointerButton::Secondary, Modifiers::NONE);
    assert_eq!(r.px(15, 20), 0);
    r.e.exec("undo", &json!({"steps": 2})).unwrap();
    assert_eq!(r.px(20, 10), 256);
}

#[test]
fn canvas_shapes_fill_select_and_move() {
    let mut r = Rig::new();
    r.e.o.tool = Tool::Rect;
    r.e.o.filled = true;
    r.drag((5, 5), (24, 14));
    assert_eq!(r.px(5, 5), 15);
    assert_eq!(r.px(24, 14), 15);
    assert_eq!(r.px(25, 15), 256);
    assert_eq!(r.e.undo.len(), 1, "live preview frames must not add undo steps");

    r.e.o.tool = Tool::Ellipse;
    r.e.o.filled = false;
    r.e.o.fg = 40;
    r.drag((40, 40), (60, 60));
    assert_eq!(r.px(50, 40), 40);
    assert_eq!(r.px(50, 50), 256);

    r.e.o.tool = Tool::Fill;
    r.e.o.fg = 99;
    let p = r.at(50, 50);
    r.frame(vec![Event::PointerMoved(p)]);
    r.button(p, PointerButton::Primary, true, Modifiers::NONE);
    r.button(p, PointerButton::Primary, false, Modifiers::NONE);
    assert_eq!(r.px(50, 50), 99);
    assert_eq!(r.px(0, 0), 256, "fill stays inside the ellipse");

    r.e.o.tool = Tool::Select;
    r.drag((5, 5), (24, 14));
    assert_eq!(r.e.doc.sel_bounds(), Some((5, 5, 24, 14)));
    r.drag_with((30, 5), (34, 9), PointerButton::Primary, Modifiers::SHIFT);
    assert_eq!(r.e.doc.sel_bounds(), Some((5, 5, 34, 14)), "shift adds to the selection");
    r.e.exec("select_rect", &json!({"x": 5, "y": 5, "w": 20, "h": 10})).unwrap();

    r.e.o.tool = Tool::Move;
    let undo = r.e.undo.len();
    r.drag((10, 10), (110, 60));
    assert!(r.e.doc.float.is_some(), "moved pixels float until committed");
    assert_eq!(r.e.undo.len(), undo + 1);
    r.e.exec("float_commit", &json!({})).unwrap();
    assert_eq!(r.px(105, 55), 15);
    assert_eq!(r.px(5, 5), 256);
    assert_eq!(r.e.doc.sel_bounds(), Some((105, 55, 124, 64)));

    // Alt-click picks a colour instead of painting.
    r.e.o.tool = Tool::Pencil;
    let p = r.at(110, 60);
    r.frame(vec![Event::PointerMoved(p)]);
    r.button(p, PointerButton::Primary, true, Modifiers::ALT);
    r.button(p, PointerButton::Primary, false, Modifiers::ALT);
    r.mods = Modifiers::NONE;
    assert_eq!(r.e.o.fg, 15);
}

#[test]
fn canvas_tile_stamp_and_sync_drawing() {
    let mut r = Rig::new();
    r.e.exec("tileset_create", &json!({"tile_w": 16, "tile_h": 16})).unwrap();
    r.e.exec("layer_to_tiles", &json!({})).unwrap();
    r.e.exec("draw_rect", &json!({"x": 0, "y": 0, "w": 16, "h": 16, "color": 30, "filled": true})).unwrap();
    assert_eq!(r.e.doc.tileset.as_ref().unwrap().tiles.len(), 2);
    r.e.o.tool = Tool::TileStamp;
    r.e.o.active_tile = 1;
    r.drag((40, 40), (90, 40));
    let map = r.e.doc.layers[0].map.clone().unwrap();
    let cols = 320 / 16;
    assert_eq!((map[2 * cols + 2], map[2 * cols + 5], map[2 * cols + 6]), (1, 1, 0));
    assert_eq!(r.px(88, 44), 30);
    // Sync mode: drawing in one instance shows up in the others, live.
    r.e.o.tool = Tool::Pencil;
    r.e.o.tile_mode = TileMode::Sync;
    r.e.o.fg = 77;
    r.drag((2, 2), (6, 2));
    assert_eq!(r.px(32 + 4, 32 + 2), 77);
    assert_eq!(r.e.doc.tileset.as_ref().unwrap().tiles.len(), 2);
}

#[test]
fn canvas_zoom_and_pan() {
    let mut r = Rig::new();
    let z0 = r.e.view.zoom;
    let p = r.at(100, 100);
    r.frame(vec![Event::PointerMoved(p)]);
    r.frame(vec![Event::MouseWheel { unit: egui::MouseWheelUnit::Line, delta: vec2(0.0, 3.0), modifiers: Modifiers::NONE }]);
    for _ in 0..30 {
        r.frame(vec![]);
    }
    assert!(r.e.view.zoom > z0, "scrolling up zooms in");
    let q = r.at(100, 100);
    assert!((q - p).length() < r.e.view.zoom + 1.0, "zoom keeps the pixel under the pointer in place");
    let (cx, undo) = (r.e.view.cx, r.e.undo.len());
    r.drag_with((100, 100), (90, 100), PointerButton::Middle, Modifiers::NONE);
    assert!(r.e.view.cx > cx, "middle-drag pans");
    assert_eq!(r.e.undo.len(), undo, "panning never touches the document");
}
