//! Toolbar, tool options, status bar and the docked panels.

use crate::app::{Gui, ACCENT};
use crate::canvas;
use crate::commands;
use crate::doc::*;
use crate::engine::*;
use crate::paint::{BrushShape, PaintMode};
use crate::remote;
use eframe::egui::{self, pos2, vec2, Color32, Painter, Pos2, Rect, RichText, Sense, Stroke, StrokeKind};
use serde_json::json;

fn c32(c: [u8; 4]) -> Color32 {
    Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
}

/// A colour chip; transparency shows a checker.
fn swatch(p: &Painter, r: Rect, c: [u8; 4]) {
    if c[3] < 255 {
        p.rect_filled(r, 2.0, Color32::from_gray(200));
        let h = r.size() / 2.0;
        p.rect_filled(Rect::from_min_size(r.min, h), 0.0, Color32::from_gray(120));
        p.rect_filled(Rect::from_min_size(r.min + h, h), 0.0, Color32::from_gray(120));
    }
    p.rect_filled(r, 2.0, c32(c));
}

fn cell_label(e: &Engine, c: Cell) -> String {
    if cell_empty(c) {
        return "transparent".into();
    }
    if e.doc.rgba() {
        let c = unpack(c);
        return if c[3] == 255 { hex6([c[0], c[1], c[2]]) } else { hex8(c) };
    }
    format!("{} {}", c, hex6(e.doc.palette.colors.get(c as usize).copied().unwrap_or([0, 0, 0])))
}

// ------------------------------------------------------------------ tool icons

fn icon(p: &Painter, r: Rect, tool: Tool, col: Color32) {
    let at = |x: f32, y: f32| pos2(r.left() + r.width() * x, r.top() + r.height() * y);
    let s = Stroke::new(1.6_f32, col);
    let line = |a: Pos2, b: Pos2| {
        p.line_segment([a, b], s);
    };
    match tool {
        Tool::Pencil => {
            p.line_segment([at(0.3, 0.7), at(0.85, 0.15)], Stroke::new(3.4_f32, col));
            p.add(egui::Shape::convex_polygon(vec![at(0.08, 0.92), at(0.16, 0.66), at(0.34, 0.84)], col, Stroke::NONE));
        }
        Tool::Eraser => {
            let pts = vec![at(0.45, 0.12), at(0.92, 0.5), at(0.55, 0.9), at(0.08, 0.52)];
            p.add(egui::Shape::closed_line(pts, s));
            line(at(0.28, 0.32), at(0.74, 0.7));
        }
        Tool::Line => {
            line(at(0.14, 0.86), at(0.86, 0.14));
            p.circle_filled(at(0.14, 0.86), 2.2, col);
            p.circle_filled(at(0.86, 0.14), 2.2, col);
        }
        Tool::Rect => {
            p.rect_stroke(Rect::from_two_pos(at(0.1, 0.2), at(0.9, 0.8)), 1.0, s, StrokeKind::Middle);
        }
        Tool::Ellipse => {
            p.add(egui::Shape::ellipse_stroke(r.center(), vec2(r.width() * 0.44, r.height() * 0.34), s));
        }
        Tool::Fill => {
            p.add(egui::Shape::closed_line(vec![at(0.42, 0.1), at(0.84, 0.52), at(0.46, 0.9), at(0.06, 0.5)], s));
            p.add(egui::Shape::convex_polygon(vec![at(0.12, 0.5), at(0.8, 0.5), at(0.46, 0.86)], col, Stroke::NONE));
            p.circle_filled(at(0.92, 0.82), 2.0, col);
        }
        Tool::Gradient => {
            for i in 0..5 {
                let x = 0.08 + i as f32 * 0.17;
                p.rect_filled(Rect::from_two_pos(at(x, 0.18), at(x + 0.15, 0.82)), 0.0, col.gamma_multiply(0.2 + 0.2 * i as f32));
            }
        }
        Tool::Picker => {
            p.line_segment([at(0.14, 0.86), at(0.6, 0.4)], Stroke::new(2.6_f32, col));
            p.circle_filled(at(0.72, 0.28), r.width() * 0.2, col);
        }
        Tool::Select => {
            p.extend(egui::Shape::dashed_line(&[at(0.1, 0.18), at(0.9, 0.18), at(0.9, 0.82), at(0.1, 0.82), at(0.1, 0.18)], s, 3.0, 2.4));
        }
        Tool::Wand => {
            line(at(0.1, 0.9), at(0.58, 0.42));
            for (dx, dy) in [(0.0, 0.2), (0.2, 0.0), (0.14, 0.14), (-0.14, 0.14)] {
                line(at(0.7 - dx, 0.3 - dy), at(0.7 + dx, 0.3 + dy));
            }
        }
        Tool::Move => {
            line(at(0.5, 0.06), at(0.5, 0.94));
            line(at(0.06, 0.5), at(0.94, 0.5));
            for (x, y, dx, dy) in [(0.5, 0.06, 0.14, 0.16), (0.5, 0.94, 0.14, -0.16)] {
                line(at(x, y), at(x - dx, y + dy));
                line(at(x, y), at(x + dx, y + dy));
            }
            for (x, y, dx, dy) in [(0.06, 0.5, 0.16, 0.14), (0.94, 0.5, -0.16, 0.14)] {
                line(at(x, y), at(x + dx, y - dy));
                line(at(x, y), at(x + dx, y + dy));
            }
        }
        Tool::TileStamp => {
            for (x, y) in [(0.08, 0.08), (0.56, 0.08), (0.08, 0.56)] {
                p.rect_stroke(Rect::from_two_pos(at(x, y), at(x + 0.36, y + 0.36)), 1.0, s, StrokeKind::Middle);
            }
            p.rect_filled(Rect::from_two_pos(at(0.56, 0.56), at(0.92, 0.92)), 1.0, col);
        }
        Tool::Pan => {
            p.rect_filled(Rect::from_two_pos(at(0.24, 0.5), at(0.8, 0.92)), 3.0, col);
            for x in [0.3, 0.44, 0.58, 0.72] {
                p.line_segment([at(x, 0.56), at(x, if x == 0.44 || x == 0.58 { 0.1 } else { 0.2 })], Stroke::new(2.4_f32, col));
            }
            p.line_segment([at(0.26, 0.74), at(0.08, 0.52)], Stroke::new(2.4_f32, col));
        }
    }
}

pub fn toolbar(ui: &mut egui::Ui, e: &mut Engine, _g: &mut Gui) {
    ui.spacing_mut().item_spacing.y = 3.0;
    ui.vertical_centered(|ui| {
        ui.add_space(4.0);
        for t in Tool::ALL {
            let (rect, resp) = ui.allocate_exact_size(vec2(34.0, 30.0), Sense::click());
            let on = e.o.tool == t;
            let bg = if on { Color32::from_rgb(58, 104, 196) } else if resp.hovered() { Color32::from_rgb(56, 58, 68) } else { Color32::TRANSPARENT };
            ui.painter().rect_filled(rect, 6.0, bg);
            let size = 17.0;
            icon(ui.painter(), Rect::from_center_size(rect.center(), vec2(size, size)), t, if on { Color32::WHITE } else { Color32::from_gray(196) });
            let resp = resp.on_hover_text(format!("{}\nRight-click for options", tool_title(e, t)));
            if resp.clicked() || resp.secondary_clicked() {
                e.o.tool = t;
            }
            resp.context_menu(|ui| tool_menu(ui, e, t));
        }
        ui.add_space(10.0);
        // Foreground over background; click to swap.
        let (rect, resp) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::click());
        let p = ui.painter();
        let (fg, bg) = (e.doc.to_rgba(e.o.fg), e.doc.to_rgba(e.o.bg));
        let b = Rect::from_min_size(rect.min + vec2(13.0, 13.0), vec2(22.0, 22.0));
        let f = Rect::from_min_size(rect.min + vec2(1.0, 1.0), vec2(22.0, 22.0));
        swatch(p, b, bg);
        p.rect_stroke(b, 2.0, Stroke::new(1.0_f32, Color32::from_gray(150)), StrokeKind::Outside);
        swatch(p, f, fg);
        p.rect_stroke(f, 2.0, Stroke::new(1.5_f32, Color32::WHITE), StrokeKind::Outside);
        if resp.on_hover_text(format!("Foreground: {}\nBackground: {}\nClick or X to swap", cell_label(e, e.o.fg), cell_label(e, e.o.bg))).clicked() {
            std::mem::swap(&mut e.o.fg, &mut e.o.bg);
        }
    });
}

/// A tool's name with its key in the active keymap, when it has one.
fn tool_title(e: &Engine, t: Tool) -> String {
    match crate::keys::tool_key(e.view.keymap, t) {
        Some(k) => format!("{}  ({k})", t.label()),
        None => t.label().to_string(),
    }
}

/// Right-click menu for a toolbar button: the same settings as the options bar, for that tool.
fn tool_menu(ui: &mut egui::Ui, e: &mut Engine, tool: Tool) {
    ui.set_min_width(190.0);
    ui.label(RichText::new(tool_title(e, tool)).strong());
    ui.separator();
    let paints = matches!(tool, Tool::Pencil | Tool::Eraser | Tool::Line | Tool::Rect | Tool::Ellipse);
    if paints {
        if let Some((w, h)) = e.o.custom_brush.as_ref().map(|b| (b.w, b.h)) {
            ui.horizontal(|ui| {
                ui.label(format!("Custom brush {w}x{h}"));
                if ui.small_button("x").on_hover_text("Back to the standard brush (Esc)").clicked() {
                    e.o.custom_brush = None;
                }
            });
        } else {
            ui.horizontal(|ui| {
                ui.label("Size");
                ui.add(egui::DragValue::new(&mut e.o.brush_size).range(1..=128).speed(0.2)).on_hover_text("[ and ]");
                ui.selectable_value(&mut e.o.brush_shape, BrushShape::Round, "Round");
                ui.selectable_value(&mut e.o.brush_shape, BrushShape::Square, "Square");
            });
        }
    }
    if (paints && tool != Tool::Eraser) || tool == Tool::Fill {
        ui.label("Mode");
        // A plain list: a drop-down inside a menu would close the menu.
        egui::Grid::new("tool_menu_modes").num_columns(2).spacing([6.0, 2.0]).show(ui, |ui| {
            for (i, m) in PaintMode::ALL.into_iter().enumerate() {
                ui.selectable_value(&mut e.o.mode, m, m.name()).on_hover_text(m.hint());
                if i % 2 == 1 {
                    ui.end_row();
                }
            }
        });
        if e.o.mode == PaintMode::Dither {
            ui.horizontal(|ui| {
                ui.add(egui::Slider::new(&mut e.o.dither, 1..=15).show_value(false)).on_hover_text(format!("Dither density {}/16", e.o.dither));
                ui.checkbox(&mut e.o.dither_bg, "2-colour").on_hover_text("Paint the background colour in the gaps");
            });
        }
    }
    match tool {
        Tool::Rect | Tool::Ellipse => {
            ui.checkbox(&mut e.o.filled, "Filled");
        }
        Tool::Pencil => {
            ui.checkbox(&mut e.o.pixel_perfect, "Pixel perfect").on_hover_text("Remove corner pixels from 1 px freehand lines");
        }
        Tool::Fill | Tool::Wand => {
            ui.checkbox(&mut e.o.contiguous, "Contiguous");
        }
        Tool::Gradient => {
            ui.checkbox(&mut e.o.gradient_radial, "Radial");
            ui.checkbox(&mut e.o.gradient_dither, "Dither");
            ui.label("Colours");
            ui.selectable_value(&mut e.o.active_ramp, None, "FG to BG");
            for (i, r) in e.doc.palette.ramps.iter().enumerate() {
                ui.selectable_value(&mut e.o.active_ramp, Some(i), &r.name);
            }
        }
        Tool::Picker => {
            ui.weak("Alt-click with a drawing tool also picks.");
        }
        Tool::Select => {
            ui.weak("Shift adds, Alt subtracts.");
        }
        Tool::Move => {
            ui.weak("Drag to move the selection or layer.\nEnter commits, Esc cancels.");
        }
        Tool::TileStamp => {
            ui.weak(format!("Stamping tile {}.\nRight-click the canvas to pick a tile.", e.o.active_tile));
        }
        Tool::Pan => {
            ui.weak("Middle-drag pans with any tool.");
        }
        _ => {}
    }
    if paints {
        ui.horizontal(|ui| {
            ui.label("Mirror");
            ui.toggle_value(&mut e.o.sym_x, "X");
            ui.toggle_value(&mut e.o.sym_y, "Y");
        });
    }
    if e.doc.tileset.is_some() && (paints || matches!(tool, Tool::Fill | Tool::Gradient)) {
        ui.horizontal(|ui| {
            ui.label("Tiles");
            ui.selectable_value(&mut e.o.tile_mode, TileMode::Auto, "Auto").on_hover_text("Edited cells become new tiles (or match existing ones)");
            ui.selectable_value(&mut e.o.tile_mode, TileMode::Sync, "Sync").on_hover_text("Editing a cell edits its tile everywhere it is used");
        });
    }
}

// ------------------------------------------------------------------ options bar

pub fn options(ui: &mut egui::Ui, e: &mut Engine, g: &mut Gui) {
    ui.horizontal_centered(|ui| {
        let tool = e.o.tool;
        ui.label(RichText::new(tool.label()).strong());
        ui.separator();
        let paints = matches!(tool, Tool::Pencil | Tool::Eraser | Tool::Line | Tool::Rect | Tool::Ellipse);
        if paints {
            if let Some(b) = &e.o.custom_brush {
                ui.label(format!("Custom brush {}x{}", b.w, b.h));
                if ui.small_button("x").on_hover_text("Back to the standard brush (Esc)").clicked() {
                    e.o.custom_brush = None;
                }
            } else {
                ui.label("Size");
                ui.add(egui::DragValue::new(&mut e.o.brush_size).range(1..=128).speed(0.2)).on_hover_text("[ and ]");
                ui.selectable_value(&mut e.o.brush_shape, BrushShape::Round, "Round");
                ui.selectable_value(&mut e.o.brush_shape, BrushShape::Square, "Square");
            }
            ui.separator();
        }
        if (paints && tool != Tool::Eraser) || tool == Tool::Fill {
            ui.label("Mode");
            egui::ComboBox::from_id_salt("mode").width(92.0).selected_text(e.o.mode.name()).show_ui(ui, |ui| {
                for m in PaintMode::ALL {
                    ui.selectable_value(&mut e.o.mode, m, m.name()).on_hover_text(m.hint());
                }
            });
            if e.o.mode == PaintMode::Dither {
                ui.add(egui::Slider::new(&mut e.o.dither, 1..=15).show_value(false)).on_hover_text(format!("Dither density {}/16", e.o.dither));
                ui.checkbox(&mut e.o.dither_bg, "2-colour").on_hover_text("Paint the background colour in the gaps");
            }
            ui.separator();
        }
        match tool {
            Tool::Rect | Tool::Ellipse => {
                ui.checkbox(&mut e.o.filled, "Filled");
            }
            Tool::Pencil => {
                ui.checkbox(&mut e.o.pixel_perfect, "Pixel perfect").on_hover_text("Remove corner pixels from 1 px freehand lines");
            }
            Tool::Fill | Tool::Wand => {
                ui.checkbox(&mut e.o.contiguous, "Contiguous");
            }
            Tool::Gradient => {
                ui.checkbox(&mut e.o.gradient_radial, "Radial");
                ui.checkbox(&mut e.o.gradient_dither, "Dither");
                let name = e.o.active_ramp.and_then(|r| e.doc.palette.ramps.get(r)).map(|r| r.name.clone()).unwrap_or_else(|| "FG to BG".into());
                egui::ComboBox::from_id_salt("gramp").selected_text(name).show_ui(ui, |ui| {
                    ui.selectable_value(&mut e.o.active_ramp, None, "FG to BG");
                    for (i, r) in e.doc.palette.ramps.iter().enumerate() {
                        ui.selectable_value(&mut e.o.active_ramp, Some(i), &r.name);
                    }
                });
            }
            Tool::Select => {
                ui.weak("Shift adds, Alt subtracts");
            }
            Tool::Move => {
                ui.weak("Drag to move the selection or layer. Enter commits, Esc cancels.");
            }
            Tool::TileStamp => {
                ui.weak(format!("Tile {} - right-click the canvas to pick a tile", e.o.active_tile));
            }
            _ => {}
        }
        if paints {
            ui.separator();
            ui.label("Mirror");
            ui.toggle_value(&mut e.o.sym_x, "X");
            ui.toggle_value(&mut e.o.sym_y, "Y");
        }
        if e.doc.tileset.is_some() {
            ui.separator();
            ui.label("Tiles");
            ui.selectable_value(&mut e.o.tile_mode, TileMode::Auto, "Auto").on_hover_text("Edited cells become new tiles (or match existing ones)");
            ui.selectable_value(&mut e.o.tile_mode, TileMode::Sync, "Sync").on_hover_text("Editing a cell edits its tile everywhere it is used");
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("Fit").clicked() {
                e.view.fit = true;
            }
            if ui.small_button("+").clicked() {
                crate::app::zoom(e, 1);
            }
            ui.label(format!("{}%", (e.view.zoom * 100.0).round()));
            if ui.small_button("-").clicked() {
                crate::app::zoom(e, -1);
            }
            ui.toggle_value(&mut e.view.grid, "Grid");
            let _ = g;
        });
    });
}

// ------------------------------------------------------------------ status bar

pub fn status(ui: &mut egui::Ui, e: &mut Engine, g: &mut Gui) {
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 14.0;
        if e.doc.rgba() {
            ui.weak(format!("{} x {}", e.doc.w, e.doc.h));
            ui.label(RichText::new("True colour").color(Color32::from_rgb(240, 190, 90))).on_hover_text("Every pixel is its own colour. Image > Convert to Indexed makes it a palette image.");
        } else {
            ui.weak(format!("{} x {}  {} colours", e.doc.w, e.doc.h, e.doc.palette.colors.len()));
        }
        match g.hover {
            Some((x, y)) => {
                ui.monospace(format!("{x:>4},{y:<4}"));
                if let Some(c) = e.raw_get(x as i64, y as i64) {
                    ui.weak(cell_label(e, c));
                }
            }
            None => {
                ui.monospace("    ,    ");
            }
        }
        if let Some((x0, y0, x1, y1)) = e.doc.sel_bounds() {
            ui.weak(format!("sel {}x{} at {x0},{y0}", x1 - x0 + 1, y1 - y0 + 1));
        }
        if !g.status.is_empty() {
            let col = if g.status_err { Color32::from_rgb(255, 120, 110) } else { Color32::from_gray(200) };
            ui.add(egui::Label::new(RichText::new(&g.status).color(col)).truncate());
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            match g.mcp_port {
                Some(p) => {
                    if ui.add(egui::Label::new(RichText::new(format!("MCP :{p}")).color(Color32::from_rgb(120, 210, 150))).sense(Sense::click())).on_hover_text("AI clients can drive this window. Click for setup.").clicked() {
                        g.dialog = crate::app::Dialog::Mcp;
                    }
                }
                None => {
                    ui.weak("MCP off");
                }
            }
            ui.weak(format!("undo {}", e.undo.len()));
        });
    });
}

// ------------------------------------------------------------------ palette

pub fn palette(ui: &mut egui::Ui, e: &mut Engine, g: &mut Gui) {
    let n = e.doc.palette.colors.len();
    if g.usage_rev != e.rev {
        g.usage_rev = e.rev;
        g.usage = vec![0; n];
        for l in &e.doc.layers {
            for &c in l.data.iter() {
                if let Some(u) = g.usage.get_mut(c as usize) {
                    *u += 1;
                }
            }
        }
    }
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        for (label, cell, is_fg) in [("FG", e.o.fg, true), ("BG", e.o.bg, false)] {
            let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
            swatch(ui.painter(), r, e.doc.to_rgba(cell));
            ui.painter().rect_stroke(r, 2.0, Stroke::new(1.0_f32, Color32::from_gray(if is_fg { 255 } else { 140 })), StrokeKind::Outside);
            resp.on_hover_text(format!("{label}: {}", cell_label(e, cell)));
            ui.label(RichText::new(format!("{label} {}", cell_label(e, cell))).small());
        }
    });
    let rgba = e.doc.rgba();
    if rgba {
        // True colour: any colour can be painted, so the colours are edited here directly
        // and the grid below is a set of swatches to pick from.
        ui.horizontal(|ui| {
            for (label, is_fg) in [("FG", true), ("BG", false)] {
                let cell = if is_fg { e.o.fg } else { e.o.bg };
                let mut c = if cell_empty(cell) { [0, 0, 0, 0] } else { unpack(cell) };
                ui.label(RichText::new(label).small());
                if ui.color_edit_button_srgba_unmultiplied(&mut c).on_hover_text("Pick any colour, with its opacity (0 is transparent)").changed() {
                    *(if is_fg { &mut e.o.fg } else { &mut e.o.bg }) = pack(c);
                }
            }
        });
        ui.label(RichText::new("True colour: the palette is a set of swatches. Editing a swatch does not change the image.").small().weak());
    }
    ui.add_space(2.0);

    // ---- colour grid
    let cols = 16usize;
    let cell = ((ui.available_width() - 2.0) / cols as f32).floor().clamp(9.0, 26.0);
    let rows = n.div_ceil(cols);
    let (rect, resp) = ui.allocate_exact_size(vec2(cell * cols as f32, cell * rows as f32), Sense::click());
    let p = ui.painter_at(rect.expand(2.0));
    let hover = resp.hover_pos().map(|pp| ((pp.y - rect.top()) / cell) as usize * cols + ((pp.x - rect.left()) / cell) as usize).filter(|i| *i < n);
    let cell_rect = |i: usize| Rect::from_min_size(rect.min + vec2((i % cols) as f32 * cell, (i / cols) as f32 * cell), vec2(cell, cell));
    for i in 0..n {
        let r = cell_rect(i);
        let c = e.doc.palette.colors[i];
        p.rect_filled(r.shrink(0.5), 1.0, Color32::from_rgb(c[0], c[1], c[2]));
        if g.usage.get(i).copied().unwrap_or(0) > 0 {
            // Colours the image actually uses get a small corner dot.
            let mark = if luma(c) > 128 { Color32::from_black_alpha(170) } else { Color32::from_white_alpha(190) };
            p.rect_filled(Rect::from_min_size(r.right_bottom() - vec2(4.5, 4.5), vec2(2.5, 2.5)), 0.0, mark);
        }
    }
    if let Some((a, b)) = g.pal_sel {
        for i in a.min(b)..=a.max(b).min(n.saturating_sub(1)) {
            p.rect_stroke(cell_rect(i).shrink(0.5), 1.0, Stroke::new(1.0_f32, ACCENT), StrokeKind::Inside);
        }
    }
    let mark = |cellv: Cell, col: Color32, inset: f32| {
        if let Some(i) = Some(cellv as usize).filter(|i| *i < n) {
            p.rect_stroke(cell_rect(i).shrink(inset), 1.0, Stroke::new(1.0_f32, Color32::BLACK), StrokeKind::Outside);
            p.rect_stroke(cell_rect(i).shrink(inset), 1.0, Stroke::new(1.5_f32, col), StrokeKind::Inside);
        }
    };
    mark(e.o.bg, Color32::from_gray(170), 2.5);
    mark(e.o.fg, Color32::WHITE, 0.0);
    if let Some(i) = hover {
        let (shift, alt) = ui.input(|inp| (inp.modifiers.shift, inp.modifiers.alt));
        if alt {
            g.flash = Some(i);
        }
        if resp.clicked() {
            if shift {
                g.pal_sel = Some((g.pal_sel.map(|s| s.0).unwrap_or(i), i));
            } else {
                e.o.fg = if rgba { pack(e.doc.palette.rgba(i)) } else { i as Cell };
                g.pal_sel = Some((i, i));
            }
        }
        if resp.secondary_clicked() {
            e.o.bg = if rgba { pack(e.doc.palette.rgba(i)) } else { i as Cell };
        }
        let c = e.doc.palette.colors[i];
        let used = if rgba { String::new() } else { format!("\n{} px", g.usage.get(i).copied().unwrap_or(0)) };
        let flash = if rgba { "" } else { "   Alt: flash on canvas" };
        resp.on_hover_text(format!("{i}  {}{used}\nClick: foreground   Right-click: background\nShift-click: select a range{flash}", hex6(c)));
    }

    // ---- selected colour / range
    ui.add_space(4.0);
    if let Some((a, b)) = g.pal_sel.filter(|s| s.0 < n && s.1 < n) {
        let (lo, hi) = (a.min(b), a.max(b));
        ui.horizontal(|ui| {
            let mut rgb = e.doc.palette.colors[b];
            ui.label(format!("#{b}"));
            let mut changed = ui.color_edit_button_srgb(&mut rgb).changed();
            for (ch, name) in rgb.iter_mut().zip(["R", "G", "B"]) {
                changed |= ui.add(egui::DragValue::new(ch).prefix(name).range(0..=255)).changed();
            }
            if changed {
                if let Err(err) = e.exec_coalesced("palette_set", &json!({ "index": b, "color": [rgb[0], rgb[1], rgb[2]] }), &format!("pal{b}")) {
                    g.error(err);
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            let range = hi > lo;
            if ui.add_enabled(hi > lo + 1, egui::Button::new("Blend").small()).on_hover_text("Smooth run of shades between the first and last selected colour").clicked() {
                g.run(e, "palette_gradient", json!({ "from": lo, "to": hi }));
            }
            if ui.add_enabled(range, egui::Button::new("Ramp").small()).on_hover_text("Make the selected range a colour ramp (for shading, gradients and cycling)").clicked() {
                if let Some(v) = g.run(e, "ramp_add", json!({ "from": lo, "to": hi })) {
                    e.o.active_ramp = v["ramp"].as_u64().map(|r| r as usize);
                }
            }
            if ui.add_enabled(range, egui::Button::new("Sort").small()).on_hover_text("Sort the range dark to light; the image keeps its look").clicked() {
                g.run(e, "palette_sort", json!({ "from": lo, "to": hi }));
            }
            if ui.add_enabled(range, egui::Button::new("Swap").small()).on_hover_text("Swap the first and last selected colours").clicked() {
                g.run(e, "palette_swap", json!({ "a": lo, "b": hi }));
            }
            if ui.add_enabled(n < 256, egui::Button::new("+").small()).on_hover_text("Append a colour").clicked() {
                g.run(e, "palette_set", json!({ "index": n, "color": hex6(e.doc.palette.colors[b]) }));
            }
        });
    } else {
        ui.weak("Click a colour to edit it; Shift-click for a range.");
    }

    // ---- ramps
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.strong("Ramps");
        let mut cyc = e.view.cycling;
        let why = if rgba { "Colour cycling needs an indexed document: it works by rotating palette entries" } else { "Preview colour cycling for ramps with cycling enabled" };
        if ui.add_enabled(!rgba, egui::Checkbox::new(&mut cyc, "Cycle")).on_hover_text(why).on_disabled_hover_text(why).changed() {
            e.view.cycling = cyc;
            e.rev += 1;
        }
    });
    let mut action: Option<(&str, serde_json::Value, Option<String>)> = None;
    for (ri, r) in e.doc.palette.ramps.iter().enumerate() {
        ui.horizontal(|ui| {
            let on = e.o.active_ramp == Some(ri);
            if ui.add(egui::SelectableLabel::new(on, RichText::new(&r.name).small())).on_hover_text("Active ramp for the gradient tool and lighten/darken").clicked() {
                e.o.active_ramp = if on { None } else { Some(ri) };
            }
            let strip_w = (ui.available_width() - 96.0).max(40.0);
            let (sr, _) = ui.allocate_exact_size(vec2(strip_w, 14.0), Sense::hover());
            let k = r.indices.len().max(1);
            for (j, &ix) in r.indices.iter().enumerate() {
                let c = e.doc.palette.colors.get(ix as usize).copied().unwrap_or([0, 0, 0]);
                let x0 = sr.left() + sr.width() * j as f32 / k as f32;
                let x1 = sr.left() + sr.width() * (j + 1) as f32 / k as f32;
                ui.painter().rect_filled(Rect::from_min_max(pos2(x0, sr.top()), pos2(x1, sr.bottom())), 0.0, Color32::from_rgb(c[0], c[1], c[2]));
            }
            let mut cycle = r.cycle;
            if ui.checkbox(&mut cycle, "").on_hover_text("Colour cycling").changed() {
                action = Some(("ramp_set", json!({ "ramp": ri, "cycle": cycle }), None));
            }
            let mut speed = r.speed;
            if ui.add(egui::DragValue::new(&mut speed).range(0.5..=60.0).speed(0.1).max_decimals(1)).on_hover_text("Cycle steps per second").changed() {
                action = Some(("ramp_set", json!({ "ramp": ri, "speed": speed }), Some(format!("ramp{ri}speed"))));
            }
            if ui.small_button("x").on_hover_text("Delete ramp").clicked() {
                action = Some(("ramp_delete", json!({ "ramp": ri }), None));
            }
        });
    }
    if let Some((name, params, key)) = action {
        // Only a continuous drag (speed) shares one undo step; clicks are a step each.
        let r = match key {
            Some(k) => e.exec_coalesced(name, &params, &k),
            None => e.exec(name, &params),
        };
        if let Err(err) = r {
            g.error(err);
        }
    }
}

// ------------------------------------------------------------------ layers

fn eye(ui: &mut egui::Ui, on: bool) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(20.0, 18.0), Sense::click());
    let col = if on { Color32::from_gray(220) } else { Color32::from_gray(90) };
    let p = ui.painter();
    p.add(egui::Shape::ellipse_stroke(r.center(), vec2(7.0, 4.0), Stroke::new(1.3_f32, col)));
    if on {
        p.circle_filled(r.center(), 2.2, col);
    } else {
        p.line_segment([r.center() + vec2(-6.0, 5.0), r.center() + vec2(6.0, -5.0)], Stroke::new(1.3_f32, col));
    }
    resp
}

fn padlock(ui: &mut egui::Ui, on: bool) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(16.0, 18.0), Sense::click());
    let col = if on { Color32::from_rgb(255, 196, 96) } else { Color32::from_gray(80) };
    let p = ui.painter();
    let c = r.center();
    p.circle_stroke(c + vec2(0.0, -3.0), 3.2, Stroke::new(1.3_f32, col));
    p.rect_filled(Rect::from_center_size(c + vec2(0.0, 2.5), vec2(10.0, 7.5)), 1.5, col);
    resp
}

pub fn layers(ui: &mut egui::Ui, e: &mut Engine, g: &mut Gui) {
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        let a = e.doc.active;
        if ui.small_button("New").clicked() {
            g.run(e, "layer_add", json!({}));
        }
        if ui.small_button("Copy").on_hover_text("Duplicate layer").clicked() {
            g.run(e, "layer_duplicate", json!({}));
        }
        if ui.add_enabled(a + 1 < e.doc.layers.len(), egui::Button::new("Up").small()).clicked() {
            g.run(e, "layer_move", json!({ "to": a + 1 }));
        }
        if ui.add_enabled(a > 0, egui::Button::new("Down").small()).clicked() {
            g.run(e, "layer_move", json!({ "to": a - 1 }));
        }
        if ui.add_enabled(a > 0, egui::Button::new("Merge").small()).on_hover_text("Merge into the layer below").clicked() {
            g.run(e, "layer_merge_down", json!({}));
        }
        if ui.add_enabled(e.doc.layers.len() > 1, egui::Button::new("Delete").small()).clicked() {
            g.run(e, "layer_delete", json!({}));
        }
    });
    ui.separator();
    let mut action: Option<(&str, serde_json::Value)> = None;
    for li in (0..e.doc.layers.len()).rev() {
        let l = &e.doc.layers[li];
        let active = li == e.doc.active;
        let frame = egui::Frame::new().fill(if active { Color32::from_rgb(44, 62, 104) } else { Color32::TRANSPARENT }).corner_radius(4.0).inner_margin(egui::Margin::symmetric(4, 2));
        frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                if eye(ui, l.visible).on_hover_text("Show / hide").clicked() {
                    action = Some(("layer_set", json!({ "layer": li, "visible": !l.visible })));
                }
                if padlock(ui, l.locked).on_hover_text("Lock").clicked() {
                    action = Some(("layer_set", json!({ "layer": li, "locked": !l.locked })));
                }
                let editing = g.rename.as_ref().is_some_and(|r| r.0 == l.id);
                if editing {
                    let (_, text) = g.rename.as_mut().unwrap();
                    let r = ui.add(egui::TextEdit::singleline(text).desired_width(ui.available_width() - 30.0));
                    r.request_focus();
                    if r.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        action = Some(("layer_set", json!({ "layer": li, "name": text.clone() })));
                        g.rename = None;
                    }
                } else {
                    let name = RichText::new(&l.name).color(if active { Color32::WHITE } else { Color32::from_gray(200) });
                    let r = ui.add(egui::Label::new(name).truncate().sense(Sense::click()));
                    if r.double_clicked() {
                        g.rename = Some((l.id, l.name.clone()));
                    } else if r.clicked() {
                        action = Some(("layer_select", json!({ "layer": li })));
                    }
                    if l.map.is_some() {
                        ui.label(RichText::new("tiles").small().color(ACCENT)).on_hover_text("Tile-mapped layer");
                    }
                    // Clicking the empty part of the row selects the layer too.
                    let rest = ui.allocate_response(vec2(ui.available_width(), 18.0), Sense::click());
                    if rest.clicked() {
                        action = Some(("layer_select", json!({ "layer": li })));
                    }
                }
            });
        });
    }
    if let Some((name, params)) = action {
        g.run(e, name, params);
    }
    ui.separator();
    let mut alpha_lock = e.doc.layers[e.doc.active].alpha_lock;
    if ui.checkbox(&mut alpha_lock, "Lock transparency").on_hover_text("Only paint on pixels that already have colour").changed() {
        g.run(e, "layer_set", json!({ "alpha_lock": alpha_lock }));
    }
    ui.weak("Double-click a name to rename.");
}

// ------------------------------------------------------------------ tiles

fn update_tile_texture(ctx: &egui::Context, e: &Engine, g: &mut Gui) -> Option<(usize, usize)> {
    let ts = e.doc.tileset.as_ref()?;
    let (tw, th) = (ts.tw as usize, ts.th as usize);
    let cols = 16usize.min(ts.tiles.len());
    let rows = ts.tiles.len().div_ceil(cols);
    if g.tile_tex.is_none() || g.tile_key != e.rev {
        g.tile_key = e.rev;
        let (w, h) = (cols * tw, rows * th);
        let mut rgba = vec![0u8; w * h * 4];
        for (i, t) in ts.tiles.iter().enumerate() {
            let (ox, oy) = ((i % cols) * tw, (i / cols) * th);
            for y in 0..th {
                for x in 0..tw {
                    let c = e.doc.to_rgba(t[y * tw + x]);
                    let chk = if ((x >> 2) + (y >> 2)) & 1 == 0 { 0x4e } else { 0x5e };
                    let o = ((oy + y) * w + ox + x) * 4;
                    let a = c[3] as u32;
                    for k in 0..3 {
                        rgba[o + k] = ((c[k] as u32 * a + chk * (255 - a)) / 255) as u8;
                    }
                    rgba[o + 3] = 255;
                }
            }
        }
        let img = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
        match &mut g.tile_tex {
            Some(t) => t.set(img, egui::TextureOptions::NEAREST),
            None => g.tile_tex = Some(ctx.load_texture("tiles", img, egui::TextureOptions::NEAREST)),
        }
    }
    Some((cols, rows))
}

pub fn tiles(ui: &mut egui::Ui, e: &mut Engine, g: &mut Gui) {
    ui.add_space(2.0);
    let Some((cols, rows)) = update_tile_texture(ui.ctx(), e, g) else {
        ui.label("Tile mapping builds a tile set from a layer: identical cells share one tile, and the map can be exported.");
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Tile size");
            ui.add(egui::DragValue::new(&mut g.tileset_size.0).range(2..=256));
            ui.label("x");
            ui.add(egui::DragValue::new(&mut g.tileset_size.1).range(2..=256));
            for s in [8, 16, 32] {
                if ui.small_button(s.to_string()).clicked() {
                    g.tileset_size = (s, s);
                }
            }
        });
        if ui.button("Create tile set from the active layer").clicked() {
            let (tw, th) = g.tileset_size;
            let r = e.group(|e| {
                e.exec("tileset_create", &json!({ "tile_w": tw, "tile_h": th }))?;
                e.exec("layer_to_tiles", &json!({}))
            });
            match r {
                Ok(v) => g.info(format!("Tile set created with {} tiles", v["tiles"])),
                Err(err) => g.error(err),
            }
        }
        return;
    };
    let ts = e.doc.tileset.as_ref().unwrap();
    let (tw, th, count) = (ts.tw, ts.th, ts.tiles.len());
    let mapped = e.doc.layers[e.doc.active].map.is_some();
    ui.horizontal_wrapped(|ui| {
        ui.label(format!("{count} tiles, {tw}x{th}"));
        if ui.small_button("New").on_hover_text("Add a blank tile").clicked() {
            g.run(e, "tile_add", json!({}));
        }
        if ui.small_button("Copy").on_hover_text("Duplicate the active tile").clicked() {
            g.run(e, "tile_add", json!({ "copy_of": e.o.active_tile }));
        }
        if ui.add_enabled(e.o.active_tile != 0, egui::Button::new("Delete").small()).clicked() {
            g.run(e, "tile_delete", json!({ "tile": e.o.active_tile }));
        }
        if ui.small_button("Tidy").on_hover_text("Merge identical tiles and remove unused ones").clicked() {
            let r = e.group(|e| {
                e.exec("tiles_dedupe", &json!({}))?;
                e.exec("tiles_remove_unused", &json!({}))
            });
            match r {
                Ok(v) => g.info(format!("{} tiles", v["count"])),
                Err(err) => g.error(err),
            }
        }
    });
    ui.horizontal(|ui| {
        if mapped {
            if ui.small_button("Detach layer").on_hover_text("Make the active layer an ordinary layer again").clicked() {
                g.run(e, "layer_detach_tiles", json!({}));
            }
        } else if ui.small_button("Map active layer").on_hover_text("Cut the active layer into tiles").clicked() {
            g.run(e, "layer_to_tiles", json!({}));
        }
        ui.checkbox(&mut e.view.tile_ids, "Numbers");
    });
    ui.separator();
    let Some(tex) = g.tile_tex.as_ref().map(|t| t.id()) else { return };
    let scale = (40.0 / tw.max(th) as f32).floor().max(1.0);
    let size = vec2(tw as f32 * scale, th as f32 * scale);
    let mut pick = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
        for i in 0..count {
            let (r, resp) = ui.allocate_exact_size(size, Sense::click());
            let (cx, cy) = ((i % cols) as f32, (i / cols) as f32);
            let uv = Rect::from_min_max(pos2(cx / cols as f32, cy / rows as f32), pos2((cx + 1.0) / cols as f32, (cy + 1.0) / rows as f32));
            ui.painter().image(tex, r, uv, Color32::WHITE);
            if e.o.active_tile as usize == i {
                ui.painter().rect_stroke(r, 0.0, Stroke::new(2.0_f32, ACCENT), StrokeKind::Outside);
            } else if resp.hovered() {
                ui.painter().rect_stroke(r, 0.0, Stroke::new(1.0_f32, Color32::WHITE), StrokeKind::Outside);
            }
            if resp.on_hover_text(format!("Tile {i}")).clicked() {
                pick = Some(i);
            }
        }
    });
    if let Some(i) = pick {
        e.o.active_tile = i as u16;
        e.o.tool = Tool::TileStamp;
    }
}

// ------------------------------------------------------------------ preview

pub fn preview(ui: &mut egui::Ui, e: &mut Engine, g: &mut Gui) {
    canvas::update_texture(ui.ctx(), e, g);
    let avail = ui.available_size();
    let (resp, p) = ui.allocate_painter(avail, Sense::hover());
    let (w, h) = (e.doc.w as f32, e.doc.h as f32);
    let fit = ((avail.x - 8.0) / w).min((avail.y - 8.0) / h);
    let scale = if fit >= 1.0 { fit.floor() } else { fit.max(0.01) };
    let r = Rect::from_center_size(resp.rect.center(), vec2(w, h) * scale);
    if let Some(t) = &g.tex {
        p.image(t.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    // Outline of what the main canvas is showing.
    let c = g.canvas_rect;
    if c.width() > 0.0 {
        let half = c.size() / e.view.zoom / 2.0;
        let view = Rect::from_min_max(
            r.min + (vec2(e.view.cx, e.view.cy) - half).max(vec2(0.0, 0.0)) * scale,
            r.min + (vec2(e.view.cx, e.view.cy) + half).min(vec2(w, h)) * scale,
        );
        if view.width() < r.width() - 1.0 || view.height() < r.height() - 1.0 {
            p.rect_stroke(view, 0.0, Stroke::new(1.0_f32, ACCENT), StrokeKind::Inside);
        }
    }
}

// ------------------------------------------------------------------ animation

/// Point the animation at the selection, or at the whole image when nothing is selected.
/// Make a new animation from the selection (or the whole image) and put it on show.
pub fn animate_selection(e: &mut Engine, g: &mut Gui) {
    let (x, y, w, h) = match e.doc.sel_bounds() {
        Some((x0, y0, x1, y1)) => (x0, y0, (x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32),
        None => (0, 0, e.doc.w, e.doc.h),
    };
    // Start from the frame size in use, shrunk if the region is smaller than one frame.
    let (fw, fh) = e.anim().and_then(|a| a.grid()).map(|a| (a.fw, a.fh)).or(g.anim_size).unwrap_or(e.default_frame());
    let name = crate::commands::fresh_anim_name(&e.doc);
    let p = json!({ "name": name, "x": x, "y": y, "w": w, "h": h, "frame_w": fw.clamp(1, w), "frame_h": fh.clamp(1, h) });
    if let Some(v) = g.run(e, "anim_set", p) {
        g.info(format!("{name}: {} frames of {}x{} - Left / Right step and Space plays over the Animation tab", v["frames"], v["frame_w"], v["frame_h"]));
        g.show_tab = Some(crate::app::Tab::Anim);
    }
}

fn anim_import(e: &mut Engine, g: &mut Gui) {
    if let Some(p) = rfd::FileDialog::new().add_filter("Animations (JSON)", &["json"]).pick_file() {
        if let Some(v) = g.run(e, "anim_import_json", json!({ "path": p.display().to_string() })) {
            g.info(format!("Imported {} animations from {}", v["imported"], p.display()));
        }
    }
}

pub fn animation(ui: &mut egui::Ui, e: &mut Engine, g: &mut Gui) {
    g.anim_rect = ui.max_rect();
    ui.add_space(2.0);
    let source = if e.doc.sel_bounds().is_some() { "selection" } else { "whole image" };
    let Some(a) = e.anim().cloned() else {
        ui.label("Plays part of the image as an animation. Select the frames on the canvas and set the size of one frame; they are read left to right, then top to bottom.");
        ui.add_space(4.0);
        let mut size = g.anim_size.unwrap_or(e.default_frame());
        ui.horizontal(|ui| {
            ui.label("Frame size");
            let mut changed = ui.add(egui::DragValue::new(&mut size.0).range(1..=MAX_DIM)).changed();
            ui.label("x");
            changed |= ui.add(egui::DragValue::new(&mut size.1).range(1..=MAX_DIM)).changed();
            for s in [8, 16, 32, 64] {
                if ui.small_button(s.to_string()).clicked() {
                    size = (s, s);
                    changed = true;
                }
            }
            if changed {
                g.anim_size = Some(size);
            }
        });
        ui.horizontal(|ui| {
            if ui.button(format!("Animate the {source}")).clicked() {
                animate_selection(e, g);
            }
            if ui.button("Import...").on_hover_text("Read animations from a JSON file").clicked() {
                anim_import(e, g);
            }
        });
        return;
    };

    // Which animation, and the ones beside it.
    let index = e.view.anim_index;
    ui.horizontal_wrapped(|ui| {
        let mut pick = None;
        egui::ComboBox::from_id_salt("anim_pick").width(120.0).selected_text(a.name.clone()).show_ui(ui, |ui| {
            for (i, x) in e.doc.anims.iter().enumerate() {
                if ui.selectable_label(i == index, format!("{}  ({})", x.name, x.count())).clicked() {
                    pick = Some(x.name.clone());
                }
            }
        });
        if let Some(name) = pick {
            g.anim_name = None;
            g.run(e, "anim_control", json!({ "name": name }));
        }
        if ui.button("New").on_hover_text(format!("A new animation from the {source}")).clicked() {
            g.anim_name = None;
            animate_selection(e, g);
        }
        if ui.button("Remove").on_hover_text("Remove this animation's setup; no pixels change").clicked() {
            g.anim_name = None;
            g.run(e, "anim_clear", json!({ "name": a.name }));
        }
        if ui.button("Import...").on_hover_text("Read animations from a JSON file; one with the name of an existing animation replaces it").clicked() {
            anim_import(e, g);
        }
        if ui.button("Export...").on_hover_text("Write every animation to a JSON file").clicked() {
            if let Some(p) = rfd::FileDialog::new().add_filter("Animations (JSON)", &["json"]).set_file_name("animations.json").save_file() {
                if g.run(e, "anim_export_json", json!({ "path": p.display().to_string() })).is_some() {
                    g.info(format!("Exported {}", p.display()));
                }
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label("Name");
        // The text belongs to the animation it was typed for, whichever is on show when it is applied.
        let (of, mut name) = g.anim_name.clone().unwrap_or_else(|| (a.name.clone(), a.name.clone()));
        let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(150.0).char_limit(Anim::NAME_MAX));
        if r.changed() {
            g.anim_name = Some((of, name.clone()));
        }
        if r.lost_focus() {
            // Renamed when the field is left; a name that cannot be used is reported and dropped.
            if let Some((of, to)) = g.anim_name.take().map(|(of, n)| (of, n.trim().to_string())).filter(|(of, n)| n != of) {
                g.run(e, "anim_rename", json!({ "name": of, "to": to }));
            }
        }
    });
    let Some(a) = e.anim().cloned() else { return };

    // Playback: move on by however many frames are due.
    let now = ui.input(|i| i.time);
    let (n, fps) = (a.count(), a.fps as f64);
    if e.view.anim_playing && n > 1 {
        let due = ((now - g.anim_clock) * fps).floor();
        if due >= 1.0 {
            e.view.anim_frame = ((e.view.anim_frame as f64 + due) % n as f64) as u32;
            // Carry the remainder so the speed is exact, unless the tab was hidden for a while.
            g.anim_clock = if due > fps { now } else { g.anim_clock + due / fps };
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64((g.anim_clock + 1.0 / fps - now).max(0.0)));
    } else {
        g.anim_clock = now;
    }

    let playing = e.view.anim_playing;
    ui.horizontal_wrapped(|ui| {
        if ui.button("<").on_hover_text("Previous frame (Left)").clicked() {
            g.run(e, "anim_control", json!({ "step": -1, "playing": false }));
        }
        if ui.add_sized([52.0, 20.0], egui::Button::new(if playing { "Pause" } else { "Play" })).on_hover_text("Space").clicked() {
            g.run(e, "anim_control", json!({ "playing": !playing }));
        }
        if ui.button(">").on_hover_text("Next frame (Right)").clicked() {
            g.run(e, "anim_control", json!({ "step": 1, "playing": false }));
        }
        ui.label(format!("Frame {} of {n}", e.view.anim_frame.min(n.saturating_sub(1)) + 1));
    });
    // Setup changes go through anim_set, so they are saved, undoable and visible to scripts.
    let mut set = serde_json::Map::new();
    let mut speed = a.fps;
    match a.grid() {
        Some(grid) => {
            let (mut fw, mut fh, mut limit) = (grid.fw, grid.fh, grid.limit);
            ui.horizontal_wrapped(|ui| {
                ui.label("Frame size");
                if ui.add(egui::DragValue::new(&mut fw).range(1..=grid.w)).changed() {
                    set.insert("frame_w".into(), json!(fw));
                }
                ui.label("x");
                if ui.add(egui::DragValue::new(&mut fh).range(1..=grid.h)).changed() {
                    set.insert("frame_h".into(), json!(fh));
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("Frames");
                let frames = egui::DragValue::new(&mut limit).range(0..=grid.total()).custom_formatter(|v, _| if v < 1.0 { "all".into() } else { format!("{v:.0}") });
                if ui.add(frames).on_hover_text("How many frames play; 0 plays all of them. Use it to stop before empty cells in the last row.").changed() {
                    set.insert("frames".into(), json!(limit));
                }
                ui.label("Speed");
                if ui.add(egui::DragValue::new(&mut speed).range(Anim::FPS).speed(0.1).max_decimals(1).suffix(" fps")).changed() {
                    set.insert("fps".into(), json!(speed));
                }
            });
        }
        None => {
            ui.horizontal_wrapped(|ui| {
                ui.label("Speed");
                if ui.add(egui::DragValue::new(&mut speed).range(Anim::FPS).speed(0.1).max_decimals(1).suffix(" fps")).changed() {
                    set.insert("fps".into(), json!(speed));
                }
                ui.weak(format!("{n} frames, each its own rectangle (set by a script, an AI client or a JSON file)"));
            });
        }
    }
    if !set.is_empty() {
        set.insert("name".into(), json!(a.name));
        if let Err(err) = e.exec_coalesced("anim_set", &serde_json::Value::Object(set), &format!("anim:{}", a.name)) {
            g.error(err);
        }
    }
    if let Some(grid) = a.grid() {
        ui.horizontal_wrapped(|ui| {
            if ui.button(format!("Use the {source}")).on_hover_text("Cut this animation's frames from the selection instead (or from the whole image when nothing is selected)").clicked() {
                let (x, y, w, h) = match e.doc.sel_bounds() {
                    Some((x0, y0, x1, y1)) => (x0, y0, (x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32),
                    None => (0, 0, e.doc.w, e.doc.h),
                };
                g.run(e, "anim_set", json!({ "name": a.name, "x": x, "y": y, "w": w, "h": h, "frame_w": grid.fw.clamp(1, w), "frame_h": grid.fh.clamp(1, h) }));
            }
            ui.weak(format!("{}x{} at {},{}", grid.w, grid.h, grid.x, grid.y));
        });
    }
    ui.checkbox(&mut e.view.anim_guides, "Show frames on the canvas").on_hover_text("Lines where the cuts fall, with the cells that do not play dimmed (also View > Animation Frames)");
    ui.weak("Left / Right step and Space plays while the pointer is over this tab.");

    let Some(a) = e.anim().cloned() else { return };
    let Some(f) = a.frame(e.view.anim_frame.min(a.count().saturating_sub(1))) else { return };
    canvas::update_texture(ui.ctx(), e, g);
    let avail = ui.available_size();
    if avail.x < 16.0 || avail.y < 16.0 {
        return;
    }
    let (resp, p) = ui.allocate_painter(avail, Sense::hover());
    // The box holds every frame at its offset, so frames of different sizes stay lined up.
    let (bx, by, bw, bh) = a.bounds();
    let (bw, bh, dw, dh) = (bw as f32, bh as f32, e.doc.w as f32, e.doc.h as f32);
    let fit = ((avail.x - 8.0) / bw).min((avail.y - 8.0) / bh);
    let scale = if fit >= 1.0 { fit.floor() } else { fit.max(0.01) };
    let r = Rect::from_center_size(resp.rect.center(), vec2(bw, bh) * scale);
    if let Some(t) = &g.tex {
        // The frame is cut from the canvas texture, so it shows exactly what the canvas shows
        // (visible layers, colour cycling) and follows edits as they are drawn.
        let at = r.min + vec2((f.ox - bx) as f32, (f.oy - by) as f32) * scale;
        let uv = Rect::from_min_max(pos2(f.x as f32 / dw, f.y as f32 / dh), pos2((f.x + f.w) as f32 / dw, (f.y + f.h) as f32 / dh));
        p.image(t.id(), Rect::from_min_size(at, vec2(f.w as f32, f.h as f32) * scale), uv, Color32::WHITE);
    }
    p.rect_stroke(r, 0.0, Stroke::new(1.0_f32, Color32::from_white_alpha(40)), StrokeKind::Outside);
}

// ------------------------------------------------------------------ script

pub fn script(ui: &mut egui::Ui, _e: &mut Engine, g: &mut Gui) {
    let run_key = ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui.add(egui::Button::new(RichText::new("Run").color(Color32::WHITE)).fill(Color32::from_rgb(58, 104, 196))).on_hover_text("Cmd/Ctrl + Enter").clicked() || run_key {
            g.script_req = true;
        }
        if ui.button("Open...").clicked() {
            if let Some(p) = rfd::FileDialog::new().add_filter("Lua", &["lua"]).pick_file() {
                match std::fs::read_to_string(&p) {
                    Ok(s) => g.script = s,
                    Err(err) => g.error(format!("{}: {err}", p.display())),
                }
            }
        }
        if ui.button("Save...").clicked() {
            let mut d = rfd::FileDialog::new().add_filter("Lua", &["lua"]).set_file_name("script.lua");
            if let Some(dir) = remote::scripts_dir() {
                let _ = std::fs::create_dir_all(&dir);
                d = d.set_directory(dir);
            }
            if let Some(p) = d.save_file() {
                if let Err(err) = std::fs::write(&p, &g.script) {
                    g.error(format!("{}: {err}", p.display()));
                }
            }
        }
        ui.menu_button("Scripts", |ui| {
            let files: Vec<_> = remote::scripts_dir().and_then(|d| std::fs::read_dir(d).ok()).map(|rd| rd.flatten().map(|f| f.path()).filter(|p| p.extension().is_some_and(|x| x == "lua")).collect()).unwrap_or_default();
            if files.is_empty() {
                ui.weak("Save scripts to ~/.jon_motion/scripts to list them here");
            }
            for f in files {
                if ui.button(f.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()).clicked() {
                    if let Ok(s) = std::fs::read_to_string(&f) {
                        g.script = s;
                    }
                    ui.close_menu();
                }
            }
        });
        ui.weak("A script is one undo step; an error rolls it back.");
    });
    let h = ui.available_height();
    egui::ScrollArea::vertical().id_salt("code").max_height(h * 0.5).show(ui, |ui| {
        ui.add(egui::TextEdit::multiline(&mut g.script).code_editor().desired_width(f32::INFINITY).desired_rows(14).lock_focus(true));
    });
    ui.separator();
    ui.columns(2, |c| {
        c[0].strong("Output");
        egui::ScrollArea::vertical().id_salt("out").auto_shrink([false, false]).show(&mut c[0], |ui| {
            ui.add(egui::Label::new(RichText::new(&g.script_out).monospace()).wrap());
        });
        c[1].horizontal(|ui| {
            ui.strong("Commands");
            ui.add(egui::TextEdit::singleline(&mut g.cmd_filter).hint_text("filter").desired_width(120.0));
        });
        egui::ScrollArea::vertical().id_salt("ref").auto_shrink([false, false]).show(&mut c[1], |ui| {
            let filter = g.cmd_filter.to_ascii_lowercase();
            for cmd in commands::registry().iter().filter(|c| filter.is_empty() || c.name.contains(&filter) || c.desc.to_ascii_lowercase().contains(&filter)) {
                let r = ui.add(egui::Label::new(RichText::new(format!("jm.{}", cmd.signature(false))).monospace().small()).sense(Sense::click()).truncate());
                if r.on_hover_text(format!("{}\n\n{}\n\nClick to insert", cmd.desc, cmd.signature(true))).clicked() {
                    let args: Vec<String> = cmd.params.iter().filter(|p| p.req).map(|p| format!("{} = ", p.name)).collect();
                    g.script.push_str(&format!("\njm.{}{{ {} }}", cmd.name, args.join(", ")));
                }
            }
        });
    });
}
