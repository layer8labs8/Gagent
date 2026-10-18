//! Colors, styling and the Gagent logo.

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Painter, Pos2, Shape, Stroke, TextStyle, Vec2,
};

pub const BG: Color32 = Color32::from_rgb(0x0B, 0x0F, 0x17);
pub const SIDEBAR_BG: Color32 = Color32::from_rgb(0x08, 0x0B, 0x12);
pub const PANEL_BG: Color32 = Color32::from_rgb(0x0E, 0x13, 0x1D);
pub const CARD_BG: Color32 = Color32::from_rgb(0x12, 0x18, 0x24);
pub const CARD_BG_HOVER: Color32 = Color32::from_rgb(0x18, 0x20, 0x30);
pub const INPUT_BG: Color32 = Color32::from_rgb(0x0F, 0x15, 0x20);
pub const BORDER: Color32 = Color32::from_rgb(0x1E, 0x29, 0x3B);
pub const TEXT: Color32 = Color32::from_rgb(0xE2, 0xE8, 0xF0);
pub const TEXT_STRONG: Color32 = Color32::from_rgb(0xF8, 0xFA, 0xFC);
pub const MUTED: Color32 = Color32::from_rgb(0x94, 0xA3, 0xB8);
pub const FAINT: Color32 = Color32::from_rgb(0x64, 0x74, 0x8B);
pub const ACCENT: Color32 = Color32::from_rgb(0x22, 0xD3, 0xEE);
pub const ACCENT_DEEP: Color32 = Color32::from_rgb(0x25, 0x63, 0xEB);
pub const ACCENT_BG: Color32 = Color32::from_rgb(0x0B, 0x2A, 0x3A);
pub const OK: Color32 = Color32::from_rgb(0x34, 0xD3, 0x99);
pub const WARN: Color32 = Color32::from_rgb(0xFB, 0xBF, 0x24);
pub const ERR: Color32 = Color32::from_rgb(0xF8, 0x71, 0x71);
pub const ERR_BG: Color32 = Color32::from_rgb(0x2A, 0x12, 0x16);
pub const USER_BG: Color32 = Color32::from_rgb(0x16, 0x20, 0x33);
pub const CODE_BG: Color32 = Color32::from_rgb(0x09, 0x0D, 0x14);

pub const RADIUS: u8 = 8;

/// Applies the Gagent look to an egui context.
pub fn apply(ctx: &egui::Context) {
    let mut style = (*ctx.global_style()).clone();
    style.text_styles = [
        (
            TextStyle::Heading,
            FontId::new(20.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(14.5, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(14.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Small,
            FontId::new(12.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(13.0, FontFamily::Monospace),
        ),
    ]
    .into();
    style.spacing.item_spacing = Vec2::new(8.0, 6.0);
    style.spacing.button_padding = Vec2::new(10.0, 5.0);
    style.spacing.scroll.bar_width = 8.0;

    let v = &mut style.visuals;
    *v = egui::Visuals::dark();
    v.override_text_color = Some(TEXT);
    v.panel_fill = BG;
    v.window_fill = PANEL_BG;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.extreme_bg_color = INPUT_BG;
    v.faint_bg_color = CARD_BG;
    v.code_bg_color = CODE_BG;
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = Color32::from_rgb(0x15, 0x4E, 0x6B);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    let radius = CornerRadius::same(RADIUS);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = radius;
    }
    v.widgets.noninteractive.bg_fill = PANEL_BG;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.inactive.bg_fill = CARD_BG;
    v.widgets.inactive.weak_bg_fill = CARD_BG;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.hovered.bg_fill = CARD_BG_HOVER;
    v.widgets.hovered.weak_bg_fill = CARD_BG_HOVER;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x33, 0x41, 0x55));
    v.widgets.active.bg_fill = ACCENT_BG;
    v.widgets.active.weak_bg_fill = ACCENT_BG;
    v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    v.widgets.open.bg_fill = CARD_BG_HOVER;
    v.widgets.open.weak_bg_fill = CARD_BG_HOVER;
    v.popup_shadow = egui::Shadow::NONE;
    v.window_shadow = egui::Shadow::NONE;
    ctx.set_global_style(style);
}

/// The six corners of a flat-topped hexagon.
fn hexagon(center: Pos2, radius: f32) -> Vec<Pos2> {
    (0..6)
        .map(|i| {
            let angle = std::f32::consts::FRAC_PI_3 * i as f32 - std::f32::consts::FRAC_PI_2;
            center + Vec2::angled(angle) * radius
        })
        .collect()
}

/// Draws the Gagent emblem: a hexagon outline around a solid hexagon core.
pub fn paint_logo(painter: &Painter, center: Pos2, size: f32) {
    let outer = hexagon(center, size * 0.5);
    painter.add(Shape::closed_line(
        outer,
        Stroke::new((size * 0.09).max(1.5), ACCENT),
    ));
    let mid = hexagon(center, size * 0.33);
    painter.add(Shape::closed_line(
        mid,
        Stroke::new((size * 0.05).max(1.0), ACCENT_DEEP),
    ));
    let core = hexagon(center, size * 0.17);
    painter.add(Shape::convex_polygon(core, ACCENT, Stroke::NONE));
}

/// Allocates space and paints the logo.
pub fn logo(ui: &mut egui::Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    paint_logo(ui.painter(), rect.center(), size);
}

/// Draws a rotating arc. Keeps the UI repainting while it is visible.
pub fn paint_spinner(painter: &Painter, ctx: &egui::Context, center: Pos2, size: f32) {
    let t = ctx.input(|i| i.time) as f32;
    let r = (size * 0.5 - 1.5).max(2.0);
    painter.circle_stroke(center, r, Stroke::new(1.5, BORDER));
    let start = t * 5.0;
    let points: Vec<Pos2> = (0..=14)
        .map(|i| {
            let angle = start + i as f32 * (1.9 / 14.0);
            center + Vec2::angled(angle) * r
        })
        .collect();
    painter.add(Shape::line(points, Stroke::new(2.0, ACCENT)));
    ctx.request_repaint_after(std::time::Duration::from_millis(33));
}

/// Allocates space and draws a spinner.
pub fn spinner(ui: &mut egui::Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    paint_spinner(ui.painter(), ui.ctx(), rect.center(), size);
}

/// A small triangle that points right (collapsed) or down (expanded).
pub fn chevron(ui: &mut egui::Ui, expanded: bool, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(12.0), egui::Sense::hover());
    let c = rect.center();
    let points = if expanded {
        vec![
            c + Vec2::new(-4.0, -2.0),
            c + Vec2::new(4.0, -2.0),
            c + Vec2::new(0.0, 3.5),
        ]
    } else {
        vec![
            c + Vec2::new(-2.0, -4.0),
            c + Vec2::new(-2.0, 4.0),
            c + Vec2::new(3.5, 0.0),
        ]
    };
    ui.painter()
        .add(Shape::convex_polygon(points, color, Stroke::NONE));
}

/// RGBA pixels of the window icon (size x size), drawn with anti-aliasing.
pub fn icon_rgba(size: u32) -> Vec<u8> {
    let mut out = vec![0u8; (size * size * 4) as usize];
    let s = size as f32;
    let center = s / 2.0;
    // Signed distance to a hexagon with the given circumradius (pointy-top).
    let hex_dist = |x: f32, y: f32, r: f32| -> f32 {
        let (px, py) = ((x - center).abs(), (y - center).abs());
        let k = (3.0f32).sqrt() / 2.0;
        let d = (px * k + py * 0.5).max(py) - r * k;
        d
    };
    let samples = 4;
    for py in 0..size {
        for px in 0..size {
            let mut cover = [0f32; 3]; // outer ring, mid ring, core
            for sy in 0..samples {
                for sx in 0..samples {
                    let x = px as f32 + (sx as f32 + 0.5) / samples as f32;
                    let y = py as f32 + (sy as f32 + 0.5) / samples as f32;
                    let ring = |r: f32, w: f32| (hex_dist(x, y, r).abs() <= w / 2.0) as u8 as f32;
                    cover[0] += ring(s * 0.46, s * 0.085);
                    cover[1] += ring(s * 0.30, s * 0.05);
                    cover[2] += (hex_dist(x, y, s * 0.16) <= 0.0) as u8 as f32;
                }
            }
            let n = (samples * samples) as f32;
            let t = (px as f32 + py as f32) / (2.0 * s);
            let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
            let cyan = [lerp(0x22, 0x25), lerp(0xD3, 0x63), lerp(0xEE, 0xEB)];
            let blue = [0x25, 0x63, 0xEB];
            let i = ((py * size + px) * 4) as usize;
            let mut rgb = [0f32; 3];
            let mut alpha = 0.0f32;
            for (c, col) in [
                (cover[0] / n, cyan),
                (cover[1] / n, blue),
                (cover[2] / n, cyan),
            ] {
                if c > 0.0 {
                    let new_alpha = alpha + c * (1.0 - alpha);
                    for k in 0..3 {
                        rgb[k] = (rgb[k] * alpha * (1.0 - c) + col[k] as f32 * c)
                            / new_alpha.max(f32::EPSILON);
                    }
                    alpha = new_alpha;
                }
            }
            out[i] = rgb[0] as u8;
            out[i + 1] = rgb[1] as u8;
            out[i + 2] = rgb[2] as u8;
            out[i + 3] = (alpha * 255.0) as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_has_expected_size_and_visible_pixels() {
        let px = icon_rgba(64);
        assert_eq!(px.len(), 64 * 64 * 4);
        let visible = px.chunks(4).filter(|p| p[3] > 128).count();
        assert!(
            visible > 200,
            "the emblem must be visible, got {visible} opaque pixels"
        );
        // The corners stay transparent.
        assert_eq!(px[3], 0);
        // The center is part of the solid core.
        let c = ((32 * 64 + 32) * 4) as usize;
        assert!(px[c + 3] > 200);
    }
}
