# Jon Motion

A fast, scriptable pixel art editor in Rust for macOS and Windows: indexed-palette and true-colour
painting, layers, colour ramps and cycling, paint modes, and tile mapping - in the spirit of the
classic palette-first pixel tools, with a modern dark UI.

Every feature is a **command**. The GUI, Lua scripts and AI clients (over MCP) all call the same
commands, so anything you can do by hand can be automated, and an AI can work on the document you
have open while you keep drawing.

## Run

```sh
cargo run --release              # the editor
cargo run --release -- art.jmn   # open a project (or a .png)
```

Prebuilt macOS (universal) and Windows builds are on the
[releases page](https://github.com/solutionrooms/jon_motion_ng/releases). They are unsigned -
on macOS, right-click the app and choose Open the first time.

To cut a release, bump `version` in `Cargo.toml` and push a matching tag (`git tag v0.1.0 && git push --tags`);
CI tests, builds both platforms and publishes them. Local packages: `./scripts/package.sh` (run on macOS;
writes `dist/*.zip`, cross-compiling Windows with `rustup target add x86_64-pc-windows-gnu` and `brew install mingw-w64`).

## What's in it

- **Drawing** - pencil, eraser, line, rectangle, ellipse, flood fill, gradient, colour picker;
  round/square brushes to 128 px, custom brushes grabbed from the image, X/Y mirror, pixel-perfect lines.
  Right mouse button paints with the background colour.
- **Paint modes** - normal, dither (ordered, 15 densities, optional two-colour), lighten/darken
  (steps pixels along their colour ramp), smooth, smear, replace-colour, behind.
- **Palette** - 256-colour indexed documents or RGBA. Blend, sort, swap, de-duplicate, remove unused,
  11 presets, colour ramps, live colour-cycling preview, quantise/reduce colours with ordered or
  Floyd-Steinberg dithering. Palette edits renumber pixels so the image keeps its look.
- **Layers** - opacity, visibility, lock, transparency lock, reorder, merge, flatten.
- **Selections** - rectangle, magic wand, add/subtract, move as floating pixels, cut/copy/paste,
  system clipboard images.
- **Tile mapping** - turn a layer into a tile map; identical cells share one tile. *Auto* mode makes
  or matches tiles as you draw; *Sync* mode edits a tile everywhere it is used, live. Tile stamp tool,
  tile set PNG and map CSV export.
- **Files** - native `.jmn` projects (everything), PNG in and out (indexed PNGs stay indexed).

Help > Keyboard Shortcuts lists the keys.

## Scripting (Lua)

The **Script** tab runs Lua 5.4 against the open document. Each command is a function on `jm`
taking a table of named parameters; the tab has a searchable command list.

```lua
jm.new_document{ width = 64, height = 64, force = true }
jm.draw_ellipse{ x = 8, y = 8, w = 48, h = 48, color = 40, filled = true }
jm.draw_rect{ x = 0, y = 56, w = 64, h = 8, color = 100, mode = "dither", dither = 8, filled = true }
for x = 0, 63 do jm.set(x, 0, x % 16) end          -- fast pixel access: jm.get / jm.set / jm.size
print(jm.get_info().width)
```

A script is atomic and one undo step: if it raises an error, nothing is applied. Scripts are
sandboxed (no file or OS access; 60 s limit). Headless: `jon_motion run script.lua [file]`.
Full reference: `jon_motion commands [filter]`.

## AI / MCP

Jon Motion is an MCP server. Register it once with your client:

```sh
claude mcp add jon-motion -- /path/to/jon_motion mcp
```

- If the editor window is open, the AI works on **your live document**. Its calls run between your
  brush strokes (never during one), you see each change as it lands, and each call is one undo step.
- If the editor is not open, the same command works on a headless document (`save_document` to keep it).
- The window also serves MCP directly at `http://127.0.0.1:47800/mcp` (port shown in the status bar;
  Help > AI / MCP Setup has copy-paste commands). It listens on localhost only.

Tools: `get_info`, `list_commands`, `execute_command`, `batch` (atomic), `run_lua`, `screenshot`
(returns an image so the model can see its work). The AI cannot discard your unsaved work:
replacing the document is refused unless it passes `force`.

## Code map

| File | Role |
| --- | --- |
| `src/doc.rs` | Document model: cells, palette, layers, tile set, floating pixels, compositing |
| `src/paint.rs` | Brushes, paint modes, shapes, fill, gradient |
| `src/engine.rs` | Engine state, undo, strokes, tile sync, `exec` |
| `src/commands.rs` | The command registry - add a feature here and it is scriptable everywhere |
| `src/script.rs` | Lua bindings |
| `src/remote.rs` | MCP: tools, JSON-RPC, HTTP server, stdio bridge |
| `src/app.rs`, `canvas.rs`, `panels.rs` | The egui app |

`cargo test` runs the Lua command suite (`tests/smoke.lua`), the MCP protocol, and the interactive
canvas driven by synthetic mouse input.
