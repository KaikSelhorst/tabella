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
    /// Outline of controls (inputs, checkboxes, radio buttons, buttons): 3:1 against every
    /// surface, so a field can be told from the panel it sits on (WCAG 1.4.11).
    pub control: Color32,
    /// The same outline under the pointer.
    pub control_hover: Color32,
    pub foreground: Color32,
    pub muted_foreground: Color32,
    /// Row numbers, placeholders, NULL.
    pub subtle: Color32,
    /// Brand accent for fills: primary buttons, tints, the active tab's line.
    pub lime: Color32,
    /// The accent for text, icons and thin marks (caret, chart lines): the lime itself on dark,
    /// a darker green on light, where lime text can't be read.
    pub accent_text: Color32,
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
    /// Text on a destructive fill (Delete, Run anyway).
    pub on_destructive: Color32,
}

thread_local! {
    /// Per thread: the UI draws on one, and each test's harness gets its own, so a test that
    /// switches the theme doesn't repaint the others running beside it.
    static LIGHT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Switches between the dark and the light palette; call `apply` afterwards.
pub fn set_light(light: bool) {
    LIGHT.set(light);
}

pub fn is_light() -> bool {
    LIGHT.get()
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
        control: brand(0.60),
        control_hover: brand(0.48),
        foreground: brand(0.22),
        muted_foreground: brand(0.45),
        subtle: brand(0.53),
        lime,
        accent_text: oklch(0.50, 0.15, 128.0),
        ring: mix(oklch(0.58, 0.16, 125.0), 0.8, background),
        selection: mix(lime, 0.22, background),
        destructive: oklch(0.53, 0.2, 25.0),
        warning: oklch(0.55, 0.14, 65.0),
        info: oklch(0.50, 0.16, 264.0),
        number: oklch(0.48, 0.13, 240.0),
        boolean: oklch(0.50, 0.17, 300.0),
        date: oklch(0.50, 0.08, 65.0),
        on_accent: brand(0.18),
        on_destructive: brand(1.0),
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
        control: brand(0.50),
        control_hover: brand(0.62),
        foreground: brand(0.92),
        muted_foreground: brand(0.64),
        subtle: brand(0.60),
        lime,
        accent_text: lime,
        ring: mix(lime, 0.65, background),
        selection: mix(lime, 0.12, background),
        // Light enough for red text on its own tinted bands, and for dark text on a red button.
        destructive: oklch(0.68, 0.19, 25.0),
        warning: oklch(0.80, 0.13, 75.0),
        info: oklch(0.72, 0.13, 264.0),
        number: oklch(0.80, 0.09, 235.0),
        boolean: oklch(0.74, 0.14, 300.0),
        date: oklch(0.76, 0.045, 75.0),
        on_accent: background,
        on_destructive: background,
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
    v.text_cursor.stroke = Stroke::new(2.0, t.accent_text);
    v.striped = false;

    // Controls are quiet outlined boxes; only the primary action uses lime.
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = t.card;
    w.noninteractive.weak_bg_fill = t.card;
    w.noninteractive.bg_stroke = Stroke::new(1.0, t.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, t.foreground);
    w.inactive.bg_fill = t.card;
    w.inactive.weak_bg_fill = t.card;
    w.inactive.bg_stroke = Stroke::new(1.0, t.control);
    w.inactive.fg_stroke = Stroke::new(1.0, t.foreground);
    w.hovered.bg_fill = brand(0.24);
    w.hovered.weak_bg_fill = brand(0.24);
    w.hovered.bg_stroke = Stroke::new(1.0, t.control_hover);
    w.hovered.fg_stroke = Stroke::new(1.0, t.foreground);
    w.active.bg_fill = brand(0.28);
    w.active.weak_bg_fill = brand(0.28);
    w.active.bg_stroke = Stroke::new(1.0, t.ring);
    w.active.fg_stroke = Stroke::new(1.0, t.foreground);
    w.open.bg_fill = brand(0.24);
    w.open.weak_bg_fill = brand(0.24);
    w.open.bg_stroke = Stroke::new(1.0, t.control_hover);
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
            (TextStyle::Small, FontId::new(12.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
        ]
        .into();
    });
}

/// Names for screen readers. An icon-only button's own label is a glyph from the icon font,
/// which a screen reader can't read, so it is named after its tooltip instead.
pub trait A11y {
    /// The widget's name for screen readers, replacing the text of its label.
    fn named(self, name: &str) -> Self;
    /// A tooltip that also names the widget.
    fn tip(self, text: impl Into<String>) -> Self;
}

impl A11y for egui::Response {
    fn named(self, name: &str) -> Self {
        self.ctx.accesskit_node_builder(self.id, |b| b.set_label(name));
        self
    }

    fn tip(self, text: impl Into<String>) -> Self {
        let text = text.into();
        self.named(&text).on_hover_text(text)
    }
}

/// Gives a dialog's safe button (Cancel, Back) the focus when the dialog opens, so an Enter or
/// Space meant for something else can't confirm a destructive action.
pub fn focus_on_open(ui: &egui::Ui, button: &egui::Response) {
    let id = button.id.with("focus_on_open");
    let pass = ui.ctx().cumulative_pass_nr();
    let shown_before = ui.data(|d| d.get_temp::<u64>(id)).is_some_and(|p| p + 1 >= pass);
    if !shown_before {
        button.request_focus();
    }
    ui.data_mut(|d| d.insert_temp(id, pass));
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
    // Darker on the light theme: the band and the dots must stand out from a white page.
    oklch(if is_light() { 0.55 } else { 0.70 }, 0.16, hue)
}

/// Fades the edges of a sideways scroll area where more content lies beyond, since its scroll
/// bar only shows on hover: tabs cut at the left, columns hidden past the right edge.
pub fn scroll_fades(ui: &egui::Ui, viewport: egui::Rect, offset: f32, content: f32, bg: Color32, left: bool) {
    const WIDTH: f32 = 28.0;
    let fade = |rect: egui::Rect, opaque_left: bool| {
        let (a, b) = if opaque_left {
            (bg, Color32::TRANSPARENT)
        } else {
            (Color32::TRANSPARENT, bg)
        };
        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(rect.left_top(), a);
        mesh.colored_vertex(rect.right_top(), b);
        mesh.colored_vertex(rect.right_bottom(), b);
        mesh.colored_vertex(rect.left_bottom(), a);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        ui.painter().with_clip_rect(viewport).add(mesh);
    };
    if left && offset > 1.0 {
        fade(
            egui::Rect::from_min_size(viewport.min, egui::vec2(WIDTH, viewport.height())),
            true,
        );
    }
    if content - offset - viewport.width() > 1.0 {
        let min = egui::pos2(viewport.right() - WIDTH, viewport.top());
        fade(
            egui::Rect::from_min_size(min, egui::vec2(WIDTH, viewport.height())),
            false,
        );
    }
}

/// A dialog over a dimmed window. egui's default backdrop barely shows on the dark theme, and
/// the dialog then reads as part of the page behind it.
pub fn modal(id: egui::Id) -> egui::Modal {
    let alpha = if is_light() { 80 } else { 160 };
    egui::Modal::new(id).backdrop_color(Color32::from_black_alpha(alpha))
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
/// A toolbar label: icon and text, or only the icon when the bar is short of room (the name
/// then goes in the tooltip).
pub fn tool(
    glyph: &str,
    text: &str,
    compact: bool,
    icon_color: Color32,
    text_color: Color32,
) -> egui::WidgetText {
    if compact {
        icon(glyph).color(icon_color).into()
    } else {
        icon_text(glyph, text, icon_color, text_color).into()
    }
}

pub fn primary_button(glyph: Option<&str>, text: &str) -> egui::Button<'static> {
    let t = tokens();
    let label: egui::WidgetText = match glyph {
        Some(g) => icon_text(g, text, t.on_accent, t.on_accent).into(),
        None => egui::RichText::new(text).color(t.on_accent).into(),
    };
    // On the light theme lime is too pale to outline itself against the page.
    let edge = if is_light() { t.accent_text } else { t.lime };
    egui::Button::new(label)
        .fill(t.lime)
        .stroke(Stroke::new(1.0, edge))
}

/// Outline button: faint white tint over the surface, border-colored stroke.
pub fn outline_button(text: impl Into<String>) -> egui::Button<'static> {
    let t = tokens();
    egui::Button::new(text.into())
        .fill(mix(t.foreground, 0.05, t.background))
        .stroke(Stroke::new(1.0, t.control))
}

/// Underline tab with a leading icon: active = foreground with a 2px lime bar, inactive = muted.
/// A tab's icon and name. The active tab's underline is drawn by `tab_underline`, once the
/// whole tab (close button included) is laid out.
pub fn tab_label(ui: &mut egui::Ui, glyph: &str, text: &str, active: bool) -> egui::Response {
    let t = tokens();
    let (ic, tc) = if active {
        (t.muted_foreground, t.foreground)
    } else {
        (t.subtle, t.muted_foreground)
    };
    // Not selectable: dragging must move the tab, not select its name like text.
    let r = ui.add(
        egui::Label::new(icon_text(glyph, text, ic, tc))
            .selectable(false)
            .sense(egui::Sense::click_and_drag()),
    );
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// While a tab is dragged: the original fades and a copy follows the pointer.
pub fn tab_drag_ghost(ui: &egui::Ui, label: &egui::Response, glyph: &str, text: &str) {
    let t = tokens();
    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    ui.painter()
        .rect_filled(label.rect.expand(2.0), 2.0, t.sidebar.gamma_multiply(0.7));
    let Some(pos) = ui.ctx().pointer_interact_pos() else {
        return;
    };
    egui::Area::new(egui::Id::new("tab_drag_ghost"))
        .order(egui::Order::Tooltip)
        .fixed_pos(pos + egui::vec2(8.0, 6.0))
        .interactable(false)
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(t.card)
                .stroke(Stroke::new(1.0, t.lime))
                .corner_radius(4.0)
                .inner_margin(egui::Margin::symmetric(8, 4))
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(icon_text(glyph, text, t.muted_foreground, t.foreground))
                            .selectable(false),
                    );
                });
        });
}

/// The line under the selected tab, across all of it: from its icon to its close button. The
/// accent in the pane with focus, grey in the other one of a split view.
pub fn tab_underline(ui: &egui::Ui, tab: egui::Rect, focused: bool) {
    let y = ui.max_rect().bottom() - 1.0;
    let t = tokens();
    let color = if focused { t.accent_text } else { t.subtle };
    ui.painter()
        .hline(tab.x_range().expand(4.0), y, Stroke::new(2.0, color));
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

    /// WCAG contrast ratio of two colors (1 to 21).
    fn contrast(a: Color32, b: Color32) -> f64 {
        let lum = |c: Color32| {
            let f = |v: u8| {
                let v = f64::from(v) / 255.0;
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
        };
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    /// Every text color reads on every surface, in both themes: WCAG AA for small text (4.5:1),
    /// which most of the app's text is.
    #[test]
    fn text_colors_are_readable() {
        for t in [build_tokens(), build_light_tokens()] {
            let text = [
                ("foreground", t.foreground),
                ("muted_foreground", t.muted_foreground),
                ("subtle", t.subtle),
                ("accent_text", t.accent_text),
                ("number", t.number),
                ("boolean", t.boolean),
                ("date", t.date),
                ("info", t.info),
                ("warning", t.warning),
                ("destructive", t.destructive),
            ];
            for (name, c) in text {
                for (surface, bg) in [
                    ("background", t.background),
                    ("sidebar", t.sidebar),
                    ("card", t.card),
                ] {
                    let r = contrast(c, bg);
                    assert!(r >= 4.5, "{name} on {surface}: {r:.2}");
                }
            }
            // The hierarchy stays: subtle is still the faintest.
            assert!(contrast(t.subtle, t.background) < contrast(t.muted_foreground, t.background));
            // Text on the accent (primary buttons) and on the red of destructive ones.
            assert!(contrast(t.on_accent, t.lime) >= 4.5);
            let r = contrast(t.on_destructive, t.destructive);
            assert!(r >= 4.5, "on_destructive: {r:.2}");
        }
    }

    /// Control outlines (and their hover state) stand out from every surface: WCAG 1.4.11
    /// asks 3:1 for what identifies a control. The light theme's primary button gets an edge
    /// in the accent text color, since its lime fill is paler than that.
    #[test]
    fn controls_are_visible() {
        for t in [build_tokens(), build_light_tokens()] {
            for (surface, bg) in [
                ("background", t.background),
                ("sidebar", t.sidebar),
                ("card", t.card),
            ] {
                for (name, c) in [("control", t.control), ("control_hover", t.control_hover)] {
                    let r = contrast(c, bg);
                    assert!(r >= 3.0, "{name} on {surface}: {r:.2}");
                }
            }
        }
        assert!(contrast(build_light_tokens().accent_text, build_light_tokens().background) >= 3.0);
    }

    /// Text on the tinted bands and cells: an error in red on its pink band, pending edits,
    /// selected cells, the "exported" band.
    #[test]
    fn text_on_tints_is_readable() {
        for t in [build_tokens(), build_light_tokens()] {
            let tint = |c, pct| mix(c, pct, t.background);
            for (name, fg, bg) in [
                ("error band", t.destructive, tint(t.destructive, 0.12)),
                ("plan hot spot", t.foreground, tint(t.destructive, 0.22)),
                ("deleted row", t.muted_foreground, tint(t.destructive, 0.14)),
                ("edited cell", t.foreground, tint(t.warning, 0.18)),
                ("selected cell", t.foreground, t.selection),
                ("current match", t.foreground, tint(t.warning, 0.45)),
                ("exported band", t.foreground, tint(t.lime, 0.10)),
            ] {
                let r = contrast(fg, bg);
                assert!(r >= 4.5, "{name} (light: {}): {r:.2}", t.background.r() > 128);
            }
        }
    }

    /// Connection colors (window band, the dot by the name) read on the sidebar in both themes.
    #[test]
    fn tag_colors_are_visible() {
        for light in [false, true] {
            set_light(light);
            let t = if light {
                build_light_tokens()
            } else {
                build_tokens()
            };
            for tag in crate::db::Tag::ALL {
                let r = contrast(tag_color(tag), t.sidebar);
                assert!(r >= 3.0, "{tag:?} (light: {light}): {r:.2}");
            }
        }
        set_light(false);
    }
}
