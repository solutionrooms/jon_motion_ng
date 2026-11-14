//! Colour quantisation: median cut palette generation and dithered remapping.

use crate::doc::*;
use crate::paint::BAYER4;
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dither {
    None,
    Bayer,
    Floyd,
}

impl Dither {
    pub fn parse(s: &str) -> Option<Dither> {
        match s.to_ascii_lowercase().as_str() {
            "none" | "" => Some(Dither::None),
            "bayer" | "ordered" => Some(Dither::Bayer),
            "floyd" | "fs" | "floyd-steinberg" => Some(Dither::Floyd),
            _ => None,
        }
    }
}

type Entry = ([u8; 3], u32);

fn widest(b: &[Entry]) -> (usize, i32) {
    let mut best = (0, -1);
    for ch in 0..3 {
        let lo = b.iter().map(|c| c.0[ch]).min().unwrap_or(0) as i32;
        let hi = b.iter().map(|c| c.0[ch]).max().unwrap_or(0) as i32;
        // Weight channels by perceptual importance.
        let range = (hi - lo) * [3, 6, 1][ch];
        if range > best.1 {
            best = (ch, range);
        }
    }
    best
}

/// Reduce a colour histogram to at most `n` representative colours, sorted dark to light.
pub fn median_cut(hist: &HashMap<[u8; 3], u32>, n: usize) -> Vec<[u8; 3]> {
    let mut colors: Vec<Entry> = hist.iter().map(|(k, v)| (*k, *v)).collect();
    colors.sort();
    let mut out: Vec<[u8; 3]> = if colors.len() <= n {
        colors.into_iter().map(|c| c.0).collect()
    } else {
        let mut boxes: Vec<Vec<Entry>> = vec![colors];
        while boxes.len() < n {
            let mut best: Option<(usize, usize)> = None;
            let mut best_score = 0.0f64;
            for (i, b) in boxes.iter().enumerate() {
                if b.len() < 2 {
                    continue;
                }
                let (ch, range) = widest(b);
                let count: f64 = b.iter().map(|c| c.1 as f64).sum();
                let score = (range as f64 + 1.0) * count.sqrt();
                if score > best_score {
                    best_score = score;
                    best = Some((i, ch));
                }
            }
            let Some((i, ch)) = best else { break };
            let mut b = boxes.swap_remove(i);
            b.sort_by_key(|c| c.0[ch]);
            let total: u64 = b.iter().map(|c| c.1 as u64).sum();
            let (mut acc, mut cut) = (0u64, 1usize);
            for (j, c) in b.iter().enumerate() {
                acc += c.1 as u64;
                if acc * 2 >= total {
                    cut = (j + 1).clamp(1, b.len() - 1);
                    break;
                }
            }
            let b2 = b.split_off(cut);
            boxes.push(b);
            boxes.push(b2);
        }
        boxes
            .iter()
            .map(|b| {
                let total: u64 = b.iter().map(|c| c.1 as u64).sum::<u64>().max(1);
                let mut s = [0u64; 3];
                for c in b {
                    for (k, sk) in s.iter_mut().enumerate() {
                        *sk += c.0[k] as u64 * c.1 as u64;
                    }
                }
                [(s[0] / total) as u8, (s[1] / total) as u8, (s[2] / total) as u8]
            })
            .collect()
    };
    out.sort_by_key(|c| (luma(*c), *c));
    out.dedup();
    out
}

/// Map RGBA pixels onto palette indices. Alpha below 128 becomes transparent.
pub fn to_indexed(rgba: &[[u8; 4]], w: usize, h: usize, pal: &Palette, dither: Dither) -> Vec<Cell> {
    let mut cache: HashMap<[u8; 3], u8> = HashMap::new();
    let mut near = |c: [u8; 3]| *cache.entry(c).or_insert_with(|| pal.nearest(c));
    let mut out = vec![TRANSP; w * h];
    match dither {
        Dither::None | Dither::Bayer => {
            for y in 0..h {
                for x in 0..w {
                    let p = rgba[y * w + x];
                    if p[3] < 128 {
                        continue;
                    }
                    let mut c = [p[0], p[1], p[2]];
                    if dither == Dither::Bayer {
                        let t = (BAYER4[y & 3][x & 3] as i32 - 8) * 3;
                        for v in &mut c {
                            *v = (*v as i32 + t).clamp(0, 255) as u8;
                        }
                    }
                    out[y * w + x] = near(c) as Cell;
                }
            }
        }
        Dither::Floyd => {
            let mut err = vec![[0f32; 3]; w * h];
            for y in 0..h {
                for x in 0..w {
                    let i = y * w + x;
                    let p = rgba[i];
                    if p[3] < 128 {
                        continue;
                    }
                    let want = [p[0] as f32 + err[i][0], p[1] as f32 + err[i][1], p[2] as f32 + err[i][2]];
                    let q = [want[0].clamp(0.0, 255.0) as u8, want[1].clamp(0.0, 255.0) as u8, want[2].clamp(0.0, 255.0) as u8];
                    let idx = near(q);
                    out[i] = idx as Cell;
                    let got = pal.colors[idx as usize];
                    let e = [want[0] - got[0] as f32, want[1] - got[1] as f32, want[2] - got[2] as f32];
                    let mut spread = |dx: i32, dy: i32, f: f32| {
                        let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                        if nx >= 0 && nx < w as i32 && ny < h as i32 {
                            let j = ny as usize * w + nx as usize;
                            for k in 0..3 {
                                err[j][k] += e[k] * f;
                            }
                        }
                    };
                    spread(1, 0, 7.0 / 16.0);
                    spread(-1, 1, 3.0 / 16.0);
                    spread(0, 1, 5.0 / 16.0);
                    spread(1, 1, 1.0 / 16.0);
                }
            }
        }
    }
    out
}

/// A palette of at most `n` colours chosen for these pixels (transparent ones are ignored).
pub fn palette_for<'a>(px: impl Iterator<Item = &'a [u8; 4]>, n: usize) -> Palette {
    let mut hist: HashMap<[u8; 3], u32> = HashMap::new();
    for p in px.filter(|p| p[3] >= 128) {
        *hist.entry([p[0], p[1], p[2]]).or_insert(0) += 1;
    }
    let mut colors = median_cut(&hist, n.clamp(1, 256));
    if colors.is_empty() {
        colors.push([0, 0, 0]);
    }
    Palette { colors, ramps: vec![] }
}
