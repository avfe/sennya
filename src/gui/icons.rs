//! Small vector icons drawn with the painter, so they look crisp at any scale
//! and do not depend on which symbols a font happens to contain.

use egui::{pos2, vec2, Color32, Painter, Pos2, Rect, Sense, Stroke, StrokeKind, Ui};

use super::term_view::Palette;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Sidebar,
    SplitRight,
    SplitDown,
    Search,
    Plus,
    Close,
    /// Expanded (pointing down) or collapsed (pointing right).
    Chevron(bool),
    History,
    /// Hide a pane (a dash at the bottom, like minimize).
    Minimize,
    LayoutGrid,
    LayoutColumns,
    LayoutRows,
    LayoutMainStack,
    Bell,
    /// Crescent: asleep.
    Moon,
    /// Expand: two arrows pointing at opposite corners.
    Zoom,
    Check,
    Exclamation,
    /// Six dots: something you can drag.
    Grip,
    Gear,
}

pub fn paint(p: &Painter, icon: Icon, c: Pos2, color: Color32) {
    let s = Stroke::new(1.4, color);
    match icon {
        Icon::Sidebar | Icon::SplitRight | Icon::SplitDown => {
            let r = Rect::from_center_size(c, vec2(16.0, 13.0));
            p.rect_stroke(r, 3.0, s, StrokeKind::Inside);
            match icon {
                Icon::Sidebar => {
                    let x = r.left() + 5.5;
                    p.rect_filled(Rect::from_min_max(r.min + vec2(1.0, 1.0), pos2(x, r.bottom() - 1.0)), 2.0, color.gamma_multiply(0.35));
                    p.line_segment([pos2(x, r.top() + 0.7), pos2(x, r.bottom() - 0.7)], s);
                }
                Icon::SplitRight => {
                    p.line_segment([pos2(c.x, r.top() + 0.7), pos2(c.x, r.bottom() - 0.7)], s);
                }
                _ => {
                    p.line_segment([pos2(r.left() + 0.7, c.y), pos2(r.right() - 0.7, c.y)], s);
                }
            }
        }
        Icon::Search => {
            let o = c + vec2(-1.5, -1.5);
            p.circle_stroke(o, 4.6, s);
            p.line_segment([o + vec2(3.4, 3.4), o + vec2(7.0, 7.0)], Stroke::new(1.7, color));
        }
        Icon::Plus => {
            p.line_segment([c + vec2(-5.5, 0.0), c + vec2(5.5, 0.0)], s);
            p.line_segment([c + vec2(0.0, -5.5), c + vec2(0.0, 5.5)], s);
        }
        Icon::Close => {
            p.line_segment([c + vec2(-4.0, -4.0), c + vec2(4.0, 4.0)], s);
            p.line_segment([c + vec2(4.0, -4.0), c + vec2(-4.0, 4.0)], s);
        }
        Icon::LayoutGrid | Icon::LayoutColumns | Icon::LayoutRows | Icon::LayoutMainStack => {
            let r = Rect::from_center_size(c, vec2(15.0, 12.0));
            let cells: Vec<Rect> = match icon {
                Icon::LayoutGrid => {
                    let (w, h) = (r.width() / 2.0, r.height() / 2.0);
                    (0..4).map(|i| Rect::from_min_size(r.min + vec2((i % 2) as f32 * w, (i / 2) as f32 * h), vec2(w, h))).collect()
                }
                Icon::LayoutColumns => {
                    let w = r.width() / 3.0;
                    (0..3).map(|i| Rect::from_min_size(r.min + vec2(i as f32 * w, 0.0), vec2(w, r.height()))).collect()
                }
                Icon::LayoutRows => {
                    let h = r.height() / 3.0;
                    (0..3).map(|i| Rect::from_min_size(r.min + vec2(0.0, i as f32 * h), vec2(r.width(), h))).collect()
                }
                _ => {
                    let main = Rect::from_min_size(r.min, vec2(r.width() * 0.58, r.height()));
                    let x = main.right();
                    let h = r.height() / 2.0;
                    vec![main, Rect::from_min_size(pos2(x, r.top()), vec2(r.right() - x, h)), Rect::from_min_size(pos2(x, r.top() + h), vec2(r.right() - x, h))]
                }
            };
            for cell in cells {
                p.rect_filled(cell.shrink(0.9), 1.5, color);
            }
        }
        Icon::Moon => {
            let pts: Vec<Pos2> = (0..=16)
                .map(|i| {
                    let a = (40.0 + 280.0 * i as f32 / 16.0).to_radians();
                    c + vec2(a.cos(), -a.sin()) * 5.5
                })
                .collect();
            p.add(egui::Shape::line(pts, Stroke::new(2.0, color)));
        }
        Icon::Bell => {
            let pts: Vec<Pos2> = [(-5.0, 3.0), (-4.0, 1.8), (-4.0, -1.0), (-2.8, -3.6), (0.0, -4.8), (2.8, -3.6), (4.0, -1.0), (4.0, 1.8), (5.0, 3.0)]
                .iter()
                .map(|(x, y)| c + vec2(*x, *y))
                .collect();
            p.add(egui::Shape::closed_line(pts, s));
            p.circle_filled(c + vec2(0.0, 5.0), 1.3, color);
        }
        Icon::Zoom => {
            let (a, b) = (c + vec2(-4.5, 4.5), c + vec2(4.5, -4.5));
            p.line_segment([a, b], s);
            for (tip, dx, dy) in [(b, -3.0, 3.0), (a, 3.0, -3.0)] {
                p.line_segment([tip, tip + vec2(dx, 0.0)], s);
                p.line_segment([tip, tip + vec2(0.0, dy)], s);
            }
        }
        Icon::Check => {
            let s = Stroke::new(1.8, color);
            p.line_segment([c + vec2(-3.5, 0.2), c + vec2(-1.0, 2.7)], s);
            p.line_segment([c + vec2(-1.0, 2.7), c + vec2(3.6, -2.6)], s);
        }
        Icon::Exclamation => {
            p.line_segment([c + vec2(0.0, -4.0), c + vec2(0.0, 1.0)], Stroke::new(1.9, color));
            p.circle_filled(c + vec2(0.0, 3.6), 1.1, color);
        }
        Icon::Grip => {
            for (x, y) in [(-1.8, -3.2), (1.8, -3.2), (-1.8, 0.0), (1.8, 0.0), (-1.8, 3.2), (1.8, 3.2)] {
                p.circle_filled(c + vec2(x, y), 1.1, color);
            }
        }
        Icon::Gear => {
            p.circle_stroke(c, 3.4, s);
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::FRAC_PI_4;
                let d = vec2(a.cos(), a.sin());
                p.line_segment([c + d * 5.0, c + d * 7.0], Stroke::new(2.0, color));
            }
        }
        Icon::Minimize => {
            p.line_segment([c + vec2(-5.0, 1.5), c + vec2(5.0, 1.5)], Stroke::new(1.6, color));
        }
        Icon::History => {
            p.circle_stroke(c, 6.5, s);
            p.line_segment([c, c + vec2(0.0, -4.0)], s);
            p.line_segment([c, c + vec2(3.0, 1.5)], s);
        }
        Icon::Chevron(expanded) => {
            let pts = if expanded {
                [c + vec2(-4.0, -2.0), c + vec2(0.0, 2.0), c + vec2(4.0, -2.0)]
            } else {
                [c + vec2(-2.0, -4.0), c + vec2(2.0, 0.0), c + vec2(-2.0, 4.0)]
            };
            p.line_segment([pts[0], pts[1]], Stroke::new(1.6, color));
            p.line_segment([pts[1], pts[2]], Stroke::new(1.6, color));
        }
    }
}

/// Flat icon button: no background until hovered.
pub fn button(ui: &mut Ui, icon: Icon, pal: &Palette, tip: impl Into<String>) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 26.0), Sense::click());
    let hovered = resp.hovered();
    if hovered {
        ui.painter().rect_filled(rect, 5.0, pal.panel_hover);
    }
    paint(ui.painter(), icon, rect.center(), if hovered { pal.text } else { pal.dim });
    resp.on_hover_text(tip.into())
}
