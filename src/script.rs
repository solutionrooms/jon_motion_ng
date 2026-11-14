//! Lua scripting. A script is one atomic, single-undo-step change:
//! if it raises an error the document is left exactly as it was.

use crate::commands;
use crate::engine::Engine;
use mlua::{ChunkMode, HookTriggers, Lua, LuaOptions, LuaSerdeExt, MultiValue, SerializeOptions, StdLib, Value as LV, Variadic, VmState};
use serde_json::Value;
use std::cell::{Cell as Flag, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

const TIME_LIMIT: Duration = Duration::from_secs(60);
const MEMORY_LIMIT: usize = 256 << 20;
const MAX_OUTPUT: usize = 64 * 1024;
/// How long a script gets to stop by itself after its limit (or Stop) before it is abandoned.
const GRACE: Duration = Duration::from_secs(1);

static CANCEL: AtomicBool = AtomicBool::new(false);

/// Ask the running script (if any) to stop; it ends with an error and is rolled back.
pub fn cancel() {
    CANCEL.store(true, Ordering::Relaxed);
}

pub const LUA_HELP: &str = "\
Lua 5.4 scripting (sandboxed: math, string, table, utf8, coroutine; no io/os libraries - files are only reached \
through the document commands, which read .jmn/.png and write .jmn/.png/.csv; 60 s and 256 MB limits; no __gc finalizers).
Every editor command is a function on the global `jm` taking one table of named parameters:
    jm.draw_line{ x0=0, y0=0, x1=31, y1=31, color=15 }
    local info = jm.get_info()            -- commands return tables
    jm.layer_add{ name='Shadows' }
Fast pixel access on the active layer:
    jm.get(x, y)            -> palette index (-1 = transparent)
    jm.set(x, y, color)     -- color: palette index, -1 to erase, or '#rrggbb' / jm.set(x, y, r, g, b) for the nearest palette colour
    jm.size()               -> width, height
    jm.help('text')         -> command reference, optionally filtered
print(...) output and the script's return value are returned to the caller.
A script is atomic and is one undo step; an error rolls everything back.";

// The wrappers keep the sandbox's limits firm:
// * `load` only takes source text (Lua does not verify bytecode).
// * Everything that can catch an error (pcall, xpcall, coroutine.resume/close, load
//   with a reader function) re-raises the time-limit or cancel error, so a script
//   cannot swallow it and carry on.
// * The instruction hook that enforces the limits belongs to one Lua thread at a time,
//   so it is moved onto a coroutine while that coroutine runs and back afterwards;
//   `hook` also checks the limits itself, as moving the hook restarts its count.
// * Lua runs error handlers and finalizers with hooks switched off, so an xpcall
//   handler is skipped once the script is over and __gc metamethods are refused.
// These are the first line of defence only: run_lua_limited abandons any script that
// still overruns (for example inside a slow C library call).
const PRELUDE: &str = r#"
do
  local load_, pcall_, xpcall_, error_, select_ = load, pcall, xpcall, error, select
  local create_, resume_, close_, running_ = coroutine.create, coroutine.resume, coroutine.close, coroutine.running
  local dead, hook = __jm_dead, __jm_hook
  local setmetatable_, rawget_, type_ = setmetatable, rawget, type
  setmetatable = function(t, mt)
    if type_(mt) == 'table' and rawget_(mt, '__gc') ~= nil then error_('__gc finalizers are not available in scripts', 2) end
    return setmetatable_(t, mt)
  end
  local function pass(ok, ...)
    if not ok and dead() then error_((...), 0) end
    return ok, ...
  end
  local function loaded(f, ...)
    if not f and dead() then error_((...), 0) end
    return f, ...
  end
  load = function(chunk, name, mode, ...)
    if select_('#', ...) > 0 then return loaded(load_(chunk, name, 't', (...))) end
    return loaded(load_(chunk, name, 't'))
  end
  pcall = function(f, ...) return pass(pcall_(f, ...)) end
  xpcall = function(f, h, ...)
    return pass(xpcall_(f, function(...) if dead() then return ... end return h(...) end, ...))
  end
  -- Run fn(co, ...) with the hook on `co`, then put the hook back on the caller.
  -- Going in, the limits are checked before the hook moves; coming back, after:
  -- either way a failed check leaves the hook on the thread that carries on running.
  local function back(ok, ...)
    hook(running_(), false)
    if not ok then error_((...), 0) end
    return ...
  end
  local function on(fn, co, ...)
    hook(co, true)
    return back(pcall_(fn, co, ...))
  end
  local function unwrap(ok, ...)
    if not ok then error_((...), 0) end
    return ...
  end
  coroutine.resume = function(co, ...) return pass(on(resume_, co, ...)) end
  coroutine.close = function(co) return pass(on(close_, co)) end
  coroutine.wrap = function(f)
    local co = create_(f)
    return function(...) return unwrap(on(resume_, co, ...)) end
  end
  string.dump = nil
  __jm_dead, __jm_hook = nil, nil
end
jm = setmetatable({}, { __index = function(t, k)
  local f = function(p) return __jm_exec(k, p) end
  rawset(t, k, f)
  return f
end })
jm.get, jm.set, jm.size, jm.help = __jm_get, __jm_set, __jm_size, __jm_help
"#;

pub struct ScriptOut {
    pub output: String,
    pub result: Value,
    pub error: Option<String>,
}

fn ser() -> SerializeOptions {
    SerializeOptions::new().serialize_none_to_null(false).serialize_unit_to_null(false)
}

fn rt(e: String) -> mlua::Error {
    mlua::Error::runtime(e)
}

pub fn run_lua(engine: &mut Engine, code: &str) -> ScriptOut {
    run_lua_limited(engine, code, TIME_LIMIT)
}

/// Run a script on a working copy of the engine, on its own thread, and take the result
/// over only if it succeeds. The limits are enforced from inside Lua where possible; a
/// script that still has not finished shortly after its time limit (or after Stop) is
/// abandoned: the live document is untouched and the copy refuses further commands.
pub fn run_lua_limited(engine: &mut Engine, code: &str, limit: Duration) -> ScriptOut {
    let fail = |msg: String| ScriptOut { output: String::new(), result: Value::Null, error: Some(msg) };
    // Adopting a script's result under an open brush stroke would fold it into that stroke's undo step.
    if engine.busy() {
        return fail("busy: a stroke is in progress; finish it and run the script again".into());
    }
    CANCEL.store(false, Ordering::Relaxed);
    let halt = Arc::new(AtomicBool::new(false));
    let mut copy = engine.fork(halt.clone());
    let (tx, rx) = mpsc::channel();
    let code = code.to_string();
    let worker = std::thread::Builder::new().name("lua-script".into()).stack_size(8 << 20).spawn(move || {
        let out = run_on(&mut copy, &code, limit);
        let _ = tx.send((copy, out));
    });
    if let Err(e) = worker {
        return fail(format!("could not start the script: {e}"));
    }
    let deadline = Instant::now() + limit + GRACE;
    let mut cancelled: Option<Instant> = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok((copy, out)) => {
                if out.error.is_none() {
                    engine.adopt(copy);
                } else if copy.modified {
                    // The script saved and then failed: the file no longer matches the document.
                    engine.modified = true;
                }
                return out;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return fail("the script crashed".into()),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if cancelled.is_none() && CANCEL.load(Ordering::Relaxed) {
                    cancelled = Some(Instant::now() + GRACE);
                }
                let now = Instant::now();
                if cancelled.is_some_and(|t| now > t) || now > deadline {
                    halt.store(true, Ordering::Relaxed);
                    let why = if cancelled.is_some() { "script cancelled".to_string() } else { format!("script exceeded the {} second time limit", limit.as_secs()) };
                    return fail(format!("{why} (it would not stop and was abandoned)"));
                }
            }
        }
    }
}

fn run_on(engine: &mut Engine, code: &str, limit: Duration) -> ScriptOut {
    let out = RefCell::new(String::new());
    let mut result = Value::Null;
    let r = engine.group(|e| {
        let cell = RefCell::new(e);
        run_inner(&cell, &out, code, limit).map(|v| result = v)
    });
    ScriptOut { output: out.into_inner(), result, error: r.err() }
}

fn run_inner(eng: &RefCell<&mut Engine>, out: &RefCell<String>, code: &str, limit: Duration) -> Result<Value, String> {
    let libs = StdLib::MATH | StdLib::STRING | StdLib::TABLE | StdLib::UTF8 | StdLib::COROUTINE;
    let lua = Lua::new_with(libs, LuaOptions::default()).map_err(|e| e.to_string())?;
    lua.set_memory_limit(MEMORY_LIMIT).map_err(|e| e.to_string())?;
    let start = Instant::now();
    // Once set, the script is over: every later check fails too.
    let dead = Rc::new(Flag::new(false));
    let check = {
        let dead = dead.clone();
        move || -> mlua::Result<()> {
            if CANCEL.load(Ordering::Relaxed) {
                dead.set(true);
                Err(mlua::Error::runtime("script cancelled"))
            } else if start.elapsed() > limit {
                dead.set(true);
                Err(mlua::Error::runtime(format!("script exceeded the {} second time limit", limit.as_secs())))
            } else {
                Ok(())
            }
        }
    };
    let triggers = || HookTriggers::new().every_nth_instruction(20_000);
    let hook_check = check.clone();
    lua.set_hook(triggers(), move |_, _| hook_check().map(|_| VmState::Continue));
    // mlua keeps one hook, tied to one Lua thread; the prelude calls this to move it
    // to whichever coroutine is about to run (see PRELUDE).
    let move_hook = lua
        .create_function(move |_, (v, check_first): (LV, bool)| {
            if check_first {
                check()?;
            }
            if let LV::Thread(t) = v {
                let c = check.clone();
                t.set_hook(triggers(), move |_, _| c().map(|_| VmState::Continue));
            }
            check()
        })
        .map_err(|e| e.to_string())?;
    lua.globals().set("__jm_hook", move_hook).map_err(|e| e.to_string())?;
    let r: mlua::Result<Value> = lua.scope(|scope| {
        let g = lua.globals();
        for k in ["dofile", "loadfile", "collectgarbage"] {
            g.set(k, LV::Nil)?;
        }
        g.set("__jm_dead", scope.create_function(|_, ()| Ok(dead.get()))?)?;
        g.set(
            "__jm_exec",
            scope.create_function(|lua, (name, params): (String, LV)| {
                let p: Value = if params.is_nil() { Value::Null } else { lua.from_value(params)? };
                let r = eng.borrow_mut().exec(&name, &p).map_err(rt)?;
                lua.to_value_with(&r, ser())
            })?,
        )?;
        g.set(
            "__jm_get",
            scope.create_function(|_, (x, y): (f64, f64)| {
                let e = eng.borrow();
                let mut ret = Variadic::new();
                match e.raw_get(x.floor() as i64, y.floor() as i64) {
                    None => ret.push(LV::Nil),
                    Some(c) => ret.push(LV::Integer(if c >= 256 { -1 } else { c as i64 })),
                }
                Ok(ret)
            })?,
        )?;
        g.set(
            "__jm_set",
            scope.create_function(|lua, args: Variadic<LV>| {
                let num = |i: usize| args.get(i).and_then(|v| v.as_f64().or_else(|| v.as_i64().map(|n| n as f64)));
                let (Some(x), Some(y)) = (num(0), num(1)) else { return Err(rt("jm.set(x, y, color): x and y must be numbers".into())) };
                let mut e = eng.borrow_mut();
                let cell = if args.len() >= 5 {
                    let ch = |i: usize| num(i).unwrap_or(255.0).clamp(0.0, 255.0) as u8;
                    e.doc.from_rgba([ch(2), ch(3), ch(4), ch(5)])
                } else {
                    let v: Value = lua.from_value(args.get(2).cloned().unwrap_or(LV::Nil))?;
                    e.parse_color(&v).map_err(rt)?
                };
                e.raw_set(x.floor() as i64, y.floor() as i64, cell).map_err(rt)
            })?,
        )?;
        g.set("__jm_size", scope.create_function(|_, ()| Ok((eng.borrow().doc.w, eng.borrow().doc.h)))?)?;
        g.set("__jm_help", scope.create_function(|_, q: Option<String>| Ok(commands::reference(None, q.as_deref())))?)?;
        g.set(
            "print",
            scope.create_function(|_, args: Variadic<LV>| {
                let line: Vec<String> = args.iter().map(|v| v.to_string().unwrap_or_else(|_| "?".into())).collect();
                let mut o = out.borrow_mut();
                if o.len() < MAX_OUTPUT {
                    o.push_str(&line.join("\t"));
                    o.push('\n');
                }
                Ok(())
            })?,
        )?;
        lua.load(PRELUDE).set_name("prelude").exec()?;
        let vals: MultiValue = lua.load(code).set_name("script").set_mode(ChunkMode::Text).eval()?;
        Ok(match vals.into_iter().next() {
            Some(v) if !v.is_nil() => lua.from_value::<Value>(v).unwrap_or(Value::Null),
            _ => Value::Null,
        })
    });
    r.map_err(|e| {
        // Keep the message, drop the Lua stack traceback noise.
        let s = e.to_string();
        s.split("\nstack traceback").next().unwrap_or(&s).trim().to_string()
    })
}
