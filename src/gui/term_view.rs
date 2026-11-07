//! Rendering a terminal frame with egui's painter, plus link detection.

use std::collections::HashMap;

use egui::text::{LayoutJob, TextFormat};
use egui::{pos2, Color32, FontFamily, FontId, Painter, Pos2, Rect, Stroke, Vec2};

use crate::config::Theme;
use crate::protocol::{flags, Color, CursorShape, Frame, NAMED_BG};

use super::fonts::BOLD_FAMILY;

pub fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

#[derive(Clone)]
pub struct Palette {
    pub table: [Color32; 256],
    pub fg: Color32,
    pub bg: Color32,
    pub cursor: Color32,
    pub selection: Color32,
    pub accent: Color32,
    pub dark: bool,
    pub panel: Color32,
    pub panel_hover: Color32,
    pub panel_active: Color32,
    pub border: Color32,
    pub text: Color32,
    pub dim: Color32,
    pub danger: Color32,
}

impl Palette {
    pub fn new(theme: &Theme) -> Palette {
        let mut table = [Color32::BLACK; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            *slot = rgb(theme.indexed(i as u8));
        }
        let bg = rgb(theme.background);
        let fg = rgb(theme.foreground);
        let accent = rgb(theme.accent);
        let panel = match theme.panel {
            Some(p) => rgb(p),
            None if theme.dark => mix(bg, Color32::BLACK, 0.3),
            None => mix(bg, Color32::BLACK, 0.04),
        };
        Palette {
            table,
            fg,
            bg,
            cursor: rgb(theme.cursor),
            selection: rgb(theme.selection),
            accent,
            dark: theme.dark,
            panel,
            panel_hover: mix(panel, fg, 0.07),
            panel_active: mix(panel, accent, 0.22),
            border: mix(bg, fg, 0.13),
            text: fg,
            dim: mix(fg, bg, 0.45),
            danger: rgb(theme.ansi[1]),
        }
    }

    pub fn color(&self, c: Color) -> Color32 {
        match c {
            Color::Named(n) => match n {
                0..=15 => self.table[n as usize],
                256 | 267 => self.fg,
                257 => self.bg,
                258 => self.cursor,
                259..=266 => mix(self.table[(n - 259) as usize], self.bg, 0.35),
                268 => mix(self.fg, self.bg, 0.35),
                _ => self.fg,
            },
            Color::Indexed(i) => self.table[i as usize],
            Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
        }
    }

    /// Final foreground/background of a run after inverse, dim and selection.
    fn resolve(&self, fg: Color, bg: Color, f: u16) -> (Color32, Color32, bool) {
        let mut fgc = self.color(fg);
        let default_bg = bg == Color::Named(NAMED_BG);
        let mut bgc = self.color(bg);
        let mut has_bg = !default_bg;
        if f & flags::INVERSE != 0 {
            std::mem::swap(&mut fgc, &mut bgc);
            has_bg = true;
        }
        if f & flags::DIM != 0 {
            fgc = mix(fgc, bgc, 0.4);
        }
        if f & flags::SELECTED != 0 {
            bgc = self.selection;
            has_bg = true;
        }
        (fgc, bgc, has_bg)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Metrics {
    pub font: FontId,
    pub bold: FontId,
    pub size: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    pub text_dy: f32,
    pub ppp: f32,
}

impl Metrics {
    pub fn new(ctx: &egui::Context, size: f32, line_height: f32) -> Metrics {
        let font = FontId::monospace(size);
        let bold = FontId::new(size, FontFamily::Name(BOLD_FAMILY.into()));
        let (w, row_h) = ctx.fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
        let ppp = ctx.pixels_per_point();
        let snap = |v: f32| (v * ppp).round() / ppp;
        let cell_h = snap(row_h * line_height.max(1.0));
        Metrics { font, bold, size, cell_w: w, cell_h, text_dy: snap((cell_h - row_h) / 2.0), ppp }
    }

    /// Columns and rows that fit into `size`.
    pub fn grid_size(&self, size: Vec2) -> (u16, u16) {
        let cols = (size.x / self.cell_w).floor().max(2.0) as u16;
        let rows = (size.y / self.cell_h).floor().max(1.0) as u16;
        (cols, rows)
    }

    pub fn cell_px(&self) -> (u16, u16) {
        ((self.cell_w * self.ppp).round() as u16, (self.cell_h * self.ppp).round() as u16)
    }
}

/// Remembers which characters are exactly one cell wide in the terminal font.
#[derive(Default)]
pub struct GlyphCache {
    fits: HashMap<(char, bool), bool>,
}

impl GlyphCache {
    pub fn clear(&mut self) {
        self.fits.clear();
    }

    fn fits(&mut self, ctx: &egui::Context, m: &Metrics, bold: bool, c: char) -> bool {
        if c.is_ascii() {
            return true;
        }
        *self.fits.entry((c, bold)).or_insert_with(|| {
            let font = if bold { &m.bold } else { &m.font };
            let w = ctx.fonts_mut(|f| f.glyph_width(font, c));
            (w - m.cell_w).abs() <= m.cell_w * 0.05
        })
    }
}

pub fn is_zero_width(c: char) -> bool {
    matches!(c as u32,
        0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF |
        0xFE20..=0xFE2F | 0x200B..=0x200F | 0xFE00..=0xFE0F | 0xE0100..=0xE01EF)
}

/// Box drawing (U+2500..U+257F) arms as "up right down left": 0 none, 1 light, 2 heavy, 3 double.
/// Empty entries (dashes, arcs, diagonals) are handled separately.
const BOX_ARMS: [&str; 128] = [
    "0101", "0202", "1010", "2020", "", "", "", "", "", "", "", "", "0110", "0210", "0120", "0220",
    "0011", "0012", "0021", "0022", "1100", "1200", "2100", "2200", "1001", "1002", "2001", "2002", "1110", "1210", "2110", "1120",
    "2120", "2210", "1220", "2220", "1011", "1012", "2011", "1021", "2021", "2012", "1022", "2022", "0111", "0112", "0211", "0212",
    "0121", "0122", "0221", "0222", "1101", "1102", "1201", "1202", "2101", "2102", "2201", "2202", "1111", "1112", "1211", "1212",
    "2111", "1121", "2121", "2112", "2211", "1122", "1221", "2212", "1222", "2122", "2221", "2222", "", "", "", "",
    "0303", "3030", "0310", "0130", "0330", "0013", "0031", "0033", "1300", "3100", "3300", "1003", "3001", "3003", "1310", "3130",
    "3330", "1013", "3031", "3033", "0313", "0131", "0333", "1303", "3101", "3303", "1313", "3131", "3333", "", "", "",
    "", "", "", "", "0001", "1000", "0100", "0010", "0002", "2000", "0200", "0020", "0201", "1020", "0102", "2010",
];

/// Characters drawn as geometry so lines and blocks join seamlessly across cells.
pub fn is_procedural(c: char) -> bool {
    matches!(c as u32, 0x2500..=0x259F | 0xE0B0..=0xE0B3)
}

pub fn draw_procedural(p: &Painter, c: char, cell: Rect, color: Color32, ppp: f32) {
    let snap = |v: f32| (v * ppp).round() / ppp;
    let cell = Rect::from_min_max(pos2(snap(cell.min.x), snap(cell.min.y)), pos2(snap(cell.max.x), snap(cell.max.y)));
    let (w, h) = (cell.width(), cell.height());
    let light = ((w / 8.0) * ppp).round().max(1.0) / ppp;
    let heavy = light * 2.0;
    let cx = snap(cell.center().x - light / 2.0) + light / 2.0;
    let cy = snap(cell.center().y - light / 2.0) + light / 2.0;
    let fill = |r: Rect| {
        p.rect_filled(r, 0.0, color);
    };
    let code = c as u32;
    match code {
        0x2504 | 0x2505 | 0x2508 | 0x2509 | 0x254C | 0x254D | 0x2506 | 0x2507 | 0x250A | 0x250B | 0x254E | 0x254F => {
            let n = match code {
                0x2504..=0x2507 => 3.0,
                0x2508..=0x250B => 4.0,
                _ => 2.0,
            };
            let t = if code % 2 == 1 { heavy } else { light };
            let vertical = matches!(code, 0x2506 | 0x2507 | 0x250A | 0x250B | 0x254E | 0x254F);
            let len = if vertical { h } else { w };
            let seg = len / n;
            for i in 0..n as usize {
                let a = i as f32 * seg + seg * 0.2;
                let b = a + seg * 0.6;
                if vertical {
                    fill(Rect::from_min_max(pos2(cx - t / 2.0, cell.top() + a), pos2(cx + t / 2.0, cell.top() + b)));
                } else {
                    fill(Rect::from_min_max(pos2(cell.left() + a, cy - t / 2.0), pos2(cell.left() + b, cy + t / 2.0)));
                }
            }
        }
        0x256D..=0x2570 => {
            // Rounded corners: (sx, sy) point toward the horizontal and vertical arms.
            let (sx, sy) = match code {
                0x256D => (1.0, 1.0),
                0x256E => (-1.0, 1.0),
                0x256F => (-1.0, -1.0),
                _ => (1.0, -1.0),
            };
            let r = (w / 2.0).min(h / 2.0);
            let center = pos2(cx + sx * r, cy + sy * r);
            let mut pts = vec![pos2(cx, if sy > 0.0 { cell.bottom() } else { cell.top() })];
            for i in 0..=8 {
                let t = std::f32::consts::FRAC_PI_2 * i as f32 / 8.0;
                pts.push(pos2(center.x - sx * r * t.cos(), center.y - sy * r * t.sin()));
            }
            pts.push(pos2(if sx > 0.0 { cell.right() } else { cell.left() }, cy));
            p.add(egui::Shape::line(pts, Stroke::new(light, color)));
        }
        0x2571..=0x2573 => {
            let s = Stroke::new(light, color);
            if code != 0x2572 {
                p.line_segment([cell.left_bottom(), cell.right_top()], s);
            }
            if code != 0x2571 {
                p.line_segment([cell.left_top(), cell.right_bottom()], s);
            }
        }
        0x2500..=0x257F => {
            let arms: Vec<u8> = BOX_ARMS[(code - 0x2500) as usize].bytes().map(|b| b - b'0').collect();
            if arms.len() != 4 {
                return;
            }
            let thick = |a: u8| match a {
                1 => light,
                2 => heavy,
                3 => light * 3.0,
                _ => 0.0,
            };
            let ext = arms.iter().map(|a| thick(*a)).fold(0.0f32, f32::max) / 2.0;
            for (dir, &a) in arms.iter().enumerate() {
                if a == 0 {
                    continue;
                }
                let t = if a == 2 { heavy } else { light };
                let offsets: &[f32] = if a == 3 { &[-light, light] } else { &[0.0] };
                for &off in offsets {
                    let r = match dir {
                        0 => Rect::from_min_max(pos2(cx + off - t / 2.0, cell.top()), pos2(cx + off + t / 2.0, cy + ext)),
                        1 => Rect::from_min_max(pos2(cx - ext, cy + off - t / 2.0), pos2(cell.right(), cy + off + t / 2.0)),
                        2 => Rect::from_min_max(pos2(cx + off - t / 2.0, cy - ext), pos2(cx + off + t / 2.0, cell.bottom())),
                        _ => Rect::from_min_max(pos2(cell.left(), cy + off - t / 2.0), pos2(cx + ext, cy + off + t / 2.0)),
                    };
                    fill(r);
                }
            }
        }
        0x2580..=0x259F => {
            let (x0, y0, x1, y1) = (cell.left(), cell.top(), cell.right(), cell.bottom());
            let rect = |a: f32, b: f32, c: f32, d: f32| Rect::from_min_max(pos2(snap(a), snap(b)), pos2(snap(c), snap(d)));
            match code {
                0x2580 => fill(rect(x0, y0, x1, y0 + h / 2.0)),
                0x2581..=0x2587 => fill(rect(x0, y1 - h * (code - 0x2580) as f32 / 8.0, x1, y1)),
                0x2588 => fill(cell),
                0x2589..=0x258F => fill(rect(x0, y0, x0 + w * (0x2590 - code) as f32 / 8.0, y1)),
                0x2590 => fill(rect(x0 + w / 2.0, y0, x1, y1)),
                0x2591..=0x2593 => {
                    p.rect_filled(cell, 0.0, color.gamma_multiply((code - 0x2590) as f32 * 0.25));
                }
                0x2594 => fill(rect(x0, y0, x1, y0 + h / 8.0)),
                0x2595 => fill(rect(x1 - w / 8.0, y0, x1, y1)),
                _ => {
                    // Quadrants: 1 upper left, 2 upper right, 4 lower left, 8 lower right.
                    let mask = [4u8, 8, 1, 1 | 4 | 8, 1 | 8, 1 | 2 | 4, 1 | 2 | 8, 2, 2 | 4, 2 | 4 | 8][(code - 0x2596) as usize];
                    let (mx, my) = (x0 + w / 2.0, y0 + h / 2.0);
                    for (bit, r) in [(1, rect(x0, y0, mx, my)), (2, rect(mx, y0, x1, my)), (4, rect(x0, my, mx, y1)), (8, rect(mx, my, x1, y1))] {
                        if mask & bit != 0 {
                            fill(r);
                        }
                    }
                }
            }
        }
        _ => {
            // Powerline separators.
            let (l, r, t, b, m) = (cell.left(), cell.right(), cell.top(), cell.bottom(), cell.center().y);
            let pts = if code <= 0xE0B1 { vec![pos2(l, t), pos2(r, m), pos2(l, b)] } else { vec![pos2(r, t), pos2(l, m), pos2(r, b)] };
            if code == 0xE0B0 || code == 0xE0B2 {
                p.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
            } else {
                p.add(egui::Shape::line(pts, Stroke::new(light, color)));
            }
        }
    }
}

pub struct DrawOpts<'a> {
    pub focused: bool,
    pub cursor_on: bool,
    /// (row, start col, end col) cells to underline as a hovered link.
    pub link: &'a [(u16, u16, u16)],
}

fn text_job(text: String, font: &FontId, color: Color32, italic: bool) -> LayoutJob {
    LayoutJob::single_section(
        text,
        TextFormat { font_id: font.clone(), color, italics: italic, ..Default::default() },
    )
}

#[allow(clippy::too_many_arguments)]
fn draw_text(
    painter: &Painter,
    ctx: &egui::Context,
    cache: &mut GlyphCache,
    m: &Metrics,
    text: &str,
    origin: Pos2,
    cells: u16,
    font: &FontId,
    color: Color32,
    italic: bool,
    bold: bool,
) {
    let mut seg = String::new();
    let mut seg_col = 0u16;
    let mut col = 0u16;
    let flush = |seg: &mut String, seg_col: u16| {
        let trimmed = seg.trim_end();
        if !trimmed.is_empty() {
            let galley = painter.layout_job(text_job(trimmed.to_string(), font, color, italic));
            painter.galley(Pos2::new(origin.x + seg_col as f32 * m.cell_w, origin.y + m.text_dy), galley, color);
        }
        seg.clear();
    };
    for c in text.chars() {
        if is_zero_width(c) {
            seg.push(c);
            continue;
        }
        if seg.is_empty() && c == ' ' {
            col += 1;
            seg_col = col;
            continue;
        }
        if is_procedural(c) {
            flush(&mut seg, seg_col);
            let cell = Rect::from_min_size(pos2(origin.x + col as f32 * m.cell_w, origin.y), Vec2::new(m.cell_w, m.cell_h));
            draw_procedural(painter, c, cell, color, m.ppp);
            col += 1;
            seg_col = col;
            continue;
        }
        if cells == 1 && !cache.fits(ctx, m, bold, c) {
            flush(&mut seg, seg_col);
            let galley = painter.layout_job(text_job(c.to_string(), font, color, italic));
            let dx = ((m.cell_w - galley.size().x) / 2.0).max(0.0);
            painter.galley(Pos2::new(origin.x + col as f32 * m.cell_w + dx, origin.y + m.text_dy), galley, color);
            col += 1;
            seg_col = col;
            continue;
        }
        if seg.is_empty() {
            seg_col = col;
        }
        seg.push(c);
        col += cells;
    }
    flush(&mut seg, seg_col);
}

pub fn draw(
    painter: &Painter,
    ctx: &egui::Context,
    cache: &mut GlyphCache,
    origin: Pos2,
    frame: &Frame,
    m: &Metrics,
    pal: &Palette,
    opts: &DrawOpts<'_>,
) {
    let (cw, ch) = (m.cell_w, m.cell_h);
    for (r, line) in frame.lines.iter().enumerate() {
        let y = origin.y + r as f32 * ch;
        for run in &line.runs {
            let (_, bg, has_bg) = pal.resolve(run.fg, run.bg, run.flags);
            if has_bg {
                let rect = Rect::from_min_size(
                    Pos2::new(origin.x + run.col as f32 * cw, y),
                    Vec2::new(run.width as f32 * cw, ch),
                );
                painter.rect_filled(rect, 0.0, bg);
            }
        }
        for run in &line.runs {
            if run.flags & flags::HIDDEN != 0 {
                continue;
            }
            let (fg, _, _) = pal.resolve(run.fg, run.bg, run.flags);
            let x = origin.x + run.col as f32 * cw;
            let bold = run.flags & flags::BOLD != 0;
            let font = if bold { &m.bold } else { &m.font };
            let italic = run.flags & flags::ITALIC != 0;
            if run.flags & flags::WIDE != 0 {
                let galley = painter.layout_job(text_job(run.text.clone(), font, fg, italic));
                let dx = ((2.0 * cw - galley.size().x) / 2.0).max(0.0);
                painter.galley(Pos2::new(x + dx, y + m.text_dy), galley, fg);
            } else if run.text.chars().any(|c| c != ' ') {
                draw_text(painter, ctx, cache, m, &run.text, Pos2::new(x, y), 1, font, fg, italic, bold);
            }
            let w = run.width as f32 * cw;
            let stroke = Stroke::new(1.0, fg);
            if run.flags & (flags::UNDERLINE | flags::DOUBLE_UNDERLINE) != 0 || (run.link != 0 && opts.link.is_empty()) {
                painter.hline(x..=x + w, y + ch - 1.5, if run.link != 0 { Stroke::new(1.0, fg.gamma_multiply(0.5)) } else { stroke });
            }
            if run.flags & flags::DOUBLE_UNDERLINE != 0 {
                painter.hline(x..=x + w, y + ch - 3.5, stroke);
            }
            if run.flags & flags::UNDERCURL != 0 {
                let mut pts = Vec::new();
                let mut px = x;
                let mut up = true;
                while px <= x + w {
                    pts.push(Pos2::new(px, y + ch - if up { 3.0 } else { 1.0 }));
                    px += 2.0;
                    up = !up;
                }
                painter.add(egui::Shape::line(pts, stroke));
            }
            if run.flags & flags::STRIKE != 0 {
                painter.hline(x..=x + w, y + ch / 2.0, stroke);
            }
        }
    }

    for &(row, start, end) in opts.link {
        let y = origin.y + row as f32 * ch + ch - 1.5;
        painter.hline(
            origin.x + start as f32 * cw..=origin.x + end as f32 * cw,
            y,
            Stroke::new(1.5, pal.accent),
        );
    }

    if let Some(cursor) = frame.cursor.filter(|_| opts.cursor_on) {
        let width = if cursor.wide { 2.0 } else { 1.0 };
        let rect = Rect::from_min_size(
            Pos2::new(origin.x + cursor.col as f32 * cw, origin.y + cursor.row as f32 * ch),
            Vec2::new(cw * width, ch),
        );
        let color = pal.cursor;
        match (cursor.shape, opts.focused) {
            (CursorShape::Block, true) => {
                painter.rect_filled(rect, 1.0, color);
                if let Some((c, bold)) = char_at(frame, cursor.row, cursor.col) {
                    if c != ' ' {
                        let font = if bold { &m.bold } else { &m.font };
                        let galley = painter.layout_job(text_job(c.to_string(), font, pal.bg, false));
                        let dx = ((rect.width() - galley.size().x) / 2.0).max(0.0);
                        painter.galley(Pos2::new(rect.min.x + dx, rect.min.y + m.text_dy), galley, pal.bg);
                    }
                }
            }
            (CursorShape::Beam, _) => {
                painter.rect_filled(Rect::from_min_size(rect.min, Vec2::new(2.0, ch)), 0.0, color);
            }
            (CursorShape::Underline, _) => {
                painter.rect_filled(
                    Rect::from_min_size(Pos2::new(rect.min.x, rect.max.y - 2.0), Vec2::new(rect.width(), 2.0)),
                    0.0,
                    color,
                );
            }
            _ => {
                painter.rect_stroke(rect.shrink(0.5), 1.0, Stroke::new(1.0, color), egui::StrokeKind::Inside);
            }
        }
    }
}

fn char_at(frame: &Frame, row: u16, col: u16) -> Option<(char, bool)> {
    let line = frame.lines.get(row as usize)?;
    for run in &line.runs {
        if col >= run.col && col < run.col + run.width {
            let step = if run.flags & flags::WIDE != 0 { 2 } else { 1 };
            let mut c_col = run.col;
            for c in run.text.chars().filter(|c| !is_zero_width(*c)) {
                if col >= c_col && col < c_col + step {
                    return Some((c, run.flags & flags::BOLD != 0));
                }
                c_col += step;
            }
        }
    }
    Some((' ', false))
}

fn row_chars(frame: &Frame, row: usize) -> Vec<char> {
    let mut out = vec![' '; frame.cols as usize];
    if let Some(line) = frame.lines.get(row) {
        for run in &line.runs {
            let step = if run.flags & flags::WIDE != 0 { 2 } else { 1 };
            let mut col = run.col as usize;
            for c in run.text.chars().filter(|c| !is_zero_width(*c)) {
                if col < out.len() {
                    out[col] = c;
                }
                if step == 2 && col + 1 < out.len() {
                    out[col + 1] = '\0';
                }
                col += step;
            }
        }
    }
    out
}

fn is_url_char(c: char) -> bool {
    !c.is_whitespace()
        && !c.is_control()
        && c != '\0'
        && !"<>\"`{}|\\^".contains(c)
        && !('\u{2500}'..='\u{259F}').contains(&c)
}

/// Character range of a URL covering index `idx`.
pub fn url_span(chars: &[char], idx: usize) -> Option<(usize, usize)> {
    if idx >= chars.len() || !is_url_char(chars[idx]) {
        return None;
    }
    let mut s = idx;
    while s > 0 && is_url_char(chars[s - 1]) {
        s -= 1;
    }
    let mut e = idx + 1;
    while e < chars.len() && is_url_char(chars[e]) {
        e += 1;
    }
    let token: String = chars[s..e].iter().collect();
    let byte_pos = ["https://", "http://", "file://", "ftp://", "mailto:"]
        .iter()
        .filter_map(|scheme| token.find(scheme))
        .min()?;
    let start = s + token[..byte_pos].chars().count();
    let mut end = e;
    loop {
        let c = chars[end - 1];
        let count = |ch: char| chars[start..end].iter().filter(|x| **x == ch).count();
        let strip = ".,;:!?'\"".contains(c)
            || (c == ')' && count('(') < count(')'))
            || (c == ']' && count('[') < count(']'));
        if strip && end > start + 1 {
            end -= 1;
        } else {
            break;
        }
    }
    (idx >= start && idx < end && end - start > 8).then_some((start, end))
}

/// The link under a cell: an OSC 8 hyperlink or a URL in the text (across soft wraps).
pub fn link_at(frame: &Frame, row: u16, col: u16) -> Option<(String, Vec<(u16, u16, u16)>)> {
    let line = frame.lines.get(row as usize)?;
    if let Some(run) = line.runs.iter().find(|r| r.link != 0 && col >= r.col && col < r.col + r.width) {
        let url = frame.links.get(run.link as usize - 1)?.clone();
        let ranges = frame
            .lines
            .iter()
            .enumerate()
            .flat_map(|(r, l)| {
                l.runs.iter().filter(|x| x.link == run.link).map(move |x| (r as u16, x.col, x.col + x.width))
            })
            .collect();
        return Some((url, ranges));
    }
    let rows = frame.lines.len();
    let cols = frame.cols as usize;
    let mut start = row as usize;
    while start > 0 && frame.lines[start - 1].wrapped {
        start -= 1;
    }
    let mut end = row as usize;
    while end + 1 < rows && frame.lines[end].wrapped {
        end += 1;
    }
    let chars: Vec<char> = (start..=end).flat_map(|r| row_chars(frame, r)).collect();
    let idx = (row as usize - start) * cols + col as usize;
    let (s, e) = url_span(&chars, idx)?;
    let url: String = chars[s..e].iter().filter(|c| **c != '\0').collect();
    let mut ranges = Vec::new();
    for r in start..=end {
        let row_start = (r - start) * cols;
        let a = s.max(row_start);
        let b = e.min(row_start + cols);
        if a < b {
            ranges.push((r as u16, (a - row_start) as u16, (b - row_start) as u16));
        }
    }
    Some((url, ranges))
}

/// Open a link with the system handler. Only well-known schemes are allowed.
pub fn open_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    if !["http://", "https://", "file://", "ftp://", "mailto:"].iter().any(|s| lower.starts_with(s)) {
        return false;
    }
    crate::client::spawn_detached(&mut crate::sys::open_command(url.as_ref())).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(s: &str, at: usize) -> Option<String> {
        let chars: Vec<char> = s.chars().collect();
        url_span(&chars, at).map(|(a, b)| chars[a..b].iter().collect())
    }

    #[test]
    fn urls() {
        assert_eq!(span("see https://example.com/a?b=1.", 10).as_deref(), Some("https://example.com/a?b=1"));
        assert_eq!(span("(https://en.wikipedia.org/wiki/Rust_(lang))", 5).as_deref(), Some("https://en.wikipedia.org/wiki/Rust_(lang)"));
        assert_eq!(span("no link here", 3), None);
        assert_eq!(span("│https://x.io/path│", 3).as_deref(), Some("https://x.io/path"));
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    use crate::protocol::{Color, Line, Run};
    use std::time::Instant;

    fn frame(cols: u16, rows: u16, seed: usize) -> Frame {
        let words = ["cargo", "build", "--release", "│", "warning:", "unused", "src/main.rs:42", "Ok(())", "привет", "⏺"];
        let lines = (0..rows)
            .map(|r| {
                let mut runs = Vec::new();
                let mut col = 0u16;
                let mut i = r as usize + seed;
                while col + 12 < cols {
                    let text = format!("{} ", words[i % words.len()]);
                    let width = text.chars().count() as u16;
                    runs.push(Run {
                        col,
                        width,
                        text,
                        fg: Color::Named((i % 8) as u16),
                        bg: if i % 7 == 0 { Color::Indexed(236) } else { Color::Named(257) },
                        flags: 0,
                        link: 0,
                    });
                    col += width;
                    i += 3;
                }
                Line { runs, wrapped: false }
            })
            .collect();
        Frame { id: 1, cols, rows, display_offset: 0, history: 0, mode: 0, cursor: None, lines, links: vec![] }
    }

    /// `cargo test --release --lib bench -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn render_cost() {
        let ctx = egui::Context::default();
        let theme = crate::config::Theme::builtin("sennya").unwrap();
        let pal = Palette::new(&theme);
        let mut cache = GlyphCache::default();
        let input = || egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(2400.0, 1400.0))),
            ..Default::default()
        };
        let _ = ctx.run(input(), |_| {});
        let m = Metrics::new(&ctx, 13.5, 1.25);
        for (panes, cols, rows) in [(1u16, 200u16, 60u16), (4, 100, 30), (9, 66, 20)] {
            for changing in [false, true] {
                let mut times = Vec::new();
                for n in 0..60 {
                    let frames: Vec<Frame> = (0..panes).map(|p| frame(cols, rows, if changing { n + p as usize } else { p as usize })).collect();
                    let t = Instant::now();
                    let out = ctx.run(input(), |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            for (i, f) in frames.iter().enumerate() {
                                let origin = Pos2::new((i % 3) as f32 * 700.0, (i / 3) as f32 * 450.0);
                                let opts = DrawOpts { focused: i == 0, cursor_on: true, link: &[] };
                                draw(ui.painter(), ctx, &mut cache, origin, f, &m, &pal, &opts);
                            }
                        });
                    });
                    let _prims = ctx.tessellate(out.shapes, out.pixels_per_point);
                    times.push(t.elapsed());
                }
                times.sort();
                println!(
                    "{panes} pane(s) {cols}x{rows} {}: p50 {:?}  max {:?}",
                    if changing { "all text changing" } else { "static" },
                    times[30],
                    times[59]
                );
            }
        }
    }
}

#[cfg(test)]
mod metrics_check {
    use super::*;

    /// `cargo test --release --lib metrics_check -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn text_matches_cells() {
        let ctx = egui::Context::default();
        super::super::fonts::install(&ctx, "JetBrains Mono, SF Mono, Menlo");
        for ppp in [1.0f32, 2.0] {
            ctx.set_pixels_per_point(ppp);
            let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))), ..Default::default() };
            let _ = ctx.run(input.clone(), |_| {});
            let _ = ctx.run(input, |_| {});
            for size in [12.0f32, 13.5, 14.0] {
                let m = Metrics::new(&ctx, size, 1.25);
                let text = "(base) me@laptop ~ % ".repeat(3);
                let n = text.chars().count() as f32;
                let w = ctx.fonts_mut(|f| f.layout_no_wrap(text.clone(), m.font.clone(), Color32::WHITE).size().x);
                println!("ppp {ppp} size {size}: cell_w {:.4}  galley/char {:.4}  drift over {n} chars {:.2}pt", m.cell_w, w / n, w - n * m.cell_w);
            }
        }
    }
}

#[cfg(test)]
mod ui_font_check {
    use super::*;

    /// `cargo test --release --lib ui_font_check -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn ui_font() {
        let ctx = egui::Context::default();
        super::super::fonts::install(&ctx, "SF Mono");
        ctx.set_pixels_per_point(2.0);
        let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))), ..Default::default() };
        let text = "Click to collapse · + adds a terminal here  New terminal ⌘\u{2009}T  ⇧\u{2009}⌘\u{2009}D ⌥ ⌃ ↩ Привет";
        for _ in 0..2 {
            let _ = ctx.run(input.clone(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.label(egui::RichText::new(text).size(13.0));
                });
            });
        }
        let font = FontId::proportional(13.0);
        for c in ['⌘', '⇧', '⌥', '⌃', '↩', '\u{2009}', 'П'] {
            let has = ctx.fonts_mut(|f| f.has_glyph(&font, c));
            println!("glyph {c:?}: {has}");
        }
        let img = ctx.fonts(|f| f.image());
        let (w, h) = (img.size[0] as u32, img.size[1] as u32);
        let rows = (0..h as usize).take_while(|y| img.pixels[y * w as usize..(y + 1) * w as usize].iter().any(|p| p.a() > 0)).count().max(1) as u32;
        let mut bytes = Vec::new();
        for y in 0..rows as usize {
            for x in 0..w as usize {
                let a = img.pixels[y * w as usize + x].a();
                bytes.extend_from_slice(&[255 - a, 255 - a, 255 - a, 255]);
            }
        }
        let path = std::env::temp_dir().join("sennya-ui-font.png");
        let file = std::fs::File::create(&path).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, rows);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&bytes).unwrap();
        println!("atlas: {}", path.display());
    }
}
