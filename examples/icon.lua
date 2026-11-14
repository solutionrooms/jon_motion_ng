-- Draws the Jon Motion app icon. Run from the repo root:
--   jon_motion run examples/icon.lua
jm.new_document{ width = 32, height = 32, force = true }

-- Rounded tile with a dithered indigo gradient.
jm.select_rect{ x = 2, y = 0, w = 28, h = 32 }
jm.select_rect{ x = 0, y = 2, w = 32, h = 28, op = "add" }
jm.select_rect{ x = 1, y = 1, w = 30, h = 30, op = "add" }
jm.gradient{ x0 = 0, y0 = 0, x1 = 31, y1 = 31, colors = { 196, 194, 193, 192 }, dither = true }
jm.select_none{}

-- Four palette swatches, each shaded with its own ramp.
local swatches = { { 5, 5, 16 }, { 17, 5, 64 }, { 5, 17, 96 }, { 17, 17, 160 } }
for _, s in ipairs(swatches) do
  local x, y, ramp = s[1], s[2], s[3]
  jm.draw_rect{ x = x, y = y, w = 10, h = 10, color = ramp + 9, filled = true }
  jm.draw_line{ x0 = x, y0 = y, x1 = x + 9, y1 = y, color = ramp + 12 }
  jm.draw_line{ x0 = x, y0 = y, x1 = x, y1 = y + 9, color = ramp + 12 }
  jm.draw_line{ x0 = x, y0 = y + 9, x1 = x + 9, y1 = y + 9, color = ramp + 6 }
  jm.draw_line{ x0 = x + 9, y0 = y, x1 = x + 9, y1 = y + 9, color = ramp + 6 }
end

-- A pencil across the corner.
jm.layer_add{ name = "Pencil" }
jm.draw_line{ x0 = 12, y0 = 27, x1 = 26, y1 = 13, color = 2, size = 5, shape = "round" }
jm.draw_line{ x0 = 13, y0 = 26, x1 = 26, y1 = 13, color = 70, size = 3, shape = "round" }
jm.draw_line{ x0 = 14, y0 = 24, x1 = 25, y1 = 13, color = 76 }
jm.draw_line{ x0 = 24, y0 = 15, x1 = 27, y1 = 12, color = 245, size = 3, shape = "round" }
jm.put_pixels{ x = 9, y = 26, rows = { { -1, -1, 13, 13 }, { -1, 13, 13, 13 }, { 2, 13, 13, -1 }, { 2, 2, -1, -1 } } }

jm.export_png{ path = "assets/icon.png", scale = 16 }
jm.export_png{ path = "assets/icon_1024.png", scale = 32 }
print("icon written")
