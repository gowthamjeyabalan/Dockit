//! Visual design: an Apple-flavoured theme (Inter typeface, soft greys, a single blue accent),
//! plus a few small widgets (switch, segmented control, section headings, cards).
//!
//! Copied from the Grapht design system (`design/` holds the source docs and tokens).
//! Like Overlay, Dockit adds the derived light theme from `tokens.css` (`[data-theme="light"]`): both
//! styles are registered with egui and the theme preference (dark, light, follow the system) picks one.

use egui::{
    Align2, Color32, Context, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Response, RichText, Sense, Shadow, Stroke, TextStyle, Theme,
    Ui, Vec2, WidgetText, vec2,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Every colour token for one appearance. Names follow `tokens.css`.
#[derive(Clone, Copy, Debug)]
pub struct Pal {
    pub bg: Color32,
    pub bg_elevated: Color32,
    pub bg_control: Color32,
    pub bg_control_hover: Color32,
    pub bg_control_active: Color32,
    pub hairline: Color32,
    pub text: Color32,
    pub text_secondary: Color32,
    /// Text on hovered or pressed controls (white in dark, the text colour in light).
    pub text_strong: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub accent_text: Color32,
    pub danger: Color32,
    pub warning: Color32,
    pub window_fill: Color32,
    pub window_stroke: Color32,
    pub well: Color32,
    pub faint_bg: Color32,
    pub inner_card_fill: Color32,
    pub inner_card_stroke: Color32,
    /// Selected segment of a segmented control.
    pub segment_selected: Color32,
    pub shadow_window: u8,
    pub shadow_popup: u8,
}

const fn rgb(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

// ── palette (inspired by the macOS dark appearance) ──
pub const DARK: Pal = Pal {
    bg: rgb(0x1c1c1f),
    bg_elevated: rgb(0x26262a),
    bg_control: rgb(0x38383c),
    bg_control_hover: rgb(0x46464b),
    bg_control_active: rgb(0x54545a),
    hairline: rgb(0x343439),
    text: rgb(0xf0f0f5),
    text_secondary: rgb(0x9898a2),
    text_strong: Color32::WHITE,
    accent: rgb(0x0a84ff),
    accent_hover: rgb(0x409cff),
    accent_text: rgb(0x64aaff),
    danger: rgb(0xff6961),
    warning: rgb(0xffcc00),
    window_fill: rgb(0x2c2c30),
    window_stroke: rgb(0x404046),
    well: rgb(0x161619),
    faint_bg: rgb(0x222226),
    inner_card_fill: rgb(0x2e2e33),
    inner_card_stroke: rgb(0x3c3c42),
    segment_selected: rgb(0x54545a),
    shadow_window: 110,
    shadow_popup: 100,
};

// ── derived light theme (tokens.css, values chosen to hold 4.5:1 contrast) ──
pub const LIGHT: Pal = Pal {
    bg: rgb(0xf5f5f7),
    bg_elevated: rgb(0xffffff),
    bg_control: rgb(0xe5e5ea),
    bg_control_hover: rgb(0xd8d8dd),
    bg_control_active: rgb(0xc8c8cc),
    hairline: rgb(0xd1d1d6),
    text: rgb(0x14161c),
    text_secondary: rgb(0x6e6e73),
    text_strong: rgb(0x14161c),
    accent: rgb(0x0a84ff),
    accent_hover: rgb(0x409cff),
    accent_text: rgb(0x0064d1),
    danger: rgb(0xd70015),
    warning: rgb(0x8a6d00),
    window_fill: rgb(0xffffff),
    window_stroke: rgb(0xd1d1d6),
    well: rgb(0xe5e5ea),
    faint_bg: rgb(0xececf0),
    inner_card_fill: rgb(0xffffff),
    inner_card_stroke: rgb(0xd1d1d6),
    segment_selected: rgb(0xffffff),
    shadow_window: 40,
    shadow_popup: 36,
};

/// The palette for the appearance egui is drawing with right now.
pub fn pal(ctx: &Context) -> &'static Pal {
    match ctx.theme() {
        Theme::Dark => &DARK,
        Theme::Light => &LIGHT,
    }
}

/// The user's appearance choice.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub enum Appearance {
    #[default]
    System,
    Dark,
    Light,
}

pub fn set_appearance(ctx: &Context, a: Appearance) {
    ctx.set_theme(match a {
        Appearance::System => egui::ThemePreference::System,
        Appearance::Dark => egui::ThemePreference::Dark,
        Appearance::Light => egui::ThemePreference::Light,
    });
}

pub fn semibold_family() -> FontFamily {
    FontFamily::Name("semibold".into())
}
pub fn medium_family() -> FontFamily {
    FontFamily::Name("medium".into())
}

/// Text in the semibold weight.
pub fn semibold(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).family(semibold_family())
}

fn install_fonts(ctx: &Context) {
    let mut fonts = FontDefinitions::default();
    let faces: [(&str, &[u8]); 3] = [
        ("Inter-Regular", include_bytes!("../assets/fonts/Inter-Regular.ttf")),
        ("Inter-Medium", include_bytes!("../assets/fonts/Inter-Medium.ttf")),
        ("Inter-SemiBold", include_bytes!("../assets/fonts/Inter-SemiBold.ttf")),
    ];
    for (name, bytes) in faces {
        fonts.font_data.insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    }
    // Inter first; egui's built-in fonts stay behind it as a fallback for symbols and arrows.
    fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "Inter-Regular".to_owned());
    let fallback = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    for (family, face) in [(medium_family(), "Inter-Medium"), (semibold_family(), "Inter-SemiBold")] {
        let mut list = vec![face.to_owned()];
        list.extend(fallback.iter().filter(|f| !f.starts_with("Inter-")).cloned());
        fonts.families.insert(family, list);
    }
    // Manrope is for the wordmark and brand surfaces only.
    fonts.font_data.insert("Manrope-ExtraBold".to_owned(), Arc::new(FontData::from_static(include_bytes!("../assets/fonts/Manrope-ExtraBold.ttf"))));
    fonts.families.insert(display_family(), vec!["Manrope-ExtraBold".to_owned()]);
    ctx.set_fonts(fonts);
}

pub fn display_family() -> FontFamily {
    FontFamily::Name("display".into())
}

/// The Dockit mark drawn into `rect`. It is rasterized by mark.rs, the same code that draws the
/// app icon, so the letter has exactly the icon's shape at every size.
pub fn paint_mark(painter: &egui::Painter, rect: egui::Rect, ink: Color32, dot: Color32) {
    let ctx = painter.ctx();
    let side = rect.width().min(rect.height());
    let px = (side * ctx.pixels_per_point()).round().max(8.0) as u32;
    let id = egui::Id::new(("dockit-mark", px));
    let textures = ctx.data(|d| d.get_temp::<(egui::TextureHandle, egui::TextureHandle)>(id));
    let (ink_tex, dot_tex) = textures.unwrap_or_else(|| {
        let (a, b) = crate::mark::masks(px);
        let image = |alpha: Vec<u8>| {
            let rgba: Vec<u8> = alpha.iter().flat_map(|&v| [255, 255, 255, v]).collect();
            egui::ColorImage::from_rgba_unmultiplied([px as usize, px as usize], &rgba)
        };
        let t = (
            ctx.load_texture(format!("mark-ink-{px}"), image(a), egui::TextureOptions::LINEAR),
            ctx.load_texture(format!("mark-dot-{px}"), image(b), egui::TextureOptions::LINEAR),
        );
        ctx.data_mut(|d| d.insert_temp(id, t.clone()));
        t
    });
    let r = egui::Rect::from_center_size(rect.center(), egui::vec2(side, side));
    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    painter.image(ink_tex.id(), r, uv, ink);
    painter.image(dot_tex.id(), r, uv, dot);
}

/// Install fonts and both styles, then pick the appearance.
pub fn apply(ctx: &Context, appearance: Appearance) {
    install_fonts(ctx);
    ctx.style_mut_of(Theme::Dark, |s| style(s, &DARK, true));
    ctx.style_mut_of(Theme::Light, |s| style(s, &LIGHT, false));
    set_appearance(ctx, appearance);
}

fn style(style: &mut egui::Style, p: &Pal, dark: bool) {
    style.text_styles = [
        (TextStyle::Small, FontId::new(11.0, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(13.0, medium_family())),
        (TextStyle::Heading, FontId::new(16.0, semibold_family())),
        (TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace)),
    ]
    .into();

    let sp = &mut style.spacing;
    sp.item_spacing = vec2(8.0, 7.0);
    sp.button_padding = vec2(11.0, 4.0);
    sp.interact_size = vec2(34.0, 24.0);
    sp.window_margin = Margin::same(14);
    sp.menu_margin = Margin::same(6);
    sp.indent = 14.0;
    sp.combo_height = 320.0;
    sp.slider_rail_height = 4.0;
    sp.icon_width = 16.0;
    sp.icon_width_inner = 8.0;
    sp.icon_spacing = 7.0;

    let v = &mut style.visuals;
    *v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.override_text_color = None;
    v.panel_fill = p.bg;
    v.window_fill = p.window_fill;
    v.window_stroke = Stroke::new(1.0, p.window_stroke);
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(10);
    v.window_shadow = Shadow { offset: [0, 10], blur: 30, spread: 0, color: Color32::from_black_alpha(p.shadow_window) };
    v.popup_shadow = Shadow { offset: [0, 6], blur: 20, spread: 0, color: Color32::from_black_alpha(p.shadow_popup) };
    v.faint_bg_color = p.faint_bg;
    v.extreme_bg_color = p.well;
    v.code_bg_color = p.window_fill;
    v.hyperlink_color = p.accent_text;
    v.warn_fg_color = p.warning;
    v.error_fg_color = p.danger;
    v.selection.bg_fill = p.accent.gamma_multiply(0.55);
    v.selection.stroke = Stroke::new(1.0, if dark { Color32::WHITE } else { p.text });
    v.slider_trailing_fill = true;
    v.striped = false;
    v.indent_has_left_vline = false;
    v.handle_shape = egui::style::HandleShape::Circle;

    let r = CornerRadius::same(7);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.bg;
    w.noninteractive.weak_bg_fill = p.bg;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.hairline);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = r;

    w.inactive.bg_fill = p.bg_control;
    w.inactive.weak_bg_fill = p.bg_control;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = r;

    w.hovered.bg_fill = p.bg_control_hover;
    w.hovered.weak_bg_fill = p.bg_control_hover;
    w.hovered.bg_stroke = Stroke::NONE;
    w.hovered.fg_stroke = Stroke::new(1.0, p.text_strong);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = p.bg_control_active;
    w.active.weak_bg_fill = p.bg_control_active;
    w.active.bg_stroke = Stroke::NONE;
    w.active.fg_stroke = Stroke::new(1.0, p.text_strong);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;

    w.open.bg_fill = p.bg_control_hover;
    w.open.weak_bg_fill = p.bg_control_hover;
    w.open.bg_stroke = Stroke::NONE;
    w.open.fg_stroke = Stroke::new(1.0, p.text_strong);
    w.open.corner_radius = r;
}

/// Colour for secondary (de-emphasised) text.
pub fn secondary(ui: &Ui, text: impl Into<String>) -> RichText {
    RichText::new(text.into()).color(pal(ui.ctx()).text_secondary)
}

// ── layout helpers ──

/// A small, quiet section title with some air above it.
pub fn heading(ui: &mut Ui, text: &str) {
    let c = pal(ui.ctx()).text_secondary;
    ui.add_space(8.0);
    ui.label(RichText::new(text).family(semibold_family()).size(11.5).color(c));
    ui.add_space(-2.0);
}

/// A rounded container for a group of related controls inside a panel (e.g. one filter rule).
pub fn inner_card(ui: &Ui) -> egui::Frame {
    let p = pal(ui.ctx());
    egui::Frame::new()
        .fill(p.inner_card_fill)
        .stroke(Stroke::new(1.0, p.inner_card_stroke))
        .corner_radius(CornerRadius::same(9))
        .inner_margin(Margin::symmetric(10, 8))
}

/// Panel body: gives sliders and combo boxes a sensible width for the current panel size.
pub fn panel_body<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let w = ui.available_width();
    ui.spacing_mut().slider_width = (w * 0.5).clamp(70.0, 260.0);
    ui.spacing_mut().combo_width = (w * 0.62).clamp(110.0, 320.0);
    add(ui)
}

pub trait UiExt {
    /// iOS-style on/off switch with a label to its right.
    fn switch(&mut self, on: &mut bool, label: impl Into<WidgetText>) -> Response;
    /// Segmented control: one option is always selected. Returns true if the value changed.
    fn segmented<T: PartialEq + Copy>(&mut self, value: &mut T, options: &[(T, &str)]) -> bool;
}

impl UiExt for Ui {
    fn switch(&mut self, on: &mut bool, label: impl Into<WidgetText>) -> Response {
        let p = pal(self.ctx());
        let label: WidgetText = label.into();
        let galley = if label.is_empty() {
            None
        } else {
            Some(label.into_galley(self, Some(egui::TextWrapMode::Wrap), (self.available_width() - 48.0).max(40.0), TextStyle::Body))
        };
        let track = vec2(32.0, 19.0);
        let gap = if galley.is_some() { 8.0 } else { 0.0 };
        let text_size = galley.as_ref().map(|g| g.size()).unwrap_or(Vec2::ZERO);
        let size = vec2(track.x + gap + text_size.x, track.y.max(text_size.y + 2.0));
        let (rect, mut resp) = self.allocate_exact_size(size, Sense::click());
        if resp.clicked() {
            *on = !*on;
            resp.mark_changed();
        }
        let t = self.ctx().animate_bool_responsive(resp.id, *on);
        if self.is_rect_visible(rect) {
            let track_rect = egui::Rect::from_min_size(egui::pos2(rect.left(), rect.center().y - track.y / 2.0), track);
            let off = if resp.hovered() { p.bg_control_active } else { p.bg_control_hover };
            let fill = off.lerp_to_gamma(if resp.hovered() { p.accent_hover } else { p.accent }, t);
            let painter = self.painter();
            painter.rect_filled(track_rect, CornerRadius::same(10), fill);
            let knob_x = egui::lerp((track_rect.left() + 9.5)..=(track_rect.right() - 9.5), t);
            painter.circle_filled(egui::pos2(knob_x, track_rect.center().y + 0.5), 8.2, Color32::from_black_alpha(60));
            painter.circle_filled(egui::pos2(knob_x, track_rect.center().y), 8.0, Color32::WHITE);
            if let Some(g) = galley {
                let pos = egui::pos2(track_rect.right() + gap, rect.center().y - g.size().y / 2.0);
                painter.galley(pos, g, p.text);
            }
        }
        resp
    }

    fn segmented<T: PartialEq + Copy>(&mut self, value: &mut T, options: &[(T, &str)]) -> bool {
        let p = pal(self.ctx());
        let mut changed = false;
        let n = options.len().max(1) as f32;
        let total = self.available_width().min(420.0);
        let h = 26.0;
        let (rect, _) = self.allocate_exact_size(vec2(total, h), Sense::hover());
        self.painter().rect_filled(rect, CornerRadius::same(8), p.well);
        let seg_w = (total - 4.0) / n;
        for (k, (v, text)) in options.iter().enumerate() {
            let r = egui::Rect::from_min_size(egui::pos2(rect.left() + 2.0 + seg_w * k as f32, rect.top() + 2.0), vec2(seg_w, h - 4.0));
            let id = self.next_auto_id().with(("seg", k, *text));
            let resp = self.interact(r, id, Sense::click());
            let selected = *value == *v;
            if selected {
                self.painter().rect_filled(r, CornerRadius::same(6), p.segment_selected);
            } else if resp.hovered() {
                self.painter().rect_filled(r, CornerRadius::same(6), p.bg_control.gamma_multiply(0.7));
            }
            let font = FontId::new(12.0, if selected { semibold_family() } else { medium_family() });
            self.painter().text(r.center(), Align2::CENTER_CENTER, *text, font, if selected { p.text_strong } else { p.text_secondary });
            if resp.clicked() && !selected {
                *value = *v;
                changed = true;
            }
        }
        changed
    }
}

/// Style for the docking area (tab bars and separators), matching the rest of the theme.
pub fn dock_style(ctx: &Context) -> egui_dock::Style {
    let p = pal(ctx);
    let mut s = egui_dock::Style::from_egui(&ctx.global_style());
    s.dock_area_padding = Some(Margin::same(6));
    s.main_surface_border_stroke = Stroke::NONE;
    s.separator.width = 6.0;
    s.separator.extra_interact_width = 4.0;
    // Smallest a pane can be dragged to. Low enough for a narrow tool column.
    s.separator.extra = 64.0;
    s.separator.color_idle = Color32::TRANSPARENT;
    s.separator.color_hovered = p.accent.gamma_multiply(0.6);
    s.separator.color_dragged = p.accent;
    s.tab_bar.bg_fill = Color32::TRANSPARENT;
    s.tab_bar.height = 30.0;
    s.tab_bar.hline_color = p.hairline;
    s.tab_bar.corner_radius = CornerRadius { nw: 10, ne: 10, sw: 0, se: 0 };
    s.tab_bar.inner_margin = Margin { left: 4, right: 4, top: 3, bottom: 0 };
    s.tab.spacing = 2.0;
    s.tab.hline_below_active_tab_name = false;
    let rr = CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 };
    for (st, fill, text) in [
        (&mut s.tab.active, p.bg_elevated, p.text),
        (&mut s.tab.focused, p.bg_elevated, p.text_strong),
        (&mut s.tab.inactive, Color32::TRANSPARENT, p.text_secondary),
        (&mut s.tab.hovered, p.bg_control.gamma_multiply(0.6), p.text),
    ] {
        st.bg_fill = fill;
        st.text_color = text;
        st.outline_color = Color32::TRANSPARENT;
        st.corner_radius = rr;
    }
    s.tab.active_with_kb_focus = s.tab.active.clone();
    s.tab.inactive_with_kb_focus = s.tab.inactive.clone();
    s.tab.focused_with_kb_focus = s.tab.focused.clone();
    s.tab.tab_body.bg_fill = p.bg_elevated;
    s.tab.tab_body.stroke = Stroke::NONE;
    s.tab.tab_body.corner_radius = CornerRadius { nw: 0, ne: 0, sw: 10, se: 10 };
    s.tab.tab_body.inner_margin = Margin::symmetric(12, 8);
    s.buttons.close_tab_color = p.text_secondary;
    s.buttons.close_tab_active_color = p.text_strong;
    s.buttons.close_tab_bg_fill = p.bg_control;
    s.overlay.selection_color = p.accent.gamma_multiply(0.35);
    s.overlay.button_color = p.text_secondary;
    s
}
