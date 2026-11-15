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
    let mut d = Doc::new(8, 8, Palette::preset("pico8").unwrap());
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
fn everything_is_indexed() {
    use crate::doc::*;
    use crate::io::{self, PngPixels};
    let is_indexed = |p: &str| matches!(io::load_png(std::path::Path::new(p)).unwrap().pixels, PngPixels::Indexed { .. });

    // A true-colour PNG opens as an indexed document with a palette made for it.
    let (w, h) = (20u32, 20u32);
    let rgba: Vec<u8> = (0..w * h).flat_map(|i| if i % 7 == 0 { [0, 0, 0, 0] } else { [(i % w % 10 * 25) as u8, (i / w * 12) as u8, 90, 255] }).collect();
    let src = tmp("truecolour.png");
    std::fs::write(&src, io::encode_png_rgba(w, h, &rgba).unwrap()).unwrap();
    let mut e = Engine::new("headless");
    e.exec("open_document", &json!({"path": src})).unwrap();
    assert_eq!(e.doc.mode, ColorMode::Indexed);
    assert!(e.doc.palette.colors.len() <= 256);
    let cells = &e.doc.layers[0].data;
    assert_eq!(cells[0], TRANSP, "transparent pixels stay transparent");
    assert!(cells.iter().all(|&c| c == TRANSP || (c as usize) < e.doc.palette.colors.len()));
    // The image has 200 colours, so every one fits in the palette exactly.
    assert_eq!(e.doc.to_rgba(cells[45]), [125, 24, 90, 255]);
    // ... and as a layer it maps onto the palette already there.
    e.exec("new_document", &json!({"width": 20, "height": 20, "palette": "pico8", "force": true})).unwrap();
    e.exec("import_png_layer", &json!({"path": src})).unwrap();
    assert!(e.doc.layers[1].data.iter().all(|&c| c == TRANSP || c < 16));

    // Exports are indexed PNGs, scaled or not, hidden layers or not.
    e.exec("layer_set", &json!({"layer": 0, "visible": false})).unwrap();
    for (name, params) in [("plain.png", json!({})), ("scaled.png", json!({"scale": 3})), ("layer.png", json!({"layer": 1, "scale": 2}))] {
        let mut p = params;
        p["path"] = json!(tmp(name));
        e.exec("export_png", &p).unwrap();
        assert!(is_indexed(&tmp(name)), "{name} is not an indexed PNG");
    }
    let scaled = io::load_png(std::path::Path::new(&tmp("scaled.png"))).unwrap();
    assert_eq!((scaled.w, scaled.h), (60, 60));
    let back = crate::commands::doc_from_png(scaled);
    assert_eq!(back.to_rgba(back.layers[0].data[4 * 60 + 5]), e.doc.to_rgba(e.doc.layers[1].data[20 + 1]), "scaled pixels keep their colour");

    // All 256 colours in use plus transparency cannot be an indexed PNG: say so rather than write RGBA.
    e.exec("new_document", &json!({"width": 32, "height": 16, "force": true})).unwrap();
    assert_eq!(script::run_lua(&mut e, "for i = 0, 255 do jm.set(i % 32, i // 32, i) end").error, None);
    let err = e.exec("export_png", &json!({"path": tmp("full.png")})).unwrap_err();
    assert!(err.contains("256 palette colours"), "{err}");
    e.exec("draw_rect", &json!({"x": 0, "y": 8, "w": 32, "h": 8, "color": 0, "filled": true})).unwrap();
    e.exec("export_png", &json!({"path": tmp("full.png")})).unwrap();
    assert!(is_indexed(&tmp("full.png")));

    // A true-colour project written by 0.1.0-0.1.2 opens as true colour with its pixels as
    // they were. Layer opacity (gone) goes into the pixels, so the picture looks the same.
    let mut d = Doc::new(4, 4, Palette::preset("pico8").unwrap());
    d.mode = ColorMode::Rgba;
    d.layers[0].opacity = 100;
    d.layers[0].data = std::sync::Arc::new((0..16u32).map(|i| if i == 0 { 0 } else { u32::from_le_bytes([200, (i * 10) as u8, 30, 255]) }).collect());
    let solid = d.new_layer("solid");
    d.layers.push(solid);
    d.layers[1].data = std::sync::Arc::new((0..16u32).map(|i| if i < 8 { 5 } else { u32::from_le_bytes([1, 2, 3, 77]) }).collect());
    let old = tmp("old_rgba.jmn");
    io::save_doc(&d, std::path::Path::new(&old)).unwrap();
    // 0.1.0-0.1.2 wrote true colour under the same file marker as indexed documents.
    let mut bytes = std::fs::read(&old).unwrap();
    assert_eq!(bytes[4], 2);
    bytes[4] = 1;
    std::fs::write(&old, bytes).unwrap();
    let v = e.exec("open_document", &json!({"path": old, "force": true})).unwrap();
    assert_eq!((v["mode"].as_str(), v["legacy_truecolor"].as_bool()), (Some("truecolor"), Some(true)));
    assert_eq!((e.doc.mode, e.doc.layers[0].opacity), (ColorMode::Rgba, 255));
    assert_eq!(e.doc.palette, Palette::preset("pico8").unwrap(), "the swatches are kept");
    assert_eq!(e.doc.layers[0].data[0], TRANSP);
    assert_eq!(unpack(e.doc.layers[0].data[5]), [200, 50, 30, 100], "alpha 255 at layer opacity 100");
    // A layer that was fully opaque is untouched, apart from alpha 0 becoming the one transparent value.
    assert_eq!((e.doc.layers[1].data[0], unpack(e.doc.layers[1].data[8])), (TRANSP, [1, 2, 3, 77]));
    // Saved from here it is an ordinary true-colour project, which opens the same and is no longer "legacy".
    let before = e.doc.clone();
    // Until it is saved, the window asks before Save rewrites the file (legacy_rewrite). Not
    // while it is indexed, since that is saved in the form the old version opens.
    assert!(e.legacy_rewrite());
    e.exec("convert_mode", &json!({"mode": "indexed"})).unwrap();
    assert!(!e.legacy_rewrite());
    e.undo().unwrap();
    assert!(e.legacy_rewrite() && e.doc.layers == before.layers);
    // A script's working copy knows too, and a failed command does not lose it.
    assert_eq!(script::run_lua(&mut e, "jm.set(0, 0, '#010203')").error, None);
    e.exec("save_document", &json!({"path": tmp("nope.txt")})).unwrap_err();
    assert!(e.legacy_rewrite());
    e.undo().unwrap();
    // Saved somewhere else the original is untouched, and the document is no longer the old file.
    let copy = tmp("old_rgba_copy.jmn");
    let v = e.exec("save_document", &json!({"path": copy})).unwrap();
    assert!(v.get("rewrote_legacy_truecolor").is_none() && !e.legacy_rewrite());
    assert_eq!((std::fs::read(&old).unwrap()[4], std::fs::read(&copy).unwrap()[4]), (1, 2));
    // Saved over itself, a caller is told what happened (it cannot be asked first).
    e.exec("open_document", &json!({"path": old, "force": true})).unwrap();
    assert!(e.legacy_rewrite());
    let v = e.exec("save_document", &json!({})).unwrap();
    assert_eq!(v["rewrote_legacy_truecolor"], true);
    assert!(!e.legacy_rewrite());
    assert_eq!(std::fs::read(&old).unwrap()[4], 2);
    assert!(e.exec("save_document", &json!({})).unwrap().get("rewrote_legacy_truecolor").is_none());
    let v = e.exec("open_document", &json!({"path": old, "force": true})).unwrap();
    assert!(v.get("legacy_truecolor").is_none() && e.doc.layers == before.layers);
    // An old indexed project is not affected: no flag, and its layer opacity is simply dropped.
    let mut d = Doc::new(4, 4, Palette::preset("pico8").unwrap());
    d.layers[0].opacity = 100;
    d.layers[0].data = std::sync::Arc::new(vec![3; 16]);
    let old = tmp("old_indexed.jmn");
    io::save_doc(&d, std::path::Path::new(&old)).unwrap();
    let v = e.exec("open_document", &json!({"path": old, "force": true})).unwrap();
    assert!(v.get("legacy_truecolor").is_none());
    assert_eq!((e.doc.mode, e.doc.layers[0].opacity, e.doc.layers[0].data[0]), (ColorMode::Indexed, 255, 3));
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

#[test]
fn a_converted_image_with_transparency_can_always_be_exported() {
    // Far more than 256 colours, and a transparent corner.
    let (w, h) = (96u32, 96u32);
    let mut rgba = vec![];
    for y in 0..h {
        for x in 0..w {
            rgba.extend_from_slice(&if x < 8 && y < 8 { [0, 0, 0, 0] } else { [(x * 5 / 2) as u8, (y * 5 / 2) as u8, ((x * 7 + y * 13) % 256) as u8, 255] });
        }
    }
    let (src, out, opaque) = (tmp("many_transparent.png"), tmp("many_transparent_out.png"), tmp("many_opaque.png"));
    std::fs::write(&src, crate::io::encode_png_rgba(w, h, &rgba).unwrap()).unwrap();
    let mut e = Engine::new("headless");
    e.exec("open_document", &json!({"path": src})).unwrap();
    assert_eq!(e.doc.palette.colors.len(), 255, "one entry is left for transparency");
    e.exec("export_png", &json!({"path": out})).unwrap();
    e.exec("open_document", &json!({"path": out, "force": true})).unwrap();
    assert_eq!((pixel(&mut e, 0, 0), e.doc.palette.colors.len()), (-1, 255), "the transparent corner survives the round trip");
    assert!(pixel(&mut e, 50, 50) >= 0);
    // Asking for 256 generated colours on such an image stops at 255 too.
    e.exec("reduce_colors", &json!({"colors": 256})).unwrap();
    assert!(e.doc.palette.colors.len() <= 255);
    e.exec("export_png", &json!({"path": out})).unwrap();
    // An image with no transparent pixel still gets all 256.
    rgba.chunks_exact_mut(4).for_each(|p| p[3] = 255);
    std::fs::write(&opaque, crate::io::encode_png_rgba(w, h, &rgba).unwrap()).unwrap();
    e.exec("open_document", &json!({"path": opaque, "force": true})).unwrap();
    assert_eq!(e.doc.palette.colors.len(), 256);
    e.exec("export_png", &json!({"path": out})).unwrap();
}

#[test]
fn many_colour_images_convert_quickly_and_closely() {
    // Noise has far more colours than the exact paths handle; it takes the pooled route.
    let n = 512usize;
    let mut s = 12345u32;
    let px: Vec<[u8; 4]> = (0..n * n)
        .map(|_| {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            let b = s.to_le_bytes();
            [b[1], b[2], b[3], 255]
        })
        .collect();
    let pal = crate::quant::palette_for(px.iter(), 256);
    assert_eq!(pal.colors.len(), 256);
    let cells = crate::quant::to_indexed(&px, n, n, &pal, crate::quant::Dither::None);
    // Every pixel lands on a palette colour about as close as the true nearest one. Pixels that
    // share a bucket differ by at most 3 per channel, which is 30 in `dist`'s weighted units,
    // so the answer can be at most twice that further away than the best one.
    let mut worse = 0;
    for (p, &c) in px.iter().zip(&cells) {
        let rgb = [p[0], p[1], p[2]];
        let (got, best) = (crate::doc::dist(pal.colors[c as usize], rgb), crate::doc::dist(pal.colors[pal.nearest(rgb) as usize], rgb));
        assert!((got as f64).sqrt() <= (best as f64).sqrt() + 60.0, "pixel {rgb:?} mapped far from its nearest colour");
        worse += (got > best) as usize;
    }
    assert!(worse < px.len() / 4, "{worse} of {} pixels missed their nearest colour", px.len());
}

#[test]
fn deluxe_paint_keys() {
    use crate::engine::Keymap;
    use crate::paint::PaintMode;
    let ctx = egui::Context::default();
    let (mut e, mut g) = (Engine::new("gui"), Gui::new());
    let mut time = 0.0;
    let mut frame = |e: &mut Engine, g: &mut Gui, events: Vec<Event>| {
        time += 1.0 / 60.0;
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 800.0))), time: Some(time), events, ..Default::default() };
        let _ = ctx.run(raw, |ctx| crate::app::shortcuts(ctx, e, g));
    };
    let text = |s: &str| vec![Event::Text(s.to_string())];
    let key = |key: egui::Key| vec![Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }];

    // The standard preset is untouched: R is the rectangle, and DPaint's characters do nothing.
    frame(&mut e, &mut g, key(egui::Key::R));
    assert_eq!(e.o.tool, Tool::Rect);
    frame(&mut e, &mut g, text("v"));
    assert_eq!(e.o.tool, Tool::Rect, "typed characters are not standard shortcuts");

    // Switching is a view setting, so scripts and AI clients can do it too.
    assert!(e.exec("set_view", &json!({"keymap": "amiga"})).is_err());
    e.exec("set_view", &json!({"keymap": "dpaint"})).unwrap();
    assert_eq!(e.exec("get_view", &json!({})).unwrap()["keymap"], "dpaint");
    assert_eq!(e.view.keymap, Keymap::DPaint);

    // Upper and lower case differ: r is an outlined rectangle, R a filled one.
    frame(&mut e, &mut g, text("R"));
    assert_eq!((e.o.tool, e.o.filled), (Tool::Rect, true));
    frame(&mut e, &mut g, text("r"));
    assert_eq!((e.o.tool, e.o.filled), (Tool::Rect, false));
    frame(&mut e, &mut g, text("v"));
    assert_eq!(e.o.tool, Tool::Line);
    frame(&mut e, &mut g, text("d"));
    assert_eq!(e.o.tool, Tool::Pencil);
    // A standard-only key does nothing: L would have been the line tool.
    frame(&mut e, &mut g, key(egui::Key::L));
    assert_eq!(e.o.tool, Tool::Pencil);

    // [ ] step the foreground colour (they are brush size in the standard preset), { } the background.
    let (fg, bg, size) = (e.o.fg, e.o.bg, e.o.brush_size);
    frame(&mut e, &mut g, text("["));
    assert_eq!((e.o.fg, e.o.brush_size), (fg - 1, size));
    frame(&mut e, &mut g, text("]"));
    assert_eq!(e.o.fg, fg);
    e.o.fg = e.doc.palette.colors.len() as u32 - 1;
    frame(&mut e, &mut g, text("]"));
    assert_eq!(e.o.fg, 0, "stepping past the last colour wraps");
    e.o.fg = fg;
    frame(&mut e, &mut g, text("}"));
    assert_eq!(e.o.bg, bg + 1);
    frame(&mut e, &mut g, text("="));
    assert_eq!(e.o.brush_size, size + 1);
    frame(&mut e, &mut g, text("."));
    assert_eq!(e.o.brush_size, 1);

    // Function keys pick paint modes; u undoes; K clears the layer.
    frame(&mut e, &mut g, key(egui::Key::F8));
    assert_eq!(e.o.mode, PaintMode::Smooth);
    frame(&mut e, &mut g, key(egui::Key::F1));
    assert_eq!(e.o.mode, PaintMode::Normal);
    e.exec("draw_rect", &json!({"x": 0, "y": 0, "w": 4, "h": 4, "color": 9, "filled": true})).unwrap();
    frame(&mut e, &mut g, text("K"));
    assert_eq!(e.doc.layers[0].data[0], crate::doc::TRANSP, "K clears");
    frame(&mut e, &mut g, text("u"));
    assert_eq!(e.doc.layers[0].data[0], 9, "u undoes");
    frame(&mut e, &mut g, text("g"));
    assert!(e.view.grid);
    // Keys DPaint has but this editor does not are left alone.
    frame(&mut e, &mut g, text("a"));
    frame(&mut e, &mut g, text("q"));
    assert_eq!(e.o.tool, Tool::Pencil);
    // Every key in the table is distinct, and each tool's tooltip key is the one that selects it.
    let mut seen = std::collections::HashSet::new();
    assert!(crate::keys::DPAINT.iter().all(|b| seen.insert(b.key)), "duplicate key in the preset");
    assert_eq!(crate::keys::tool_key(Keymap::DPaint, Tool::Rect).as_deref(), Some("r"));
    assert_eq!(crate::keys::tool_key(Keymap::DPaint, Tool::Eraser), None);
    assert_eq!(crate::keys::tool_key(Keymap::Standard, Tool::Eraser).as_deref(), Some("E"));

    // Help > Keyboard Shortcuts lists the active preset's keys.
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
    g.dialog = crate::app::Dialog::Shortcuts;
    for (map, shown, hidden) in [(Keymap::DPaint, "Filled rectangle", "Swap colours"), (Keymap::Standard, "Swap colours", "Filled rectangle")] {
        e.view.keymap = map;
        let mut out = None;
        for _ in 0..3 {
            let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 800.0))), ..Default::default() };
            out = Some(ctx.run(raw, |ctx| crate::app::dialogs(ctx, &mut e, &mut g)));
        }
        let shapes = out.unwrap().shapes;
        assert!(has(&shapes, shown), "{map:?}: '{shown}' is listed");
        assert!(!has(&shapes, hidden), "{map:?}: '{hidden}' is not listed");
    }
}

// ---------------------------------------------------------------- sprite atlas viewer

#[test]
fn a_sprite_atlas_is_read_exactly_and_only_read() {
    use crate::atlas::{self, AFrame};
    use std::path::{Path, PathBuf};
    let dir = PathBuf::from(tmp("atlas"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Two pages: a WebP with soft-edged, many-coloured pixels and a PNG.
    let page = |w: u32, h: u32, seed: u32| -> Vec<u8> { (0..w * h).flat_map(|i| [(i * 7 + seed) as u8, (i * 13) as u8, (i / w * 9) as u8, if i % 5 == 0 { (i * 3) as u8 } else { 255 }]).collect() };
    let (p0, p1) = (page(64, 32, 1), page(16, 16, 9));
    let mut webp = vec![];
    image_webp::WebPEncoder::new(&mut webp).encode(&p0, 64, 32, image_webp::ColorType::Rgba8).unwrap();
    std::fs::write(dir.join("atlas_0.webp"), &webp).unwrap();
    std::fs::write(dir.join("atlas_1.webp"), {
        let mut v = vec![];
        image_webp::WebPEncoder::new(&mut v).encode(&p1, 16, 16, image_webp::ColorType::Rgba8).unwrap();
        v
    })
    .unwrap();
    let desc = json!({"scale": 1, "prefix": "atlas_", "pages": 2, "ext": ".webp", "objects": [
        {"name": "ball", "flags": "", "mcName": "ball",
         "labels": [["roll", 0], ["roll_end", 1], ["pop", 3], ["junk", 99], [7, 0], ["bad\u{202e}name", 2]],
         "frames": [[0, 0, 0, 16, 16, 16.4, 16.0, -8, -8], [0, 16, 0, 20, 12, 20.0, 12.5, -10.0, -4], [-1, 0, 0, 0, 0, 0, 0, 3, 3], [1, 0, 0, 16, 16, 16, 16, -8, -12]]},
        {"name": "post\u{7}", "labels": [], "frames": [[0, 40, 8, 8, 24, 8, 24, 1e9, -1e9]]},
        {"labels": [], "frames": []},
    ]});
    let file = dir.join("atlas.json");
    std::fs::write(&file, desc.to_string()).unwrap();
    let listing = |d: &Path| {
        let mut v: Vec<(String, Vec<u8>)> = std::fs::read_dir(d).unwrap().map(|e| e.unwrap()).map(|e| (e.file_name().to_string_lossy().to_string(), std::fs::read(e.path()).unwrap_or_default())).collect();
        v.sort();
        v
    };
    let before = listing(&dir);

    let a = atlas::load(&file).unwrap();
    assert_eq!((a.pages.len(), a.pages[0].w, a.pages[0].h, a.pages[1].w, a.objects.len()), (2, 64, 32, 16, 3));
    assert!(a.pages[0].rgba == p0 && a.pages[1].rgba == p1, "every pixel, partly transparent ones included, is exactly the file's");
    let ball = &a.objects[0];
    assert_eq!(ball.frames[1], Some(AFrame { page: 0, x: 16, y: 0, w: 20, h: 12, ox: -10, oy: -4 }));
    assert_eq!((ball.frames[2], ball.frames[3].map(|f| f.page)), (None, Some(1)), "an empty frame, and a frame on another page");
    assert_eq!(ball.bounds, (-10, -12, 20, 20), "the box holds every frame at its offset");
    let seqs: Vec<(&str, u32, u32)> = ball.seqs.iter().map(|s| (s.name.as_str(), s.start, s.end)).collect();
    assert_eq!(seqs, vec![("roll", 0, 1), ("badname", 2, 2), ("pop", 3, 3)], "labels pair up with their _end; nonsense ones are left out");
    assert_eq!((a.objects[1].name.as_str(), a.objects[1].frames[0].map(|f| (f.ox, f.oy)), a.objects[2].name.as_str()), ("post", Some((32768, -32768)), "object 2"));
    assert_eq!(a.objects[2].bounds, (0, 0, 1, 1));

    // The viewer steps inside what is playing and wraps round.
    let mut v = atlas::View::new(a);
    assert_eq!((v.object, v.range()), (0, (0, 3)));
    v.step(-1);
    assert_eq!(v.frame, 3);
    v.play_seq(Some(0));
    assert_eq!((v.frame, v.range()), (0, (0, 1)));
    v.step(3);
    assert_eq!(v.frame, 1);
    v.select(1);
    assert_eq!((v.object, v.seq, v.frame, v.range()), (1, None, 0, (0, 0)));
    v.step(5);
    v.select(2);
    v.step(1);
    v.select(99);
    assert_eq!((v.object, v.frame), (2, 0));

    // Nothing was written, and no command or tool knows the viewer exists.
    assert_eq!(listing(&dir), before);
    assert!(crate::commands::registry().iter().all(|c| !c.name.contains("atlas") && !c.desc.to_lowercase().contains("atlas")));
    assert!(!crate::remote::tools().to_string().to_lowercase().contains("atlas"));

    // The description can say anything. The page names come from it, so they may only ever
    // be plain names in the atlas's own folder.
    let head = |prefix: Value, ext: Value, pages: Value| atlas::header(&json!({"prefix": prefix, "ext": ext, "pages": pages}));
    assert_eq!(head(json!("atlas_"), json!(".WEBP"), json!(3)).unwrap(), ("atlas_".to_string(), ".webp".to_string(), 3));
    for (prefix, ext, pages) in [
        (json!("../atlas_"), json!(".webp"), json!(1)), (json!("/etc/"), json!(".webp"), json!(1)), (json!("a/b"), json!(".png"), json!(1)), (json!("a\\b"), json!(".png"), json!(1)),
        (json!(".."), json!(".png"), json!(1)), (json!(""), json!(".png"), json!(1)), (json!("x".repeat(65)), json!(".png"), json!(1)), (json!(7), json!(".png"), json!(1)),
        (json!("a"), json!(".exe"), json!(1)), (json!("a"), json!("/../x.png"), json!(1)), (json!("a"), json!(""), json!(1)),
        (json!("a"), json!(".png"), json!(0)), (json!("a"), json!(".png"), json!(17)), (json!("a"), json!(".png"), json!(1.5)), (json!("a"), json!(".png"), json!("3")), (json!("a"), json!(".png"), json!(-1)),
    ] {
        assert!(head(prefix.clone(), ext.clone(), pages.clone()).is_err(), "{prefix} {ext} {pages}");
    }
    let sizes = [(64u32, 32u32), (16, 16)];
    let objs = |frames: Value| atlas::parse(&json!({"objects": [{"name": "o", "labels": [["a", 0]], "frames": frames}]}), &sizes);
    assert!(objs(json!([[0, 0, 0, 64, 32]])).is_ok() && objs(json!([[1, 15, 15, 1, 1, 0, 0]])).is_ok() && objs(json!([[0, 5, 5, 0, 0]])).unwrap()[0].frames[0].is_none());
    for frames in [
        json!([[2, 0, 0, 1, 1]]), json!([[-2, 0, 0, 1, 1]]), json!([[0, 60, 0, 8, 8]]), json!([[0, 0, 30, 8, 8]]), json!([[1, 0, 0, 17, 1]]), json!([[0, -1, 0, 8, 8]]),
        json!([[0, 0.5, 0, 8, 8]]), json!([[0, 0, 0, 1e300, 8]]), json!([[0, 4294967295u32, 0, 4294967295u32, 1]]), json!([[0, 0, 0, 8]]), json!([["0", 0, 0, 8, 8]]), json!([{"x": 0}]),
        json!([7]), json!("frames"), json!(null), json!((0..atlas::FRAMES_MAX + 1).map(|_| json!([-1, 0, 0, 0, 0])).collect::<Vec<_>>()),
    ] {
        assert!(objs(frames.clone()).is_err(), "{:.60}", frames.to_string());
    }
    assert!(atlas::parse(&json!({"objects": (0..atlas::OBJECTS_MAX + 1).map(|_| json!({"frames": []})).collect::<Vec<_>>()}), &sizes).is_err());
    assert!(atlas::parse(&json!({"objects": {"a": 1}}), &sizes).is_err() && atlas::parse(&json!([1, 2]), &sizes).is_err());
    let odd = atlas::parse(&json!({"objects": [{"name": 5, "labels": "x", "frames": [[0, 0, 0, 4, 4, "w", null, "x", {}]]}, {"name": "l", "labels": [["a_end", 0], ["b", 1], ["b_end", 0], [], [[], 0]], "frames": [[-1, 0, 0, 0, 0], [-1, 0, 0, 0, 0]]}]}), &sizes).unwrap();
    assert_eq!((odd[0].name.as_str(), odd[0].frames[0].map(|f| (f.ox, f.oy)), odd[0].seqs.len()), ("object 0", Some((0, 0)), 0));
    assert!(odd[1].seqs.iter().all(|s| s.start <= s.end && (s.end as usize) < 2), "{:?}", odd[1].seqs);

    // Files: a page that is missing, not an image, cut short, a link out of the folder, or too big.
    let fails = |why: &str| {
        let err = atlas::load(&file).err().unwrap_or_default();
        assert!(err.contains(why), "expected '{why}', got '{err}'");
    };
    std::fs::write(dir.join("atlas_1.webp"), &webp[..webp.len() / 2]).unwrap();
    fails("WebP");
    std::fs::write(dir.join("atlas_1.webp"), b"RIFF\xff\xff\xff\x7fWEBPVP8 not really").unwrap();
    fails("WebP");
    std::fs::write(dir.join("atlas_1.webp"), b"").unwrap();
    fails("WebP");
    std::fs::remove_file(dir.join("atlas_1.webp")).unwrap();
    fails("atlas_1.webp");
    #[cfg(unix)]
    {
        let outside = tmp("outside_the_atlas.webp");
        std::fs::write(&outside, &webp).unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("atlas_1.webp")).unwrap();
        fails("not a plain file");
        std::fs::remove_file(dir.join("atlas_1.webp")).unwrap();
    }
    std::fs::create_dir_all(dir.join("atlas_1.webp")).unwrap();
    fails("not a plain file");
    std::fs::remove_dir_all(dir.join("atlas_1.webp")).unwrap();
    std::fs::write(dir.join("atlas_1.webp"), &webp).unwrap();
    assert!(atlas::load(&file).is_ok(), "a page may be larger than the frames need");
    // The pixel budget is checked from the header, before a page is decoded.
    let mut budget = 100;
    assert!(atlas::decode_page(&webp, true, &mut budget).err().unwrap_or_default().contains("too large") && budget == 100);
    let mut budget = 64 * 32;
    assert!(atlas::decode_page(&webp, true, &mut budget).is_ok() && budget == 0);
    // PNG pages work too, and the description itself has a size limit and must be a .json.
    std::fs::write(dir.join("p0.png"), crate::io::encode_png_rgba(64, 32, &p0).unwrap()).unwrap();
    std::fs::write(&file, json!({"prefix": "p", "pages": 1, "ext": ".png", "objects": [{"name": "o", "labels": [], "frames": [[0, 0, 0, 64, 32]]}]}).to_string()).unwrap();
    assert!(atlas::load(&file).unwrap().pages[0].rgba == p0);
    std::fs::write(&file, vec![b' '; (atlas::JSON_MAX + 1) as usize]).unwrap();
    fails("larger than");
    std::fs::write(&file, "{ nope").unwrap();
    fails("not valid JSON");
    // A byte-order mark in front of the text is fine; a .json that is something else says what it is.
    let png_atlas = json!({"prefix": "p", "pages": 1, "ext": ".png", "objects": [{"name": "o", "labels": [["x_end", 0], ["go", 0]], "frames": [[0, 0, 0, 64, 32]]}]}).to_string();
    std::fs::write(&file, [b"\xef\xbb\xbf".as_slice(), png_atlas.as_bytes()].concat()).unwrap();
    let seqs: Vec<String> = atlas::load(&file).unwrap().objects[0].seqs.iter().map(|s| s.name.clone()).collect();
    assert_eq!(seqs, ["go"], "an _end with nothing to close is left out");
    for (text, why) in [("{}", "not a sprite atlas"), ("[]", "not a sprite atlas"), ("null", "not a sprite atlas"), (r#"{"format": "jon-motion-animations", "animations": []}"#, "Animation tab")] {
        std::fs::write(&file, text).unwrap();
        fails(why);
    }
    // A PNG page's size is read from its header and refused before the pixels are decoded.
    std::fs::write(&file, &png_atlas).unwrap();
    let mut big = Vec::new();
    png::Encoder::new(&mut big, 5000, 4).write_header().unwrap().write_image_data(&vec![0u8; 5000 * 4]).unwrap();
    std::fs::write(dir.join("p0.png"), &big).unwrap();
    fails("PNG pages can be up to 4096x4096");
    std::fs::write(dir.join("p0.png"), b"\x89PNG\r\n\x1a\ngarbage that is not a png").unwrap();
    fails("not a readable PNG image");
    assert!(atlas::load(&dir.join("p0.png")).is_err() && atlas::load(&dir).is_err() && atlas::load(&dir.join("missing.json")).is_err());
    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------- tabs

#[test]
fn several_documents_open_as_tabs() {
    use crate::app::{answer, request, Pending};
    let mut e = eng(32, 32);
    let tabs = |e: &Engine| e.tab_list().iter().map(|t| format!("{}{}{}", t["id"], if t["active"] == true { "!" } else { "" }, if t["modified"] == true { "*" } else { "" })).collect::<Vec<_>>().join(" ");
    assert_eq!((tabs(&e), e.exec("get_info", &json!({})).unwrap()["open_documents"].as_i64()), ("1!".to_string(), Some(1)));

    // Each document keeps its own pixels, undo history, file, selection, view, colours and animations.
    e.exec("draw_rect", &json!({"x": 0, "y": 0, "w": 4, "h": 4, "color": 9, "filled": true})).unwrap();
    e.exec("select_rect", &json!({"x": 0, "y": 0, "w": 8, "h": 8})).unwrap();
    e.exec("set_colors", &json!({"fg": 9})).unwrap();
    e.exec("set_options", &json!({"tool": "line", "brush_size": 5})).unwrap();
    e.exec("set_view", &json!({"zoom": 8, "grid": true})).unwrap();
    e.exec("anim_set", &json!({"name": "a", "frame_w": 8, "frame_h": 8})).unwrap();
    e.exec("copy", &json!({})).unwrap();
    let (undo_a, rev_a) = (e.undo.len(), e.rev);
    let v = e.exec("tab_new", &json!({"width": 64, "height": 16, "palette": "gameboy"})).unwrap();
    assert_eq!((v["id"].as_i64(), tabs(&e), e.doc.w, e.undo.len(), e.modified, e.doc.selection.is_some(), e.doc.anims.len()), (Some(2), "1* 2!".to_string(), 64, 0, false, false, 0));
    assert!(e.rev > rev_a, "the window sees a different document");
    // The tool in hand comes along; the colours are the new document's own.
    let o = e.exec("get_options", &json!({})).unwrap();
    assert_eq!((o["tool"].as_str(), o["brush_size"].as_i64(), e.o.fg), (Some("line"), Some(5), 15.min(e.doc.palette.colors.len() as u32 - 1)));
    assert!(e.view.zoom != 8.0 || e.view.fit, "its own view");
    // The clipboard comes along too, and a paste keeps its look in the other palette.
    e.exec("paste", &json!({"x": 0, "y": 0})).unwrap();
    assert!(pixel(&mut e, 1, 1) >= 0 && e.modified);
    e.exec("undo", &json!({})).unwrap();
    e.exec("draw_rect", &json!({"x": 0, "y": 0, "w": 2, "h": 2, "color": 1, "filled": true})).unwrap();
    e.exec("tab_select", &json!({"id": 1})).unwrap();
    assert_eq!((tabs(&e), e.doc.w, e.undo.len(), pixel(&mut e, 1, 1), e.o.fg, e.view.zoom, e.doc.anims.len(), e.doc.sel_bounds()), ("1!* 2*".to_string(), 32, undo_a, 9, 9, 8.0, 1, Some((0, 0, 7, 7))));
    // Undo in one tab does not touch the other.
    e.exec("undo", &json!({})).unwrap();
    e.exec("tab_select", &json!({"id": 2})).unwrap();
    assert_eq!((pixel(&mut e, 1, 1), e.undo.len()), (1, 1));
    assert!(e.exec("tab_select", &json!({"id": 99})).is_err());
    assert_eq!(e.exec("get_info", &json!({})).unwrap()["document"], json!({"id": 2, "name": "Untitled"}));

    // Files: a new tab per file; the same project twice shows the tab it is in.
    let (pa, pb) = (tmp("tab_a.jmn"), tmp("tab_b.jmn"));
    e.exec("save_document", &json!({"path": pb})).unwrap();
    e.exec("tab_select", &json!({"id": 1})).unwrap();
    e.exec("save_document", &json!({"path": pa})).unwrap();
    assert_eq!(tabs(&e), "1! 2");
    let v = e.exec("tab_open", &json!({"path": pb})).unwrap();
    assert_eq!((v["id"].as_i64(), v["already_open"].as_bool(), tabs(&e)), (Some(2), Some(true), "1 2!".to_string()));
    e.exec("tab_close", &json!({})).unwrap();
    assert_eq!(tabs(&e), "1!");
    let v = e.exec("tab_open", &json!({"path": pb})).unwrap();
    assert_eq!((v["id"].as_i64(), tabs(&e), e.doc.w, e.title()), (Some(3), "1 3!".to_string(), 64, "tab_b.jmn".to_string()));
    assert!(e.exec("tab_open", &json!({"path": tmp("missing.jmn")})).is_err() && tabs(&e) == "1 3!");
    // open_document and new_document still replace the active document, with the same rule as before.
    e.exec("draw_rect", &json!({"x": 5, "y": 5, "w": 1, "h": 1, "color": 2, "filled": true})).unwrap();
    assert!(e.exec("open_document", &json!({"path": pa})).is_err() && e.exec("new_document", &json!({"width": 4, "height": 4})).is_err());
    // A document with unsaved changes is not closed unless the caller says so; the last one leaves a blank.
    assert!(e.exec("tab_close", &json!({})).unwrap_err().contains("unsaved"));
    assert!(e.exec("tab_close", &json!({"id": 7})).is_err());
    e.exec("tab_close", &json!({"id": 1})).unwrap();
    assert_eq!(tabs(&e), "3!*");
    e.exec("tab_close", &json!({"force": true})).unwrap();
    assert!(tabs(&e) == "4!" && e.pristine() && e.parked.is_empty());
    // A blank document nobody has touched can be reused instead of piling up tabs.
    assert_eq!(e.exec("tab_open", &json!({"path": pa, "reuse_blank": true})).unwrap()["reused"], true);
    assert_eq!((tabs(&e), e.doc.w), ("4!".to_string(), 32));
    assert_eq!(e.exec("tab_new", &json!({"reuse_blank": true})).unwrap()["id"], 5, "an opened file is not blank");
    // There is a limit.
    while e.order.len() < MAX_TABS {
        e.exec("tab_new", &json!({"width": 2, "height": 2})).unwrap();
    }
    assert!(e.exec("tab_new", &json!({})).unwrap_err().contains("limit") && e.exec("tab_open", &json!({"path": pb})).is_err());
    assert_eq!((e.order.len(), e.parked.len()), (MAX_TABS, MAX_TABS - 1));
    for id in e.order.clone().into_iter().filter(|&id| id > 5) {
        e.exec("tab_close", &json!({"id": id})).unwrap();
    }
    assert_eq!(tabs(&e), "4 5!");

    // A script or batch works on one document from start to finish: it cannot change tabs, and
    // what it did lands on the document it started on.
    let out = script::run_lua(&mut e, "local n = #jm.tab_list().tabs jm.draw_rect{ x = 0, y = 0, w = 1, h = 1, color = 3, filled = true } return n");
    assert_eq!((out.error, out.result, e.id, pixel(&mut e, 0, 0)), (None, json!(2), 5, 3));
    for code in ["jm.tab_select{ id = 4 }", "jm.tab_new{}", "jm.tab_close{ id = 4 }", "jm.tab_open{ path = 'x.jmn' }"] {
        let out = script::run_lua(&mut e, code);
        assert!(out.error.unwrap().contains("one document") && tabs(&e) == "4 5!*", "{code}");
    }
    let r = crate::remote::call_tool(&mut e, "batch", &json!({"commands": [{"command": "tab_select", "params": {"id": 4}}]}));
    assert!(r.is_error && e.id == 5);
    // Nor under an open brush stroke.
    e.stroke_begin(e.paint_opts(false, false)).unwrap();
    assert!(e.exec("tab_select", &json!({"id": 4})).is_err() && e.exec("tab_new", &json!({})).is_err() && e.id == 5);
    e.stroke_end();

    // An AI client can name the document it is working on; the user's tab does not change.
    let text = |o: &crate::remote::ToolOut| o.content.iter().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n");
    let r = crate::remote::call_tool(&mut e, "execute_command", &json!({"command": "draw_rect", "params": {"x": 5, "y": 5, "w": 1, "h": 1, "color": 5, "filled": true}, "document": 4}));
    assert_eq!((r.is_error, e.id, e.tab(4).unwrap().modified, pixel(&mut e, 5, 5) != 5), (false, 5, true, true), "{}", text(&r));
    let r = crate::remote::call_tool(&mut e, "get_info", &json!({"document": 4}));
    assert!(text(&r).contains("\"id\":4") && !text(&r).contains("worked on document"));
    let r = crate::remote::call_tool(&mut e, "run_lua", &json!({"code": "return jm.get_pixel{ x = 5, y = 5 }.color", "document": 4}));
    assert!(text(&r).contains("result: 5") && e.id == 5, "{}", text(&r));
    assert!(!crate::remote::call_tool(&mut e, "screenshot", &json!({"document": 4})).is_error && e.id == 5);
    // Without one, the call works on the active tab and says which that was.
    let r = crate::remote::call_tool(&mut e, "execute_command", &json!({"command": "get_pixel", "params": {"x": 0, "y": 0}}));
    assert!(text(&r).contains("worked on document 5") && text(&r).contains("\"document\": 5"), "{}", text(&r));
    for bad in [json!({"document": 77}), json!({"document": "4"}), json!({"document": -1}), json!({"document": 1.5})] {
        assert!(crate::remote::call_tool(&mut e, "get_info", &bad).is_error && e.id == 5, "{bad}");
    }
    assert!(crate::remote::call_tool(&mut e, "execute_command", &json!({"command": "tab_close", "params": {"id": 5}, "document": 4})).is_error);
    let r = crate::remote::call_tool(&mut e, "execute_command", &json!({"command": "tab_list"}));
    assert!(!r.is_error && !text(&r).contains("worked on document"));
    // With one document open nothing is added to the answers.
    let mut solo = eng(8, 8);
    assert_eq!(crate::remote::call_tool(&mut solo, "execute_command", &json!({"command": "get_pixel", "params": {"x": 0, "y": 0}})).content.len(), 1);

    // ---- the window: closing a tab or quitting asks about every document with unsaved changes
    let mut g = Gui::new();
    e.exec("save_document", &json!({"path": tmp("tab_five.jmn")})).unwrap();
    assert_eq!(tabs(&e), "4* 5!");
    request(&mut e, &mut g, Pending::CloseTab(4));
    assert!(matches!(g.pending, Some(Pending::CloseTab(4))) && tabs(&e) == "4!* 5", "the document in question is shown, and nothing is closed yet");
    answer(&mut e, &mut g, 2);
    assert!(g.pending.is_none() && tabs(&e) == "4!* 5", "cancel");
    request(&mut e, &mut g, Pending::CloseTab(5));
    assert!(g.pending.is_none() && tabs(&e) == "4!*", "a saved document just closes");
    request(&mut e, &mut g, Pending::CloseTab(4));
    answer(&mut e, &mut g, 0);
    assert!(tabs(&e).ends_with('!') && e.pristine() && !crate::io::load_doc(std::path::Path::new(&pa)).unwrap().layers.is_empty(), "save, then close; the last tab leaves a blank");
    assert_eq!(pixel(&mut e, 0, 0), -1);
    let reopened = { let mut t = Engine::new("headless"); t.exec("open_document", &json!({"path": pa})).unwrap(); pixel(&mut t, 5, 5) };
    assert_eq!(reopened, 5, "what the other tab held was written by Save");

    // Quit: three documents, two with unsaved changes, the active one clean.
    let mut e = Engine::new("headless");
    for (name, dirty) in [("q1.jmn", true), ("q2.jmn", false), ("q3.jmn", true)] {
        e.exec("tab_new", &json!({"width": 8, "height": 8, "reuse_blank": true})).unwrap();
        e.exec("save_document", &json!({"path": tmp(name)})).unwrap();
        if dirty {
            e.exec("draw_rect", &json!({"x": 0, "y": 0, "w": 1, "h": 1, "color": 4, "filled": true})).unwrap();
        }
    }
    e.exec("tab_select", &json!({"id": 2})).unwrap();
    assert_eq!(tabs(&e), "1* 2! 3*");
    request(&mut e, &mut g, Pending::Quit);
    assert!(matches!(g.pending, Some(Pending::Quit)) && tabs(&e) == "1!* 2 3*" && !g.allow_close);
    answer(&mut e, &mut g, 1);
    assert!(matches!(g.pending, Some(Pending::Quit)) && tabs(&e) == "1* 2 3!*" && !g.allow_close, "not saving one moves on to the next; it is not closed or changed");
    answer(&mut e, &mut g, 2);
    assert!(g.pending.is_none() && !g.allow_close && tabs(&e) == "1* 2 3!*", "cancel stops the quit with everything still open");
    request(&mut e, &mut g, Pending::Quit);
    assert!(tabs(&e) == "1* 2 3!*", "asking again starts with the document on show, and asks about the first again later");
    answer(&mut e, &mut g, 0);
    assert!(tabs(&e) == "1!* 2 3" && matches!(g.pending, Some(Pending::Quit)) && !g.allow_close);
    answer(&mut e, &mut g, 1);
    assert!(g.pending.is_none() && g.allow_close, "every document has been dealt with");
    let mut clean = Engine::new("headless");
    let mut g = Gui::new();
    request(&mut clean, &mut g, Pending::Quit);
    assert!(g.allow_close && g.pending.is_none());

    // A quit answer meant for one document never waves another through: if the document on
    // show changed while the question was up, the question is asked again.
    let mut e = Engine::new("headless");
    for name in ["w1.jmn", "w2.jmn"] {
        e.exec("tab_new", &json!({"width": 8, "height": 8, "reuse_blank": true})).unwrap();
        e.exec("save_document", &json!({"path": tmp(name)})).unwrap();
        e.exec("draw_rect", &json!({"x": 0, "y": 0, "w": 1, "h": 1, "color": 4, "filled": true})).unwrap();
    }
    let mut g = Gui::new();
    request(&mut e, &mut g, Pending::Quit);
    assert_eq!((tabs(&e), g.quit_asked), ("1* 2!*".to_string(), 2));
    e.exec("tab_select", &json!({"id": 1})).unwrap();
    answer(&mut e, &mut g, 1);
    assert!(g.quit_skip.is_empty() && !g.allow_close && matches!(g.pending, Some(Pending::Quit)), "nothing was waved through");
    answer(&mut e, &mut g, 1);
    answer(&mut e, &mut g, 1);
    assert!(g.allow_close && g.quit_skip.len() == 2, "each document was asked about once it was the one on show");

    // A remote call on a background document leaves the user's tools and clipboard as they were.
    e.exec("set_options", &json!({"tool": "rect", "brush_size": 7})).unwrap();
    e.exec("select_rect", &json!({"x": 0, "y": 0, "w": 2, "h": 2})).unwrap();
    e.exec("copy", &json!({})).unwrap();
    let (me, other) = (e.id, if e.id == 1 { 2 } else { 1 });
    let clip = e.clipboard.as_ref().map(|c| (c.w, c.h));
    for (cmd, params) in [("set_options", json!({"tool": "fill", "brush_size": 2})), ("select_all", json!({})), ("copy", json!({}))] {
        assert!(!crate::remote::call_tool(&mut e, "execute_command", &json!({"command": cmd, "params": params, "document": other})).is_error, "{cmd}");
    }
    assert_eq!((e.id, e.o.tool, e.o.brush_size, e.clipboard.as_ref().map(|c| (c.w, c.h))), (me, Tool::Rect, 7, clip));

    // One file, one tab: a document is not opened or saved over a file another tab holds.
    let held = e.tab(other).unwrap().path.clone().unwrap();
    let err = e.exec("open_document", &json!({"path": held, "force": true})).unwrap_err();
    assert!(err.contains("open in another tab"), "{err}");
    let err = e.exec("save_document", &json!({"path": held})).unwrap_err();
    assert!(err.contains("open in another tab"), "{err}");
    let r = script::run_lua(&mut e, &format!("return (pcall(jm.save_document, {{ path = {:?} }}))", held.display().to_string()));
    assert_eq!((r.result, r.error), (json!(false), None), "a script is refused the same way");
    e.exec("save_document", &json!({})).unwrap();
    assert!(e.exec("open_document", &json!({"path": e.path.clone().unwrap()})).is_ok(), "reloading a document's own file is still allowed");
}

// ---------------------------------------------------------------- animation

#[test]
fn animation_setup_is_part_of_the_document_and_playback_is_not() {
    let mut e = eng(128, 64);
    assert_eq!(e.exec("anim_info", &json!({})).unwrap()["active"], false);
    assert!(e.exec("anim_control", &json!({"step": 1})).is_err(), "nothing to step yet");

    // With nothing selected the whole image is cut up, at the grid size.
    let v = e.exec("anim_set", &json!({})).unwrap();
    assert_eq!((v["w"].as_i64(), v["frame_w"].as_i64(), v["cols"].as_i64(), v["rows"].as_i64(), v["frames"].as_i64()), (Some(128), Some(16), Some(8), Some(4), Some(32)));
    e.exec("anim_clear", &json!({})).unwrap();

    // A selection gives the start and extent; part frames at the right and bottom edges are left out.
    e.exec("select_rect", &json!({"x": 8, "y": 16, "w": 70, "h": 40})).unwrap();
    let v = e.exec("anim_set", &json!({"frame_w": 16, "frame_h": 16, "fps": 12})).unwrap();
    assert_eq!((v["x"].as_i64(), v["y"].as_i64(), v["cols"].as_i64(), v["rows"].as_i64(), v["frames"].as_i64()), (Some(8), Some(16), Some(4), Some(2), Some(8)));
    assert_eq!(v["playing"], true, "a new animation starts playing");

    // Frames run left to right, then top to bottom, and stepping wraps both ways.
    let at = |e: &mut Engine, p: Value| {
        let v = e.exec("anim_control", &p).unwrap();
        (v["frame"].as_i64().unwrap(), v["frame_x"].as_i64().unwrap(), v["frame_y"].as_i64().unwrap())
    };
    assert_eq!(at(&mut e, json!({"frame": 3})), (3, 56, 16));
    assert_eq!(at(&mut e, json!({"step": 1})), (4, 8, 32), "after the end of a row comes the start of the next");
    assert_eq!(at(&mut e, json!({"step": 4})), (0, 8, 16));
    assert_eq!(at(&mut e, json!({"step": -1})), (7, 56, 32));
    assert!(e.exec("anim_control", &json!({"frame": 8})).is_err());

    // A frame count stops before the empty cells of the last row; later changes keep what was set.
    let v = e.exec("anim_set", &json!({"frames": 6})).unwrap();
    assert_eq!((v["frames"].as_i64(), v["frame"].as_i64(), v["fps"].as_f64(), v["x"].as_i64()), (Some(6), Some(5), Some(12.0), Some(8)));
    assert_eq!(at(&mut e, json!({"step": 1})), (0, 8, 16));
    for bad in [json!({"frame_w": 0}), json!({"frame_w": 71}), json!({"fps": 0}), json!({"fps": 500}), json!({"frames": -1}), json!({"x": 500}), json!({"w": -4}), json!({"speed": 3})] {
        assert!(e.exec("anim_set", &bad).is_err(), "{bad} is refused");
    }
    assert_eq!(e.exec("anim_info", &json!({})).unwrap()["frames"], 6, "a refused change leaves the setup alone");

    // A region hanging off the image is clipped to it.
    let v = e.exec("anim_set", &json!({"x": 96, "y": 0, "w": 400, "h": 16, "frames": 0})).unwrap();
    assert_eq!((v["w"].as_i64(), v["frames"].as_i64()), (Some(32), Some(2)));

    // The setup is a document change (undoable, marks it modified); stepping and playing are not.
    let (undo, doc) = (e.undo.len(), e.doc.clone());
    e.modified = false;
    e.exec("anim_control", &json!({"step": 1, "playing": false})).unwrap();
    assert!(e.undo.len() == undo && !e.modified && e.doc == doc, "playback leaves the document alone");
    e.exec("anim_set", &json!({"fps": 20})).unwrap();
    assert!(e.undo.len() == undo + 1 && e.modified);
    e.exec("undo", &json!({})).unwrap();
    assert_eq!(e.exec("anim_info", &json!({})).unwrap()["fps"], 12.0);
    // No pixels were touched by any of it.
    assert!(e.doc.layers == doc.layers);

    // It is saved in the project, and comes back on open.
    let p = tmp("anim.jmn");
    e.exec("save_document", &json!({"path": p})).unwrap();
    let saved = e.doc.anims.clone();
    assert_eq!(saved.len(), 1);
    e.exec("new_document", &json!({"width": 8, "height": 8, "force": true})).unwrap();
    assert_eq!(e.exec("anim_info", &json!({})).unwrap()["active"], false);
    e.exec("open_document", &json!({"path": p})).unwrap();
    assert_eq!(e.doc.anims, saved);

    // An animation that no longer fits the image goes, and undo brings it back.
    e.exec("resize_canvas", &json!({"width": 100, "height": 64})).unwrap();
    assert_eq!(e.exec("anim_info", &json!({})).unwrap()["active"], false);
    e.exec("undo", &json!({})).unwrap();
    assert_eq!(e.doc.anims, saved);

    // Scripts drive it too.
    let out = script::run_lua(&mut e, "jm.anim_set{ x = 0, y = 0, w = 64, h = 32, frame_w = 32, frame_h = 32 } jm.anim_control{ frame = 1 } return jm.anim_info().frame_x");
    assert_eq!((out.error, out.result), (None, json!(32)));
}

#[test]
fn project_files_stay_compatible_with_published_builds() {
    use crate::doc::*;
    use std::io::{Read, Write};
    let inflate = |path: &str| {
        let bytes = std::fs::read(path).unwrap();
        let mut raw = vec![];
        flate2::read::DeflateDecoder::new(&bytes[6..]).read_to_end(&mut raw).unwrap();
        (bytes[..6].to_vec(), raw)
    };
    let write = |path: &str, magic: &[u8], raw: &[u8]| {
        let mut out = magic.to_vec();
        let mut enc = flate2::write::DeflateEncoder::new(&mut out, flate2::Compression::default());
        enc.write_all(raw).unwrap();
        enc.finish().unwrap();
        std::fs::write(path, out).unwrap();
    };
    let mut d = Doc::new(64, 32, Palette::default_256());
    d.layers[0] = { let mut l = d.layers[0].clone(); std::sync::Arc::make_mut(&mut l.data)[5] = 9; l };
    let plain = bincode::serialize(&d).unwrap();

    // A project without an animation is byte-for-byte what 0.1.3 wrote.
    let p = tmp("compat.jmn");
    crate::io::save_doc(&d, std::path::Path::new(&p)).unwrap();
    let (magic, raw) = inflate(&p);
    assert_eq!(raw, plain);

    // With animations, the document comes first and unchanged, so 0.1.3's loader -
    // bincode::deserialize of the whole buffer - still reads it; it just does not see them.
    let grid = Grid { x: 0, y: 0, w: 64, h: 32, fw: 16, fh: 16, limit: 5 };
    let list = Frames::List(std::sync::Arc::new(vec![Frame { x: 1, y: 2, w: 9, h: 7, ox: -3, oy: 4 }, Frame { x: 20, y: 0, w: 30, h: 32, ox: 0, oy: 0 }]));
    d.anims = vec![Anim { name: "jump".into(), fps: 24.0, frames: list }, Anim { name: "walk".into(), fps: 10.0, frames: Frames::Grid(grid) }];
    let path = std::path::Path::new(&p);
    crate::io::save_doc(&d, path).unwrap();
    let (_, raw) = inflate(&p);
    assert!(raw.len() > plain.len() && raw[..plain.len()] == plain[..]);
    let old: Doc = bincode::deserialize(&raw).unwrap();
    assert!(old.layers == d.layers && old.anims.is_empty());
    assert_eq!(crate::io::load_doc(path).unwrap().anims, d.anims);

    // 0.1.4 reads one grid animation from an "ANIM" chunk and skips chunks it does not know.
    // This is its reader; it finds the first grid animation in a file this build wrote.
    type V1 = (u32, u32, u32, u32, u32, u32, u32, f32);
    let as_014 = |mut rest: &[u8]| {
        let mut got: Option<V1> = None;
        while rest.len() >= 8 {
            let len = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]) as usize;
            let Some(payload) = rest[8..].get(..len) else { break };
            if &rest[..4] == b"ANIM" {
                if let Ok(a) = bincode::deserialize(payload) {
                    got = Some(a);
                }
            }
            rest = &rest[8 + len..];
        }
        got
    };
    let tail = &raw[plain.len()..];
    assert_eq!(as_014(tail), Some((0, 0, 64, 32, 16, 16, 5, 10.0)));
    // A document with only frame lists has nothing 0.1.4 can show, so no "ANIM" chunk is written.
    let mut lists_only = d.clone();
    lists_only.anims.truncate(1);
    crate::io::save_doc(&lists_only, path).unwrap();
    assert_eq!(as_014(&inflate(&p).1[plain.len()..]), None);
    assert_eq!(crate::io::load_doc(path).unwrap().anims, lists_only.anims);

    // And a file 0.1.4 saved - its one chunk - opens with that animation.
    let v1 = |a: V1| [b"ANIM".as_slice(), &32u32.to_le_bytes(), &bincode::serialize(&a).unwrap()].concat();
    let chunk = |tag: &[u8; 4], body: &[u8]| [tag.as_slice(), &(body.len() as u32).to_le_bytes(), body].concat();
    let from_014 = Anim { name: "Animation".into(), fps: 10.0, frames: Frames::Grid(grid) };
    write(&p, &magic, &[plain.as_slice(), &v1((0, 0, 64, 32, 16, 16, 5, 10.0))].concat());
    assert_eq!(crate::io::load_doc(path).unwrap().anims, vec![from_014.clone()]);

    // Chunks this build does not know are skipped, in any order; damaged ones are ignored, and
    // when the full list cannot be read the single animation 0.1.4 would see is used.
    let unknown = chunk(b"WHAT", &[1, 2, 3]);
    let names = |a: &[Anim]| a.iter().map(|a| a.name.clone()).collect::<Vec<_>>().join(",");
    for (extra, expect) in [
        ([unknown.as_slice(), tail].concat(), "jump,walk"),
        ([tail, unknown.as_slice()].concat(), "jump,walk"),
        (tail[..tail.len() - 1].to_vec(), "Animation"),
        ([b"ANIM".as_slice(), &u32::MAX.to_le_bytes(), &[0; 40]].concat(), ""),
        (chunk(b"ANIM", &[7, 7]), ""),
        ([tail, &chunk(b"ANIM", &[7, 7])].concat(), "jump,walk"),
        ([tail, &chunk(b"ANMS", b"{ not json")].concat(), "jump,walk"),
        (chunk(b"ANMS", b"[1, 2, 3]"), ""),
        (chunk(b"ANMS", br#"{"format":"something-else","animations":[]}"#), ""),
        (vec![0xff; 5], ""),
    ] {
        write(&p, &magic, &[plain.as_slice(), &extra].concat());
        let got = crate::io::load_doc(path).unwrap();
        assert_eq!(names(&got.anims), expect, "extra bytes {:?}", &extra[..extra.len().min(24)]);
        assert!(got.layers == d.layers);
    }

    // A stored list far larger than this program writes is not parsed at all, and a list with
    // nothing usable in it does not hide the animation 0.1.4 would show.
    let padded = format!("{}{}", " ".repeat((crate::io::ANIM_JSON_MAX + 1) as usize), std::str::from_utf8(&tail[tail.len() - 40..]).map(|_| "").unwrap_or(""));
    let walk_only = json!({"format": "jon-motion-animations", "animations": [{"name": "kept", "frames": [[0, 0, 4, 4]]}]}).to_string();
    for (extra, expect) in [
        (chunk(b"ANMS", format!("{padded}{walk_only}").as_bytes()), ""),
        ([v1((0, 0, 64, 32, 16, 16, 5, 10.0)), chunk(b"ANMS", format!("{padded}{walk_only}").as_bytes())].concat(), "Animation"),
        (chunk(b"ANMS", walk_only.as_bytes()), "kept"),
        ([v1((0, 0, 64, 32, 16, 16, 5, 10.0)), chunk(b"ANMS", br#"{"format":"jon-motion-animations","animations":[]}"#)].concat(), "Animation"),
        ([v1((0, 0, 64, 32, 16, 16, 5, 10.0)), chunk(b"ANMS", br#"{"format":"jon-motion-animations","animations":[{"name":"bad","frames":[[99,0,4,4]]}]}"#)].concat(), "Animation"),
    ] {
        write(&p, &magic, &[plain.as_slice(), &extra].concat());
        assert_eq!(names(&crate::io::load_doc(path).unwrap().anims), expect);
    }

    // 0.1.4's chunk is checked like the rest of the file.
    let good: V1 = (0, 0, 64, 32, 16, 16, 5, 10.0);
    let g = |f: &dyn Fn(&mut V1)| { let mut a = good; f(&mut a); a };
    for (a, expect) in [
        (g(&|a| a.7 = f32::NAN), Some((5, 8.0))),
        (g(&|a| a.7 = -3.0), Some((5, 8.0))),
        (g(&|a| a.7 = f32::INFINITY), Some((5, 8.0))),
        (g(&|a| a.6 = u32::MAX), Some((0, 10.0))),
        (g(&|a| a.4 = 0), None),
        (g(&|a| a.5 = 0), None),
        (g(&|a| a.4 = 65), None),
        (g(&|a| a.0 = 1), None),
        (g(&|a| (a.0, a.1) = (u32::MAX, u32::MAX)), None),
        (g(&|a| (a.2, a.3, a.4, a.5) = (u32::MAX, u32::MAX, 1, 1)), None),
    ] {
        write(&p, &magic, &[plain.as_slice(), &v1(a)].concat());
        let mut e = Engine::new("headless");
        e.exec("open_document", &json!({"path": p})).unwrap();
        assert_eq!(e.anim().map(|x| (x.grid().unwrap().limit, x.fps)), expect, "{a:?}");
        // Whatever survived can be shown and stepped without trouble.
        e.exec("anim_info", &json!({})).unwrap();
        let _ = e.exec("anim_control", &json!({"step": 1}));
    }

    // The full list is JSON written by this program, but a file can say anything: entries that
    // are not sound for the image are left out one by one, and nothing is trusted to be in range.
    let hostile = json!({"format": "jon-motion-animations", "version": 99, "animations": [
        {"name": "ok", "fps": 12, "grid": {"x": 0, "y": 0, "w": 64, "h": 32, "frame_w": 8, "frame_h": 8, "frames": 4000000000u64}},
        {"name": "ok", "fps": 12, "frames": [[0, 0, 4, 4]]},
        {"name": "off the image", "frames": [[60, 0, 8, 8]]},
        {"name": "negative", "frames": [[-1, 0, 8, 8]]},
        {"name": "fraction", "frames": [[0.5, 0, 8, 8]]},
        {"name": "huge", "frames": [[0, 0, 1e300, 8]]},
        {"name": "wild offset", "frames": [[0, 0, 8, 8, 99999999, 0]]},
        {"name": "zero", "grid": {"x": 0, "y": 0, "w": 64, "h": 32, "frame_w": 0, "frame_h": 8}},
        {"name": "fast", "fps": 1e9, "frames": [[0, 0, 4, 4]]},
        {"name": "text", "fps": "12", "frames": [[0, 0, 4, 4]]},
        {"name": "both", "grid": {"x": 0, "y": 0, "w": 8, "h": 8, "frame_w": 8, "frame_h": 8}, "frames": [[0, 0, 4, 4]]},
        {"name": "neither"},
        {"name": "empty", "frames": []},
        {"name": " padded ", "frames": [[0, 0, 4, 4]]},
        {"name": "", "frames": [[0, 0, 4, 4]]},
        {"name": "bidi\u{202e}", "frames": [[0, 0, 4, 4]]},
        {"name": "x".repeat(65), "frames": [[0, 0, 4, 4]]},
        {"name": 7, "frames": [[0, 0, 4, 4]]},
        {"frames": [[0, 0, 4, 4]]},
        "text", null, 4, [],
        {"name": "last", "frames": [{"x": 3, "y": 3, "w": 5, "h": 5, "offset_x": -2, "offset_y": 2}, [0, 0, 4, 4]]},
    ]});
    write(&p, &magic, &[plain.as_slice(), &chunk(b"ANMS", hostile.to_string().as_bytes())].concat());
    let mut e = Engine::new("headless");
    e.exec("open_document", &json!({"path": p})).unwrap();
    assert_eq!(names(&e.doc.anims), "ok,last");
    assert_eq!((e.doc.anims[0].grid().unwrap().limit, e.doc.anims[1].bounds()), (0, (-2, 0, 6, 7)));
    for i in 0..2 {
        e.exec("anim_control", &json!({"name": e.doc.anims[i].name, "step": -1})).unwrap();
    }
    // Too many of anything is cut off, not allocated.
    let many: Vec<Value> = (0..Anim::MAX + 50).map(|i| json!({"name": format!("a{i}"), "frames": [[0, 0, 1, 1]]})).collect();
    let long: Vec<Value> = (0..Anim::MAX_FRAMES + 1).map(|_| json!([0, 0, 1, 1])).collect();
    for (body, n) in [(json!({"format": "jon-motion-animations", "animations": many}), 0), (json!({"format": "jon-motion-animations", "animations": [{"name": "long", "frames": long}]}), 0)] {
        write(&p, &magic, &[plain.as_slice(), &chunk(b"ANMS", body.to_string().as_bytes())].concat());
        assert_eq!(crate::io::load_doc(path).unwrap().anims.len(), n);
    }
}

#[test]
fn a_document_holds_many_named_animations() {
    use crate::doc::*;
    let mut e = eng(128, 64);
    let names = |e: &mut Engine| e.exec("anim_list", &json!({})).unwrap()["animations"].as_array().unwrap().iter().map(|a| a["name"].as_str().unwrap().to_string()).collect::<Vec<_>>().join(",");
    assert_eq!(e.exec("anim_list", &json!({})).unwrap(), json!({"current": null, "animations": []}));

    // A name that is not in use makes a new animation and puts it on show; with no name the
    // one on show is changed.
    let v = e.exec("anim_set", &json!({"name": "walk", "x": 0, "y": 0, "w": 64, "h": 16, "frame_w": 16, "frame_h": 16})).unwrap();
    assert_eq!((v["name"].as_str(), v["kind"].as_str(), v["frames"].as_i64(), v["index"].as_i64(), v["count"].as_i64()), (Some("walk"), Some("grid"), Some(4), Some(0), Some(1)));
    let v = e.exec("anim_set", &json!({"name": "run", "x": 0, "y": 16, "w": 96, "h": 16, "frame_w": 16, "frame_h": 16, "fps": 20})).unwrap();
    assert_eq!((v["name"].as_str(), v["frames"].as_i64(), v["index"].as_i64(), v["current"].as_bool(), v["playing"].as_bool()), (Some("run"), Some(6), Some(1), Some(true), Some(true)));
    e.exec("anim_set", &json!({"frames": 5})).unwrap();
    assert_eq!((e.doc.anims[0].count(), e.doc.anims[1].count()), (4, 5), "no name means the one on show");
    // Naming another one changes it without taking over the display.
    let v = e.exec("anim_set", &json!({"name": "walk", "fps": 30})).unwrap();
    assert_eq!((v["fps"].as_f64(), v["current"].as_bool(), v["playing"].as_bool(), e.view.anim_index), (Some(30.0), Some(false), Some(false), 1));
    assert_eq!(e.exec("anim_info", &json!({})).unwrap()["name"], "run");
    assert_eq!(e.exec("anim_info", &json!({"name": "walk"})).unwrap()["frames"], 4);
    assert!(e.exec("anim_info", &json!({"name": "nope"})).is_err());

    // A frame list: every frame its own rectangle, with offsets that keep trimmed frames lined up.
    let v = e.exec("anim_frames", &json!({"name": "jump", "fps": 12, "frames": [[0, 32, 10, 20], [10, 32, 14, 24, -2, -4], {"x": 24, "y": 32, "w": 8, "h": 8, "offset_x": 1, "offset_y": 12}]})).unwrap();
    assert_eq!((v["kind"].as_str(), v["frames"].as_i64(), v["box_w"].as_i64(), v["box_h"].as_i64(), v["index"].as_i64()), (Some("frames"), Some(3), Some(14), Some(24), Some(2)));
    let v = e.exec("anim_control", &json!({"step": 1, "playing": false})).unwrap();
    assert_eq!((v["frame_x"].as_i64(), v["frame_w"].as_i64(), v["frame_h"].as_i64(), v["offset_x"].as_i64(), v["offset_y"].as_i64()), (Some(10), Some(14), Some(24), Some(-2), Some(-4)));
    assert_eq!(e.exec("anim_control", &json!({"step": 2})).unwrap()["frame"], 0, "stepping wraps");
    // On a frame list anim_set changes the speed and nothing else, and a name that is not in
    // use is only a new animation when it comes with a region or frame size.
    assert_eq!(e.exec("anim_set", &json!({"fps": 6})).unwrap()["kind"], "frames");
    assert!(e.exec("anim_set", &json!({"frames": 1})).unwrap_err().contains("frame list"));
    assert!(e.exec("anim_set", &json!({"name": "jump", "frame_w": 8, "fps": 3})).is_err());
    assert_eq!((e.doc.anims[2].fps, e.doc.anims[2].count()), (6.0, 3));
    assert!(e.exec("anim_set", &json!({"name": "wlak", "fps": 20})).unwrap_err().contains("no animation named"));
    e.exec("anim_set", &json!({"name": "walk", "fps": 8.3})).unwrap();
    assert_eq!(e.exec("anim_info", &json!({"name": "walk"})).unwrap()["fps"], 8.3, "a speed reads back as it was given");
    e.exec("anim_set", &json!({"name": "walk", "fps": 30})).unwrap();
    for bad in [
        json!({"frames": []}), json!({"frames": [[0, 0, 4]]}), json!({"frames": [[0, 0, 0, 4]]}), json!({"frames": [[120, 0, 16, 4]]}), json!({"frames": [[-1, 0, 4, 4]]}),
        json!({"frames": [[0.5, 0, 4, 4]]}), json!({"frames": [[0, 0, 4, 4, 5000, 0]]}), json!({"frames": [["a", 0, 4, 4]]}), json!({"frames": "all"}), json!({"frames": [[0, 0, 4, 4]], "fps": 0}),
        json!({"frames": [[0, 0, 4, 4]], "name": ""}), json!({"frames": [[0, 0, 4, 4]], "name": " x"}), json!({"frames": [[0, 0, 4, 4]], "name": "a\nb"}), json!({"frames": [[0, 0, 4, 4]], "name": "y".repeat(65)}),
        json!({"frames": (0..Anim::MAX_FRAMES + 1).map(|_| json!([0, 0, 1, 1])).collect::<Vec<_>>()}),
    ] {
        assert!(e.exec("anim_frames", &bad).is_err(), "{:.80} is refused", bad.to_string());
    }
    assert_eq!((names(&mut e), e.doc.anims[2].count()), ("walk,run,jump".to_string(), 3), "a refused change leaves everything alone");

    // Choosing which is on show is playback, not a document change.
    let (undo, doc) = (e.undo.len(), e.doc.clone());
    e.modified = false;
    let v = e.exec("anim_control", &json!({"name": "walk", "frame": 2})).unwrap();
    assert_eq!((v["name"].as_str(), v["frame"].as_i64(), v["frame_x"].as_i64(), e.view.anim_index), (Some("walk"), Some(2), Some(32), 0));
    assert_eq!(e.exec("anim_control", &json!({"name": "run"})).unwrap()["frame"], 0, "another animation starts at its first frame");
    assert!(e.exec("anim_control", &json!({"name": "nope"})).is_err());
    assert!(e.undo.len() == undo && !e.modified && e.doc == doc);

    // Rename, remove, undo.
    assert!(e.exec("anim_rename", &json!({"to": "walk"})).is_err(), "names are unique");
    assert!(e.exec("anim_rename", &json!({"to": "bad\u{202e}"})).is_err());
    e.exec("anim_rename", &json!({"to": "run"})).unwrap();
    e.exec("anim_rename", &json!({"name": "walk", "to": "stroll"})).unwrap();
    assert_eq!(names(&mut e), "stroll,run,jump");
    e.exec("undo", &json!({})).unwrap();
    assert_eq!(names(&mut e), "walk,run,jump");
    e.exec("anim_control", &json!({"name": "jump"})).unwrap();
    e.exec("anim_clear", &json!({"name": "walk"})).unwrap();
    assert_eq!((names(&mut e), e.exec("anim_info", &json!({})).unwrap()["name"].as_str().map(str::to_string)), ("run,jump".to_string(), Some("jump".to_string())), "the one on show stays on show");
    assert!(e.exec("anim_clear", &json!({"name": "walk"})).is_err());
    e.exec("anim_clear", &json!({})).unwrap();
    assert_eq!(names(&mut e), "run");
    e.exec("anim_clear", &json!({"all": true})).unwrap();
    assert_eq!(e.exec("anim_info", &json!({})).unwrap()["active"], false);
    e.exec("anim_clear", &json!({})).unwrap();
    for _ in 0..3 {
        e.exec("undo", &json!({})).unwrap();
    }
    assert_eq!(names(&mut e), "walk,run,jump");
    // New ones get a name of their own.
    e.exec("anim_clear", &json!({"all": true})).unwrap();
    e.exec("anim_set", &json!({})).unwrap();
    e.exec("anim_set", &json!({"name": "Animation 2", "frame_w": 8})).unwrap();
    assert_eq!(crate::commands::fresh_anim_name(&e.doc), "Animation 3");
    e.exec("undo", &json!({})).unwrap();
    e.exec("undo", &json!({})).unwrap();
    e.exec("undo", &json!({})).unwrap();

    // All of it is saved in the project.
    let p = tmp("named.jmn");
    e.exec("save_document", &json!({"path": p})).unwrap();
    let saved = e.doc.anims.clone();
    e.exec("new_document", &json!({"width": 8, "height": 8, "force": true})).unwrap();
    e.exec("open_document", &json!({"path": p})).unwrap();
    assert_eq!(e.doc.anims, saved);

    // An animation that no longer fits the image goes (the others stay), and undo brings it back.
    e.exec("resize_canvas", &json!({"width": 80, "height": 64})).unwrap();
    assert_eq!(names(&mut e), "walk,jump");
    e.exec("undo", &json!({})).unwrap();
    assert_eq!(e.doc.anims, saved);
    // A document may hold only so many.
    let mut full = eng(8, 8);
    for i in 0..Anim::MAX {
        full.doc.anims.push(Anim { name: format!("a{i}"), fps: 8.0, frames: Frames::Grid(Grid { x: 0, y: 0, w: 8, h: 8, fw: 8, fh: 8, limit: 0 }) });
    }
    assert!(full.exec("anim_set", &json!({"name": "one more", "frame_w": 8, "frame_h": 8})).unwrap_err().contains("at most"));

    // ---- JSON: export, edit by hand, import
    let file = tmp("anims.json");
    let _ = std::fs::remove_file(&file);
    assert_eq!(e.exec("anim_export_json", &json!({"path": file})).unwrap()["animations"], 3);
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!((v["format"].as_str(), v["version"].as_i64(), v["image"]["width"].as_i64(), v["animations"].as_array().unwrap().len()), (Some("jon-motion-animations"), Some(1), Some(128), 3));
    assert_eq!(v["animations"][0], json!({"name": "walk", "fps": 30, "grid": {"x": 0, "y": 0, "w": 64, "h": 16, "frame_w": 16, "frame_h": 16, "frames": 0}}));
    assert_eq!((&v["animations"][2]["frames"][0], &v["animations"][2]["frames"][1]), (&json!([0, 32, 10, 20]), &json!([10, 32, 14, 24, -2, -4])));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("\n      [10, 32, 14, 24, -2, -4],\n") && text.contains("\"fps\": 30,"), "one frame per line, speeds as typed:\n{text}");
    // It round-trips, onto an empty document or over the same names.
    e.exec("anim_clear", &json!({"all": true})).unwrap();
    assert_eq!(e.exec("anim_import_json", &json!({"path": file})).unwrap()["imported"], 3);
    assert_eq!(e.doc.anims, saved);
    e.exec("anim_set", &json!({"name": "walk", "fps": 2})).unwrap();
    e.exec("anim_set", &json!({"name": "mine", "x": 0, "y": 0, "w": 8, "h": 8, "frame_w": 8, "frame_h": 8})).unwrap();
    let v = e.exec("anim_import_json", &json!({"path": file})).unwrap();
    assert_eq!((v["imported"].as_i64(), v["count"].as_i64(), names(&mut e), e.doc.anims[0].fps), (Some(3), Some(4), "walk,run,jump,mine".to_string(), 30.0), "same names are replaced, the rest kept");
    e.exec("undo", &json!({})).unwrap();
    assert_eq!(e.doc.anims[0].fps, 2.0, "an import is one undo step");
    e.exec("anim_import_json", &json!({"path": file, "replace": true})).unwrap();
    assert_eq!(e.doc.anims, saved);

    // Export never writes over a file that is not an animations file, and only writes .json.
    let other = tmp("package.json");
    std::fs::write(&other, r#"{"name": "someone else's"}"#).unwrap();
    assert!(e.exec("anim_export_json", &json!({"path": other})).is_err());
    assert_eq!(std::fs::read_to_string(&other).unwrap(), r#"{"name": "someone else's"}"#);
    std::fs::write(&other, "").unwrap();
    assert!(e.exec("anim_export_json", &json!({"path": other})).is_err(), "not even an empty one");
    assert!(e.exec("anim_export_json", &json!({"path": tmp("anims.txt")})).is_err());
    let dir = tmp("folder.json");
    std::fs::create_dir_all(&dir).unwrap();
    assert!(e.exec("anim_export_json", &json!({"path": dir})).is_err() && std::path::Path::new(&dir).is_dir());
    e.exec("anim_export_json", &json!({"path": file})).unwrap();
    let mut none = eng(8, 8);
    assert!(none.exec("anim_export_json", &json!({"path": file})).is_err(), "nothing to export leaves the file alone");

    // Nor over a file that merely carries the format marker among other things, nor into the app's own folder.
    std::fs::write(&other, json!({"format": "jon-motion-animations", "animations": [], "keymap": "dpaint", "recent": []}).to_string()).unwrap();
    assert!(e.exec("anim_export_json", &json!({"path": other})).is_err());
    assert!(std::fs::read_to_string(&other).unwrap().contains("keymap"));
    let app_dir = std::path::Path::new(&dir);
    assert!(crate::commands::anim_export_allowed(&app_dir.join("settings.json"), Some(app_dir)).unwrap_err().contains("settings folder"));
    assert!(crate::commands::anim_export_allowed(&app_dir.join("..").join("folder.json").join("x.json"), Some(app_dir)).is_err(), "however the folder is spelled");
    assert!(crate::commands::anim_export_allowed(std::path::Path::new(&tmp("fresh.json")), Some(app_dir)).is_ok());

    // A document holds only so many frame-list frames, so its largest export can be read back.
    let mut big = eng(8, 8);
    let frames: Vec<Value> = (0..Anim::MAX_FRAMES).map(|i| json!([i % 4, 0, 4, 8, -1, i % 3])).collect();
    for i in 0..Anim::MAX_LIST_FRAMES / Anim::MAX_FRAMES {
        big.exec("anim_frames", &json!({"name": format!("n{i}"), "frames": frames})).unwrap();
    }
    assert!(big.exec("anim_frames", &json!({"name": "over", "frames": [[0, 0, 1, 1]]})).unwrap_err().contains("in all"));
    big.exec("anim_set", &json!({"name": "grids are not counted", "frame_w": 4, "frame_h": 4})).unwrap();
    big.exec("anim_frames", &json!({"name": "n0", "frames": frames})).unwrap();
    let (max_file, max_jmn) = (tmp("max.json"), tmp("max.jmn"));
    let _ = std::fs::remove_file(&max_file);
    big.exec("anim_export_json", &json!({"path": max_file})).unwrap();
    assert!(std::fs::metadata(&max_file).unwrap().len() < crate::io::ANIM_JSON_MAX);
    let all = big.doc.anims.clone();
    big.exec("anim_import_json", &json!({"path": max_file, "replace": true})).unwrap();
    assert_eq!(big.doc.anims, all);
    big.exec("save_document", &json!({"path": max_jmn})).unwrap();
    assert_eq!(crate::io::load_doc(std::path::Path::new(&max_jmn)).unwrap().anims, all);
    let mut small = eng(8, 8);
    small.exec("anim_frames", &json!({"name": "mine", "frames": [[0, 0, 1, 1]]})).unwrap();
    assert!(small.exec("anim_import_json", &json!({"path": max_file})).unwrap_err().contains("frame-list frames"));

    // After an undo changes the list, the animation that was on show stays on show.
    let mut u = eng(32, 32);
    for n in ["a", "b", "c"] {
        u.exec("anim_set", &json!({"name": n, "frame_w": 8})).unwrap();
    }
    u.exec("anim_control", &json!({"name": "c"})).unwrap();
    u.exec("anim_clear", &json!({"name": "a"})).unwrap();
    assert_eq!(u.anim().unwrap().name, "c");
    u.exec("undo", &json!({})).unwrap();
    assert_eq!((u.anim().unwrap().name.as_str(), u.view.anim_index), ("c", 2));
    u.exec("anim_rename", &json!({"to": "sea"})).unwrap();
    u.exec("anim_clear", &json!({"name": "b"})).unwrap();
    assert_eq!(u.anim().unwrap().name, "sea");

    // Import is all or nothing, and a file can say anything.
    let before = e.doc.anims.clone();
    let wrap = |anims: Value| json!({"format": "jon-motion-animations", "version": 1, "animations": anims}).to_string();
    let one = json!({"name": "new", "frames": [[0, 0, 4, 4]]});
    for (text, why) in [
        (wrap(json!([one, {"name": "off", "frames": [[127, 0, 4, 4]]}])), "\"off\""),
        (wrap(json!([one, one])), "two animations"),
        (wrap(json!([{"name": "neg", "frames": [[0, -4, 4, 4]]}])), "\"neg\""),
        (wrap(json!([{"name": "frac", "grid": {"x": 0, "y": 0, "w": 8.5, "h": 8, "frame_w": 4, "frame_h": 4}}])), "whole number"),
        (wrap(json!([{"name": "big", "grid": {"x": 0, "y": 0, "w": 1e12, "h": 8, "frame_w": 4, "frame_h": 4}}])), "whole number"),
        (wrap(json!([{"name": "fps", "fps": -1, "frames": [[0, 0, 4, 4]]}])), "fps"),
        (wrap(json!([{"name": "", "frames": [[0, 0, 4, 4]]}])), "animation 0"),
        (wrap(json!([{"name": "ctl\u{7}", "frames": [[0, 0, 4, 4]]}])), "animation 0"),
        (wrap(json!([7])), "animation 0"),
        (wrap(json!((0..Anim::MAX + 1).map(|i| json!({"name": format!("n{i}"), "frames": [[0, 0, 1, 1]]})).collect::<Vec<_>>())), "limit"),
        (wrap(json!("no")), "animations"),
        (json!({"animations": [one]}).to_string(), "format"),
        (json!({"scale": 1, "prefix": "atlas_", "pages": 3, "objects": []}).to_string(), "format"),
        ("[1, 2".to_string(), "not valid JSON"),
        (String::new(), "not valid JSON"),
    ] {
        let bad = tmp("bad.json");
        std::fs::write(&bad, &text).unwrap();
        let err = e.exec("anim_import_json", &json!({"path": bad})).unwrap_err();
        assert!(err.contains(why), "{:.60}: {err}", text);
        assert!(!err.contains('\u{7}') && e.doc.anims == before);
    }
    assert!(e.exec("anim_import_json", &json!({"path": tmp("missing.json")})).is_err());
    assert!(e.exec("anim_import_json", &json!({"path": p})).unwrap_err().contains(".json"), "only .json files are read");
    let big = tmp("big.json");
    std::fs::write(&big, vec![b' '; (8 << 20) + 1]).unwrap();
    assert!(e.exec("anim_import_json", &json!({"path": big})).is_err());
    std::fs::remove_file(&big).ok();
    // The limit counts what the document would end up with.
    assert!(full.exec("anim_import_json", &json!({"path": file})).is_err() && full.doc.anims.len() == Anim::MAX);

    // Scripts can do all of it.
    let out = script::run_lua(&mut e, "jm.anim_frames{ name = 'lua', frames = { {0, 0, 8, 8}, {8, 0, 8, 8, 1, 1} } } local n = 0 for _, a in ipairs(jm.anim_list().animations) do n = n + a.frames end return n");
    assert_eq!((out.error, out.result), (None, json!(4 + 5 + 3 + 2)));
}

#[test]
fn animation_keys_apply_over_its_tab_only() {
    let ctx = egui::Context::default();
    let (mut e, mut g) = (Engine::new("gui"), Gui::new());
    e.exec("new_document", &json!({"width": 64, "height": 64, "force": true})).unwrap();
    let mut time = 0.0;
    let mut frame = |e: &mut Engine, g: &mut Gui, events: Vec<Event>| {
        time += 1.0 / 60.0;
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 800.0))), time: Some(time), events, ..Default::default() };
        let _ = ctx.run(raw, |ctx| {
            crate::app::shortcuts(ctx, e, g);
            egui::SidePanel::right("anim").exact_width(300.0).show(ctx, |ui| crate::panels::animation(ui, e, g));
            egui::CentralPanel::default().show(ctx, |ui| canvas::show(ui, e, g));
        });
    };
    let key = |key: egui::Key| vec![Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }];
    let (over_tab, over_canvas) = (Event::PointerMoved(pos2(850.0, 400.0)), Event::PointerMoved(pos2(300.0, 400.0)));
    let sel_x = |e: &Engine| e.doc.sel_bounds().unwrap().0;

    e.exec("select_rect", &json!({"x": 8, "y": 8, "w": 32, "h": 16})).unwrap();
    // No animation yet: over the tab or not, the arrows nudge the selection as they always did.
    frame(&mut e, &mut g, vec![over_tab.clone()]);
    frame(&mut e, &mut g, key(egui::Key::ArrowRight));
    assert_eq!(sel_x(&e), 9);

    crate::panels::animate_selection(&mut e, &mut g);
    let a = e.anim().unwrap().grid().unwrap();
    assert_eq!((a.x, a.y, a.w, a.h, a.count()), (9, 8, 32, 16, 2), "the selection is the strip; frames are grid sized");
    assert_eq!(g.show_tab, Some(crate::app::Tab::Anim));
    e.exec("anim_set", &json!({"frame_w": 8, "frame_h": 8, "fps": 10})).unwrap();
    assert!(e.view.anim_playing);

    // Playing advances with time, at the chosen speed.
    frame(&mut e, &mut g, vec![]);
    let start = e.view.anim_frame;
    for _ in 0..60 {
        frame(&mut e, &mut g, vec![]);
    }
    assert_eq!((e.view.anim_frame + 8 - start) % 8, 10 % 8, "a second at 10 fps is ten frames");

    // Over the tab: Space pauses and plays, Left / Right step (and pause); the selection stays put.
    frame(&mut e, &mut g, key(egui::Key::Space));
    assert!(!e.view.anim_playing);
    let f = e.view.anim_frame;
    frame(&mut e, &mut g, key(egui::Key::ArrowRight));
    frame(&mut e, &mut g, key(egui::Key::ArrowRight));
    assert_eq!(e.view.anim_frame, (f + 2) % 8);
    frame(&mut e, &mut g, key(egui::Key::ArrowLeft));
    assert_eq!(e.view.anim_frame, (f + 1) % 8);
    assert_eq!(sel_x(&e), 9, "the arrows did not nudge the selection");
    for _ in 0..30 {
        frame(&mut e, &mut g, vec![]);
    }
    assert_eq!(e.view.anim_frame, (f + 1) % 8, "paused means paused");
    frame(&mut e, &mut g, key(egui::Key::Space));
    assert!(e.view.anim_playing);
    frame(&mut e, &mut g, key(egui::Key::ArrowLeft));
    assert!(!e.view.anim_playing, "stepping stops playback");
    // Shift + arrow steps as well, rather than falling through to the 8 px selection nudge.
    let f = e.view.anim_frame;
    frame(&mut e, &mut g, vec![Event::Key { key: egui::Key::ArrowRight, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::SHIFT }]);
    assert_eq!((e.view.anim_frame, sel_x(&e)), ((f + 1) % 8, 9));
    // The frame lines on the canvas are a view setting.
    e.exec("set_view", &json!({"anim_guides": false})).unwrap();
    assert_eq!(e.exec("get_view", &json!({})).unwrap()["anim_guides"], false);
    frame(&mut e, &mut g, vec![]);
    e.exec("set_view", &json!({"anim_guides": true})).unwrap();

    // Over the canvas the same keys keep their usual jobs.
    let f = e.view.anim_frame;
    frame(&mut e, &mut g, vec![over_canvas]);
    frame(&mut e, &mut g, key(egui::Key::ArrowRight));
    frame(&mut e, &mut g, key(egui::Key::Space));
    assert_eq!((sel_x(&e), e.view.anim_frame, e.view.anim_playing), (10, f, false));

    // The Deluxe Paint preset leaves these keys alone too.
    e.view.keymap = Keymap::DPaint;
    frame(&mut e, &mut g, vec![over_tab]);
    let mut space = key(egui::Key::Space);
    space.push(Event::Text(" ".into()));
    frame(&mut e, &mut g, space);
    assert!(e.view.anim_playing);
    frame(&mut e, &mut g, key(egui::Key::ArrowRight));
    assert_eq!((e.view.anim_frame, sel_x(&e)), ((f + 1) % 8, 10));
}

// ---------------------------------------------------------------- open recent

#[test]
fn true_colour_is_a_separate_kind_of_document() {
    use crate::doc::*;
    use crate::io::{self, PngPixels};
    let path = |n: &str| std::path::PathBuf::from(tmp(n));
    let px = |e: &mut Engine, x: i64, y: i64| e.exec("get_pixel", &json!({"x": x, "y": y})).unwrap()["color"].as_str().unwrap_or("?").to_string();

    // A picture with opaque, part-transparent and fully transparent pixels, and far more than 256 colours.
    let (w, h) = (40u32, 30u32);
    let src: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 6) as u8, (i / w * 8) as u8, (i * 7 % 251) as u8, if i % 11 == 0 { 0 } else if i % 5 == 0 { 97 } else { 255 }]).collect();
    let png = tmp("tc_src.png");
    std::fs::write(&png, io::encode_png_rgba(w, h, &src.concat()).unwrap()).unwrap();

    // Opened the usual way it is still an indexed document, and the answer says colours were given up.
    let mut e = Engine::new("headless");
    let v = e.exec("open_document", &json!({"path": png})).unwrap();
    assert_eq!((e.doc.mode, v["mode"].as_str(), v["converted_to_indexed"].as_bool()), (ColorMode::Indexed, Some("indexed"), Some(true)));

    // Asked for by name, every pixel is kept exactly.
    let v = e.exec("open_document", &json!({"path": png, "mode": "truecolor", "force": true})).unwrap();
    assert_eq!((e.doc.mode, v["mode"].as_str(), v["converted_to_indexed"].as_bool()), (ColorMode::Rgba, Some("truecolor"), Some(false)));
    let want: Vec<Cell> = src.iter().map(|&c| pack(c)).collect();
    assert_eq!(*e.doc.layers[0].data, want);
    assert_eq!((want[0], want[5], px(&mut e, 0, 0), px(&mut e, 5, 0)), (TRANSP, pack(src[5]), "transparent".to_string(), hex8(src[5])));
    assert!(want.iter().all(|&c| c == TRANSP || c >= 1 << 24), "a colour is never mistaken for an index");

    // It exports as an RGBA PNG holding the same pixels (transparent ones as 0,0,0,0), scaled or not.
    let out = tmp("tc_out.png");
    e.exec("export_png", &json!({"path": out})).unwrap();
    let PngPixels::Rgba(back) = io::load_png(&path("tc_out.png")).unwrap().pixels else { panic!("not an RGBA PNG") };
    let clean: Vec<[u8; 4]> = src.iter().map(|&c| if c[3] == 0 { [0; 4] } else { c }).collect();
    assert_eq!(back, clean);
    e.exec("export_png", &json!({"path": tmp("tc_out2.png"), "scale": 2})).unwrap();
    let big = io::load_png(&path("tc_out2.png")).unwrap();
    let PngPixels::Rgba(bp) = big.pixels else { panic!("not an RGBA PNG") };
    assert_eq!((big.w, big.h, bp[(2 * 3 * 80 + 2 * 7) as usize]), (80, 60, clean[3 * 40 + 7]));

    // The project file: a different first six bytes, so builds that only know indexed documents
    // refuse it instead of converting it; indexed projects keep the marker they always had.
    let jmn = tmp("tc.jmn");
    e.exec("anim_set", &json!({"name": "a", "x": 0, "y": 0, "w": 40, "h": 10, "frame_w": 10, "frame_h": 10})).unwrap();
    e.exec("save_document", &json!({"path": jmn})).unwrap();
    assert_eq!(&std::fs::read(&jmn).unwrap()[..6], b"JMNG\x02\n");
    let saved = e.doc.clone();
    e.exec("open_document", &json!({"path": jmn})).unwrap();
    assert_eq!((e.doc.mode, &e.doc.layers, &e.doc.palette, e.doc.anims.len()), (ColorMode::Rgba, &saved.layers, &saved.palette, 1));
    assert!(e.exec("open_document", &json!({"path": jmn, "mode": "indexed"})).unwrap_err().contains("'mode' is for PNG files"));
    let mut plain = Engine::new("headless");
    plain.exec("save_document", &json!({"path": tmp("tc_indexed.jmn")})).unwrap();
    assert_eq!(&std::fs::read(tmp("tc_indexed.jmn")).unwrap()[..6], b"JMNG\x01\n");
    // A true-colour file that holds an indexed document is damaged, not something to guess at.
    let mut bytes = std::fs::read(tmp("tc_indexed.jmn")).unwrap();
    bytes[4] = 2;
    std::fs::write(tmp("tc_bad.jmn"), bytes).unwrap();
    assert!(io::load_doc(&path("tc_bad.jmn")).unwrap_err().contains("corrupt"));
    // Values that are neither a colour nor transparent are read as transparent.
    let mut odd = Doc::new_rgba(2, 1, Palette::preset("pico8").unwrap());
    odd.layers[0].data = std::sync::Arc::new(vec![7, pack([1, 2, 3, 4])]);
    io::save_doc(&odd, &path("tc_odd.jmn")).unwrap();
    assert_eq!(*io::load_doc(&path("tc_odd.jmn")).unwrap().layers[0].data, vec![TRANSP, pack([1, 2, 3, 4])]);

    // Painting sets exactly the colour asked for; a number names a swatch.
    let mut e = Engine::new("headless");
    e.exec("new_document", &json!({"width": 16, "height": 16, "mode": "truecolor", "palette": "pico8"})).unwrap();
    e.exec("draw_rect", &json!({"x": 0, "y": 0, "w": 8, "h": 8, "color": "#10203040", "filled": true})).unwrap();
    e.exec("draw_rect", &json!({"x": 8, "y": 0, "w": 8, "h": 8, "color": 8, "filled": true})).unwrap();
    assert_eq!((px(&mut e, 3, 3), px(&mut e, 9, 3), px(&mut e, 3, 12)), ("#10203040".to_string(), "#ff004dff".to_string(), "transparent".to_string()));
    e.exec("fill", &json!({"x": 3, "y": 3, "color": [1, 2, 3]})).unwrap();
    e.exec("set_pixels", &json!({"pixels": [[0, 15, "#ffffff01"], [1, 15, -1]]})).unwrap();
    assert_eq!((px(&mut e, 7, 7), px(&mut e, 0, 15), px(&mut e, 1, 15)), ("#010203ff".to_string(), "#ffffff01".to_string(), "transparent".to_string()));
    e.exec("draw_rect", &json!({"x": 8, "y": 0, "w": 2, "h": 2, "mode": "darken", "filled": true})).unwrap();
    assert!(px(&mut e, 8, 0) != "#ff004dff" && px(&mut e, 8, 0).ends_with("ff") && px(&mut e, 11, 0) == "#ff004dff", "darken changes the colour itself");
    e.exec("gradient", &json!({"x0": 0, "y0": 12, "x1": 15, "y1": 12, "colors": ["#000000", "#ffffff"]})).unwrap();
    let row: std::collections::HashSet<Cell> = (0..16).map(|x| e.doc.layers[0].data[12 * 16 + x]).collect();
    assert!(row.len() > 8, "a true-colour gradient blends: {} colours", row.len());
    e.exec("set_colors", &json!({"fg": 12, "bg": "#11223344"})).unwrap();
    let v = e.exec("get_info", &json!({})).unwrap();
    assert_eq!((v["fg"].as_str(), v["bg"].as_str()), (Some("#29adffff"), Some("#11223344")));
    e.exec("draw_rect", &json!({"x": 0, "y": 0, "w": 1, "h": 1, "color": "transparent", "filled": true})).unwrap();
    assert_eq!(e.doc.layers[0].data[0], TRANSP);

    // The palette is a set of swatches: editing it never changes a pixel, and commands that
    // renumber or count pixels are refused rather than doing nothing quietly.
    let before = e.doc.layers[0].data.clone();
    for (cmd, p) in [("palette_set", json!({"index": 8, "color": "#00ff00"})), ("palette_sort", json!({})), ("palette_swap", json!({"a": 1, "b": 2})),
                     ("palette_load_preset", json!({"name": "gameboy"})), ("palette_resize", json!({"count": 2})), ("palette_cleanup", json!({}))] {
        e.exec(cmd, &p).unwrap_or_else(|err| panic!("{cmd}: {err}"));
        assert!(e.doc.layers[0].data == before, "{cmd} changed the picture");
    }
    for (cmd, p) in [("palette_cleanup", json!({"unused": true})), ("palette_usage", json!({})), ("reduce_colors", json!({"colors": 4}))] {
        assert!(e.exec(cmd, &p).unwrap_err().contains("indexed document"), "{cmd}");
    }
    assert_eq!(e.exec("get_info", &json!({})).unwrap()["fg"], "#29adffff", "the colours in hand are colours, not swatch numbers");

    // Layers blend where a pixel is part transparent, on screen, when flattened and when merged.
    let mut e = Engine::new("headless");
    e.exec("new_document", &json!({"width": 2, "height": 1, "mode": "truecolor"})).unwrap();
    e.exec("set_pixels", &json!({"pixels": [[0, 0, "#0000ffff"], [1, 0, "#0000ffff"]]})).unwrap();
    e.exec("layer_add", &json!({})).unwrap();
    e.exec("set_pixels", &json!({"pixels": [[0, 0, "#ff000080"]]})).unwrap();
    let shown = e.doc.composite(&[], None, false);
    assert_eq!((&shown[..4], &shown[4..]), (&[128u8, 0, 127, 255][..], &[0u8, 0, 255, 255][..]));
    assert_eq!(e.exec("get_pixel", &json!({"x": 0, "y": 0, "merged": true})).unwrap()["color"], "#80007fff");
    e.exec("layer_merge_down", &json!({})).unwrap();
    assert_eq!((e.doc.layers.len(), px(&mut e, 0, 0), px(&mut e, 1, 0)), (1, "#80007fff".to_string(), "#0000ffff".to_string()));

    // convert_mode is the only thing that changes a document's kind, and it is one undo step.
    let mut e = eng(16, 16);
    e.exec("draw_rect", &json!({"x": 2, "y": 2, "w": 6, "h": 6, "color": 40, "filled": true})).unwrap();
    e.exec("set_colors", &json!({"fg": 40, "bg": 3})).unwrap();
    let (look, cells, pal) = (e.doc.composite(&e.doc.palette.colors, None, false), e.doc.layers[0].data.clone(), e.doc.palette.clone());
    assert_eq!(e.exec("convert_mode", &json!({"mode": "truecolor"})).unwrap()["mode"], "truecolor");
    assert_eq!((e.doc.mode, e.doc.composite(&[], None, false) == look, e.o.fg, &e.doc.palette), (ColorMode::Rgba, true, pack(pal.rgba(40)), &pal));
    assert!(e.exec("convert_mode", &json!({"mode": "truecolor"})).unwrap_err().contains("already"));
    e.exec("undo", &json!({})).unwrap();
    assert_eq!((e.doc.mode, &e.doc.layers[0].data, e.o.fg, e.o.bg), (ColorMode::Indexed, &cells, 40, 3), "undo brings back the indexed document, indices and all");
    e.exec("redo", &json!({})).unwrap();
    e.exec("draw_rect", &json!({"x": 0, "y": 0, "w": 1, "h": 1, "color": "#abcdef", "filled": true})).unwrap();
    let v = e.exec("convert_mode", &json!({"mode": "indexed", "colors": 16})).unwrap();
    assert_eq!((v["mode"].as_str(), e.doc.mode, v["palette_count"].as_i64()), (Some("indexed"), ColorMode::Indexed, Some(2)));
    assert_eq!((e.doc.to_rgba(e.doc.layers[0].data[0]), e.doc.to_rgba(e.doc.layers[0].data[3 * 16 + 3]), e.doc.layers[0].data[255]), ([0xab, 0xcd, 0xef, 255], pal.rgba(40), TRANSP));
    assert!(e.doc.layers[0].data.iter().all(|&c| c <= TRANSP) && e.o.fg < 256 && e.o.bg < 256);

    // A picture with thousands of colours and transparent pixels, made indexed, leaves room
    // for transparency in its palette, so it can always be exported.
    let mut e = Engine::new("headless");
    e.exec("open_document", &json!({"path": png, "mode": "truecolor"})).unwrap();
    assert_eq!(e.exec("get_pixel", &json!({"x": 7, "y": 3, "merged": true})).unwrap()["color"], hex8(src[3 * 40 + 7]));
    let v = e.exec("convert_mode", &json!({"mode": "indexed"})).unwrap();
    assert!(v["palette_count"].as_i64().is_some_and(|n| n > 200 && n <= 255), "{v}");
    e.exec("export_png", &json!({"path": tmp("tc_conv.png")})).unwrap();
    assert!(matches!(io::load_png(&path("tc_conv.png")).unwrap().pixels, PngPixels::Indexed { .. }));

    // Tabs: each document is its own kind, the tab list says which, and copy / paste between
    // the two keeps the look (exactly into true colour, nearest palette colour into indexed).
    let mut e = eng(8, 8);
    e.exec("draw_rect", &json!({"x": 0, "y": 0, "w": 4, "h": 4, "color": 40, "filled": true})).unwrap();
    let c40 = e.doc.palette.rgba(40);
    e.exec("copy", &json!({})).unwrap();
    e.exec("tab_new", &json!({"width": 8, "height": 8, "mode": "truecolor"})).unwrap();
    assert_eq!(e.tab_list().iter().map(|t| t["mode"].as_str().unwrap_or("?").to_string()).collect::<Vec<_>>(), ["indexed", "truecolor"]);
    e.exec("paste", &json!({"x": 0, "y": 0})).unwrap();
    assert_eq!((e.doc.layers[0].data[0], e.doc.layers[0].data[63]), (pack(c40), TRANSP));
    e.exec("set_pixels", &json!({"pixels": [[7, 7, "#fefefe"], [6, 7, "#00000010"]]})).unwrap();
    e.exec("select_all", &json!({})).unwrap();
    e.exec("copy", &json!({})).unwrap();
    e.exec("tab_select", &json!({"id": 1})).unwrap();
    e.exec("layer_add", &json!({})).unwrap();
    e.exec("paste", &json!({"x": 0, "y": 0})).unwrap();
    let d = &e.doc.layers[1].data;
    assert_eq!((d[0], d[63], d[62], e.doc.mode), (40, e.doc.palette.nearest([0xfe, 0xfe, 0xfe]) as Cell, TRANSP, ColorMode::Indexed));
    assert!(d.iter().all(|&c| c <= TRANSP), "no colour leaks into an indexed document");
    let v = e.exec("tab_open", &json!({"path": png, "mode": "truecolor"})).unwrap();
    assert_eq!((v["converted_to_indexed"].as_bool(), e.doc.mode, e.doc.w), (Some(false), ColorMode::Rgba, 40));
    // A PNG added as a layer keeps its exact pixels in a true-colour document.
    e.exec("import_png_layer", &json!({"path": png})).unwrap();
    assert_eq!(*e.doc.layers[1].data, want);
    // An indexed PNG opened in true colour brings its palette as swatches.
    plain.exec("draw_rect", &json!({"x": 0, "y": 0, "w": 4, "h": 4, "color": 20, "filled": true})).unwrap();
    plain.exec("export_png", &json!({"path": tmp("tc_idx.png")})).unwrap();
    assert!(matches!(io::load_png(&path("tc_idx.png")).unwrap().pixels, PngPixels::Indexed { .. }), "indexed documents still export indexed PNGs");
    let c20 = plain.doc.palette.rgba(20);
    plain.exec("open_document", &json!({"path": tmp("tc_idx.png"), "mode": "truecolor", "force": true})).unwrap();
    assert_eq!((plain.doc.mode, plain.doc.layers[0].data[0], plain.doc.layers[0].data[5], plain.doc.palette.colors[20]), (ColorMode::Rgba, pack(c20), TRANSP, [c20[0], c20[1], c20[2]]));
}

#[test]
fn recent_files_and_settings() {
    use crate::app::{read_settings, request, use_dropped, write_settings, Dialog, Pending, RECENT_MAX};
    use std::path::{Path, PathBuf};
    let abs = |name: &str| PathBuf::from(tmp(name));

    // Newest first, no duplicates, capped; relative paths are stored absolute.
    let mut g = Gui::new();
    for i in 0..RECENT_MAX + 3 {
        g.remember(&abs(&format!("r{i}.jmn")));
    }
    assert_eq!(g.recent.len(), RECENT_MAX);
    assert_eq!(g.recent[0], abs(&format!("r{}.jmn", RECENT_MAX + 2)));
    g.remember(&abs("r5.jmn"));
    assert_eq!((g.recent[0].clone(), g.recent.len()), (abs("r5.jmn"), RECENT_MAX));
    assert_eq!(g.recent.iter().filter(|p| **p == abs("r5.jmn")).count(), 1);
    g.remember(Path::new("relative.jmn"));
    assert!(g.recent[0].is_absolute() && g.recent_dirty);

    // Writing keeps keys this build does not know, and one setting does not wipe the other.
    let file = abs("settings.json");
    std::fs::write(&file, r#"{"keymap":"standard","future":{"x":1}}"#).unwrap();
    write_settings(&file, Keymap::DPaint, Some(&g.recent)).unwrap();
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!((&v["keymap"], &v["future"]["x"], v["recent"].as_array().unwrap().len()), (&json!("dpaint"), &json!(1), RECENT_MAX));
    assert_eq!(read_settings(&file), (Some(Keymap::DPaint), g.recent.clone()));
    write_settings(&file, Keymap::Standard, None).unwrap();
    assert_eq!(read_settings(&file), (Some(Keymap::Standard), g.recent.clone()), "changing the preset keeps the list");
    assert!(!file.with_extension(format!("json.{}.tmp", std::process::id())).exists(), "written beside the file, then renamed over it");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600, "the list of file names is private");
    }

    // The file is the user's to edit: whatever is wrong in it is ignored.
    let (a, b) = (abs("a.jmn"), abs("b.png"));
    let (sa, sb) = (a.to_str().unwrap(), b.to_str().unwrap());
    let long = format!("/{}", "x".repeat(5000));
    let many: Vec<String> = (0..500).map(|i| abs(&format!("m{i}.jmn")).display().to_string()).collect();
    for (text, keymap, recent) in [
        (json!({"keymap": 7, "recent": [sa, 3, null, {"p": 1}, "relative.jmn", "", sa, long, "/tmp/new\nline.jmn", "/tmp/\u{202e}gnp.exe", "/tmp/nul\u{0}.jmn", sb]}).to_string(), None, vec![a.clone(), b.clone()]),
        (json!({"recent": "not a list"}).to_string(), None, vec![]),
        (json!({"keymap": "dpaint", "recent": {"0": sa}}).to_string(), Some(Keymap::DPaint), vec![]),
        (json!({"recent": many}).to_string(), None, many[..RECENT_MAX].iter().map(PathBuf::from).collect()),
        (json!([sa, sb]).to_string(), None, vec![]),
        ("{ not json".to_string(), None, vec![]),
        (String::new(), None, vec![]),
    ] {
        std::fs::write(&file, &text).unwrap();
        assert_eq!(read_settings(&file), (keymap, recent), "{}", &text[..text.len().min(60)]);
    }
    std::fs::write(&file, vec![b' '; (1 << 20) + 1]).unwrap();
    assert_eq!(read_settings(&file), (None, vec![]), "an oversized file is not read");
    // A file that is not a JSON object is replaced rather than left broken.
    std::fs::write(&file, "[1, 2]").unwrap();
    write_settings(&file, Keymap::Standard, Some(&[a.clone()])).unwrap();
    assert_eq!(read_settings(&file), (Some(Keymap::Standard), vec![a.clone()]));
    // Whatever is at the path is never written through or re-moded: a link is replaced by a
    // real file, a directory is left alone, and a file the user made read-only stays as it is.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777;
        let (target, link, dir, locked) = (abs("link_target.txt"), abs("link.json"), abs("dir.json"), abs("locked.json"));
        std::fs::write(&target, "precious").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&target, &link).unwrap();
        write_settings(&link, Keymap::DPaint, Some(&[a.clone()])).unwrap();
        assert_eq!((std::fs::read_to_string(&target).unwrap().as_str(), mode(&target)), ("precious", 0o644));
        assert!(!std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink() && read_settings(&link).0 == Some(Keymap::DPaint));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(write_settings(&dir, Keymap::DPaint, None).is_err());
        assert!(mode(&dir) == 0o755 && !dir.with_extension(format!("json.{}.tmp", std::process::id())).exists());
        let _ = std::fs::remove_file(&locked);
        std::fs::write(&locked, r#"{"keymap":"standard"}"#).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o400)).unwrap();
        write_settings(&locked, Keymap::DPaint, Some(&[a.clone()])).unwrap();
        assert_eq!((read_settings(&locked), mode(&locked)), ((Some(Keymap::Standard), vec![]), 0o400));
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    // One entry per file however it is spelled, and names that would display deceptively are not listed.
    let mut g2 = Gui::new();
    std::fs::write(&a, b"x").unwrap();
    g2.remember(&a);
    g2.remember(&a.parent().unwrap().join("..").join(a.parent().unwrap().file_name().unwrap()).join("a.jmn"));
    g2.remember(Path::new("/tmp/\u{202e}gnp.exe"));
    g2.remember(Path::new("/tmp/two\nlines.jmn"));
    #[cfg(unix)]
    assert_eq!(g2.recent, vec![std::fs::canonicalize(&a).unwrap()]);
    assert_eq!(read_settings(&abs("no_such_settings.json")), (None, vec![]));

    // Opening from the list puts the file in its own tab, so unsaved work is never in the way.
    let mut e = eng(16, 16);
    e.exec("draw_rect", &json!({"x": 1, "y": 1, "w": 1, "h": 1, "color": 9, "filled": true})).unwrap();
    e.exec("save_document", &json!({"path": sa})).unwrap();
    e.exec("new_document", &json!({"width": 8, "height": 8, "force": true})).unwrap();
    e.exec("draw_rect", &json!({"x": 0, "y": 0, "w": 1, "h": 1, "color": 3, "filled": true})).unwrap();
    let mut g = Gui::new();
    let widths = |e: &Engine| e.tab_list().iter().map(|t| t["width"].as_i64().unwrap()).collect::<Vec<_>>();
    assert!(e.modified);
    request(&mut e, &mut g, Pending::OpenFile(a.clone()));
    let real = if cfg!(unix) { std::fs::canonicalize(&a).unwrap() } else { a.clone() };
    assert!(g.pending.is_none() && widths(&e) == [8, 16] && e.doc.w == 16 && pixel(&mut e, 1, 1) == 9 && !e.modified, "opened beside the unsaved document");
    assert!(e.parked[0].modified && e.parked[0].doc.w == 8, "which is untouched");
    assert_eq!(g.recent, vec![real.clone()], "and remembered");
    // The same project again shows its tab instead of loading a second copy.
    e.exec("tab_select", &json!({"id": 1})).unwrap();
    request(&mut e, &mut g, Pending::OpenFile(a.clone()));
    assert!(widths(&e) == [8, 16] && e.doc.w == 16 && g.status.contains("already open"));
    // A file that has gone is an error in the status bar; nothing else changes.
    request(&mut e, &mut g, Pending::OpenFile(abs("gone.jmn")));
    assert!(g.status_err && e.doc.w == 16 && widths(&e) == [8, 16] && g.recent == vec![real]);
    e.exec("tab_close", &json!({"id": 1, "force": true})).unwrap();

    // Dropping a project on the window is the same request; a dropped PNG asks whether to open
    // it or add it as a layer.
    let drop = |e: &mut Engine, g: &mut Gui, paths: &[&Path]| {
        let dropped_files = paths.iter().map(|p| egui::DroppedFile { path: Some(p.to_path_buf()), ..Default::default() }).collect();
        let _ = egui::Context::default().run(egui::RawInput { dropped_files, ..Default::default() }, |ctx| crate::app::dropped(ctx, e, g));
    };
    let real = g.recent.clone();
    let png = abs("dropped.PNG");
    let mut e = eng(12, 6);
    e.exec("export_png", &json!({"path": png.to_str().unwrap()})).unwrap();
    e.exec("draw_rect", &json!({"x": 0, "y": 0, "w": 1, "h": 1, "color": 3, "filled": true})).unwrap();
    g.info("");
    drop(&mut e, &mut g, &[]);
    assert!(g.pending.is_none() && !g.status_err && e.doc.w == 12, "no drop, nothing happens");
    // While a question is on screen a drop is refused.
    g.pending = Some(Pending::Quit);
    drop(&mut e, &mut g, &[&a]);
    assert!(g.status_err && matches!(g.pending, Some(Pending::Quit)) && widths(&e) == [12], "a drop during a prompt is refused");
    assert!(matches!(g.dialog, Dialog::None));
    g.pending = None;
    for (bad, why) in [(vec![a.as_path(), png.as_path()], "one file at a time"), (vec![Path::new("/tmp/notes.txt")], "none of them"), (vec![Path::new("/tmp")], "none of them"), (vec![Path::new("/tmp/\u{202e}gnp.jmn")], "cannot be shown")] {
        g.info("");
        drop(&mut e, &mut g, &bad);
        assert!(g.status_err && g.status.contains(why) && widths(&e) == [12] && g.pending.is_none() && g.recent == real, "{bad:?}: {}", g.status);
        assert!(!g.status.contains('\u{202e}'));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        g.info("");
        drop(&mut e, &mut g, &[Path::new(std::ffi::OsStr::from_bytes(b"/tmp/\xff.jmn"))]);
        assert!(g.status_err && g.status.contains("cannot be shown") && widths(&e) == [12]);
    }
    // A PNG only asks; nothing changes until the user picks.
    g.info("");
    drop(&mut e, &mut g, &[&png]);
    assert!(matches!(&g.dialog, Dialog::Dropped(p) if *p == png) && g.pending.is_none() && !g.status_err && e.doc.layers.len() == 1 && g.recent == real);
    drop(&mut e, &mut g, &[&a]);
    assert!(g.status_err && matches!(&g.dialog, Dialog::Dropped(p) if *p == png) && widths(&e) == [12], "a drop while a dialog is open is refused");
    g.dialog = Dialog::None;
    // As a layer: added to this document, one undo step, and not listed as an opened file.
    g.info("");
    use_dropped(&mut e, &mut g, &png, true);
    assert!(!g.status_err && e.doc.layers.len() == 2 && e.doc.w == 12 && e.modified && g.recent == real, "{}", g.status);
    e.exec("undo", &json!({})).unwrap();
    assert_eq!(e.doc.layers.len(), 1);
    use_dropped(&mut e, &mut g, &abs("gone.png"), true);
    assert!(g.status_err && e.doc.layers.len() == 1);
    // As a document: its own tab, and remembered.
    g.info("");
    use_dropped(&mut e, &mut g, &png, false);
    assert!(!g.status_err && g.pending.is_none() && widths(&e) == [12, 12] && e.doc.h == 6 && !e.modified && e.path.is_none(), "{}", g.status);
    assert_eq!(g.recent.len(), 2, "a dropped file is remembered like any other the user opened");
    drop(&mut e, &mut g, &[&a]);
    assert_eq!((widths(&e), e.doc.w, pixel(&mut e, 1, 1), g.recent.len()), (vec![12, 12, 16], 16, 9, 2), "a project opens straight away, in a third tab");
    // An untouched blank document is not kept around: the first file opened takes its place.
    let mut blank = Engine::new("headless");
    request(&mut blank, &mut g, Pending::OpenFile(a.clone()));
    assert!(widths(&blank) == [16] && blank.id == 1);

    // The menu lists file names, tells same-named files apart by folder, and offers Clear.
    let other = std::env::temp_dir().join("jmn_recent_other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("a.jmn"), b"x").unwrap();
    g.recent = vec![a.clone(), other.join("a.jmn"), b.clone()];
    let ctx = egui::Context::default();
    let mut texts = vec![];
    for _ in 0..2 {
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 800.0))), ..Default::default() };
        let out = ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| crate::app::recent_menu(ui, &mut e, &mut g));
        });
        fn walk(s: &egui::Shape, out: &mut Vec<String>) {
            match s {
                egui::Shape::Text(t) => out.push(t.galley.text().to_string()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        texts.clear();
        out.shapes.iter().for_each(|c| walk(&c.shape, &mut texts));
    }
    // (Long folders are shortened in the middle; the hover text has the whole path.)
    let twin = |end: &str| texts.iter().any(|t| t.starts_with("a.jmn  -  ") && t.ends_with(end) && t.chars().count() <= 10 + 64);
    assert!(twin(a.parent().unwrap().file_name().unwrap().to_str().unwrap()) && twin("jmn_recent_other"), "{texts:?}");
    assert!(texts.contains(&"b.png".to_string()) && texts.contains(&"Clear Recent".to_string()), "{texts:?}");
    std::fs::remove_dir_all(other).ok();
    g.recent = vec![a.clone()];

    // The list is window state: no command reports it.
    let all = format!("{} {} {}", e.exec("get_info", &json!({})).unwrap(), e.exec("get_view", &json!({})).unwrap(), e.exec("get_options", &json!({})).unwrap());
    assert!(!all.contains("recent"));
    assert!(crate::commands::registry().iter().all(|c| !c.name.contains("recent")));
}
