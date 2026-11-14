//! Keyboard presets.
//!
//! The standard keys are handled directly in `app::shortcuts`. This table is the
//! Deluxe Paint preset: the keys DPaint users have in their fingers, mapped onto the
//! features this editor has. DPaint keys whose feature does not exist here are left
//! unbound (see `DPAINT_UNBOUND`) rather than pointed at something else.

use crate::doc::TRANSP;
use crate::engine::{Engine, Keymap, Tool};
use crate::paint::PaintMode;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Act {
    Tool(Tool),
    /// A shape tool with Filled switched off or on (DPaint's r / R, c / C, e / E).
    Shape(Tool, bool),
    /// Freehand with a one-pixel brush (DPaint's D).
    FinePencil,
    Mode(PaintMode),
    BrushSize(i32),
    OnePixel,
    Fg(i32),
    Bg(i32),
    Zoom(i32),
    Magnify,
    Center,
    Grid,
    Cycling,
    Symmetry,
    Undo,
    Clear,
}

pub struct Binding {
    /// The character typed (case matters: `r` and `R` differ), or a named key: F1..F12, Tab.
    pub key: &'static str,
    pub act: Act,
    pub what: &'static str,
}

const fn b(key: &'static str, act: Act, what: &'static str) -> Binding {
    Binding { key, act, what }
}

pub const DPAINT: &[Binding] = &[
    b("d", Act::Tool(Tool::Pencil), "Freehand draw"),
    b("D", Act::FinePencil, "Freehand draw with a one-pixel brush"),
    b("v", Act::Tool(Tool::Line), "Straight line"),
    b("r", Act::Shape(Tool::Rect, false), "Rectangle"),
    b("R", Act::Shape(Tool::Rect, true), "Filled rectangle"),
    b("c", Act::Shape(Tool::Ellipse, false), "Circle (ellipse tool; Shift keeps it round)"),
    b("C", Act::Shape(Tool::Ellipse, true), "Filled circle"),
    b("e", Act::Shape(Tool::Ellipse, false), "Ellipse"),
    b("E", Act::Shape(Tool::Ellipse, true), "Filled ellipse"),
    b("f", Act::Tool(Tool::Fill), "Fill"),
    b("b", Act::Tool(Tool::Select), "Pick up a brush: select an area, then Cmd/Ctrl + B"),
    b(",", Act::Tool(Tool::Picker), "Pick a colour from the picture"),
    b(".", Act::OnePixel, "One-pixel brush"),
    b("-", Act::BrushSize(-1), "Smaller brush"),
    b("=", Act::BrushSize(1), "Larger brush"),
    b("[", Act::Fg(-1), "Previous foreground colour"),
    b("]", Act::Fg(1), "Next foreground colour"),
    b("{", Act::Bg(-1), "Previous background colour"),
    b("}", Act::Bg(1), "Next background colour"),
    b("u", Act::Undo, "Undo"),
    b("g", Act::Grid, "Grid on / off"),
    b("/", Act::Symmetry, "Symmetry on / off"),
    b("m", Act::Magnify, "Magnify on / off (at the pointer)"),
    b(">", Act::Zoom(1), "Zoom in"),
    b("<", Act::Zoom(-1), "Zoom out"),
    b("n", Act::Center, "Centre the view on the pointer"),
    b("K", Act::Clear, "Clear the layer"),
    b("Tab", Act::Cycling, "Colour cycling on / off"),
    b("F1", Act::Mode(PaintMode::Normal), "Paint mode: matte (normal)"),
    b("F4", Act::Mode(PaintMode::Smear), "Paint mode: smear"),
    b("F5", Act::Mode(PaintMode::Lighten), "Paint mode: shade (lighten; right button darkens)"),
    b("F8", Act::Mode(PaintMode::Smooth), "Paint mode: smooth"),
];

/// DPaint keys with nothing behind them in this editor.
pub const DPAINT_UNBOUND: &str = "s dotted freehand, q curve, a airbrush, w polygon, t text, B restore brush, \
x y z h H brush flip / rotate / resize, j spare page, p palette requester, F2 F3 F6 F7 colour / replace / blend / cycle modes";

/// The key that selects a tool in this keymap, for tooltips and the shortcuts list.
pub fn tool_key(map: Keymap, t: Tool) -> Option<String> {
    match map {
        Keymap::Standard => Some(t.key().to_string()),
        Keymap::DPaint => DPAINT.iter().find(|b| matches!(b.act, Act::Tool(x) | Act::Shape(x, false) if x == t)).map(|b| b.key.to_string()),
    }
}

pub fn find(key: &str) -> Option<Act> {
    DPAINT.iter().find(|b| b.key == key).map(|b| b.act)
}

/// Carry out a key's action. Editor settings change here; an action that edits the
/// document is returned as the command to run, so it goes through the usual path.
pub fn apply(e: &mut Engine, act: Act, pointer: Option<(i32, i32)>) -> Option<&'static str> {
    let step = |c: u32, d: i32, n: i32| if c == TRANSP { 0 } else { (c as i32 + d).rem_euclid(n) as u32 };
    let n = e.doc.palette.colors.len() as i32;
    match act {
        Act::Tool(t) => e.o.tool = t,
        Act::Shape(t, filled) => {
            e.o.tool = t;
            e.o.filled = filled;
        }
        Act::FinePencil => {
            e.o.tool = Tool::Pencil;
            e.o.brush_size = 1;
            e.o.custom_brush = None;
        }
        Act::Mode(m) => e.o.mode = m,
        Act::BrushSize(d) => {
            e.o.brush_size = (e.o.brush_size as i32 + d).clamp(1, 128) as _;
            e.o.custom_brush = None;
        }
        Act::OnePixel => {
            e.o.brush_size = 1;
            e.o.custom_brush = None;
        }
        Act::Fg(d) => e.o.fg = step(e.o.fg, d, n),
        Act::Bg(d) => e.o.bg = step(e.o.bg, d, n),
        Act::Zoom(d) => crate::app::zoom(e, d),
        Act::Magnify => {
            if e.view.fit {
                e.view.fit = false;
                e.view.zoom = e.view.zoom.max(8.0);
                if let Some((x, y)) = pointer {
                    (e.view.cx, e.view.cy) = (x as f32 + 0.5, y as f32 + 0.5);
                }
            } else {
                e.view.fit = true;
            }
        }
        Act::Center => {
            if let Some((x, y)) = pointer {
                (e.view.cx, e.view.cy) = (x as f32 + 0.5, y as f32 + 0.5);
                e.view.fit = false;
            }
        }
        Act::Grid => e.view.grid = !e.view.grid,
        Act::Cycling => e.view.cycling = !e.view.cycling,
        Act::Symmetry => {
            let on = e.o.sym_x || e.o.sym_y;
            (e.o.sym_x, e.o.sym_y) = (!on, false);
        }
        Act::Undo => return Some("undo"),
        Act::Clear => return Some("layer_clear"),
    }
    None
}
