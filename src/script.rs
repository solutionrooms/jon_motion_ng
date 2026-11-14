//! Lua scripting. A script is one atomic, single-undo-step change:
//! if it raises an error the document is left exactly as it was.

use crate::commands;
use crate::doc::*;
use crate::engine::Engine;
use mlua::{ChunkMode, HookTriggers, Lua, LuaOptions, LuaSerdeExt, MultiValue, SerializeOptions, StdLib, Value as LV, Variadic, VmState};
use serde_json::Value;
use std::cell::{Cell as Flag, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const TIME_LIMIT: Duration = Duration::from_secs(60);
const MEMORY_LIMIT: usize = 256 << 20;
const MAX_OUTPUT: usize = 64 * 1024;

static CANCEL: AtomicBool = AtomicBool::new(false);

/// Ask the running script (if any) to stop; it ends with an error and is rolled back.
pub fn cancel() {
    CANCEL.store(true, Ordering::Relaxed);
}

pub const LUA_HELP: &str = "\
Lua 5.4 scripting (sandboxed: math, string, table, utf8, coroutine; no io/os libraries - files are only reached \
through the document commands, which read .jmn/.png and write .jmn/.png/.csv; 60 s and 256 MB limits).
Every editor command is a function on the global `jm` taking one table of named parameters:
    jm.draw_line{ x0=0, y0=0, x1=31, y1=31, color=15 }
    local info = jm.get_info()            -- commands return tables
    jm.layer_add{ name='Shadows' }
Fast pixel access on the active layer:
    jm.get(x, y)            -> palette index (-1 = transparent), or r, g, b, a in RGBA documents
    jm.set(x, y, color)     -- color: palette index, -1 to erase, '#rrggbb', or jm.set(x, y, r, g, b [, a])
    jm.size()               -> width, height
    jm.help('text')         -> command reference, optionally filtered
print(...) output and the script's return value are returned to the caller.
A script is atomic and is one undo step; an error rolls everything back.";

// The wrappers keep the sandbox's limits firm: `load` only takes source text (Lua does
// not verify bytecode), and pcall/xpcall/coroutine.resume re-raise the time-limit or
// cancel error, so a script cannot swallow it and carry on.
const PRELUDE: &str = r#"
do
  local load_, pcall_, xpcall_, resume_, error_, select_, dead = load, pcall, xpcall, coroutine.resume, error, select, __jm_dead
  load = function(chunk, name, mode, ...)
    if select_('#', ...) > 0 then return load_(chunk, name, 't', (...)) end
    return load_(chunk, name, 't')
  end
  local function pass(ok, ...)
    if not ok and dead() then error_((...), 0) end
    return ok, ...
  end
  pcall = function(f, ...) return pass(pcall_(f, ...)) end
  xpcall = function(f, h, ...) return pass(xpcall_(f, h, ...)) end
  coroutine.resume = function(co, ...) return pass(resume_(co, ...)) end
  string.dump = nil
  __jm_dead = nil
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

pub fn run_lua_limited(engine: &mut Engine, code: &str, limit: Duration) -> ScriptOut {
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
    CANCEL.store(false, Ordering::Relaxed);
    // Once set, the script is over: every later check fails too.
    let dead = Rc::new(Flag::new(false));
    let hook_dead = dead.clone();
    lua.set_hook(HookTriggers::new().every_nth_instruction(20_000), move |_, _| {
        if CANCEL.load(Ordering::Relaxed) {
            hook_dead.set(true);
            Err(mlua::Error::runtime("script cancelled"))
        } else if start.elapsed() > limit {
            hook_dead.set(true);
            Err(mlua::Error::runtime(format!("script exceeded the {} second time limit", limit.as_secs())))
        } else {
            Ok(VmState::Continue)
        }
    });
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
                    Some(c) => match e.doc.mode {
                        ColorMode::Indexed => ret.push(LV::Integer(if c >= 256 { -1 } else { c as i64 })),
                        ColorMode::Rgba => ret.extend(unpack(c).map(|v| LV::Integer(v as i64))),
                    },
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
