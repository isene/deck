//! The bridge look: a frame of coloured bars and rounded corners in the
//! style of a starship's screens, painted in pixels through glow. The
//! text shows through holes in the picture. With the look off, none of
//! this runs; with it on, a picture goes out only when the plan changed.
//! gluon's Home and deck on its own each make a plan; this draws it.

use crust::style;
use glow::{Canvas, Display};

pub type Rgb = (u8, u8, u8);
pub const ORANGE: Rgb = (255, 153, 0);
pub const PEACH: Rgb = (255, 204, 153);
pub const LILAC: Rgb = (204, 153, 204);
pub const PERI: Rgb = (153, 153, 255);
pub const RED: Rgb = (204, 102, 102);
pub const SKY: Rgb = (153, 204, 255);

/// The sidebar's width in columns.
pub const SIDE: u16 = 12;

/// Text in black on a bar or button of colour `bg`. Written in a hole of
/// the picture, it leaves the bar whole.
pub fn on_bar(text: &str, bg: Rgb) -> String {
    style::rgb(text, Some((0, 0, 0)), Some(bg), "b")
}

/// A sidebar part: first row, rows, colour, label.
pub type Part = (u16, u16, Rgb, Option<&'static str>);

/// Rows at the foot of the state part for what Claude is doing.
pub const STATE: u16 = 7;

/// deck's buttons from row `from`: the session, the model, the context
/// and the week's limit, up to three rows each, then the state part in
/// colour `state` down to row `to`. A short screen drops the buttons that
/// do not fit. deck writes their labels.
pub fn info(from: u16, to: u16, state: Rgb) -> Vec<Part> {
    let each = (to.saturating_sub(from + STATE) / 4).clamp(1, 3);
    let mut v: Vec<Part> = [(LILAC, "tag"), (PERI, "model"), (PEACH, "ctx"), (SKY, "week")]
        .into_iter()
        .enumerate()
        .map(|(i, (c, what))| (from + i as u16 * each, each, c, Some(what)))
        .filter(|&(row, n, ..)| row + n <= to)
        .collect();
    let after = v.last().map_or(from, |&(row, n, ..)| row + n);
    if to > after {
        v.push((after, to - after, state, Some("state")));
    }
    v
}

/// The cells the sidebar's labels show through: a labelled part's last
/// row, and the state part's last STATE rows.
pub fn label_holes(parts: &[Part]) -> Vec<(u16, u16, u16)> {
    parts
        .iter()
        .flat_map(|&(row, n, _, label)| {
            let k = match label {
                Some("state") => n.min(STATE),
                Some(_) => 1,
                None => 0,
            };
            (row + n - k..row + n).map(|r| (r, 0, SIDE))
        })
        .collect()
}

/// Everything the picture shows, in cells from 0. The same plan draws
/// the same picture, so an unchanged plan sends nothing.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Plan {
    pub cols: u16,
    pub rows: u16,
    /// The row of the bar between Home and Claude.
    pub mid: u16,
    /// Sidebar parts: first row, rows, colour, label ("" for the clock,
    /// which is text alone, so its minutes send no picture). The first
    /// part is the top corner, the last the bottom corner, and the one
    /// between with no label is where the bar at `mid` crosses.
    pub parts: Vec<Part>,
    /// The parts with the keys, which reach across the gap to their text.
    pub lit: Vec<usize>,
    /// A box's header bar: row, first column, columns, colour.
    pub bars: Vec<(u16, u16, u16, Rgb)>,
    /// Cells that show text: row, column, cells.
    pub holes: Vec<(u16, u16, u16)>,
}

/// The share of pixel (`x`, `y`) inside a rectangle from (`x0`, `y0`) to
/// (`x1`, `y1`), with its corners rounded by `r` (top left, top right,
/// bottom right, bottom left).
fn rect(x: f64, y: f64, (x0, y0, x1, y1): (f64, f64, f64, f64), r: [f64; 4]) -> f64 {
    let over = |a: f64, lo: f64, hi: f64| ((a + 1.0).min(hi) - a.max(lo)).clamp(0.0, 1.0);
    let base = over(x, x0, x1) * over(y, y0, y1);
    if base == 0.0 {
        return 0.0;
    }
    let (cx, cy) = (x + 0.5, y + 0.5);
    let corners = [
        (x0 + r[0], y0 + r[0], r[0], cx < x0 + r[0] && cy < y0 + r[0]),
        (x1 - r[1], y0 + r[1], r[1], cx > x1 - r[1] && cy < y0 + r[1]),
        (x1 - r[2], y1 - r[2], r[2], cx > x1 - r[2] && cy > y1 - r[2]),
        (x0 + r[3], y1 - r[3], r[3], cx < x0 + r[3] && cy > y1 - r[3]),
    ];
    for (ox, oy, rr, inside) in corners {
        if rr > 0.0 && inside {
            let d = ((cx - ox).powi(2) + (cy - oy).powi(2)).sqrt() - rr;
            return (0.5 - d).clamp(0.0, 1.0).min(base);
        }
    }
    base
}

/// The share of pixel (`x`, `y`) in the inside curve of a corner: the
/// square of side `r` from (`x0`, `y0`), less the circle about (`ox`, `oy`).
fn curve(x: f64, y: f64, x0: f64, y0: f64, r: f64, (ox, oy): (f64, f64)) -> f64 {
    let base = rect(x, y, (x0, y0, x0 + r, y0 + r), [0.0; 4]);
    if base == 0.0 {
        return 0.0;
    }
    let d = ((x + 0.5 - ox).powi(2) + (y + 0.5 - oy).powi(2)).sqrt();
    (d - r + 0.5).clamp(0.0, 1.0).min(base)
}

/// Paint `rgb` over the pixels from (`x0`, `y0`) to (`x1`, `y1`), each as
/// much as `cover` says.
fn fill(c: &mut Canvas, (x0, y0, x1, y1): (f64, f64, f64, f64), rgb: Rgb, cover: impl Fn(f64, f64) -> f64) {
    for y in y0.floor().max(0.0) as usize..(y1.ceil() as usize).min(c.h) {
        for x in x0.floor().max(0.0) as usize..(x1.ceil() as usize).min(c.w) {
            let a = cover(x as f64, y as f64);
            if a >= 1.0 {
                // Most pixels are covered whole: no blend to work out.
                let o = (y * c.w + x) * 4;
                c.rgba[o..o + 4].copy_from_slice(&[rgb.0, rgb.1, rgb.2, 255]);
            } else if a > 0.0 {
                c.blend(x as i64, y as i64, rgb, a);
            }
        }
    }
}

/// The picture for a plan. `cell` is None on the terminal, a size in tests.
pub fn picture(p: &Plan, cell: Option<(u16, u16)>) -> Canvas {
    let mut c = Canvas::sized(p.cols, p.rows, cell);
    c.see_through();
    let (cw, ch) = (c.cell_w(), c.cell_h());
    let (w, h) = (c.w as f64, c.h as f64);
    let gap = (ch / 6.0).round().max(2.0);
    let side = SIDE as f64 * cw;
    // The arms along the top and the bottom are as thick as a row; the
    // corners' outer curve is wider, the inner one smaller.
    let (arm, outer, inner) = (ch, (ch * 1.6).min(side / 2.0), (ch * 0.6).min(cw));
    let last = p.parts.len().saturating_sub(1);
    for (i, (row, rows, rgb, label)) in p.parts.iter().enumerate() {
        let y0 = *row as f64 * ch + if i > 0 { gap } else { 0.0 };
        let y1 = (*row + *rows) as f64 * ch;
        if i == 0 {
            let bar = (0.0, 0.0, w, arm);
            let block = (0.0, 0.0, side, y1);
            let cover = |x, y| {
                rect(x, y, block, [outer, 0.0, 0.0, 0.0])
                    .max(rect(x, y, bar, [outer, arm / 2.0, arm / 2.0, 0.0]))
                    .max(curve(x, y, side, arm, inner, (side + inner, arm + inner)))
            };
            // Only the bar runs the whole width; below it are the block and its curve.
            fill(&mut c, bar, *rgb, &cover);
            fill(&mut c, (0.0, arm.ceil(), side + inner, y1), *rgb, &cover);
        } else if i == last {
            let bar = (0.0, h - arm, w, h);
            let block = (0.0, y0, side, h);
            let cover = |x, y| {
                rect(x, y, block, [0.0, 0.0, 0.0, outer])
                    .max(rect(x, y, bar, [0.0, arm / 2.0, arm / 2.0, outer]))
                    .max(curve(x, y, side, h - arm - inner, inner, (side + inner, h - arm - inner)))
            };
            fill(&mut c, (0.0, y0, side + inner, (h - arm).floor()), *rgb, &cover);
            fill(&mut c, bar, *rgb, &cover);
        } else if label.is_none() {
            // The bar between Home and Claude crosses the sidebar here,
            // curving into it above and below.
            let ya = p.mid as f64 * ch;
            let bar = (0.0, ya, w, ya + arm);
            let block = (0.0, y0, side, y1);
            let cover = |x, y| {
                rect(x, y, block, [0.0; 4])
                    .max(rect(x, y, bar, [0.0, arm / 2.0, arm / 2.0, 0.0]))
                    .max(curve(x, y, side, ya - inner, inner, (side + inner, ya - inner)))
                    .max(curve(x, y, side, ya + arm, inner, (side + inner, ya + arm + inner)))
            };
            fill(&mut c, (0.0, y0, side + inner, ya.floor()), *rgb, &cover);
            fill(&mut c, bar, *rgb, &cover);
            fill(&mut c, (0.0, (ya + arm).ceil(), side + inner, y1), *rgb, &cover);
        } else {
            // A part with the keys reaches across the gap, its end rounded.
            let (x1, r) = if p.lit.contains(&i) { (side + cw, cw / 2.0) } else { (side, 0.0) };
            fill(&mut c, (0.0, y0, x1, y1), *rgb, |x, y| rect(x, y, (0.0, y0, x1, y1), [0.0, r, r, 0.0]));
        }
    }
    // A box's header: a bar a little thinner than its row, round at both ends.
    for &(row, col, cols, rgb) in &p.bars {
        let (y0, y1) = (row as f64 * ch + ch * 0.15, (row + 1) as f64 * ch - ch * 0.15);
        let (x0, x1) = (col as f64 * cw, (col + cols) as f64 * cw);
        let r = (y1 - y0) / 2.0;
        fill(&mut c, (x0, y0, x1, y1), rgb, |x, y| rect(x, y, (x0, y0, x1, y1), [r; 4]));
    }
    for &(row, col, n) in &p.holes {
        c.hole(row as usize, col as usize, n as usize);
    }
    c
}

/// The strips that hold all of the picture's colour, as row, column, rows
/// and columns from 0: the arms along the top, the middle and the bottom,
/// the sidebar between them, and the boxes' header bars. The terminal
/// blends a picture again over each row of text that changes under it,
/// so text outside the strips costs nothing extra.
pub fn pieces(p: &Plan) -> Vec<(u16, u16, u16, u16)> {
    // A lit button reaches one column past the sidebar.
    let (strip, last) = (SIDE + 1, p.rows.saturating_sub(1));
    let mut v = vec![
        (0, 0, 1, p.cols),
        (1, 0, p.mid.saturating_sub(1), strip),
        (p.mid, 0, 1, p.cols),
        (p.mid + 1, 0, last.saturating_sub(p.mid + 1), strip),
        (last, 0, 1, p.cols),
    ];
    for &(row, col, cols, _) in &p.bars {
        v.push((row, col, 1, cols));
    }
    v.retain(|&(_, _, rows, cols)| rows > 0 && cols > 0);
    v
}

/// Those cells of a canvas as a canvas of their own.
pub fn cut(c: &Canvas, (row, col, rows, cols): (u16, u16, u16, u16)) -> Canvas {
    let px = |n: u16, cell: f64, max: usize| ((n as f64 * cell).round() as usize).min(max);
    let (x0, x1) = (px(col, c.cell_w(), c.w), px(col + cols, c.cell_w(), c.w));
    let (y0, y1) = (px(row, c.cell_h(), c.h), px(row + rows, c.cell_h(), c.h));
    let mut rgba = Vec::with_capacity((x1 - x0) * (y1 - y0) * 4);
    for y in y0..y1 {
        rgba.extend_from_slice(&c.rgba[(y * c.w + x0) * 4..(y * c.w + x1) * 4]);
    }
    Canvas { cols, rows, cell: c.cell, w: x1 - x0, h: y1 - y0, rgba }
}

/// The picture on the terminal, and the plan it was made from.
pub struct Bridge {
    display: Display,
    shown: Option<Plan>,
}

impl Bridge {
    /// None where the terminal cannot hold a picture under its text.
    pub fn new() -> Option<Bridge> {
        let display = Display::new();
        display.keeps_pictures().then_some(Bridge { display, shown: None })
    }

    /// Put the picture up, unless this plan is the one up already. True
    /// when it was sent.
    pub fn show(&mut self, plan: Plan) -> bool {
        if self.shown.as_ref() == Some(&plan) {
            return false;
        }
        let whole = picture(&plan, None);
        let cuts: Vec<_> = pieces(&plan).into_iter().map(|q| (cut(&whole, q), q.0, q.1)).collect();
        let list: Vec<_> = cuts.iter().map(|(c, row, col)| (c, col + 1, row + 1)).collect();
        self.display.swap_canvases(&list);
        self.shown = Some(plan);
        true
    }

    /// Send it again at the next show, after the screen was cleared.
    pub fn forget(&mut self) {
        self.shown = None;
    }

    pub fn hide(&mut self) {
        self.shown = None;
        self.display.drop_canvas();
    }
}
