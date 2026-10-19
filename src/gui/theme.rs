//! Colors, styling and the Gagent logo.
//!
//! A calm blue look: pale blue-white or deep navy surfaces, a blue accent, and a
//! blue-to-violet-to-pink gradient for the logo. Colors are functions because they
//! depend on whether the light or the dark theme is active.

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Painter, Pos2, Shape, Stroke, TextStyle,
    Theme, ThemePreference, Vec2,
};
use std::sync::atomic::{AtomicBool, Ordering};

static DARK: AtomicBool = AtomicBool::new(false);

/// True while the dark theme is shown.
pub fn is_dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

/// Remembers which theme egui is showing. Call once per frame.
pub fn sync(ctx: &egui::Context) {
    DARK.store(ctx.theme() == Theme::Dark, Ordering::Relaxed);
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

macro_rules! colors {
    ($($(#[$doc:meta])* $name:ident: $dark:expr, $light:expr;)*) => {
        $(
            $(#[$doc])*
            pub fn $name() -> Color32 {
                if is_dark() { rgb($dark) } else { rgb($light) }
            }
        )*
    };
}

colors! {
    /// Main background.
    bg: 0x101521, 0xF8FAFF;
    sidebar_bg: 0x0B0F19, 0xEEF3FD;
    panel_bg: 0x141A29, 0xF3F6FD;
    card_bg: 0x1A2133, 0xFFFFFF;
    card_hover: 0x232C44, 0xE9EFFC;
    input_bg: 0x1A2133, 0xFFFFFF;
    border: 0x2A3550, 0xDCE4F5;
    text: 0xDDE4F5, 0x1D2433;
    text_strong: 0xF4F7FF, 0x0E1424;
    muted: 0x9AA7C4, 0x5A6680;
    faint: 0x6B7896, 0x8E99B0;
    row_hover: 0x19213A, 0xE3EBFB;
    row_selected: 0x222D4B, 0xD6E2FA;
    accent: 0x6C9BFF, 0x3F78F2;
    ok: 0x6FCF9F, 0x1E9E6A;
    warn: 0xE2B25A, 0xB7791F;
    err: 0xF0857A, 0xD8453B;
    err_bg: 0x33202A, 0xFDECEA;
    user_bg: 0x1D2745, 0xE3EBFC;
    code_bg: 0x0C111D, 0xEEF2FB;
}

/// Text on top of an accent-colored button.
pub fn on_accent() -> Color32 {
    if is_dark() {
        rgb(0x0B1020)
    } else {
        Color32::WHITE
    }
}

pub const RADIUS: u8 = 8;

fn style_for(theme: Theme) -> egui::Style {
    let dark = theme == Theme::Dark;
    let pick = |d: u32, l: u32| if dark { rgb(d) } else { rgb(l) };
    let (bg, sidebar, panel, card, hover, border) = (
        pick(0x101521, 0xF8FAFF),
        pick(0x0B0F19, 0xEEF3FD),
        pick(0x141A29, 0xF3F6FD),
        pick(0x1A2133, 0xFFFFFF),
        pick(0x232C44, 0xE9EFFC),
        pick(0x2A3550, 0xDCE4F5),
    );
    let (text, strong) = (pick(0xDDE4F5, 0x1D2433), pick(0xF4F7FF, 0x0E1424));
    let accent = pick(0x6C9BFF, 0x3F78F2);
    let accent_bg = pick(0x1F2E52, 0xDCE8FD);

    let mut style = egui::Style::default();
    style.text_styles = [
        (
            TextStyle::Heading,
            FontId::new(21.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(15.0, FontFamily::Proportional)),
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
    style.spacing.button_padding = Vec2::new(12.0, 6.0);
    style.spacing.scroll.bar_width = 8.0;

    let v = &mut style.visuals;
    *v = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.override_text_color = Some(text);
    v.panel_fill = bg;
    v.window_fill = panel;
    v.window_stroke = Stroke::new(1.0, border);
    v.window_corner_radius = CornerRadius::same(14);
    v.menu_corner_radius = CornerRadius::same(10);
    v.extreme_bg_color = card;
    v.faint_bg_color = sidebar;
    v.code_bg_color = pick(0x0C111D, 0xEEF2FB);
    v.hyperlink_color = accent;
    v.selection.bg_fill = accent_bg;
    v.selection.stroke = Stroke::new(1.0, accent);
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
    v.widgets.noninteractive.bg_fill = panel;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, text);
    v.widgets.inactive.bg_fill = card;
    v.widgets.inactive.weak_bg_fill = card;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, border);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, text);
    v.widgets.hovered.bg_fill = hover;
    v.widgets.hovered.weak_bg_fill = hover;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, pick(0x3A4770, 0xBFCDEE));
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, strong);
    v.widgets.active.bg_fill = accent_bg;
    v.widgets.active.weak_bg_fill = accent_bg;
    v.widgets.active.bg_stroke = Stroke::new(1.0, accent);
    v.widgets.active.fg_stroke = Stroke::new(1.0, strong);
    v.widgets.open.bg_fill = hover;
    v.widgets.open.weak_bg_fill = hover;
    v.widgets.open.fg_stroke = Stroke::new(1.0, strong);
    v.popup_shadow = egui::Shadow::NONE;
    v.window_shadow = egui::Shadow {
        offset: [0, 6],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 110 } else { 40 }),
    };
    style
}

/// Installs the light and the dark style and selects the one the user asked for.
pub fn apply(ctx: &egui::Context, preference: ThemePreference) {
    ctx.set_style_of(Theme::Light, style_for(Theme::Light));
    ctx.set_style_of(Theme::Dark, style_for(Theme::Dark));
    ctx.set_theme(preference);
    sync(ctx);
}

/// How far around the circle the letter G is drawn (the gap is its opening).
const G_SWEEP: f32 = 5.585; // 320 degrees

/// The logo gradient: blue, violet, pink.
const GRADIENT: [[u8; 3]; 3] = [[0x4A, 0x86, 0xF7], [0x8F, 0x6B, 0xF2], [0xE5, 0x67, 0x9B]];

/// A color on the logo gradient, `t` from 0 (blue) to 1 (pink).
pub fn gradient(t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0) * (GRADIENT.len() - 1) as f32;
    let i = (t.floor() as usize).min(GRADIENT.len() - 2);
    let f = t - i as f32;
    let mix = |k: usize| (GRADIENT[i][k] as f32 * (1.0 - f) + GRADIENT[i + 1][k] as f32 * f) as u8;
    Color32::from_rgb(mix(0), mix(1), mix(2))
}

/// Where a point lies on the gradient: blue at the upper left, pink at the lower right.
fn gradient_at(p: Pos2, center: Pos2, reach: f32) -> Color32 {
    gradient((((p.x - center.x) + (p.y - center.y)) / (2.0 * reach)) * 0.5 + 0.5)
}

/// The Gagent mark: a letter G drawn with vector strokes in the logo gradient.
pub fn paint_logo(painter: &Painter, center: Pos2, size: f32) {
    let radius = size * 0.31;
    let width = (size * 0.17).max(2.0);
    let reach = radius + width * 0.5;
    let color = |p: Pos2| gradient_at(p, center, reach);
    // The arc starts at the right (angle 0), runs clockwise around and stops at the upper right.
    let steps = 56;
    let points: Vec<Pos2> = (0..=steps)
        .map(|i| center + Vec2::angled(G_SWEEP * i as f32 / steps as f32) * radius)
        .collect();
    for pair in points.windows(2) {
        let mid = pair[0].lerp(pair[1], 0.5);
        painter.line_segment([pair[0], pair[1]], Stroke::new(width, color(mid)));
    }
    // The bar of the G.
    let start = points[0];
    let bar_start = center + Vec2::new(radius * 0.02, 0.0);
    for i in 0..8 {
        let a = bar_start.lerp(start, i as f32 / 8.0);
        let b = bar_start.lerp(start, (i + 1) as f32 / 8.0);
        painter.line_segment([a, b], Stroke::new(width, color(a.lerp(b, 0.5))));
    }
    // Round the joints and the ends.
    for p in points.iter().step_by(2).chain([&bar_start]) {
        painter.circle_filled(*p, width * 0.5, color(*p));
    }
    painter.circle_filled(
        points[points.len() - 1],
        width * 0.5,
        color(points[points.len() - 1]),
    );
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
    painter.circle_stroke(center, r, Stroke::new(1.5, border()));
    let start = t * 5.0;
    let points: Vec<Pos2> = (0..=14)
        .map(|i| {
            let angle = start + i as f32 * (1.9 / 14.0);
            center + Vec2::angled(angle) * r
        })
        .collect();
    painter.add(Shape::line(points, Stroke::new(2.0, accent())));
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

/// RGBA pixels of the window icon (size x size): the gradient G on a dark navy
/// rounded square, drawn with anti-aliasing.
pub fn icon_rgba(size: u32) -> Vec<u8> {
    let mut out = vec![0u8; (size * size * 4) as usize];
    let s = size as f32;
    let center = s / 2.0;
    let navy = [0x0Eu8, 0x15, 0x30];
    // Signed distance to a rounded square covering the whole icon.
    let square = |x: f32, y: f32| -> f32 {
        let half = s * 0.5;
        let r = s * 0.24;
        let (qx, qy) = (
            (x - center).abs() - (half - r),
            (y - center).abs() - (half - r),
        );
        let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
        outside + qx.max(qy).min(0.0) - r
    };
    let radius = s * 0.27;
    let half_width = s * 0.075;
    let reach = radius + half_width;
    let end_angle = G_SWEEP;
    let cap = |x: f32, y: f32, px: f32, py: f32| {
        ((x - px).powi(2) + (y - py).powi(2)).sqrt() <= half_width
    };
    // True where the letter G covers the point.
    let letter = |x: f32, y: f32| -> bool {
        let (dx, dy) = (x - center, y - center);
        let rho = (dx * dx + dy * dy).sqrt();
        // Angle clockwise from the right, in 0..2pi.
        let mut ang = dy.atan2(dx);
        if ang < 0.0 {
            ang += std::f32::consts::TAU;
        }
        let on_arc = (rho - radius).abs() <= half_width && ang <= end_angle;
        let on_bar = dx >= 0.0 && dx <= radius && dy.abs() <= half_width;
        let caps = cap(
            x,
            y,
            center + radius * end_angle.cos(),
            center + radius * end_angle.sin(),
        ) || cap(x, y, center + radius, center);
        on_arc || on_bar || caps
    };
    let samples = 4;
    for py in 0..size {
        for px in 0..size {
            let (mut square_cover, mut letter_cover) = (0f32, 0f32);
            let mut letter_rgb = [0f32; 3];
            for sy in 0..samples {
                for sx in 0..samples {
                    let x = px as f32 + (sx as f32 + 0.5) / samples as f32;
                    let y = py as f32 + (sy as f32 + 0.5) / samples as f32;
                    if square(x, y) <= 0.0 {
                        square_cover += 1.0;
                        if letter(x, y) {
                            letter_cover += 1.0;
                            let g = gradient_at(Pos2::new(x, y), Pos2::new(center, center), reach);
                            letter_rgb[0] += g.r() as f32;
                            letter_rgb[1] += g.g() as f32;
                            letter_rgb[2] += g.b() as f32;
                        }
                    }
                }
            }
            let n = (samples * samples) as f32;
            let cover = square_cover / n;
            let mix = letter_cover / square_cover.max(1.0);
            let i = ((py * size + px) * 4) as usize;
            for k in 0..3 {
                let letter_color = if letter_cover > 0.0 {
                    letter_rgb[k] / letter_cover
                } else {
                    0.0
                };
                out[i + k] = (navy[k] as f32 * (1.0 - mix) + letter_color * mix) as u8;
            }
            out[i + 3] = (cover * 255.0) as u8;
        }
    }
    out
}

/// Adds a system font behind the bundled one, for letters the bundled font does
/// not have (for example accented letters).
pub fn install_fonts(ctx: &egui::Context) {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    let candidates: Vec<String> = if cfg!(windows) {
        ["segoeui.ttf", "arial.ttf", "tahoma.ttf", "calibri.ttf"]
            .iter()
            .map(|f| format!(r"{windir}\Fonts\{f}"))
            .collect()
    } else if cfg!(target_os = "macos") {
        vec![
            "/System/Library/Fonts/Supplemental/Arial.ttf".into(),
            "/Library/Fonts/Arial.ttf".into(),
        ]
    } else {
        [
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
            "/usr/share/fonts/truetype/ubuntu/Ubuntu-R.ttf",
        ]
        .iter()
        .map(|p| p.to_string())
        .collect()
    };
    let Some(bytes) = candidates.iter().find_map(|p| std::fs::read(p).ok()) else {
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "system-fallback".to_owned(),
        std::sync::Arc::new(egui::FontData::from_owned(bytes)),
    );
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("system-fallback".to_owned());
    }
    ctx.set_fonts(fonts);
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
            visible > 2000,
            "the icon must be visible, got {visible} opaque pixels"
        );
        // The corners of the rounded square stay transparent.
        assert_eq!(px[3], 0);
        // The bar of the G is bright, the area left of the letter is dark navy.
        let bar = ((32 * 64 + 40) * 4) as usize;
        assert!(
            px[bar + 3] > 200 && px[bar] > 0x70,
            "bar pixel {:?}",
            &px[bar..bar + 4]
        );
        let edge = ((32 * 64 + 6) * 4) as usize;
        assert!(px[edge + 3] > 200 && px[edge] < 0x30);
        // The letter has an opening at the upper right, so the point there is navy.
        let gap = ((25 * 64 + 50) * 4) as usize;
        assert!(px[gap] < 0x30, "opening pixel {:?}", &px[gap..gap + 4]);
    }

    #[test]
    fn gradient_runs_from_blue_to_pink() {
        let (start, end) = (gradient(0.0), gradient(1.0));
        assert!(start.b() > start.r(), "starts blue");
        assert!(end.r() > end.b() / 2 && end.r() > 0xC0, "ends pink");
        assert_ne!(gradient(0.5), start);
    }

    #[test]
    fn colors_differ_between_themes() {
        DARK.store(true, Ordering::Relaxed);
        let dark_bg = bg();
        DARK.store(false, Ordering::Relaxed);
        assert_ne!(dark_bg, bg());
    }
}
