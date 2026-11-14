//! The drawing canvas: view transform, overlays and interactive tools.

use crate::app::{Drag, Gui, ACCENT};
use crate::doc::*;
use crate::engine::*;
use crate::paint::{Brush, PaintMode};
use eframe::egui::{self, pos2, vec2, Color32, Pos2, Rect, Sense, Shape, Stroke, StrokeKind};
use serde_json::json;
use std::sync::Arc;

pub const ZOOMS: [f32; 19] = [0.125, 0.25, 0.5, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0, 12.0, 16.0, 20.0, 24.0, 32.0, 40.0, 48.0, 64.0];

pub fn step_zoom(z: f32, dir: i32) -> f32 {
    if dir > 0 {
        ZOOMS.iter().copied().find(|&v| v > z + 1e-3).unwrap_or(64.0)
    } else {
        ZOOMS.iter().rev().copied().find(|&v| v < z - 1e-3).unwrap_or(0.125)
    }
}

/// Refresh the composited canvas texture when the document or display palette changed.
pub fn update_texture(ctx: &egui::Context, e: &Engine, g: &mut Gui) {
    let time = ctx.input(|i| i.time);
    let cycling = e.view.cycling && e.doc.mode == ColorMode::Indexed && e.doc.palette.ramps.iter().any(|r| r.cycle);
    let blink = (time * 6.0) as u64 % 2;
    let key = (e.rev, if cycling { (time * 30.0) as u64 } else { 0 }, g.flash_now.map(|f| (f as i64) * 2 + blink as i64).unwrap_or(-1));
    if g.tex.is_some() && g.tex_key == key {
        return;
    }
    g.tex_key = key;
    let mut pal = e.doc.palette.display(cycling, time);
    if let Some(c) = g.flash_now.and_then(|f| pal.get_mut(f)) {
        *c = if blink == 0 { [255, 0, 255] } else { [255, 255, 255] };
    }
    let rgba = e.doc.composite(&pal, None, true);
    let img = egui::ColorImage::from_rgba_unmultiplied([e.doc.w as usize, e.doc.h as usize], &rgba);
    match &mut g.tex {
        Some(t) => t.set(img, egui::TextureOptions::NEAREST),
        None => g.tex = Some(ctx.load_texture("canvas", img, egui::TextureOptions::NEAREST)),
    }
}

/// Outline segments [x0, y0, x1, y1] (image coordinates) around a mask.
fn edges(mask: impl Fn(i32, i32) -> bool, w: i32, h: i32, ox: i32, oy: i32) -> Vec<[i32; 4]> {
    let m = |x: i32, y: i32| x >= 0 && y >= 0 && x < w && y < h && mask(x, y);
    let mut out = vec![];
    for y in 0..=h {
        let mut start = None;
        for x in 0..=w {
            let edge = x < w && m(x, y - 1) != m(x, y);
            match (edge, start) {
                (true, None) => start = Some(x),
                (false, Some(s)) => {
                    out.push([ox + s, oy + y, ox + x, oy + y]);
                    start = None;
                }
                _ => {}
            }
        }
    }
    for x in 0..=w {
        let mut start = None;
        for y in 0..=h {
            let edge = y < h && m(x - 1, y) != m(x, y);
            match (edge, start) {
                (true, None) => start = Some(y),
                (false, Some(s)) => {
                    out.push([ox + x, oy + s, ox + x, oy + y]);
                    start = None;
                }
                _ => {}
            }
        }
    }
    out
}

fn update_ants(e: &Engine, g: &mut Gui) {
    let d = &e.doc;
    let key = match (&d.float, &d.selection) {
        (Some(f), _) => (Arc::as_ptr(&f.data) as usize, f.sel.as_ref().map(|s| Arc::as_ptr(s) as usize).unwrap_or(1), f.x, f.y),
        (None, Some(s)) => (Arc::as_ptr(s) as usize, 0, 0, 0),
        (None, None) => (0, 0, 0, 0),
    };
    if key == g.ants_key {
        return;
    }
    g.ants_key = key;
    g.ants = match (&d.float, &d.selection) {
        (Some(f), _) => {
            let fw = f.w as i32;
            match &f.sel {
                Some(s) => edges(|x, y| s[(y * fw + x) as usize] != 0, fw, f.h as i32, f.x, f.y),
                None => edges(|_, _| true, fw, f.h as i32, f.x, f.y),
            }
        }
        (None, Some(s)) => {
            let w = d.w as i32;
            edges(|x, y| s[(y * w + x) as usize] != 0, w, d.h as i32, 0, 0)
        }
        _ => vec![],
    };
}

fn paint_tool(t: Tool) -> bool {
    matches!(t, Tool::Pencil | Tool::Eraser | Tool::Line | Tool::Rect | Tool::Ellipse)
}

fn constrain(tool: Tool, s: (i32, i32), c: (i32, i32)) -> (i32, i32) {
    let (dx, dy) = (c.0 - s.0, c.1 - s.1);
    match tool {
        Tool::Line | Tool::Gradient => {
            if dx.abs() > dy.abs() * 2 {
                (c.0, s.1)
            } else if dy.abs() > dx.abs() * 2 {
                (s.0, c.1)
            } else {
                let n = dx.abs().max(dy.abs());
                (s.0 + n * dx.signum(), s.1 + n * dy.signum())
            }
        }
        _ => {
            let n = dx.abs().max(dy.abs());
            (s.0 + n * if dx < 0 { -1 } else { 1 }, s.1 + n * if dy < 0 { -1 } else { 1 })
        }
    }
}

fn sel_op(m: egui::Modifiers) -> &'static str {
    if m.shift {
        "add"
    } else if m.alt {
        "subtract"
    } else {
        "replace"
    }
}

fn tile_cell(e: &Engine, p: (i32, i32)) -> Option<(i32, i32)> {
    let ts = e.doc.tileset.as_ref()?;
    let (c, r) = (p.0.div_euclid(ts.tw as i32), p.1.div_euclid(ts.th as i32));
    (p.0 >= 0 && p.1 >= 0 && c < (e.doc.w / ts.tw) as i32 && r < (e.doc.h / ts.th) as i32).then_some((c, r))
}

/// Begin a tool gesture at image pixel `p`.
fn press(e: &mut Engine, g: &mut Gui, button: usize, p: (i32, i32), mods: egui::Modifiers) -> Option<Drag> {
    let secondary = button == 1;
    let tool = e.o.tool;
    let target = if secondary { "bg" } else { "fg" };
    if tool == Tool::Picker || (mods.alt && (paint_tool(tool) || matches!(tool, Tool::Fill | Tool::Gradient))) {
        g.run(e, "pick_color", json!({ "x": p.0, "y": p.1, "target": target }));
        return None;
    }
    let begin = |e: &mut Engine, g: &mut Gui, o| match e.stroke_begin(o) {
        Ok(()) => true,
        Err(err) => {
            g.error(err);
            false
        }
    };
    match tool {
        Tool::Pencil | Tool::Eraser => {
            let o = e.paint_opts(secondary, tool == Tool::Eraser);
            if !begin(e, g, o) {
                return None;
            }
            e.stroke_to(p.0, p.1);
            Some(Drag::Stroke { button })
        }
        Tool::Line | Tool::Rect | Tool::Ellipse | Tool::Gradient => {
            let mut o = e.paint_opts(secondary, false);
            if tool == Tool::Gradient {
                o.mode = PaintMode::Normal;
                o.sym_x = false;
                o.sym_y = false;
            }
            begin(e, g, o).then_some(Drag::Shape { tool, start: p, button })
        }
        Tool::Fill => {
            let (color, bg) = if secondary { (e.o.bg, e.o.fg) } else { (e.o.fg, e.o.bg) };
            let mode = if e.o.mode == PaintMode::Smear { PaintMode::Normal } else { e.o.mode };
            let params = json!({
                "x": p.0, "y": p.1, "color": e.cell_json(color), "bg": e.cell_json(bg), "contiguous": e.o.contiguous,
                "tolerance": e.o.tolerance, "mode": mode.name(), "dither": e.o.dither, "dither_bg": e.o.dither_bg,
            });
            g.run(e, "fill", params);
            None
        }
        Tool::Select => Some(Drag::Select { start: p, cur: p, button }),
        Tool::Wand => {
            g.run(e, "select_wand", json!({ "x": p.0, "y": p.1, "contiguous": e.o.contiguous, "tolerance": e.o.tolerance, "op": sel_op(mods) }));
            None
        }
        Tool::Move => {
            if e.doc.layers[e.doc.active].locked {
                g.error("The active layer is locked");
                return None;
            }
            e.begin_interaction();
            if let Err(err) = e.doc.float_lift() {
                e.end_interaction();
                g.error(err);
                return None;
            }
            e.rev += 1;
            Some(Drag::Move { last: p, button })
        }
        Tool::TileStamp => {
            let Some((c, r)) = tile_cell(e, p) else {
                if e.doc.tileset.is_none() {
                    g.error("Create a tile set first (Tiles tab)");
                }
                return None;
            };
            if secondary {
                if let Some(v) = g.run(e, "tile_get_cell", json!({ "col": c, "row": r })) {
                    e.o.active_tile = v["tile"].as_u64().unwrap_or(0) as u16;
                }
                return None;
            }
            e.begin_interaction();
            if g.run(e, "tile_set_cell", json!({ "col": c, "row": r })).is_none() {
                e.end_interaction();
                return None;
            }
            Some(Drag::Tiles { button })
        }
        Tool::Picker | Tool::Pan => None,
    }
}

pub fn show(ui: &mut egui::Ui, e: &mut Engine, g: &mut Gui) {
    let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
    let rect = resp.rect;
    g.canvas_rect = rect;
    let (dw, dh) = (e.doc.w as f32, e.doc.h as f32);
    if e.view.fit && rect.width() > 80.0 {
        let fit = ((rect.width() - 48.0) / dw).min((rect.height() - 48.0) / dh);
        e.view.zoom = ZOOMS.iter().rev().copied().find(|&z| z <= fit).unwrap_or(0.125);
        e.view.cx = dw / 2.0;
        e.view.cy = dh / 2.0;
        e.view.fit = false;
    }

    let (pos, mods, space, pressed, down, scroll, pinch, time) = ui.input(|i| {
        let b = [egui::PointerButton::Primary, egui::PointerButton::Secondary, egui::PointerButton::Middle];
        (i.pointer.latest_pos(), i.modifiers, i.key_down(egui::Key::Space), b.map(|b| i.pointer.button_pressed(b)), b.map(|b| i.pointer.button_down(b)), i.smooth_scroll_delta, i.zoom_delta(), i.time)
    });
    let hovered = resp.hovered() || resp.contains_pointer();

    // ---- zoom about the pointer
    if hovered && g.drag.is_none() {
        g.zoom_accum += scroll.y / 40.0 + pinch.ln() * 4.0;
        let dir = if g.zoom_accum >= 1.0 { 1 } else if g.zoom_accum <= -1.0 { -1 } else { 0 };
        if dir != 0 {
            g.zoom_accum = 0.0;
            let anchor = pos.unwrap_or(rect.center());
            let old = e.view.zoom;
            let new = step_zoom(old, dir);
            let off = anchor - rect.center();
            e.view.cx += off.x / old - off.x / new;
            e.view.cy += off.y / old - off.y / new;
            e.view.zoom = new;
        }
    }
    let zoom = e.view.zoom;
    let origin = (rect.center() - vec2(e.view.cx, e.view.cy) * zoom).round();
    let to_screen = |x: f32, y: f32| origin + vec2(x, y) * zoom;
    let to_image = |p: Pos2| (((p.x - origin.x) / zoom).floor() as i32, ((p.y - origin.y) / zoom).floor() as i32);
    let cur = pos.map(to_image);
    g.hover = if hovered { cur } else { None };
    // A gesture must always be able to finish, even if the pointer has left the window.
    let pos = pos.or(g.last_pos);
    g.last_pos = pos;
    let cur = pos.map(to_image);

    // ---- gestures
    if g.drag.is_none() && hovered {
        if let (Some(button), Some(p), Some(sp)) = (pressed.iter().position(|b| *b), cur, pos) {
            ui.memory_mut(|m| {
                if let Some(id) = m.focused() {
                    m.surrender_focus(id);
                }
            });
            g.drag = if button == 2 || space || e.o.tool == Tool::Pan {
                Some(Drag::Pan { last: sp, button })
            } else {
                press(e, g, button, p, mods)
            };
        }
    }
    if let (Some(drag), Some(sp), Some(p)) = (g.drag.as_mut(), pos, cur) {
        let released = match drag {
            Drag::Pan { button, .. } | Drag::Stroke { button } | Drag::Shape { button, .. } | Drag::Select { button, .. } | Drag::Move { button, .. } | Drag::Tiles { button } => !down[*button],
        };
        match drag {
            Drag::Pan { last, .. } => {
                let d = sp - *last;
                e.view.cx -= d.x / zoom;
                e.view.cy -= d.y / zoom;
                *last = sp;
            }
            Drag::Stroke { .. } => {
                e.stroke_to(p.0, p.1);
                if released {
                    e.stroke_end();
                }
            }
            Drag::Shape { tool, start, button } => {
                let end = if mods.shift { constrain(*tool, *start, p) } else { p };
                match tool {
                    Tool::Line => e.stroke_shape(ShapeKind::Line, start.0, start.1, end.0, end.1, false),
                    Tool::Rect => e.stroke_shape(ShapeKind::Rect, start.0, start.1, end.0, end.1, e.o.filled),
                    Tool::Ellipse => e.stroke_shape(ShapeKind::Ellipse, start.0, start.1, end.0, end.1, e.o.filled),
                    _ => {
                        let stops = e.gradient_stops(*button == 1);
                        e.stroke_gradient(start.0, start.1, end.0, end.1, &stops, e.o.gradient_radial, e.o.gradient_dither);
                    }
                }
                if released {
                    e.stroke_end();
                }
            }
            Drag::Select { start, cur, .. } => {
                *cur = if mods.shift && !released { constrain(Tool::Rect, *start, p) } else { p };
                if released {
                    let (s, c) = (*start, *cur);
                    if s == c {
                        g.run(e, "select_none", json!({}));
                    } else {
                        let (x0, y0) = (s.0.min(c.0), s.1.min(c.1));
                        let params = json!({ "x": x0, "y": y0, "w": (s.0 - c.0).abs() + 1, "h": (s.1 - c.1).abs() + 1, "op": sel_op(mods) });
                        g.run(e, "select_rect", params);
                    }
                }
            }
            Drag::Move { last, .. } => {
                if let Some(f) = e.doc.float.as_mut() {
                    if p != *last {
                        f.x += p.0 - last.0;
                        f.y += p.1 - last.1;
                        *last = p;
                        e.rev += 1;
                    }
                }
                if released {
                    e.end_interaction();
                }
            }
            Drag::Tiles { .. } => {
                if let Some((c, r)) = tile_cell(e, p) {
                    let _ = e.exec("tile_set_cell", &json!({ "col": c, "row": r }));
                }
                if released {
                    e.end_interaction();
                }
            }
        }
        if released {
            g.drag = None;
        }
    }

    // ---- paint
    update_texture(ui.ctx(), e, g);
    painter.rect_filled(rect, 0.0, Color32::from_rgb(22, 23, 27));
    let img_rect = Rect::from_min_size(origin, vec2(dw, dh) * zoom);
    painter.rect_filled(img_rect.expand(1.0).translate(vec2(0.0, 3.0)), 2.0, Color32::from_black_alpha(90));
    if let Some(t) = &g.tex {
        painter.image(t.id(), img_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    let vis = img_rect.intersect(rect);
    let (ix0, iy0) = ((((vis.left() - origin.x) / zoom).floor() as i32).max(0), (((vis.top() - origin.y) / zoom).floor() as i32).max(0));
    let (ix1, iy1) = ((((vis.right() - origin.x) / zoom).ceil() as i32).min(e.doc.w as i32), (((vis.bottom() - origin.y) / zoom).ceil() as i32).min(e.doc.h as i32));
    let grid = |sx: i32, sy: i32, stroke: Stroke| {
        if sx < 1 || sy < 1 || vis.width() <= 0.0 {
            return;
        }
        let mut x = ix0.div_euclid(sx) * sx;
        while x <= ix1 {
            if x >= ix0 {
                painter.vline(origin.x + x as f32 * zoom, vis.y_range(), stroke);
            }
            x += sx;
        }
        let mut y = iy0.div_euclid(sy) * sy;
        while y <= iy1 {
            if y >= iy0 {
                painter.hline(vis.x_range(), origin.y + y as f32 * zoom, stroke);
            }
            y += sy;
        }
    };
    if e.view.pixel_grid && zoom >= 8.0 {
        grid(1, 1, Stroke::new(1.0_f32, Color32::from_black_alpha(48)));
    }
    if e.view.grid && e.view.grid_w as f32 * zoom >= 4.0 {
        grid(e.view.grid_w as i32, e.view.grid_h as i32, Stroke::new(1.0_f32, Color32::from_white_alpha(52)));
    }
    if let Some(ts) = &e.doc.tileset {
        if e.view.tile_grid && ts.tw as f32 * zoom >= 6.0 {
            grid(ts.tw as i32, ts.th as i32, Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.5)));
        }
        let mapped = e.doc.layers.get(e.doc.active).and_then(|l| l.map.as_ref()).or_else(|| e.doc.layers.iter().find_map(|l| l.map.as_ref()));
        if let (true, Some(map)) = (e.view.tile_ids && ts.tw as f32 * zoom >= 22.0, mapped) {
            let (tw, th) = (ts.tw as i32, ts.th as i32);
            let cols = e.doc.w as i32 / tw;
            for r in iy0 / th..=(iy1 - 1).max(0) / th {
                for c in ix0 / tw..=(ix1 - 1).max(0) / tw {
                    if let Some(id) = (c < cols).then(|| map.get((r * cols + c) as usize)).flatten() {
                        let at = to_screen((c * tw) as f32, (r * th) as f32) + vec2(3.0, 2.0);
                        painter.text(at + vec2(1.0, 1.0), egui::Align2::LEFT_TOP, id, egui::FontId::monospace(10.0), Color32::BLACK);
                        painter.text(at, egui::Align2::LEFT_TOP, id, egui::FontId::monospace(10.0), Color32::WHITE);
                    }
                }
            }
        }
    }
    painter.rect_stroke(img_rect, 0.0, Stroke::new(1.0_f32, Color32::from_white_alpha(40)), StrokeKind::Outside);

    let guide = Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 120, 200, 150));
    if e.o.sym_x {
        painter.vline(origin.x + dw * zoom / 2.0, img_rect.y_range(), guide);
    }
    if e.o.sym_y {
        painter.hline(img_rect.x_range(), origin.y + dh * zoom / 2.0, guide);
    }

    // ---- selection outline (marching ants)
    update_ants(e, g);
    let fancy = g.ants.len() < 2500;
    let phase = (time * 14.0 % 8.0) as f32;
    let mut shapes = vec![];
    for s in &g.ants {
        let (a, b) = (to_screen(s[0] as f32, s[1] as f32), to_screen(s[2] as f32, s[3] as f32));
        if !rect.intersects(Rect::from_two_pos(a, b).expand(1.0)) {
            continue;
        }
        shapes.push(Shape::line_segment([a, b], Stroke::new(1.0_f32, Color32::BLACK)));
        if fancy {
            shapes.extend(Shape::dashed_line_with_offset(&[a, b], Stroke::new(1.0_f32, Color32::WHITE), &[4.0], &[4.0], phase));
        }
    }
    painter.extend(shapes);
    if let Some(Drag::Select { start, cur, .. }) = &g.drag {
        let r = Rect::from_two_pos(to_screen(start.0.min(cur.0) as f32, start.1.min(cur.1) as f32), to_screen(start.0.max(cur.0) as f32 + 1.0, start.1.max(cur.1) as f32 + 1.0));
        painter.rect_stroke(r, 0.0, Stroke::new(1.0_f32, Color32::BLACK), StrokeKind::Outside);
        painter.rect_stroke(r, 0.0, Stroke::new(1.0_f32, Color32::WHITE), StrokeKind::Inside);
    }

    // ---- cursor
    if let (true, Some(p)) = (hovered || g.drag.is_some(), cur) {
        let tool = e.o.tool;
        let panning = matches!(g.drag, Some(Drag::Pan { .. })) || space || tool == Tool::Pan;
        ui.ctx().set_cursor_icon(if panning {
            if down.iter().any(|d| *d) { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab }
        } else if tool == Tool::Move {
            egui::CursorIcon::Move
        } else {
            egui::CursorIcon::Crosshair
        });
        if !panning && paint_tool(tool) && zoom >= 2.0 {
            let b = e.o.custom_brush.clone().unwrap_or_else(|| Brush::new(e.o.brush_size, e.o.brush_shape));
            let (x0, y0) = (p.0 - b.w / 2, p.1 - b.h / 2);
            let set = |x: i32, y: i32| x >= 0 && y >= 0 && x < b.w && y < b.h && b.mask[(y * b.w + x) as usize];
            let mut segs = vec![];
            for y in 0..b.h {
                for x in 0..b.w {
                    if !set(x, y) {
                        continue;
                    }
                    let (fx, fy) = ((x0 + x) as f32, (y0 + y) as f32);
                    if !set(x, y - 1) {
                        segs.push([to_screen(fx, fy), to_screen(fx + 1.0, fy)]);
                    }
                    if !set(x, y + 1) {
                        segs.push([to_screen(fx, fy + 1.0), to_screen(fx + 1.0, fy + 1.0)]);
                    }
                    if !set(x - 1, y) {
                        segs.push([to_screen(fx, fy), to_screen(fx, fy + 1.0)]);
                    }
                    if !set(x + 1, y) {
                        segs.push([to_screen(fx + 1.0, fy), to_screen(fx + 1.0, fy + 1.0)]);
                    }
                }
            }
            painter.extend(segs.iter().map(|s| Shape::line_segment(*s, Stroke::new(3.0_f32, Color32::from_black_alpha(110)))));
            painter.extend(segs.iter().map(|s| Shape::line_segment(*s, Stroke::new(1.0_f32, Color32::WHITE))));
        } else if tool == Tool::TileStamp && !panning {
            if let (Some((c, r)), Some(ts)) = (tile_cell(e, p), &e.doc.tileset) {
                let a = to_screen((c * ts.tw as i32) as f32, (r * ts.th as i32) as f32);
                painter.rect_stroke(Rect::from_min_size(a, vec2(ts.tw as f32, ts.th as f32) * zoom), 0.0, Stroke::new(2.0_f32, ACCENT), StrokeKind::Inside);
            }
        }
    }
}
