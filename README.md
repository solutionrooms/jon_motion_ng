# Jon Motion

A fast, scriptable pixel art editor in Rust for macOS and Windows: indexed-palette
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
[releases page](https://github.com/solutionrooms/jon_motion_ng/releases). They are unsigned, so
macOS refuses to open the app at first ("Apple could not verify..."). Click Done, then open System Settings >
Privacy & Security, scroll to the message about Jon Motion and click **Open Anyway** (once per download).
Or clear the download flag in Terminal: `xattr -dr com.apple.quarantine "/path/to/Jon Motion.app"`.

To cut a release, bump `version` in `Cargo.toml` and push a matching tag (`git tag v0.1.0 && git push --tags`);
macOS builds are signed and notarised when the signing secrets listed at the top of
`.github/workflows/release.yml` are set in the repository's `release` environment; without them the build is unsigned. A published release is never
overwritten, and each one carries a `SHA256SUMS.txt`.
CI tests, builds both platforms and publishes them. Local packages: `./scripts/package.sh` (run on macOS;
writes `dist/*.zip`, cross-compiling Windows with `rustup target add x86_64-pc-windows-gnu` and `brew install mingw-w64`).

## What's in it

- **Drawing** - pencil, eraser, line, rectangle, ellipse, flood fill, gradient, colour picker;
  round/square brushes to 128 px, custom brushes grabbed from the image, X/Y mirror, pixel-perfect lines.
  Right mouse button paints with the background colour.
- **Paint modes** - normal, dither (ordered, 15 densities, optional two-colour), lighten/darken
  (steps pixels along their colour ramp), smooth, smear, replace-colour, behind.
- **Palette** - a document is indexed, with up to 256 colours, unless you ask for true colour (below). Blend, sort, swap, de-duplicate, remove unused,
  11 presets, colour ramps, live colour-cycling preview, quantise/reduce colours with ordered or
  Floyd-Steinberg dithering. Palette edits renumber pixels so the image keeps its look.
- **Layers** - visibility, lock, transparency lock, reorder, merge, flatten. Layers are fully opaque where they have pixels.
- **Selections** - rectangle, magic wand, add/subtract, move as floating pixels, cut/copy/paste,
  system clipboard images.
- **Tile mapping** - turn a layer into a tile map; identical cells share one tile. *Auto* mode makes
  or matches tiles as you draw; *Sync* mode edits a tile everywhere it is used, live. Tile stamp tool,
  tile set PNG and map CSV export.
- **Animation** - select a strip of frames, give the size of one frame, and the Animation tab plays them
  (left to right, then top to bottom) while you keep drawing. Left / Right step and Space plays or pauses while
  the pointer is over the tab. A document can hold many named animations (up to 256): pick one in the tab, **New** makes
  another from the selection. An animation is either a *grid* (a region cut into equal frames) or a *frame list*,
  where every frame is its own rectangle with an optional offset so trimmed frames of different sizes line up;
  frame lists are set by script, AI client or JSON (`anim_frames`). All of it is saved in the project.
  An animation is a set of fixed rectangles: it does not follow a resize, scale or flip, and is removed if it no longer
  fits the image (undo brings it back). View > Animation Frames hides the frame lines on the canvas.
  **Import... / Export...** in the tab (`anim_import_json`, `anim_export_json`) read and write the animations as JSON:

  ```json
  { "format": "jon-motion-animations", "version": 1, "image": { "width": 128, "height": 64 },
    "animations": [
      { "name": "walk", "fps": 8, "grid": { "x": 0, "y": 0, "w": 64, "h": 16, "frame_w": 16, "frame_h": 16, "frames": 0 } },
      { "name": "jump", "fps": 12, "frames": [ [0, 32, 10, 20], [10, 32, 14, 24, -2, -4] ] } ] }
  ```

  A frame is `[x, y, w, h]` or `[x, y, w, h, offset_x, offset_y]` (an object with those names is read too). Import checks
  every animation against the image and imports nothing if one is refused; one with the name of an existing animation
  replaces it. Frame lists hold up to 4096 frames each and 32,768 in a document. Export only overwrites a file that is
  itself an animations file and nothing else, and never writes into `~/.jon_motion`. Versions 0.1.4 and older open these projects
  but know one grid animation at most: 0.1.4 shows the first grid animation, and saving from an old version
  drops the rest.
- **Files** - native `.jmn` projects (everything), PNG in and out. An indexed document always exports an indexed PNG. An indexed
  PNG opens with its own palette; a true-colour PNG is converted to a generated palette of up to 256 colours, unless
  you open it as true colour.
- **True colour** - a separate kind of document, for pictures that are not palette images. You only get one by asking:
  **True colour** in File > New, File > Open as True Colour... (or that choice when a PNG is dropped), or Image >
  Convert to True Colour. Its tab and the status bar say "true colour". Every pixel is its own colour with its own
  opacity, a PNG opens and exports with every visible pixel exactly as it is (RGBA; a fully transparent pixel is just
  transparent, so any colour a file stored under one is not kept), and the palette is a set of swatches:
  editing it never changes the picture. Painting sets exactly the colour in hand - it replaces the pixel rather than
  blending over it - while layers do blend where pixels are part transparent. Gradients blend smoothly, lighten /
  darken and smooth work on the colours themselves, and tile maps, selections, animations and scripts work as usual.
  What needs palette indices is switched off with the reason shown: colour cycling, reduce colours, remove unused
  colours, palette usage. Image > Convert to Indexed... turns it into a palette image (part-transparent pixels become
  opaque or transparent); both conversions are one undo step, and nothing else ever changes a document's kind.
  True-colour projects are saved in a form that versions 0.1.4 and older refuse to open, rather than opening them
  wrongly; indexed projects are written exactly as before. That is why Image > Convert to True Colour... asks first:
  the next Save rewrites the project file in that form. A true-colour project made by 0.1.0-0.1.2 opens as true
  colour with its pixels as they were (the status line says so, and `open_document` answers `legacy_truecolor`);
  saving it also writes the new form, so use Save As to keep the original for the old version. Layers have no
  opacity setting in either kind: where such an old project had a part-transparent layer, that opacity goes into the
  layer's pixels so the picture looks the same (a tile-mapped layer becomes fully opaque instead).

Help > Keyboard Shortcuts lists the keys. Help > Keyboard Preset switches the single-key shortcuts between the
standard set and **Deluxe Paint** keys (`d` draw, `v` line, `r`/`R` rectangle, `[` `]` colours, `F1`-`F8` modes...);
the choice is remembered in `~/.jon_motion/settings.json`.

View > Grid (`Cmd/Ctrl+G`) draws a grid over the image. It starts at one pixel per cell; set Grid size in the View
menu for a coarser one (making tiles sets it to the tile size). Each document has its own grid size. The grid is
drawn once a cell's smaller side is 4 screen pixels - from 400% zoom for the one-pixel grid - and when you switch on
a grid that is too fine for the current zoom, the status line says which zoom shows it.
View > Use as Default Grid Size makes the current size the one new documents start with, remembered in the same
settings file as `"grid_default": [width, height]`; scripts and AI clients do the same with `set_view grid_default=true`.
A new animation's frame size starts from the tile size, else the grid size, else 16x16 while the grid is single pixels.

**Tabs** - several documents can be open at once (up to 32), each in its own tab with its own undo history,
file, selection, view, colours and animations; the tool in hand and the clipboard go with you from tab to tab,
so you can copy in one and paste in another. File > New, Open, Open Recent and a dropped file each open in a new tab
(an untouched blank document is replaced instead), and a project that is already open is shown rather than loaded
twice. Cmd/Ctrl+W or the tab's **x** closes a tab; Ctrl+Tab moves between them. Closing a tab, or quitting, asks about
every document that has unsaved changes. Each open document keeps its own undo history in memory.

Drag a `.jmn` or `.png` file onto the window (one at a time). A project opens in a tab; for a PNG you choose
between opening it and adding it to the current document as a layer.

**Sprite atlas viewer** - File > Open Sprite Atlas... (or drop the file on the window, or pass it on the command
line) opens a game's baked atlas - an `atlas.json` beside its `atlas_0.webp`, `atlas_1.webp`... pages (PNG pages work
too) - in a floating viewer: every object is listed, and any of them plays with its per-frame offsets, including
labelled sequences (`run1` ... `run1_end`). Left / Right step and Space plays while the pointer is over the viewer.
It shows the pages in true colour, exactly as they are, and it is **view only**: the atlas is not a document, so no
tool, command, script or AI client can paint, convert or save it, and its files are only ever read. The format is
`{prefix, pages, ext, objects: [{name, labels: [[label, frame], ...], frames: [[page, x, y, w, h, source_w,
source_h, x_offset, y_offset], ...]}]}`, with page -1 for an empty frame. Scripts and AI clients cannot see or drive
the viewer.

File > Open Recent lists the last 10 files opened or saved from the window (not ones a script or AI client
opened). The list lives in `~/.jon_motion/settings.json` (created private to your account on macOS and Linux;
on Windows it is protected by your user profile folder), and no command exposes it.

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

A script is atomic and one undo step: if it raises an error, the document is rolled back. It works on one
document - the tab that was active when it started - and cannot open, close or switch tabs (`jm.tab_list()` shows them). Scripts are
sandboxed: no `io`/`os` libraries, a 60 s time limit and a 256 MB memory limit. The only files a script
can touch are through the document commands, which read `.jmn`/`.png`/`.json` and write `.jmn`/`.png`/`.csv`,
plus `.json` for animations (never over a file that is not an animations file).
Scripts run on a working copy of the document, off the UI thread, and a long one shows a **Stop script**
button. A script that will not stop after its limit is abandoned: the document is untouched and the
script can no longer run commands, though it may keep using CPU until you quit. Headless: `jon_motion run script.lua [file]`.
Full reference: `jon_motion commands [filter]`.

## AI / MCP

Jon Motion is an MCP server. Register it once with your client:

```sh
claude mcp add jon-motion -- /path/to/jon_motion mcp
```

- If the editor window is open, the AI works on **your live document**. Its calls run between your
  brush strokes (never during one), you see each change as it lands, and each call is one undo step.
- If the editor is not open, the same command works on a headless document (`save_document` to keep it).
- With several tabs open, tools work on the **active tab** and each answer says which document that was. Pass
  `"document": <id>` (ids from `tab_list` / `get_info`) to keep working on one document whatever the user is
  looking at; their view, tools and clipboard do not change (so tool options set, or pixels copied, on a background
  document last for that call only - use `batch` to copy and paste there). `tab_new`, `tab_open`, `tab_select` and
  `tab_close` manage tabs (not inside `batch` or `run_lua`), and `tab_list` gives the file path of every open document.
  `new_document` / `open_document` still replace the active document. A file is held by one tab at a time: opening
  or saving a document over a file that another tab has open is refused.
- The window also serves MCP directly at `http://127.0.0.1:47800/mcp` (port shown in the status bar;
  Help > AI / MCP Setup has copy-paste commands). It listens on localhost only and requires the token
  in `~/.jon_motion/token` as an `Authorization: Bearer` header, so other users of the machine and web
  pages cannot drive the editor. The `jon_motion mcp` bridge sends it for you.

- `get_info` reports `"mode"`: `indexed` or `truecolor`. In a true-colour document colours are `'#rrggbb'` /
  `'#rrggbbaa'` (a number picks that swatch's colour) and pixels read back as `'#rrggbbaa'` or `'transparent'`;
  `jm.get` returns `r, g, b, a`. `new_document`, `tab_new`, `open_document` and `tab_open` take `mode = "truecolor"`
  (PNG files only for the last two), and `convert_mode` changes a document's kind. Opening a true-colour PNG the
  default way answers `converted_to_indexed: true`.

Tools: `get_info`, `list_commands`, `execute_command`, `batch` (atomic), `run_lua`, `screenshot`
(returns an image so the model can see its work). Replacing a document that has unsaved changes is
refused unless the caller explicitly passes `force`, so it cannot happen by accident - but a client that
passes `force` does discard them. Save and export commands overwrite existing files.

## Code map

| File | Role |
| --- | --- |
| `src/doc.rs` | Document model: cells, palette, layers, tile set, floating pixels, compositing |
| `src/paint.rs` | Brushes, paint modes, shapes, fill, gradient |
| `src/engine.rs` | Engine state, undo, strokes, tile sync, `exec` |
| `src/commands.rs` | The command registry - add a feature here and it is scriptable everywhere |
| `src/script.rs` | Lua bindings |
| `src/remote.rs` | MCP: tools, JSON-RPC, HTTP server, stdio bridge |
| `src/atlas.rs` | View-only sprite atlas reader and viewer window |
| `src/app.rs`, `canvas.rs`, `panels.rs` | The egui app |

`cargo test` runs the Lua command suite (`tests/smoke.lua`), the MCP protocol, and the interactive
canvas driven by synthetic mouse input.
