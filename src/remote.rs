//! MCP (Model Context Protocol) server.
//!
//! * The running app serves MCP over HTTP on 127.0.0.1, executing tool calls
//!   against the live document between the user's own actions.
//! * `jon_motion mcp` speaks MCP over stdio. It forwards tool calls to the
//!   running app when there is one, and otherwise works on a headless document.

use crate::commands::{self, Args};
use crate::engine::{guarded, Engine};
use crate::io;
use crate::script;
use serde_json::{json, Map, Value};
use std::io::{BufRead, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const BASE_PORT: u16 = 47800;

const INSTRUCTIONS: &str = "\
Jon Motion is a pixel art editor: layers, a 256-colour indexed palette, colour ramps, paint modes and tile maps.
Coordinates are pixels with (0,0) at the top-left. Layer 0 is the bottom layer.
Every pixel is a palette index; there is no true-colour mode and layers are fully opaque. \
Colours: a palette index (0-255, -1 = transparent) or '#rrggbb' (snapped to the nearest palette colour). 'fg', 'bg' and 'transparent' also work.
Workflow: get_info first; list_commands to discover commands; execute_command or batch to act; screenshot to look at the result. \
For anything loop-heavy or procedural use run_lua. put_pixels places a whole sprite in one call.
Every call is atomic and is one undo step. When the app window is open you are editing the user's live document alongside them: \
your calls run between their brush strokes, they see changes immediately and can undo them. \
Do not replace the document (new_document/open_document) or change set_options/set_view unless asked; pass paint parameters on each drawing command instead.";

pub struct ToolOut {
    pub content: Vec<Value>,
    pub is_error: bool,
}

fn text(s: impl Into<String>) -> ToolOut {
    ToolOut { content: vec![json!({"type": "text", "text": s.into()})], is_error: false }
}

fn fail(s: impl Into<String>) -> ToolOut {
    ToolOut { content: vec![json!({"type": "text", "text": s.into()})], is_error: true }
}

fn show(v: &Value) -> String {
    if v.is_null() {
        "ok".into()
    } else {
        v.to_string()
    }
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

pub fn tools() -> Value {
    json!([
        { "name": "get_info",
          "description": "Summary of the open document: size, layers, palette size, tile set, selection, current colours, file path, which of the open documents (tabs) it is, and whether you are attached to the live app ('gui') or a headless document. Several documents can be open: tools work on the active tab unless you pass 'document' (see the tab_* commands).",
          "inputSchema": { "type": "object", "properties": { "document": { "type": "integer", "description": "Work on this open document (an id from tab_list) instead of the active tab; the user's view does not change" } } } },
        { "name": "list_commands",
          "description": "Reference for the editor's commands (every feature of the app is a command). Call with no arguments for everything, or narrow it down.",
          "inputSchema": { "type": "object", "properties": {
              "category": { "type": "string", "enum": commands::CATEGORIES, "description": "Only this category" },
              "search": { "type": "string", "description": "Only commands whose name or description contains this text" },
              "command": { "type": "string", "description": "Full JSON schema for one command" } } } },
        { "name": "execute_command",
          "description": "Run one editor command, e.g. command='draw_rect', params={x:4,y:4,w:16,h:8,color:12,filled:true}. Atomic; one undo step.",
          "inputSchema": { "type": "object", "required": ["command"], "properties": {
              "command": { "type": "string", "description": "Command name from list_commands" },
              "params": { "type": "object", "description": "Named parameters for the command" },
              "document": { "type": "integer", "description": "Work on this open document (an id from tab_list) instead of the active tab; the user's view does not change" } } } },
        { "name": "batch",
          "description": "Run several commands in order as one atomic change and one undo step. If any command fails the document is rolled back (files already written by save/export commands stay).",
          "inputSchema": { "type": "object", "required": ["commands"], "properties": {
              "commands": { "type": "array", "description": "Commands to run in order", "items": { "type": "object", "required": ["command"], "properties": {
                  "command": { "type": "string" }, "params": { "type": "object" } } } },
              "document": { "type": "integer", "description": "Work on this open document (an id from tab_list) instead of the active tab; the user's view does not change" } } } },
        { "name": "run_lua",
          "description": format!("Run a Lua script against the document.\n{}", script::LUA_HELP),
          "inputSchema": { "type": "object", "required": ["code"], "properties": {
              "code": { "type": "string", "description": "Lua source" },
              "document": { "type": "integer", "description": "Work on this open document (an id from tab_list) instead of the active tab; the user's view does not change" } } } },
        { "name": "screenshot",
          "description": "Render the document (or a region or single layer) to a PNG image so you can see it.",
          "inputSchema": { "type": "object", "properties": {
              "x": { "type": "integer", "description": "Region left (default 0)" },
              "y": { "type": "integer", "description": "Region top (default 0)" },
              "w": { "type": "integer", "description": "Region width (default: to the right edge)" },
              "h": { "type": "integer", "description": "Region height (default: to the bottom edge)" },
              "scale": { "type": "integer", "description": "Integer upscale 1-16 (default: chosen so the image is roughly 512-1024 px)" },
              "layer": { "type": ["integer", "string"], "description": "Render only this layer (index or name)" },
              "background": { "type": "string", "enum": ["checker", "transparent"], "description": "What shows behind transparent pixels (default checker)" },
              "document": { "type": "integer", "description": "Work on this open document (an id from tab_list) instead of the active tab; the user's view does not change" } } } }
    ])
}

fn screenshot(e: &Engine, m: &Map<String, Value>) -> Result<ToolOut, String> {
    let a = Args(m);
    let d = &e.doc;
    let layer = if a.get("layer").is_some() { Some(commands::layer_arg(e, &a)?) } else { None };
    let (dw, dh) = (d.w as i64, d.h as i64);
    let (x, y) = (a.oi("x")?.unwrap_or(0).clamp(0, dw - 1), a.oi("y")?.unwrap_or(0).clamp(0, dh - 1));
    let (w, h) = (a.oi("w")?.unwrap_or(dw - x).clamp(1, dw - x), a.oi("h")?.unwrap_or(dh - y).clamp(1, dh - y));
    let checker = a.os("background")? != Some("transparent");
    let full = d.composite(&d.palette.colors, layer, checker);
    let mut crop = Vec::with_capacity((w * h * 4) as usize);
    for yy in y..y + h {
        crop.extend_from_slice(&full[((yy * dw + x) * 4) as usize..((yy * dw + x + w) * 4) as usize]);
    }
    let auto = (768 / w.max(h)).clamp(1, 16);
    let scale = a.oi("scale")?.unwrap_or(auto).clamp(1, 16).min((4096 / w.max(h)).max(1)) as usize;
    let big = io::upscale(&crop, w as usize, h as usize, scale);
    let png = io::encode_png_rgba((w as usize * scale) as u32, (h as usize * scale) as u32, &big)?;
    Ok(ToolOut {
        content: vec![
            json!({"type": "image", "data": base64(&png), "mimeType": "image/png"}),
            json!({"type": "text", "text": format!("region x={x} y={y} w={w} h={h} of the {dw}x{dh} document, shown at {scale}x")}),
        ],
        is_error: false,
    })
}

/// Execute one MCP tool call against an engine: on the active document, or on the one the
/// caller names with `document`, which is put back behind the active tab afterwards.
pub fn call_tool(e: &mut Engine, name: &str, args: &Value) -> ToolOut {
    let empty = Map::new();
    let m = args.as_object().unwrap_or(&empty);
    let on_document = name != "list_commands";
    let wanted = match m.get("document").filter(|v| !v.is_null() && on_document) {
        None => None,
        Some(v) => match v.as_f64().filter(|f| f.fract() == 0.0 && *f >= 0.0 && *f <= u32::MAX as f64) {
            Some(f) => Some(f as u32),
            None => return fail("'document' must be the id of an open document (see tab_list)"),
        },
    };
    let tab_command = name == "execute_command"
        && m.get("command").and_then(|v| v.as_str()).and_then(commands::find).is_some_and(|c| c.kind == commands::Kind::Tab || c.name == "tab_list");
    if let Some(id) = wanted.filter(|&id| id != e.id) {
        if tab_command {
            return fail("tab commands say which document they mean with their own 'id'; leave 'document' out");
        }
        return e.on_tab(id, |e| tool(e, name, m)).unwrap_or_else(fail);
    }
    let mut out = tool(e, name, m);
    // With several documents open and none named, say which one this was, so a client is
    // never moved from one document to another without being told.
    if on_document && wanted.is_none() && !tab_command && e.tab_list().len() > 1 {
        out.content.push(json!({"type": "text", "text": format!(
            "(worked on document {} \"{}\", the active tab of {}; pass \"document\": {} to stay on it if the user changes tabs)", e.id, e.title(), e.tab_list().len(), e.id)}));
    }
    out
}

fn tool(e: &mut Engine, name: &str, m: &Map<String, Value>) -> ToolOut {
    let r: Result<ToolOut, String> = guarded(|| match name {
        "get_info" => Ok(text(commands::info(e).to_string())),
        "list_commands" => {
            if let Some(c) = m.get("command").and_then(|v| v.as_str()) {
                let cmd = commands::find(c).ok_or_else(|| format!("unknown command '{c}'"))?;
                return Ok(text(serde_json::to_string_pretty(&cmd.schema()).unwrap_or_default()));
            }
            let cat = m.get("category").and_then(|v| v.as_str());
            let search = m.get("search").and_then(|v| v.as_str());
            Ok(text(format!(
                "Parameters marked ? are optional. Types: int, num, bool, str, array, color (palette index | '#rrggbb' | 'transparent' | 'fg' | 'bg'), layer (index or name).\n{}",
                commands::reference(cat, search)
            )))
        }
        "execute_command" => {
            let c = m.get("command").and_then(|v| v.as_str()).ok_or("missing 'command'")?;
            e.exec(c, m.get("params").unwrap_or(&Value::Null)).map(|v| text(show(&v)))
        }
        "batch" => {
            let cmds = m.get("commands").and_then(|v| v.as_array()).ok_or("missing 'commands' array")?;
            let results = e.group(|e| {
                let mut out = vec![];
                for (i, c) in cmds.iter().enumerate() {
                    let name = c.get("command").and_then(|v| v.as_str()).ok_or_else(|| format!("commands[{i}] has no 'command'"))?;
                    let r = e.exec(name, c.get("params").unwrap_or(&Value::Null)).map_err(|err| format!("commands[{i}] failed, the document was rolled back: {err}"))?;
                    out.push(r);
                }
                Ok(out)
            })?;
            Ok(text(Value::Array(results).to_string()))
        }
        "run_lua" => {
            let code = m.get("code").and_then(|v| v.as_str()).ok_or("missing 'code'")?;
            let out = script::run_lua(e, code);
            let mut s = out.output;
            match out.error {
                Some(err) => {
                    s.push_str(&format!("error (the document was rolled back): {err}"));
                    Ok(fail(s))
                }
                None => {
                    if !out.result.is_null() {
                        s.push_str(&format!("result: {}", out.result));
                    }
                    Ok(text(if s.is_empty() { "ok".to_string() } else { s }))
                }
            }
        }
        "screenshot" => screenshot(e, m),
        _ => Err(format!("unknown tool '{name}'")),
    });
    r.unwrap_or_else(fail)
}

/// Handle one JSON-RPC message. Returns the response, or None for notifications.
pub fn handle_rpc(msg: &Value, call: &mut dyn FnMut(&str, &Value) -> ToolOut) -> Option<Value> {
    let method = msg.get("method")?.as_str()?;
    let id = msg.get("id").filter(|v| !v.is_null())?.clone();
    let result: Result<Value, (i64, String)> = match method {
        "initialize" => {
            const KNOWN: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
            let asked = msg.pointer("/params/protocolVersion").and_then(|v| v.as_str()).unwrap_or("");
            Ok(json!({
                "protocolVersion": if KNOWN.contains(&asked) { asked } else { KNOWN[0] },
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "jon-motion", "title": "Jon Motion", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => match msg.pointer("/params/name").and_then(|v| v.as_str()) {
            Some(name) => {
                let out = call(name, msg.pointer("/params/arguments").unwrap_or(&Value::Null));
                Ok(json!({ "content": out.content, "isError": out.is_error }))
            }
            None => Err((-32602, "tools/call needs params.name".into())),
        },
        "resources/list" => Ok(json!({ "resources": [] })),
        "prompts/list" => Ok(json!({ "prompts": [] })),
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err((code, message)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
    })
}

fn handle_body(body: &str, call: &mut dyn FnMut(&str, &Value) -> ToolOut) -> Option<Value> {
    match serde_json::from_str::<Value>(body) {
        Err(e) => Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("parse error: {e}") } })),
        Ok(Value::Array(msgs)) => {
            let out: Vec<Value> = msgs.iter().filter_map(|m| handle_rpc(m, call)).collect();
            (!out.is_empty()).then_some(Value::Array(out))
        }
        Ok(msg) => handle_rpc(&msg, call),
    }
}

// ------------------------------------------------------------------ app-side HTTP server

fn state_file() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".jon_motion").join("gui.json"))
}

pub fn settings_file() -> Option<PathBuf> {
    Some(state_file()?.with_file_name("settings.json"))
}

pub fn scripts_dir() -> Option<PathBuf> {
    state_file().map(|p| p.with_file_name("scripts"))
}

fn new_token() -> String {
    // RandomState is keyed from the operating system's random source.
    use std::hash::{BuildHasher, Hasher};
    let rs = std::collections::hash_map::RandomState::new();
    (0..4u64)
        .map(|i| {
            let mut h = rs.build_hasher();
            h.write_u64(i);
            h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
            format!("{:016x}", h.finish())
        })
        .collect()
}

fn read_token() -> Option<String> {
    let t = std::fs::read_to_string(state_file()?.with_file_name("token")).ok()?;
    let t = t.trim();
    (t.len() >= 32 && t.chars().all(|c| c.is_ascii_alphanumeric())).then(|| t.to_string())
}

/// The secret an MCP client must present to the app's HTTP endpoint. It lives in
/// ~/.jon_motion/token (readable only by the user) and is created on first use, so
/// other users of the machine and web pages cannot drive the editor.
pub fn auth_token() -> &'static str {
    static TOKEN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    TOKEN.get_or_init(|| {
        if let Some(t) = read_token() {
            return t;
        }
        let t = new_token();
        if let Some(f) = state_file().map(|f| f.with_file_name("token")) {
            let _ = std::fs::create_dir_all(f.parent().unwrap());
            let mut o = std::fs::OpenOptions::new();
            o.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                o.mode(0o600);
            }
            let _ = o.open(&f).and_then(|mut file| file.write_all(t.as_bytes()));
        }
        t
    })
}

/// Does an Authorization header value carry the right token?
pub fn authorized(header: Option<&str>, token: &str) -> bool {
    let Some(given) = header.and_then(|h| h.strip_prefix("Bearer ")) else { return false };
    let (a, b) = (given.trim().as_bytes(), token.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Run `f` on the live engine, waiting for the user to finish any brush stroke first.
fn with_engine(engine: &Arc<Mutex<Engine>>, f: impl FnOnce(&mut Engine) -> ToolOut) -> ToolOut {
    let start = Instant::now();
    loop {
        // try_lock: the window holds the engine while a file dialog is open, and
        // another request may be running a long script.
        let g = match engine.try_lock() {
            Ok(g) => Some(g),
            Err(std::sync::TryLockError::Poisoned(p)) => Some(p.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        };
        if let Some(mut g) = g {
            if !g.busy() {
                return f(&mut g);
            }
        }
        if start.elapsed() > Duration::from_secs(20) {
            return fail("the editor has been busy for 20 seconds (the user is mid-stroke, has a dialog open, or a script is running); nothing was done - try again shortly");
        }
        std::thread::sleep(Duration::from_millis(4));
    }
}

fn local_origin(origin: &str) -> bool {
    let host = origin.split("://").nth(1).unwrap_or(origin);
    let host = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

/// Start the MCP endpoint at http://127.0.0.1:<port>/mcp. `wake` asks the GUI to repaint.
pub fn start_http(engine: Arc<Mutex<Engine>>, wake: impl Fn() + Send + 'static) -> Result<u16, String> {
    let (server, port) = (BASE_PORT..BASE_PORT + 20)
        .find_map(|p| tiny_http::Server::http(("127.0.0.1", p)).ok().map(|s| (s, p)))
        .ok_or("no free port for the MCP server")?;
    let token = auth_token();
    if let Some(f) = state_file() {
        let _ = std::fs::create_dir_all(f.parent().unwrap());
        let _ = std::fs::write(&f, json!({"port": port, "pid": std::process::id()}).to_string());
    }
    std::thread::spawn(move || {
        let json_header = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
        for mut req in server.incoming_requests() {
            // Browsers send Origin; only local pages may talk to us (DNS-rebinding guard).
            let origin = req.headers().iter().find(|h| h.field.equiv("Origin")).map(|h| h.value.as_str().to_string());
            if origin.is_some_and(|o| !local_origin(&o)) {
                let _ = req.respond(tiny_http::Response::empty(403));
                continue;
            }
            let path = req.url().split('?').next().unwrap_or("").to_string();
            let reply = |req: tiny_http::Request, code: u16, body: String| {
                let _ = req.respond(tiny_http::Response::from_string(body).with_status_code(code).with_header(json_header.clone()));
            };
            match (req.method().clone(), path.as_str()) {
                (tiny_http::Method::Get, "/health") => reply(req, 200, json!({"app": "jon_motion", "pid": std::process::id()}).to_string()),
                (tiny_http::Method::Post, "/mcp" | "/") => {
                    let auth = req.headers().iter().find(|h| h.field.equiv("Authorization")).map(|h| h.value.as_str().to_string());
                    if !authorized(auth.as_deref(), token) {
                        reply(req, 401, json!({"error": "missing or wrong token: send 'Authorization: Bearer <contents of ~/.jon_motion/token>'"}).to_string());
                        continue;
                    }
                    let mut body = String::new();
                    if req.as_reader().take(64 << 20).read_to_string(&mut body).is_err() {
                        reply(req, 400, "{}".into());
                        continue;
                    }
                    let resp = guarded(|| {
                        Ok(handle_body(&body, &mut |name, args| {
                            let out = with_engine(&engine, |e| call_tool(e, name, args));
                            wake();
                            out
                        }))
                    });
                    match resp {
                        Err(err) => reply(req, 500, json!({"error": err}).to_string()),
                        Ok(Some(v)) => reply(req, 200, v.to_string()),
                        Ok(None) => {
                            let _ = req.respond(tiny_http::Response::empty(202));
                        }
                    }
                }
                (tiny_http::Method::Delete, "/mcp") => {
                    let _ = req.respond(tiny_http::Response::empty(200));
                }
                (_, "/mcp") => {
                    let _ = req.respond(tiny_http::Response::empty(405));
                }
                _ => {
                    let _ = req.respond(tiny_http::Response::empty(404));
                }
            }
        }
    });
    Ok(port)
}

pub fn stop_http() {
    if let Some(f) = state_file() {
        let mine = std::fs::read_to_string(&f).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()).and_then(|v| v["pid"].as_u64());
        if mine == Some(std::process::id() as u64) {
            let _ = std::fs::remove_file(f);
        }
    }
}

// ------------------------------------------------------------------ stdio server

fn gui_port() -> Option<u16> {
    let s = std::fs::read_to_string(state_file()?).ok()?;
    serde_json::from_str::<Value>(&s).ok()?["port"].as_u64().map(|p| p as u16)
}

enum Forward {
    /// The app answered.
    Done(ToolOut),
    /// Nothing of ours is listening: safe to work headless instead.
    NotRunning,
    /// The app took the call but no usable answer came back. It must not be re-run elsewhere.
    Failed(String),
}

/// Send a tool call to the running app.
fn forward(port: u16, name: &str, args: &Value) -> Forward {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}}).to_string();
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(400)) else { return Forward::NotRunning };
    let _ = s.set_read_timeout(Some(Duration::from_secs(180)));
    // HTTP/1.0 keeps the reply un-chunked and closes the socket when done.
    let head = format!(
        "POST /mcp HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\n\r\n",
        auth_token(),
        body.len()
    );
    if s.write_all(head.as_bytes()).and_then(|_| s.write_all(body.as_bytes())).is_err() {
        return Forward::NotRunning;
    }
    let lost = || Forward::Failed("the editor did not answer (it may be showing a dialog or have been closed); the call may or may not have been applied - check with get_info".into());
    let mut resp = Vec::new();
    if s.read_to_end(&mut resp).is_err() {
        return lost();
    }
    let Some(split) = resp.windows(4).position(|w| w == b"\r\n\r\n") else { return lost() };
    let status = String::from_utf8_lossy(&resp[..split]).lines().next().unwrap_or("").to_string();
    let text = String::from_utf8_lossy(&resp[split + 4..]).to_string();
    if status.contains(" 401") {
        return Forward::Failed("the editor refused the token in ~/.jon_motion/token; restart the editor".into());
    }
    if !status.contains(" 200") {
        // 404/405: some other program owns the port (a stale gui.json).
        return if status.contains(" 500") { Forward::Failed(format!("the editor reported an internal error: {text}")) } else { Forward::NotRunning };
    }
    let out = serde_json::from_str::<Value>(&text).ok().and_then(|v| {
        let r = v.get("result")?;
        Some(ToolOut { content: r.get("content")?.as_array()?.clone(), is_error: r.get("isError").and_then(|b| b.as_bool()).unwrap_or(false) })
    });
    out.map(Forward::Done).unwrap_or_else(lost)
}

/// MCP over stdin/stdout. Tool calls go to the running app when it is open, so an
/// AI client and the user share one live document; otherwise to a headless document.
pub fn run_stdio() {
    let mut local: Option<Engine> = None;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let resp = handle_body(&line, &mut |name, args| {
            match gui_port().map(|p| forward(p, name, args)) {
                Some(Forward::Done(out)) => out,
                Some(Forward::Failed(err)) => fail(err),
                Some(Forward::NotRunning) | None => call_tool(local.get_or_insert_with(|| Engine::new("headless")), name, args),
            }
        });
        if let Some(r) = resp {
            if writeln!(stdout, "{r}").and_then(|_| stdout.flush()).is_err() {
                break;
            }
        }
    }
}
