//! Brushes, paint modes and raster primitives.

use crate::doc::*;

pub const BAYER4: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaintMode {
    Normal,
    Dither,
    Lighten,
    Darken,
    Smooth,
    Smear,
    Replace,
    Behind,
}

impl PaintMode {
    pub const ALL: [PaintMode; 8] = [
        PaintMode::Normal,
        PaintMode::Dither,
        PaintMode::Lighten,
        PaintMode::Darken,
        PaintMode::Smooth,
        PaintMode::Smear,
        PaintMode::Replace,
        PaintMode::Behind,
    ];
    pub fn name(self) -> &'static str {
        match self {
            PaintMode::Normal => "normal",
            PaintMode::Dither => "dither",
            PaintMode::Lighten => "lighten",
            PaintMode::Darken => "darken",
            PaintMode::Smooth => "smooth",
            PaintMode::Smear => "smear",
            PaintMode::Replace => "replace",
            PaintMode::Behind => "behind",
        }
    }
    pub fn parse(s: &str) -> Option<PaintMode> {
        PaintMode::ALL.into_iter().find(|m| m.name() == s.to_ascii_lowercase())
    }
    pub fn hint(self) -> &'static str {
        match self {
            PaintMode::Normal => "Plain colour",
            PaintMode::Dither => "Ordered dither pattern of foreground (and optionally background)",
            PaintMode::Lighten => "Step each pixel one shade up its colour ramp",
            PaintMode::Darken => "Step each pixel one shade down its colour ramp",
            PaintMode::Smooth => "Blend each pixel with its neighbours",
            PaintMode::Smear => "Drag existing pixels along the stroke",
            PaintMode::Replace => "Only paint over pixels of the background colour",
            PaintMode::Behind => "Only paint on transparent pixels",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BrushShape {
    Round,
    Square,
}

impl BrushShape {
    pub fn name(self) -> &'static str {
        match self {
            BrushShape::Round => "round",
            BrushShape::Square => "square",
        }
    }
    pub fn parse(s: &str) -> Option<BrushShape> {
        match s.to_ascii_lowercase().as_str() {
            "round" | "circle" => Some(BrushShape::Round),
            "square" | "rect" => Some(BrushShape::Square),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Brush {
    pub w: i32,
    pub h: i32,
    pub mask: Vec<bool>,
    /// Per-pixel colours for a custom (grabbed) brush.
    pub cells: Option<Vec<Cell>>,
}

impl Brush {
    pub fn new(size: u32, shape: BrushShape) -> Brush {
        let d = size.clamp(1, 128) as i32;
        let mut mask = vec![true; (d * d) as usize];
        if shape == BrushShape::Round && d > 2 {
            let r = d as f32 / 2.0;
            for y in 0..d {
                for x in 0..d {
                    let (dx, dy) = (x as f32 + 0.5 - r, y as f32 + 0.5 - r);
                    mask[(y * d + x) as usize] = dx * dx + dy * dy <= r * r - 0.5;
                }
            }
        }
        Brush { w: d, h: d, mask, cells: None }
    }
}

#[derive(Clone, Debug)]
pub struct PaintOpts {
    pub color: Cell,
    pub bg: Cell,
    pub mode: PaintMode,
    /// Dither level 0..=16: how many of 16 Bayer cells take the foreground.
    pub dither: u8,
    pub dither_bg: bool,
    pub brush: Brush,
    pub sym_x: bool,
    pub sym_y: bool,
    pub pixel_perfect: bool,
    pub ramp: Option<usize>,
}

pub struct Painter<'a> {
    pub w: i32,
    pub h: i32,
    pub mode: ColorMode,
    pub pal: &'a Palette,
    pub data: &'a mut [Cell],
    /// Layer pixels as they were when the stroke began.
    pub base: &'a [Cell],
    pub sel: Option<&'a [u8]>,
    pub touched: &'a mut [bool],
    pub alpha_lock: bool,
    pub o: &'a PaintOpts,
}

pub fn line_points(x0: i32, y0: i32, x1: i32, y1: i32) -> Vec<(i32, i32)> {
    let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
    let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
    let (mut x, mut y, mut err) = (x0, y0, dx + dy);
    let mut pts = Vec::with_capacity((dx.max(-dy) + 1) as usize);
    loop {
        pts.push((x, y));
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
    pts
}

/// Outline of the ellipse inscribed in the given rectangle (Zingl's algorithm).
pub fn ellipse_points(xa: i32, ya: i32, xb: i32, yb: i32) -> Vec<(i32, i32)> {
    let (mut x0, mut x1) = (xa.min(xb) as i64, xa.max(xb) as i64);
    let (mut y0, mut y1) = (ya.min(yb) as i64, ya.max(yb) as i64);
    let mut a = x1 - x0;
    let b = y1 - y0;
    let mut b1 = b & 1;
    let mut dx = 4 * (1 - a) * b * b;
    let mut dy = 4 * (b1 + 1) * a * a;
    let mut err = dx + dy + b1 * a * a;
    y0 += (b + 1) / 2;
    y1 = y0 - b1;
    a *= 8 * a;
    b1 = 8 * b * b;
    let mut pts = vec![];
    loop {
        pts.extend([(x1, y0), (x0, y0), (x0, y1), (x1, y1)]);
        let e2 = 2 * err;
        if e2 <= dy {
            y0 += 1;
            y1 -= 1;
            dy += a;
            err += dy;
        }
        if e2 >= dx || 2 * err > dy {
            x0 += 1;
            x1 -= 1;
            dx += b1;
            err += dx;
        }
        if x0 > x1 {
            break;
        }
    }
    while y0 - y1 < b {
        pts.extend([(x0 - 1, y0), (x1 + 1, y0), (x0 - 1, y1), (x1 + 1, y1)]);
        y0 += 1;
        y1 -= 1;
    }
    pts.into_iter().map(|(x, y)| (x as i32, y as i32)).collect()
}

fn similar(mode: ColorMode, a: Cell, b: Cell, tol: u8) -> bool {
    match mode {
        ColorMode::Indexed => a == b || (a >= 256 && b >= 256),
        ColorMode::Rgba => {
            if a == b {
                return true;
            }
            let (a, b) = (unpack(a), unpack(b));
            (0..4).all(|i| (a[i] as i32 - b[i] as i32).abs() <= tol as i32)
        }
    }
}

/// Pixels matching the one at (x, y): the connected region, or every match when not contiguous.
pub fn flood_mask(data: &[Cell], w: i32, h: i32, mode: ColorMode, x: i32, y: i32, contiguous: bool, tol: u8) -> Vec<bool> {
    let mut mask = vec![false; (w * h) as usize];
    if x < 0 || y < 0 || x >= w || y >= h {
        return mask;
    }
    let target = data[(y * w + x) as usize];
    if !contiguous {
        for (m, &c) in mask.iter_mut().zip(data) {
            *m = similar(mode, c, target, tol);
        }
        return mask;
    }
    let mut stack = vec![(x, y)];
    mask[(y * w + x) as usize] = true;
    while let Some((px, py)) = stack.pop() {
        for (nx, ny) in [(px - 1, py), (px + 1, py), (px, py - 1), (px, py + 1)] {
            if nx < 0 || ny < 0 || nx >= w || ny >= h {
                continue;
            }
            let i = (ny * w + nx) as usize;
            if !mask[i] && similar(mode, data[i], target, tol) {
                mask[i] = true;
                stack.push((nx, ny));
            }
        }
    }
    mask
}

impl Painter<'_> {
    /// Apply the paint mode at one pixel, mirrored by the symmetry settings.
    pub fn plot(&mut self, x: i32, y: i32, over: Option<Cell>) {
        self.plot1(x, y, over);
        let (mx, my) = (self.w - 1 - x, self.h - 1 - y);
        if self.o.sym_x {
            self.plot1(mx, y, over);
        }
        if self.o.sym_y {
            self.plot1(x, my, over);
        }
        if self.o.sym_x && self.o.sym_y {
            self.plot1(mx, my, over);
        }
    }

    fn plot1(&mut self, x: i32, y: i32, over: Option<Cell>) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return;
        }
        let i = (y * self.w + x) as usize;
        if let Some(s) = self.sel {
            if s[i] == 0 {
                return;
            }
        }
        let b = self.base[i];
        if self.alpha_lock && cell_empty(self.mode, b) {
            return;
        }
        let color = over.unwrap_or(self.o.color);
        let v = match self.o.mode {
            PaintMode::Normal => color,
            PaintMode::Dither => {
                if BAYER4[(y & 3) as usize][(x & 3) as usize] < self.o.dither {
                    color
                } else if self.o.dither_bg {
                    self.o.bg
                } else {
                    return;
                }
            }
            PaintMode::Replace => {
                if b != self.o.bg {
                    return;
                }
                color
            }
            PaintMode::Behind => {
                if !cell_empty(self.mode, self.data[i]) {
                    return;
                }
                color
            }
            PaintMode::Lighten | PaintMode::Darken => {
                if self.touched[i] {
                    return;
                }
                match self.shade(b, self.o.mode == PaintMode::Lighten) {
                    Some(v) => v,
                    None => return,
                }
            }
            PaintMode::Smooth => {
                if self.touched[i] {
                    return;
                }
                match self.smooth(x, y) {
                    Some(v) => v,
                    None => return,
                }
            }
            PaintMode::Smear => return,
        };
        self.touched[i] = true;
        self.data[i] = v;
    }

    fn shade(&self, c: Cell, up: bool) -> Option<Cell> {
        if cell_empty(self.mode, c) {
            return None;
        }
        match self.mode {
            ColorMode::Indexed => {
                let cur = *self.pal.colors.get(c as usize)?;
                if let Some((r, p)) = self.pal.ramp_pos(c as u8, self.o.ramp) {
                    let ramp = &self.pal.ramps[r].indices;
                    let col = |i: u8| self.pal.colors.get(i as usize).copied().unwrap_or([0, 0, 0]);
                    // Ramps may run light-to-dark; follow the ramp's own direction.
                    let ascending = luma(col(ramp[0])) <= luma(col(ramp[ramp.len() - 1]));
                    let np = if up == ascending { (p + 1).min(ramp.len() - 1) } else { p.saturating_sub(1) };
                    return Some(ramp[np] as Cell);
                }
                let l = luma(cur);
                let f = |v: u8| if up { (v as i32 * 118 / 100 + 10).min(255) as u8 } else { (v as i32 * 82 / 100 - 4).max(0) as u8 };
                let target = [f(cur[0]), f(cur[1]), f(cur[2])];
                self.pal
                    .colors
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| if up { luma(**p) > l } else { luma(**p) < l })
                    .min_by_key(|(_, p)| dist(**p, target))
                    .map(|(i, _)| i as Cell)
            }
            ColorMode::Rgba => {
                let [r, g, b, a] = unpack(c);
                let f = |v: u8| if up { (v as i32 + (255 - v as i32) / 8 + 2).min(255) as u8 } else { (v as i32 - v as i32 / 8 - 2).max(0) as u8 };
                Some(pack([f(r), f(g), f(b), a]))
            }
        }
    }

    fn smooth(&self, x: i32, y: i32) -> Option<Cell> {
        let at = |x: i32, y: i32| -> Option<[u8; 4]> {
            if x < 0 || y < 0 || x >= self.w || y >= self.h {
                return None;
            }
            let c = self.base[(y * self.w + x) as usize];
            match self.mode {
                ColorMode::Indexed if c < 256 => Some(self.pal.rgba(c as usize)),
                ColorMode::Rgba if c >> 24 != 0 => Some(unpack(c)),
                _ => None,
            }
        };
        at(x, y)?;
        let (mut sum, mut wsum, mut asum, mut n) = ([0f32; 3], 0f32, 0f32, 0f32);
        for dy in -1..=1 {
            for dx in -1..=1 {
                let wgt = if dx == 0 && dy == 0 { 2.0 } else { 1.0 };
                n += wgt;
                if let Some(c) = at(x + dx, y + dy) {
                    let a = c[3] as f32 / 255.0 * wgt;
                    for i in 0..3 {
                        sum[i] += c[i] as f32 * a;
                    }
                    wsum += a;
                    asum += c[3] as f32 * wgt;
                }
            }
        }
        if wsum <= 0.0 {
            return None;
        }
        let rgb = [(sum[0] / wsum).round() as u8, (sum[1] / wsum).round() as u8, (sum[2] / wsum).round() as u8];
        Some(match self.mode {
            ColorMode::Indexed => self.pal.nearest(rgb) as Cell,
            ColorMode::Rgba => pack([rgb[0], rgb[1], rgb[2], (asum / n).round().max(1.0) as u8]),
        })
    }

    pub fn stamp(&mut self, x: i32, y: i32) {
        let o = self.o;
        let b = &o.brush;
        let (x0, y0) = (x - b.w / 2, y - b.h / 2);
        for by in 0..b.h {
            for bx in 0..b.w {
                let i = (by * b.w + bx) as usize;
                if b.mask[i] {
                    self.plot(x0 + bx, y0 + by, b.cells.as_ref().map(|c| c[i]));
                }
            }
        }
    }

    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, skip_first: bool) {
        for (i, (x, y)) in line_points(x0, y0, x1, y1).into_iter().enumerate() {
            if i > 0 || !skip_first {
                self.stamp(x, y);
            }
        }
    }

    pub fn rect(&mut self, xa: i32, ya: i32, xb: i32, yb: i32, filled: bool) {
        let (x0, x1, y0, y1) = (xa.min(xb), xa.max(xb), ya.min(yb), ya.max(yb));
        if filled {
            for y in y0.max(0)..=y1.min(self.h - 1) {
                for x in x0.max(0)..=x1.min(self.w - 1) {
                    self.plot(x, y, None);
                }
            }
        } else {
            self.line(x0, y0, x1, y0, false);
            self.line(x0, y1, x1, y1, false);
            self.line(x0, y0, x0, y1, false);
            self.line(x1, y0, x1, y1, false);
        }
    }

    pub fn ellipse(&mut self, xa: i32, ya: i32, xb: i32, yb: i32, filled: bool) {
        if (xa - xb).abs() < 2 || (ya - yb).abs() < 2 {
            return self.rect(xa, ya, xb, yb, filled);
        }
        let pts = ellipse_points(xa, ya, xb, yb);
        if filled {
            let y0 = ya.min(yb);
            let mut spans = vec![(i32::MAX, i32::MIN); ((ya - yb).abs() + 1) as usize];
            for (x, y) in pts {
                if let Some(s) = spans.get_mut((y - y0) as usize) {
                    s.0 = s.0.min(x);
                    s.1 = s.1.max(x);
                }
            }
            for (r, (sx0, sx1)) in spans.into_iter().enumerate() {
                for x in sx0.max(0)..=sx1.min(self.w - 1) {
                    self.plot(x, y0 + r as i32, None);
                }
            }
        } else {
            for (x, y) in pts {
                self.stamp(x, y);
            }
        }
    }

    pub fn fill_mask(&mut self, mask: &[bool]) {
        for y in 0..self.h {
            for x in 0..self.w {
                if mask[(y * self.w + x) as usize] {
                    self.plot(x, y, None);
                }
            }
        }
    }

    /// Drag the pixels under the brush along the path from (x0, y0) to (x1, y1).
    pub fn smear(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        let o = self.o;
        let b = &o.brush;
        let pts = line_points(x0, y0, x1, y1);
        for pair in pts.windows(2) {
            let ((ax, ay), (bx, by)) = (pair[0], pair[1]);
            let (dx, dy) = (bx - ax, by - ay);
            let (tx, ty) = (bx - b.w / 2, by - b.h / 2);
            let mut writes = vec![];
            for my in 0..b.h {
                for mx in 0..b.w {
                    if !b.mask[(my * b.w + mx) as usize] {
                        continue;
                    }
                    let (x, y) = (tx + mx, ty + my);
                    let (sx, sy) = (x - dx, y - dy);
                    if x < 0 || y < 0 || x >= self.w || y >= self.h || sx < 0 || sy < 0 || sx >= self.w || sy >= self.h {
                        continue;
                    }
                    let i = (y * self.w + x) as usize;
                    if self.sel.map(|s| s[i] == 0).unwrap_or(false) {
                        continue;
                    }
                    if self.alpha_lock && cell_empty(self.mode, self.base[i]) {
                        continue;
                    }
                    writes.push((i, self.data[(sy * self.w + sx) as usize]));
                }
            }
            for (i, v) in writes {
                self.data[i] = v;
            }
        }
    }

    /// Fill every paintable pixel with a gradient through `stops`.
    pub fn gradient(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, stops: &[Cell], radial: bool, dither: bool) {
        let n = stops.len();
        if n == 0 {
            return;
        }
        let (dx, dy) = (x1 - x0, y1 - y0);
        let len2 = (dx * dx + dy * dy).max(1e-6);
        let smooth = self.mode == ColorMode::Rgba && !dither;
        for y in 0..self.h {
            for x in 0..self.w {
                let (px, py) = (x as f32 - x0, y as f32 - y0);
                let t = if radial { ((px * px + py * py) / len2).sqrt() } else { (px * dx + py * dy) / len2 }.clamp(0.0, 1.0);
                let f = t * (n - 1) as f32;
                let cell = if smooth {
                    let i = (f.floor() as usize).min(n - 1);
                    let j = (i + 1).min(n - 1);
                    let k = f - i as f32;
                    let (a, b) = (unpack(stops[i]), unpack(stops[j]));
                    let mut c = [0u8; 4];
                    for q in 0..4 {
                        c[q] = (a[q] as f32 + (b[q] as f32 - a[q] as f32) * k).round() as u8;
                    }
                    pack(c)
                } else {
                    let thr = if dither { (BAYER4[(y & 3) as usize][(x & 3) as usize] as f32 + 0.5) / 16.0 } else { 0.5 };
                    stops[((f + thr).floor() as usize).min(n - 1)]
                };
                self.plot(x, y, Some(cell));
            }
        }
    }
}
