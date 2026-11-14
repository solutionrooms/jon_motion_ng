#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod canvas;
mod commands;
mod doc;
mod engine;
mod io;
mod keys;
mod paint;
mod panels;
mod quant;
mod remote;
mod script;
#[cfg(test)]
mod tests;

use std::path::PathBuf;

const USAGE: &str = "\
Jon Motion - scriptable pixel art, palette and tile map editor

  jon_motion [file]                 open the editor (optionally with a .jmn or .png)
  jon_motion mcp                    MCP server on stdio (drives the open editor, or a headless document)
  jon_motion run <script.lua> [file]  run a Lua script headless, optionally on a document
  jon_motion commands [filter]      print the command reference";

/// Release builds are GUI-subsystem on Windows; reattach to the launching console for CLI output.
fn attach_console() {
    #[cfg(windows)]
    {
        extern "system" {
            fn AttachConsole(pid: u32) -> i32;
        }
        unsafe {
            AttachConsole(u32::MAX);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("mcp") => remote::run_stdio(),
        Some("run") => {
            attach_console();
            let Some(path) = args.get(1) else {
                eprintln!("{USAGE}");
                std::process::exit(2);
            };
            let code = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("{path}: {e}");
                    std::process::exit(2);
                }
            };
            let mut e = engine::Engine::new("headless");
            if let Some(doc) = args.get(2) {
                if let Err(err) = e.exec("open_document", &serde_json::json!({ "path": doc })) {
                    eprintln!("{err}");
                    std::process::exit(1);
                }
            }
            let out = script::run_lua(&mut e, &code);
            print!("{}", out.output);
            if !out.result.is_null() {
                println!("{}", out.result);
            }
            if let Some(err) = out.error {
                eprintln!("error: {err}");
                std::process::exit(1);
            }
        }
        Some("commands") => {
            attach_console();
            println!("{}\n{}", script::LUA_HELP, commands::reference(None, args.get(1).map(String::as_str)));
        }
        Some("-h" | "--help" | "help") => {
            attach_console();
            println!("{USAGE}");
        }
        Some("--version") => {
            attach_console();
            println!("{}", env!("CARGO_PKG_VERSION"));
        }
        other => app::run(other.map(PathBuf::from)),
    }
}
