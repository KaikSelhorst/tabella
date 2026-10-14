//! Visual identity, ported from the `shortener` design system: a near-monochrome dark scale
//! (OKLCH, chroma 0) with a single lime accent and tone-on-tone status colors.

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle, Visuals,
    epaint::Shadow,
};

/// Corner radius used by every control. Square on purpose (the web design uses 6px).
pub const RADIUS: u8 = 0;

/// `oklch(l, c, h)` to sRGB. `l` is 0..=1, `h` in degrees.
pub fn oklch(l: f32, c: f32, h: f32) -> Color32 {
    let (s, co) = h.to_radians().sin_cos();
    from_oklab([l, c * co, c * s])
}

fn from_oklab([l, a, b]: [f32; 3]) -> Color32 {
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_.powi(3), m_.powi(3), s_.powi(3));
    let r = 4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_94 * s3;
    let g = -1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_38 * s3;
    let bl = -0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3;
    let enc = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        let v = if v <= 0.003_130_8 {
            12.92 * v
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        };
        (v * 255.0).round() as u8
    };
    Color32::from_rgb(enc(r), enc(g), enc(bl))
}

fn to_oklab(c: Color32) -> [f32; 3] {
    let dec = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (dec(c.r()), dec(c.g()), dec(c.b()));
    let l = (0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_8 * m - 0.808_675_77 * s,
    ]
}

/// `color-mix(in oklab, fg pct%, bg)`. Mixing in oklab keeps hue stable when blending toward gray.
pub fn mix(fg: Color32, pct: f32, bg: Color32) -> Color32 {
    let (f, b) = (to_oklab(fg), to_oklab(bg));
    from_oklab(std::array::from_fn(|i| f[i] * pct + b[i] * (1.0 - pct)))
}

pub struct Tokens {
    /// Main working surface: grid body, editor.
    pub background: Color32,
    /// Chrome around the work area: sidebar, tab strip, pager. A step darker so data stands out.
    pub sidebar: Color32,
    /// Grid header, toolbars, popups.
    pub card: Color32,
    pub hover: Color32,
    /// Hairlines between cells and panels; kept low-contrast so the grid doesn't look like a cage.
    pub border: Color32,
    pub foreground: Color32,
    pub muted_foreground: Color32,
    /// Row numbers, placeholders, NULL.
    pub subtle: Color32,
    pub lime: Color32,
    pub ring: Color32,
    pub selection: Color32,
    pub destructive: Color32,
    pub warning: Color32,
    pub info: Color32,
    // Value colors in the grid, desaturated so a full screen of data stays calm.
    pub number: Color32,
    pub boolean: Color32,
    pub date: Color32,
    /// Text on the accent (primary buttons).
    pub on_accent: Color32,
}

static LIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Switches between the dark and the light palette; call `apply` afterwards.
pub fn set_light(light: bool) {
    LIGHT.store(light, std::sync::atomic::Ordering::Relaxed);
}

pub fn is_light() -> bool {
    LIGHT.load(std::sync::atomic::Ordering::Relaxed)
}

/// The palette, computed once per theme (OKLCH conversions aren't free and this is read every
/// frame).
pub fn tokens() -> &'static Tokens {
    static DARK: std::sync::OnceLock<Tokens> = std::sync::OnceLock::new();
    static LIGHT_TOKENS: std::sync::OnceLock<Tokens> = std::sync::OnceLock::new();
    if is_light() {
        LIGHT_TOKENS.get_or_init(build_light_tokens)
    } else {
        DARK.get_or_init(build_tokens)
    }
}

/// Same roles as the dark palette, on a near-white scale; accents darkened so lines and text
/// in them stay readable on light surfaces.
fn build_light_tokens() -> Tokens {
    let brand = |l: f32| oklch(l, 0.0, 0.0);
    let background = brand(0.99);
    let lime = oklch(0.78, 0.19, 122.0);
    Tokens {
        background,
        sidebar: brand(0.965),
        card: brand(0.975),
        hover: brand(0.945),
        border: brand(0.885),
        foreground: brand(0.22),
        muted_foreground: brand(0.45),
        subtle: brand(0.58),
        lime,
        ring: mix(oklch(0.58, 0.16, 125.0), 0.8, background),
        selection: mix(lime, 0.22, background),
        destructive: oklch(0.55, 0.2, 25.0),
        warning: oklch(0.62, 0.15, 65.0),
        info: oklch(0.50, 0.16, 264.0),
        number: oklch(0.48, 0.13, 240.0),
        boolean: oklch(0.50, 0.17, 300.0),
        date: oklch(0.50, 0.08, 65.0),
        on_accent: brand(0.18),
    }
}

fn build_tokens() -> Tokens {
    let brand = |l: f32| oklch(l, 0.0, 0.0);
    let background = brand(0.165);
    let lime = oklch(0.95, 0.19, 117.0);
    Tokens {
        background,
        sidebar: brand(0.145),
        card: brand(0.19),
        hover: brand(0.205),
        border: brand(0.235),
        foreground: brand(0.92),
        muted_foreground: brand(0.64),
        subtle: brand(0.50),
        lime,
        ring: mix(lime, 0.65, background),
        selection: mix(lime, 0.12, background),
        destructive: oklch(0.62, 0.19, 25.0),
        warning: oklch(0.80, 0.13, 75.0),
        info: oklch(0.72, 0.13, 264.0),
        number: oklch(0.80, 0.09, 235.0),
        boolean: oklch(0.74, 0.14, 300.0),
        date: oklch(0.76, 0.045, 75.0),
        on_accent: background,
    }
}

pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    let t = tokens();
    let light = is_light();
    // Control states: the dark theme lightens on hover, the light one darkens.
    let brand = |l: f32| oklch(if light { 1.0 - (l - 0.165) } else { l }, 0.0, 0.0);
    let radius = CornerRadius::same(RADIUS);
    let shadow_alpha = if light { 40 } else { 140 };

    let mut v = if light { Visuals::light() } else { Visuals::dark() };
    v.override_text_color = Some(t.foreground);
    v.weak_text_color = Some(t.muted_foreground);
    v.panel_fill = t.background;
    v.window_fill = t.card;
    v.window_stroke = Stroke::new(1.0, t.border);
    v.window_corner_radius = radius;
    v.window_shadow = Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(shadow_alpha),
    };
    v.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 12,
        spread: 0,
        color: Color32::from_black_alpha(if light { 34 } else { 120 }),
    };
    v.menu_corner_radius = radius;
    v.extreme_bg_color = t.sidebar; // text inputs, code editor
    v.faint_bg_color = t.card;
    v.code_bg_color = t.card;
    v.hyperlink_color = t.info;
    v.warn_fg_color = t.warning;
    v.error_fg_color = t.destructive;
    v.selection.bg_fill = t.selection;
    v.selection.stroke = Stroke::new(1.0, t.ring);
    v.text_cursor.stroke = Stroke::new(2.0, t.lime);
    v.striped = false;

    // Controls are quiet outlined boxes; only the primary action uses lime.
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = t.card;
    w.noninteractive.weak_bg_fill = t.card;
    w.noninteractive.bg_stroke = Stroke::new(1.0, t.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, t.foreground);
    w.inactive.bg_fill = t.card;
    w.inactive.weak_bg_fill = t.card;
    w.inactive.bg_stroke = Stroke::new(1.0, t.border);
    w.inactive.fg_stroke = Stroke::new(1.0, t.foreground);
    w.hovered.bg_fill = brand(0.24);
    w.hovered.weak_bg_fill = brand(0.24);
    w.hovered.bg_stroke = Stroke::new(1.0, brand(0.32));
    w.hovered.fg_stroke = Stroke::new(1.0, t.foreground);
    w.active.bg_fill = brand(0.28);
    w.active.weak_bg_fill = brand(0.28);
    w.active.bg_stroke = Stroke::new(1.0, t.ring);
    w.active.fg_stroke = Stroke::new(1.0, t.foreground);
    w.open.bg_fill = brand(0.24);
    w.open.weak_bg_fill = brand(0.24);
    w.open.bg_stroke = Stroke::new(1.0, brand(0.32));
    w.open.fg_stroke = Stroke::new(1.0, t.foreground);
    for s in [
        &mut w.noninteractive,
        &mut w.inactive,
        &mut w.hovered,
        &mut w.active,
        &mut w.open,
    ] {
        s.corner_radius = radius;
        s.expansion = 0.0;
    }
    ctx.set_visuals(v);

    ctx.all_styles_mut(|style| {
        // Dense, data-tool spacing, still on a 4px grid.
        style.spacing.item_spacing = egui::vec2(8.0, 4.0);
        style.spacing.button_padding = egui::vec2(8.0, 2.0);
        style.spacing.interact_size = egui::vec2(24.0, 24.0);
        style.spacing.window_margin = egui::Margin::same(16);
        style.spacing.menu_margin = egui::Margin::same(4);
        style.spacing.indent = 12.0;
        style.spacing.icon_width = 12.0;
        style.spacing.icon_width_inner = 8.0;
        style.spacing.icon_spacing = 4.0;
        style.spacing.combo_height = 320.0;
        // Bars draw over the content and take no space. If they took space, a horizontal bar
        // appearing could make the vertical one appear, which removes width, which hides the
        // horizontal one... and the UI would repaint forever, burning CPU while idle.
        style.spacing.scroll = egui::style::ScrollStyle {
            floating_allocated_width: 0.0,
            ..egui::style::ScrollStyle::thin()
        };
        style.text_styles = [
            (TextStyle::Heading, FontId::new(16.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(13.0, FontFamily::Proportional)),
            (TextStyle::Small, FontId::new(11.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
        ]
        .into();
    });
}

pub fn icon_family() -> FontFamily {
    FontFamily::Name("icons".into())
}

/// A single Phosphor glyph (see [`super::icon`]) ready to color and size.
pub fn icon(glyph: &str) -> egui::RichText {
    egui::RichText::new(glyph).family(icon_family())
}

/// Text with a leading icon in its own color, e.g. a muted table glyph before a table name.
pub fn icon_text(icon: &str, text: &str, icon_color: Color32, text_color: Color32) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat};
    let mut job = LayoutJob::default();
    job.append(
        icon,
        0.0,
        TextFormat::simple(FontId::new(13.0, icon_family()), icon_color),
    );
    job.append(
        text,
        6.0,
        TextFormat::simple(FontId::proportional(13.0), text_color),
    );
    job
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "inter".into(),
        FontData::from_static(include_bytes!("../../assets/InterVariable.ttf")).into(),
    );
    fonts.font_data.insert(
        "phosphor".into(),
        FontData::from_static(egui_phosphor::Variant::Regular.font_bytes()).into(),
    );
    if let Some(list) = fonts.families.get_mut(&FontFamily::Proportional) {
        list.insert(0, "inter".into());
    }
    // Icons get their own family. They can't share Proportional with Inter: Inter has its own
    // glyphs in the private-use range Phosphor uses (so some icons would render as stray letters),
    // and putting Phosphor first instead blanks out lowercase text.
    fonts.families.insert(icon_family(), vec!["phosphor".into()]);
    fonts.font_data.insert(
        "jetbrains-mono".into(),
        FontData::from_static(include_bytes!("../../assets/JetBrainsMono-Regular.ttf")).into(),
    );
    if let Some(list) = fonts.families.get_mut(&FontFamily::Monospace) {
        list.insert(0, "jetbrains-mono".into());
        list.push("phosphor".into());
    }
    ctx.set_fonts(fonts);
}

/// Color of a connection tag (environment marker), saturated enough to read at a glance.
pub fn tag_color(tag: crate::db::Tag) -> Color32 {
    use crate::db::Tag;
    let hue = match tag {
        Tag::Red => 25.0,
        Tag::Orange => 55.0,
        Tag::Yellow => 95.0,
        Tag::Green => 150.0,
        Tag::Blue => 255.0,
        Tag::Violet => 300.0,
    };
    oklch(0.70, 0.16, hue)
}

/// Frame shared by every dialog (connection form, confirmations, palette, master password).
pub fn modal_frame(margin: i8) -> egui::Frame {
    let t = tokens();
    egui::Frame::new()
        .fill(t.card)
        .stroke(Stroke::new(1.0, t.border))
        .inner_margin(egui::Margin::same(margin))
}

/// Filled call-to-action button (`bg-primary`, dark text on lime), with an optional leading icon.
pub fn primary_button(glyph: Option<&str>, text: &str) -> egui::Button<'static> {
    let t = tokens();
    let label: egui::WidgetText = match glyph {
        Some(g) => icon_text(g, text, t.on_accent, t.on_accent).into(),
        None => egui::RichText::new(text).color(t.on_accent).into(),
    };
    egui::Button::new(label).fill(t.lime)
}

/// Outline button: faint white tint over the surface, border-colored stroke.
pub fn outline_button(text: impl Into<String>) -> egui::Button<'static> {
    let t = tokens();
    egui::Button::new(text.into())
        .fill(mix(t.foreground, 0.05, t.background))
        .stroke(Stroke::new(1.0, t.border))
}

/// Underline tab with a leading icon: active = foreground with a 2px lime bar, inactive = muted.
pub fn tab_label(ui: &mut egui::Ui, glyph: &str, text: &str, active: bool) -> egui::Response {
    let t = tokens();
    let (ic, tc) = if active {
        (t.muted_foreground, t.foreground)
    } else {
        (t.subtle, t.muted_foreground)
    };
    let r = ui.add(egui::Label::new(icon_text(glyph, text, ic, tc)).sense(egui::Sense::click()));
    if active {
        let y = ui.max_rect().bottom() - 1.0;
        ui.painter()
            .hline(r.rect.x_range().expand(4.0), y, Stroke::new(2.0, t.lime));
    }
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lime_matches_design_doc() {
        // DESIGN.md: --lime = oklch(95% 0.19 117) = #E6FF50
        let c = oklch(0.95, 0.19, 117.0);
        let close = |a: u8, b: u8| a.abs_diff(b) <= 4;
        assert!(
            close(c.r(), 0xE6) && close(c.g(), 0xFF) && close(c.b(), 0x50),
            "{c:?}"
        );
    }

    #[test]
    fn oklab_round_trip() {
        let c = Color32::from_rgb(40, 120, 200);
        let back = from_oklab(to_oklab(c));
        assert!(
            c.r().abs_diff(back.r()) <= 1 && c.g().abs_diff(back.g()) <= 1 && c.b().abs_diff(back.b()) <= 1
        );
    }

    #[test]
    fn background_is_not_pure_black() {
        let t = tokens();
        assert!(t.background.r() > 10 && t.background.r() < 30);
    }
}
