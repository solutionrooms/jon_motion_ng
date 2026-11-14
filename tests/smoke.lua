-- Exercises every command family. Run: jon_motion run tests/smoke.lua
local function eq(a, b, what)
  if a ~= b then error((what or "value") .. ": expected " .. tostring(b) .. ", got " .. tostring(a), 2) end
end
local function fails(f, what)
  if pcall(f) then error((what or "call") .. " should have failed", 2) end
end

jm.new_document{ width = 64, height = 48, force = true }
local w, h = jm.size(); eq(w, 64); eq(h, 48)

-- shapes, fill, raw pixels
jm.draw_rect{ x = 2, y = 2, w = 20, h = 12, color = 40, filled = true }
eq(jm.get(2, 2), 40, "rect corner"); eq(jm.get(21, 13), 40); eq(jm.get(22, 13), -1)
jm.draw_line{ x0 = 0, y0 = 0, x1 = 63, y1 = 47, color = 15 }
eq(jm.get(0, 0), 15); eq(jm.get(63, 47), 15)
jm.draw_ellipse{ x = 30, y = 10, w = 21, h = 21, color = 100, filled = true }
eq(jm.get(40, 20), 100, "ellipse centre"); eq(jm.get(30, 10), -1, "ellipse corner empty")
jm.draw_ellipse{ x = 30, y = 10, w = 21, h = 21, color = 105 }
eq(jm.get(40, 10), 105, "ellipse top"); eq(jm.get(30, 20), 105, "ellipse left"); eq(jm.get(50, 20), 105, "ellipse right")
jm.fill{ x = 60, y = 2, color = 3 }
eq(jm.get(63, 0), 3, "fill")
jm.set(5, 40, 77); eq(jm.get(5, 40), 77)
jm.set(5, 40, -1); eq(jm.get(5, 40), -1, "erase")
jm.set(5, 40, "#ffffff"); eq(jm.get(5, 40), 15, "hex snaps to nearest palette colour")
jm.put_pixels{ x = 0, y = 44, rows = { { 1, 2, 3 }, { 4, -1, 6 } } }
eq(jm.get(1, 44), 2); eq(jm.get(1, 45), -1)
local px = jm.get_pixels{ x = 0, y = 44, w = 3, h = 2 }
eq(px.rows[2][3], 6, "get_pixels")
jm.draw_stroke{ points = { { 10, 30 }, { 20, 30 }, { 20, 38 } }, color = 9, size = 3, shape = "round" }
eq(jm.get(15, 30), 9); eq(jm.get(15, 31), 9, "3px brush"); eq(jm.get(20, 35), 9)

-- paint modes
jm.draw_rect{ x = 0, y = 20, w = 8, h = 8, color = 8, filled = true }
jm.draw_rect{ x = 0, y = 20, w = 8, h = 8, mode = "lighten", filled = true }
eq(jm.get(3, 23), 9, "lighten steps up the grey ramp")
jm.draw_rect{ x = 0, y = 20, w = 8, h = 8, mode = "darken", filled = true }
jm.draw_rect{ x = 0, y = 20, w = 8, h = 8, mode = "darken", filled = true }
eq(jm.get(3, 23), 7, "darken steps down")
jm.draw_rect{ x = 0, y = 20, w = 8, h = 8, mode = "dither", dither = 8, color = 200, filled = true }
local a, b = jm.get(0, 20), jm.get(1, 20)
eq((a == 200) ~= (b == 200), true, "50% dither alternates")
jm.draw_rect{ x = 0, y = 20, w = 8, h = 8, mode = "replace", bg = 7, color = 33, filled = true }
eq((jm.get(0, 20) == 33) or (jm.get(1, 20) == 33), true, "replace hits bg pixels only")
jm.draw_rect{ x = 0, y = 30, w = 4, h = 4, mode = "behind", color = 50, filled = true }
eq(jm.get(0, 30), 50, "behind paints on empty")
jm.draw_line{ x0 = 0, y0 = 46, x1 = 10, y1 = 46, color = 60, symmetry = "x" }
eq(jm.get(63, 46), 60, "x symmetry")

-- layers
local l = jm.layer_add{ name = "Top" }.layer
eq(l, 1); eq(jm.get(2, 2), -1, "new layer is empty")
jm.set(2, 2, 99)
jm.layer_set{ layer = "Top", locked = true }
fails(function() jm.layer_set{ opacity = 128 } end, "layers have no opacity")
eq(jm.get_info().layers[2].opacity, nil)
fails(function() jm.draw_line{ x0 = 0, y0 = 0, x1 = 5, y1 = 5 } end, "drawing on a locked layer")
jm.layer_set{ locked = false }
jm.layer_duplicate{}
eq(#jm.get_info().layers, 3)
jm.layer_move{ layer = 2, to = 0 }
eq(jm.get_info().layers[1].name, "Top copy")
jm.layer_delete{ layer = 0 }
jm.layer_select{ layer = "Top" }
jm.layer_merge_down{}
eq(#jm.get_info().layers, 1); eq(jm.get(2, 2), 99, "merge down")
eq(jm.get_pixel{ x = 2, y = 2 }.rgba:sub(1, 1), "#")

-- selection, clipboard, floats
jm.select_rect{ x = 0, y = 0, w = 10, h = 10 }
jm.selection_fill{ color = 5 }
eq(jm.get(9, 9), 5); eq(jm.get(10, 10) ~= 5, true, "fill confined to selection")
jm.draw_rect{ x = 0, y = 0, w = 64, h = 48, color = 6, filled = true }
eq(jm.get(9, 9), 6); eq(jm.get(30, 40) ~= 6, true, "drawing confined to selection")
jm.copy{}
jm.paste{ x = 50, y = 30 }
eq(jm.get(59, 39), 6, "paste")
jm.select_rect{ x = 50, y = 30, w = 10, h = 10 }
jm.selection_move{ dx = -40, dy = 5 }
eq(jm.get(10, 35), 6, "selection_move"); eq(jm.get(59, 39), -1)
eq(jm.get_info().selection.x, 10, "selection follows the move")
jm.select_wand{ x = 10, y = 35 }
eq(jm.get_info().selection.w, 10, "wand")
jm.select_invert{}; jm.select_none{}
jm.select_rect{ x = 10, y = 35, w = 10, h = 10 }
jm.grab_brush{}
jm.select_none{}
jm.draw_line{ x0 = 40, y0 = 40, x1 = 40, y1 = 40, brush = "custom" }
eq(jm.get(40, 40), 6, "custom brush stamp")
jm.clear_brush{}
jm.flip{ axis = "h", scope = "document" }
eq(jm.get(63, 0), 6, "flip h")
jm.rotate{ turns = 1 }
eq(select(1, jm.size()), 48, "rotate swaps size")
jm.rotate{ turns = 3 }
jm.resize_canvas{ width = 80, height = 48, anchor = "top-right" }
eq(jm.get(79, 0), 6, "resize keeps right edge")
jm.scale_image{ width = 160, height = 96 }
eq(jm.get(159, 1), 6, "scale")
jm.scale_image{ width = 64, height = 48 }

-- palette
eq(jm.palette_get{}.count, 256)
jm.palette_set{ index = 1, color = "#ff0000" }
eq(jm.palette_get{ start = 1, count = 1 }.colors[1], "#ff0000")
jm.palette_set{ index = 4, color = { 0, 0, 255 } }
jm.palette_gradient{ from = 1, to = 4 }
eq(jm.palette_get{ start = 2, count = 1 }.colors[1], "#aa0055")
jm.set(0, 0, 1); jm.set(1, 0, 4)
jm.palette_swap{ a = 1, b = 4 }
eq(jm.get(0, 0), 4, "swap remaps pixels"); eq(jm.palette_get{ start = 4, count = 1 }.colors[1], "#ff0000")
jm.palette_sort{ from = 0, to = 15, by = "luma" }
local r = jm.ramp_add{ name = "Test", from = 16, to = 31, cycle = true, speed = 4 }.ramp
eq(jm.ramp_list()[r + 1].name, "Test")
jm.ramp_set{ ramp = r, cycle = false }
jm.gradient{ x0 = 0, y0 = 0, x1 = 63, y1 = 0, ramp = r, dither = true }
eq(jm.get(0, 0), 16, "gradient start"); eq(jm.get(63, 0), 31, "gradient end")
jm.ramp_delete{ ramp = r }
local used = jm.palette_usage{}.used
jm.palette_cleanup{ unused = true }
eq(jm.palette_get{}.count, used, "cleanup drops unused colours")
jm.palette_load_preset{ name = "pico8" }
eq(jm.palette_get{}.count, 16)
jm.replace_color{ from = jm.get(0, 0), to = 8 }
eq(jm.get(0, 0), 8)

-- tiles
jm.new_document{ width = 32, height = 32, force = true }
jm.tileset_create{ tile_w = 8, tile_h = 8 }
jm.draw_rect{ x = 0, y = 0, w = 8, h = 8, color = 20, filled = true }
jm.draw_rect{ x = 16, y = 0, w = 8, h = 8, color = 20, filled = true }
jm.layer_to_tiles{}
local ti = jm.tiles_info{}
eq(ti.count, 2, "blank + one shared tile"); eq(ti.cols, 4)
local m = jm.tilemap_get{}
eq(m.cells[1][1], 1); eq(m.cells[1][3], 1, "identical cells share a tile"); eq(m.cells[1][2], 0)
-- auto mode: editing one instance makes a new tile, the other is untouched
jm.set(0, 0, 30)
eq(jm.tiles_info{}.count, 3, "auto mode creates a tile")
eq(jm.get(16, 0), 20, "other instance unchanged in auto mode")
-- sync mode: editing one instance edits them all
jm.tile_set_cell{ col = 0, row = 0, tile = 1 }
eq(jm.get(0, 0), 20, "set_cell reblits")
jm.set_options{ tile_mode = "sync" }
jm.draw_line{ x0 = 1, y0 = 1, x1 = 1, y1 = 1, color = 44 }
eq(jm.get(17, 1), 44, "sync mode propagates to other instances")
eq(jm.tile_get_pixels{ tile = 1 }.rows[2][2], 44, "tile updated")
jm.set_options{ tile_mode = "auto" }
jm.tile_put_pixels{ tile = 1, rows = { { 7, 7 } } }
eq(jm.get(16, 0), 7, "tile_put_pixels updates instances")
local nt = jm.tile_add{ copy_of = 1 }.tile
jm.tilemap_set{ cells = { { nt, nt }, { 0, nt } }, col = 2, row = 2 }
eq(jm.get(24, 16), 7); eq(jm.tile_get_cell{ col = 3, row = 3 }.tile, nt)
jm.tile_make_unique{ col = 3, row = 3 }
jm.tiles_dedupe{}
eq(jm.tile_get_cell{ col = 3, row = 3 }.tile, jm.tile_get_cell{ col = 0, row = 0 }.tile, "dedupe merges copies")
local before = jm.tiles_info{}.count
jm.tiles_remove_unused{}
eq(jm.tiles_info{}.count <= before, true)
jm.layer_detach_tiles{}; jm.tileset_delete{}

-- true colour is a separate kind of document, only ever asked for by name
jm.new_document{ width = 16, height = 16, mode = "truecolor", force = true }
eq(jm.get_info().mode, "truecolor")
jm.set(0, 0, "#12345678"); local r, g, b, al = jm.get(0, 0); eq(r, 0x12); eq(g, 0x34); eq(b, 0x56); eq(al, 0x78)
jm.set(1, 0, 10, 20, 30); eq(jm.get_pixel{ x = 1, y = 0 }.color, "#0a141eff")
eq(jm.get_pixel{ x = 5, y = 5 }.color, "transparent"); eq(select("#", jm.get(5, 5)), 4)
jm.draw_rect{ x = 0, y = 4, w = 8, h = 8, color = "#ff000080", filled = true }
eq(jm.get_pixel{ x = 1, y = 5 }.color, "#ff000080", "a colour is painted exactly, opacity included")
jm.draw_rect{ x = 0, y = 4, w = 2, h = 2, color = 15, filled = true }
eq(jm.get_pixel{ x = 0, y = 4 }.color, jm.palette_get{ start = 15, count = 1 }.colors[1] .. "ff", "a number picks that swatch's colour")
jm.tileset_create{ tile_w = 8, tile_h = 8 }; jm.layer_to_tiles{}
eq(jm.tiles_info{}.count >= 2, true, "tile maps work on colours too")
jm.layer_detach_tiles{}; jm.tileset_delete{}
fails(function() jm.reduce_colors{ colors = 8 } end, "reduce_colors on a true-colour document")
fails(function() jm.palette_usage{} end, "palette_usage on a true-colour document")
fails(function() jm.convert_mode{ mode = "truecolor" } end, "converting to the mode it already has")
fails(function() jm.convert_mode{ mode = "cmyk" } end, "unknown mode")
eq(jm.convert_mode{ mode = "indexed", colors = 16 }.mode, "indexed")
eq(jm.get(1, 0) >= 0, true, "pixels are indices again")

-- indexed: colours snap to the palette, and the image can be reduced to fewer
fails(function() jm.new_document{ width = 16, height = 16, mode = "cmyk", force = true } end, "unknown mode")
jm.new_document{ width = 16, height = 16, force = true }
eq(jm.get_info().mode, "indexed", "indexed unless true colour is asked for")
jm.palette_set{ index = 1, color = "#ff0000" }
jm.set(0, 0, 255, 0, 0); eq(jm.get(0, 0), 1, "rgb snaps to the nearest palette colour")
jm.set(1, 0, "#ff0000"); eq(jm.get(1, 0), 1)
jm.gradient{ x0 = 0, y0 = 4, x1 = 15, y1 = 4, colors = { "#000000", "#ffffff" }, dither = false }
jm.reduce_colors{ colors = 8, dither = "bayer" }
eq(jm.get_info().mode, "indexed"); eq(jm.palette_get{}.count <= 8, true, "quantised")

-- animation: a strip of frames cut from the image, left to right then top to bottom
jm.new_document{ width = 64, height = 32, force = true }
eq(jm.anim_info().active, false)
jm.select_rect{ x = 0, y = 0, w = 48, h = 32 }
local an = jm.anim_set{ frame_w = 16, frame_h = 16, fps = 10 }
eq(an.cols, 3); eq(an.rows, 2); eq(an.frames, 6)
eq(jm.anim_set{ frames = 5 }.frames, 5, "stop before an empty last cell")
local fr = jm.anim_control{ frame = 3, playing = false }
eq(fr.frame_x, 0); eq(fr.frame_y, 16)
eq(jm.anim_control{ step = 2 }.frame, 0, "stepping wraps")
fails(function() jm.anim_set{ frame_w = 100 } end, "frame larger than the region")
-- several named animations; a frame list gives every frame its own rectangle
eq(jm.anim_info().name, "Animation")
jm.anim_rename{ to = "walk" }
local j = jm.anim_frames{ name = "jump", frames = { {0, 0, 10, 12}, {10, 0, 14, 16, -2, -4} }, fps = 12 }
eq(j.kind, "frames"); eq(j.frames, 2); eq(j.box_w, 14); eq(j.box_h, 16)
eq(#jm.anim_list().animations, 2); eq(jm.anim_info().name, "jump")
eq(jm.anim_control{ name = "walk", frame = 1 }.frame_x, 16)
fails(function() jm.anim_frames{ frames = { {60, 0, 10, 10} } } end, "frame outside the image")
fails(function() jm.anim_rename{ to = "jump" } end, "name in use")
jm.anim_clear{ name = "jump" }; eq(#jm.anim_list().animations, 1)
jm.anim_clear{}; eq(jm.anim_info().active, false)
fails(function() jm.anim_control{ step = 1 } end, "no animation")

-- tabs: a script sees the open documents but works on the one it started on
local tl = jm.tab_list()
eq(#tl.tabs >= 1, true); eq(tl.tabs[1].active ~= nil, true); eq(jm.get_info().document.id, tl.active)
fails(function() jm.tab_new{} end, "a script cannot change tabs")
fails(function() jm.tab_select{ id = tl.active + 1 } end, "a script cannot change tabs")

-- editor state
jm.set_options{ tool = "line", brush_size = 5, mode = "smooth", symmetry = "xy" }
eq(jm.get_options().brush_size, 5)
jm.set_view{ zoom = 8, grid = true }; eq(jm.get_view().zoom, 8.0)
jm.set_colors{ fg = "#ff0000" }; jm.swap_colors{}
fails(function() jm.no_such_command{} end, "unknown command")
fails(function() jm.draw_line{ x0 = 0 } end, "missing params")
fails(function() jm.draw_line{ x0 = 0, y0 = 0, x1 = 1, y1 = 1, colour = 3 } end, "unknown param")
print("smoke ok")
return jm.get_info().mode
