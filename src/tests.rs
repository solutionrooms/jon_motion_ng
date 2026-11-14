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

// ---------------------------------------------------------------- audit regressions

fn eng(w: u32, h: u32) -> Engine {
    let mut e = Engine::new("headless");
    e.exec("new_document", &json!({"width": w, "height": h, "force": true})).unwrap();
    e
}

fn pixel(e: &mut Engine, x: i64, y: i64) -> i64 {
    e.exec("get_pixel", &json!({"x": x, "y": y})).unwrap()["color"].as_i64().unwrap()
}

fn tmp(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("jmn_audit_{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name).display().to_string()
}

#[test]
fn sync_mode_never_draws_into_the_blank_tile() {
    let mut e = eng(32, 32);
    e.exec("tileset_create", &json!({"tile_w": 8, "tile_h": 8})).unwrap();
    e.exec("layer_to_tiles", &json!({})).unwrap();
    e.exec("set_options", &json!({"tile_mode": "sync"})).unwrap();
    e.exec("draw_line", &json!({"x0": 1, "y0": 1, "x1": 1, "y1": 1, "color": 44})).unwrap();
    assert_eq!(pixel(&mut e, 1, 1), 44);
    assert_eq!(pixel(&mut e, 25, 25), -1, "other blank cells stay blank");
    assert_eq!(e.exec("tile_get_pixels", &json!({"tile": 0})).unwrap()["rows"][1][1], -1, "tile 0 stays blank");
    assert_eq!(e.exec("tile_get_cell", &json!({"col": 0, "row": 0})).unwrap()["tile"], 1, "the cell got its own tile");
    // Drawing again in that cell edits tile 1 rather than making another.
    e.exec("draw_line", &json!({"x0": 2, "y0": 2, "x1": 2, "y1": 2, "color": 45})).unwrap();
    assert_eq!(e.doc.tileset.as_ref().unwrap().tiles.len(), 2);
}

#[test]
fn a_panic_inside_a_command_is_an_error_and_the_engine_recovers() {
    let mut e = eng(16, 16);
    let before = e.doc.clone();
    let r: Result<(), String> = e.group(|e| {
        e.exec("draw_rect", &json!({"x": 0, "y": 0, "w": 4, "h": 4}))?;
        e.stroke_begin(e.paint_opts(false, false))?;
        panic!("boom")
    });
    assert!(r.unwrap_err().contains("internal error: boom"));
    assert!(e.group == 0 && e.stroke.is_none() && !e.busy() && e.doc == before);
    e.exec("layer_add", &json!({})).unwrap();
    assert_eq!(e.exec("undo", &json!({})).unwrap()["undone"], 1);
}

#[test]
fn wild_coordinates_are_harmless() {
    let mut e = eng(16, 16);
    let big = 2147483647i64;
    let t = std::time::Instant::now();
    e.exec("draw_line", &json!({"x0": -10, "y0": 0, "x1": big, "y1": 0, "color": 3})).unwrap();
    assert_eq!(pixel(&mut e, 15, 0), 3);
    e.exec("draw_line", &json!({"x0": 0, "y0": 5, "x1": 20_000_000, "y1": 5, "size": 128, "shape": "round"})).unwrap();
    e.exec("draw_rect", &json!({"x": -big, "y": -big, "w": 9e18, "h": 9e18, "color": 4})).unwrap();
    e.exec("draw_ellipse", &json!({"x": -big, "y": 1, "w": big, "h": big, "filled": true})).unwrap();
    e.exec("draw_stroke", &json!({"points": [0, 0, 1e300, -1e300], "mode": "smear", "size": 64})).unwrap();
    e.exec("select_rect", &json!({"x": 9e18, "y": 9e18, "w": 9e18, "h": 9e18})).unwrap();
    e.exec("select_none", &json!({})).unwrap();
    e.exec("copy", &json!({})).unwrap();
    e.exec("paste", &json!({"x": big, "y": -big, "float": true})).unwrap();
    e.exec("float_move", &json!({"dx": big, "dy": big})).unwrap();
    e.exec("float_commit", &json!({})).unwrap();
    e.exec("put_pixels", &json!({"x": 9e18, "y": 9e18, "rows": [[1, 2]]})).unwrap();
    assert!(e.exec("export_png", &json!({"path": tmp("huge.png"), "scale": 64})).is_ok());
    let mut big_doc = eng(4096, 4096);
    assert!(big_doc.exec("export_png", &json!({"path": tmp("huge.png"), "scale": 64})).unwrap_err().contains("smaller scale"));
    assert!(t.elapsed().as_secs() < 20, "took {:?}", t.elapsed());
}

#[test]
fn crafted_project_files_are_sanitised() {
    use crate::doc::*;
    let mut d = Doc::new(8, 8, ColorMode::Indexed, Palette::preset("pico8").unwrap());
    d.palette.ramps.push(Ramp { name: "bad".into(), indices: vec![3, 200, 201], cycle: true, speed: 8.0 });
    d.tileset = Some(TileSet { tw: 4, th: 4, tiles: vec![std::sync::Arc::new(vec![TRANSP; 16])] });
    d.layers[0].map = Some(std::sync::Arc::new(vec![900; 3]));
    let p = tmp("crafted.jmn");
    crate::io::save_doc(&d, std::path::Path::new(&p)).unwrap();
    let mut e = Engine::new("headless");
    e.exec("open_document", &json!({"path": p})).unwrap();
    assert_eq!(e.doc.palette.ramps[0].indices, vec![3]);
    assert_eq!(e.doc.layers[0].map.as_ref().unwrap().as_slice(), &[0u16; 4], "the tile map was rebuilt");
    e.exec("palette_cleanup", &json!({"unused": true})).unwrap();
    e.exec("tile_make_unique", &json!({"col": 0, "row": 0})).unwrap();
}

#[test]
fn lua_sandbox_limits_hold() {
    let mut e = eng(8, 8);
    let out = script::run_lua(&mut e, "return load(string.dump(function() return 42 end))()");
    assert!(out.error.is_some(), "string.dump is gone: {:?}", out.result);
    let out = script::run_lua(&mut e, "local f, err = load('\\27Lua') return tostring(f) .. ' ' .. tostring(err)");
    assert!(out.result.as_str().unwrap().starts_with("nil "), "bytecode must not load: {}", out.result);
    assert!(script::run_lua(&mut e, "\x1bLua").error.is_some(), "a bytecode script must not load");
    let out = script::run_lua(&mut e, "return load('return 1 + 1')() + load('return x', 'c', 't', {x = 5})()");
    assert_eq!(out.result, json!(7), "load still works for source text: {:?}", out.error);
    let out = script::run_lua(&mut e, "return #string.rep('x', 300 * 1024 * 1024)");
    assert!(out.error.unwrap().contains("memory"));
    // pcall, xpcall and coroutines cannot swallow the time limit.
    let t = std::time::Instant::now();
    let code = "jm.set(0, 0, 5)
        while true do
          pcall(function() while true do end end)
          xpcall(function() while true do end end, function(m) return m end)
          coroutine.resume(coroutine.create(function() while true do end end))
        end";
    let out = script::run_lua_limited(&mut e, code, std::time::Duration::from_millis(300));
    assert!(out.error.unwrap().contains("time limit"));
    assert!(t.elapsed().as_secs() < 10);
    assert_eq!(pixel(&mut e, 0, 0), -1, "a timed-out script is rolled back");
    // Each of these used to escape the limit; every one must end with the time-limit error.
    let ms = std::time::Duration::from_millis(200);
    for code in [
        "coroutine.wrap(function() while true do end end)() return 1",
        "local co = coroutine.create(function() while true do end end) while true do coroutine.resume(co) end",
        "local g = coroutine.wrap(function() coroutine.yield(1) end) g() while true do end",
        "while true do local g = coroutine.wrap(function() coroutine.yield(1) end) g() end",
        "coroutine.wrap(function() coroutine.wrap(function() coroutine.yield() while true do end end)() coroutine.yield() end)() while true do end",
        "coroutine.wrap(function() local co = coroutine.wrap(function() while true do coroutine.yield() end end) while true do co() end end)()",
        "local co = coroutine.create(function() local x <close> = setmetatable({}, {__close = function() while true do end end}) coroutine.yield() end)
         coroutine.resume(co) while true do coroutine.close(co) end",
        "while true do load(function() while true do end end) end",
        "while true do pcall(coroutine.wrap(function() while true do end end)) end",
    ] {
        let t = std::time::Instant::now();
        let out = script::run_lua_limited(&mut e, code, ms);
        assert!(out.error.as_deref().is_some_and(|m| m.contains("time limit")), "{code}\n-> {:?} {:?}", out.error, out.result);
        assert!(t.elapsed().as_secs() < 5, "{code} took {:?}", t.elapsed());
    }
    // Lua runs error handlers and finalizers with hooks off; these are closed off instead.
    let out = script::run_lua_limited(&mut e, "xpcall(function() while true do end end, function() while true do end end)", ms);
    assert!(out.error.unwrap().contains("time limit"));
    let out = script::run_lua_limited(
        &mut e,
        "local co = coroutine.create(function() while true do coroutine.yield() end end)
         local x <close> = setmetatable({}, { __close = function() while true do end end })
         while true do for i = 1, 5000 do end coroutine.resume(co) end",
        ms,
    );
    assert!(out.error.unwrap().contains("time limit"));
    let out = script::run_lua(&mut e, "KEEP = setmetatable({}, { __gc = function() while true do end end }) return 1");
    assert!(out.error.unwrap().contains("__gc"));
    let out = script::run_lua(&mut e, "local mt = {} local t = setmetatable({}, mt) mt.__gc = function() end return pcall(setmetatable, t, mt)");
    assert_eq!(out.result, json!(false));
    // Whatever else a script finds, the host abandons it: here a slow C library call
    // that no Lua hook can interrupt. The document is untouched and stays usable.
    let before = e.doc.clone();
    let t = std::time::Instant::now();
    let out = script::run_lua_limited(&mut e, "jm.set(1, 1, 7) return string.rep('a', 400):find('^a*a*a*a*b')", ms);
    assert!(out.error.unwrap().contains("abandoned"));
    assert!(t.elapsed().as_secs() < 4, "took {:?}", t.elapsed());
    assert!(e.doc == before && e.group == 0);
    assert_eq!(script::run_lua(&mut e, "jm.set(2, 2, 8) return jm.get(2, 2)").result, json!(8));
    assert_eq!(script::run_lua(&mut e, "return jm.get_info().undo_steps").result, json!(e.undo.len()), "a script sees the real history length");
    e.exec("undo", &json!({})).unwrap();
    // Coroutines still behave like coroutines.
    let out = script::run_lua(
        &mut e,
        "local function gen(n) return coroutine.wrap(function() for i = 1, n do coroutine.yield(i) end end) end
         local sum = 0 for v in gen(4) do sum = sum + v end
         local co = coroutine.create(function(a, b) local c = coroutine.yield(a + b) error('bad ' .. c) end)
         local ok1, v1 = coroutine.resume(co, 2, 3)
         local ok2, v2 = coroutine.resume(co, 'x')
         local wrapped = pcall(coroutine.wrap(function() error('inner') end))
         local _, main = coroutine.running()
         return {sum, ok1, v1, ok2, tostring(v2):match('bad x') ~= nil, coroutine.status(co), wrapped, main,
                 coroutine.close(coroutine.create(print)), select('#', coroutine.resume(coroutine.create(function() return 1, 2, 3 end)))}",
    );
    assert_eq!(out.error, None);
    assert_eq!(out.result, json!([10, true, 5, false, true, "dead", false, true, true, 4]));
    let out = script::run_lua(&mut e, "local ok, err = pcall(error, 'x') return {ok, err, pcall(function() return 1, 2 end)}");
    assert_eq!(out.result, json!([false, "x", true, 1, 2]), "pcall still behaves normally");
}

#[test]
fn a_failed_command_under_pcall_leaves_nothing_behind() {
    let mut e = eng(8, 8);
    let out = script::run_lua(
        &mut e,
        "jm.layer_add{name='Top'} jm.layer_set{layer=0, locked=true}
         local ok = pcall(jm.layer_merge_down, {layer=1}) return {ok=ok, layers=#jm.get_info().layers}",
    );
    assert_eq!(out.error, None);
    assert_eq!(out.result, json!({"ok": false, "layers": 2}));
    assert_eq!(e.doc.layers.len(), 2);
}

#[test]
fn an_empty_selection_selects_nothing() {
    let mut e = eng(32, 32);
    e.exec("select_rect", &json!({"x": 0, "y": 0, "w": 4, "h": 4})).unwrap();
    e.exec("select_rect", &json!({"x": 10, "y": 10, "w": 4, "h": 4, "op": "intersect"})).unwrap();
    e.exec("selection_fill", &json!({"color": 5})).unwrap();
    let r = e.exec("draw_rect", &json!({"x": 0, "y": 0, "w": 32, "h": 32, "color": 6, "filled": true})).unwrap();
    assert!(r["note"].as_str().unwrap().contains("selection is empty"), "the reply says why nothing changed");
    assert_eq!((pixel(&mut e, 20, 20), pixel(&mut e, 1, 1)), (-1, -1));
    assert_eq!(e.exec("get_info", &json!({})).unwrap()["selection"]["w"], 0);
    e.exec("select_all", &json!({})).unwrap();
    e.exec("select_invert", &json!({})).unwrap();
    e.exec("selection_fill", &json!({"color": 5})).unwrap();
    assert_eq!(pixel(&mut e, 20, 20), -1);
    e.exec("select_none", &json!({})).unwrap();
    assert_eq!(e.exec("get_info", &json!({})).unwrap()["selection"], Value::Null);
}

#[test]
fn coalesced_edits_share_one_undo_step_without_eating_history() {
    let mut e = eng(16, 16);
    for i in 0..100 {
        e.exec("set_pixels", &json!({"pixels": [[i % 16, i / 16, 5]]})).unwrap();
    }
    let (second, original) = (e.undo[1].clone(), e.doc.palette.colors[1]);
    for i in 0..60 {
        e.exec_coalesced("palette_set", &json!({"index": 1, "color": [i, 0, 0]}), "slider").unwrap();
    }
    assert_eq!(e.undo.len(), 100);
    assert!(e.undo[0] == second, "a whole drag costs one step of a full history, not sixty");
    e.exec("undo", &json!({})).unwrap();
    assert_eq!(e.doc.palette.colors[1], original, "the drag is one undo step");

    // A stale key must not merge a later drag with an unrelated edit made in between.
    let mut e = eng(16, 16);
    e.exec_coalesced("palette_set", &json!({"index": 1, "color": [10, 0, 0]}), "slider").unwrap();
    e.exec("undo", &json!({})).unwrap();
    e.exec("set_pixels", &json!({"pixels": [[3, 3, 7]]})).unwrap();
    e.exec_coalesced("palette_set", &json!({"index": 1, "color": [20, 0, 0]}), "slider").unwrap();
    e.exec("undo", &json!({})).unwrap();
    assert_eq!(pixel(&mut e, 3, 3), 7);
}

#[test]
fn brush_clipboard_selection_and_png_round_trip_details() {
    let mut e = eng(16, 16);
    e.exec("draw_rect", &json!({"x": 0, "y": 0, "w": 4, "h": 4, "color": 5, "filled": true})).unwrap();
    e.exec("select_rect", &json!({"x": 0, "y": 0, "w": 4, "h": 4})).unwrap();
    e.exec("copy", &json!({})).unwrap();
    e.exec("paste", &json!({"x": 8, "y": 8})).unwrap();
    assert_eq!(e.exec("get_info", &json!({})).unwrap()["selection"]["w"], 4, "paste keeps the selection");
    e.exec("select_rect", &json!({"x": 0, "y": 0, "w": 2, "h": 2})).unwrap();
    e.exec("grab_brush", &json!({})).unwrap();
    assert_eq!(e.clipboard.as_ref().unwrap().w, 4, "grab_brush leaves the clipboard alone");
    e.exec("select_none", &json!({})).unwrap();
    e.exec("palette_swap", &json!({"a": 5, "b": 6})).unwrap();
    e.exec("draw_line", &json!({"x0": 13, "y0": 2, "x1": 13, "y1": 2, "brush": "custom"})).unwrap();
    assert_eq!((pixel(&mut e, 0, 0), pixel(&mut e, 13, 2)), (6, 6), "the custom brush follows palette renumbering");
    e.exec("undo", &json!({"steps": 2})).unwrap();
    e.exec("draw_line", &json!({"x0": 13, "y0": 4, "x1": 13, "y1": 4, "brush": "custom"})).unwrap();
    assert_eq!((pixel(&mut e, 0, 0), pixel(&mut e, 13, 4)), (5, 5), "and follows it back on undo");

    let mut e = eng(8, 8);
    e.exec("palette_load_preset", &json!({"name": "pico8"})).unwrap();
    e.exec("draw_rect", &json!({"x": 0, "y": 0, "w": 2, "h": 2, "color": 3})).unwrap();
    let p = tmp("round.png");
    e.exec("export_png", &json!({"path": p})).unwrap();
    let info = e.exec("open_document", &json!({"path": p, "force": true})).unwrap();
    assert_eq!(info["palette_count"], 16, "export then open keeps the palette size");
    assert_eq!((pixel(&mut e, 0, 0), pixel(&mut e, 5, 5)), (3, -1));
    assert!(e.exec("export_png", &json!({"path": tmp("evil.sh")})).unwrap_err().contains(".png"), "exports only write their own file type");
    assert!(e.exec("save_document", &json!({"path": tmp("evil.txt")})).unwrap_err().contains(".jmn"));
}

#[test]
fn a_save_followed_by_a_rollback_leaves_the_document_marked_modified() {
    let mut e = eng(8, 8);
    let p = tmp("rollback.jmn");
    e.exec("save_document", &json!({"path": p})).unwrap();
    assert!(!e.modified);
    let out = script::run_lua(&mut e, "jm.set(0, 0, 9) jm.save_document{} error('boom')");
    assert!(out.error.is_some());
    assert_eq!(pixel(&mut e, 0, 0), -1, "the edit was rolled back");
    assert!(e.modified, "the file on disk now differs from the document");
    // An ordinary failure with no save leaves a clean document clean.
    e.exec("save_document", &json!({})).unwrap();
    assert!(script::run_lua(&mut e, "jm.set(0, 0, 9) error('boom')").error.is_some());
    assert!(!e.modified);
    assert!(e.exec("bogus", &json!({})).is_err() && !e.modified);
}

#[test]
fn a_script_does_not_run_under_an_open_stroke() {
    let mut e = eng(8, 8);
    e.stroke_begin(e.paint_opts(false, false)).unwrap();
    e.stroke_to(1, 1);
    let out = script::run_lua(&mut e, "jm.set(5, 5, 9)");
    assert!(out.error.unwrap().contains("busy"));
    e.stroke_end();
    assert_eq!(script::run_lua(&mut e, "jm.set(5, 5, 9)").error, None);
    assert_eq!(e.undo.len(), 2, "the stroke and the script are separate undo steps");
    e.exec("undo", &json!({})).unwrap();
    assert_eq!((pixel(&mut e, 5, 5), pixel(&mut e, 1, 1)), (-1, 15));
}

#[test]
fn mcp_token_check() {
    assert!(remote::authorized(Some("Bearer abc123"), "abc123"));
    assert!(!remote::authorized(Some("Bearer abc124"), "abc123"));
    assert!(!remote::authorized(Some("abc123"), "abc123"));
    assert!(!remote::authorized(None, "abc123"));
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

#[test]
fn right_clicking_a_tool_selects_it_and_opens_its_options() {
    let ctx = egui::Context::default();
    let (mut e, mut g) = (Engine::new("gui"), Gui::new());
    let mut time = 0.0;
    let mut frame = |e: &mut Engine, g: &mut Gui, events: Vec<Event>| {
        time += 1.0 / 60.0;
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 800.0))), time: Some(time), events, ..Default::default() };
        let mut menu_text = false;
        let out = ctx.run(raw, |ctx| {
            egui::SidePanel::left("tools").exact_width(46.0).resizable(false).show(ctx, |ui| crate::panels::toolbar(ui, e, g));
            egui::CentralPanel::default().show(ctx, |_| {});
        });
        // The menu's contents are painted as text shapes; look for the Filled checkbox.
        fn has(shapes: &[egui::epaint::ClippedShape], needle: &str) -> bool {
            fn walk(s: &egui::Shape, needle: &str) -> bool {
                match s {
                    egui::Shape::Text(t) => t.galley.text().contains(needle),
                    egui::Shape::Vec(v) => v.iter().any(|s| walk(s, needle)),
                    _ => false,
                }
            }
            shapes.iter().any(|c| walk(&c.shape, needle))
        }
        menu_text |= has(&out.shapes, "Filled");
        menu_text
    };
    frame(&mut e, &mut g, vec![]);
    // Tool buttons are 34x30 with 3 px gaps, stacked from the top of the panel; Rectangle is the fourth.
    let rect_tool = Tool::ALL.iter().position(|t| *t == Tool::Rect).unwrap() as f32;
    let p = pos2(23.0, 6.0 + rect_tool * 33.0 + 15.0);
    assert_eq!(e.o.tool, Tool::Pencil);
    frame(&mut e, &mut g, vec![Event::PointerMoved(p)]);
    frame(&mut e, &mut g, vec![Event::PointerButton { pos: p, button: PointerButton::Secondary, pressed: true, modifiers: Modifiers::NONE }]);
    frame(&mut e, &mut g, vec![Event::PointerButton { pos: p, button: PointerButton::Secondary, pressed: false, modifiers: Modifiers::NONE }]);
    assert_eq!(e.o.tool, Tool::Rect, "right-click selects the tool");
    let mut shown = false;
    for _ in 0..3 {
        shown |= frame(&mut e, &mut g, vec![]);
    }
    assert!(shown, "the options menu for the rectangle tool is showing");
}
