use crate::config::AppTheme;
use eframe::egui::{self, Color32, Context, FontFamily, FontId, RichText, Stroke, Vec2, Visuals};

#[derive(Debug, Clone, Copy)]
pub(super) struct ThemePalette {
    pub(super) app_background: Color32,
    pub(super) top_bar: Color32,
    pub(super) panel: Color32,
    pub(super) panel_elevated: Color32,
    pub(super) panel_stroke: Color32,
    pub(super) inset: Color32,
    pub(super) inset_stroke: Color32,
    pub(super) accent: Color32,
    pub(super) accent_soft: Color32,
    pub(super) text_primary: Color32,
    pub(super) text_secondary: Color32,
    pub(super) warning: Color32,
    pub(super) danger: Color32,
    pub(super) slide_surface: Color32,
    pub(super) inactive_pill: Color32,
    pub(super) inactive_pill_text: Color32,
    pub(super) active_pill_text: Color32,
    pub(super) unsaved_pill: Color32,
    pub(super) unsaved_pill_text: Color32,
    pub(super) talk_pill: Color32,
    pub(super) talk_pill_text: Color32,
    pub(super) slide_pill: Color32,
    pub(super) slide_pill_text: Color32,
}

impl ThemePalette {
    pub(super) fn for_theme(theme: AppTheme) -> Self {
        match theme {
            AppTheme::Dark => Self {
                app_background: Color32::from_rgb(14, 18, 24),
                top_bar: Color32::from_rgb(10, 14, 20),
                panel: Color32::from_rgb(23, 29, 38),
                panel_elevated: Color32::from_rgb(30, 38, 49),
                panel_stroke: Color32::from_rgb(55, 68, 84),
                inset: Color32::from_rgb(17, 22, 30),
                inset_stroke: Color32::from_rgb(38, 48, 62),
                accent: Color32::from_rgb(80, 210, 176),
                accent_soft: Color32::from_rgb(42, 92, 82),
                text_primary: Color32::from_rgb(238, 243, 247),
                text_secondary: Color32::from_rgb(157, 170, 184),
                warning: Color32::from_rgb(255, 204, 92),
                danger: Color32::from_rgb(255, 122, 122),
                slide_surface: Color32::from_rgb(8, 10, 14),
                inactive_pill: Color32::from_rgb(38, 46, 58),
                inactive_pill_text: Color32::from_rgb(170, 182, 194),
                active_pill_text: Color32::from_rgb(186, 255, 232),
                unsaved_pill: Color32::from_rgb(86, 55, 29),
                unsaved_pill_text: Color32::from_rgb(255, 221, 147),
                talk_pill: Color32::from_rgb(34, 58, 76),
                talk_pill_text: Color32::from_rgb(192, 228, 255),
                slide_pill: Color32::from_rgb(46, 56, 40),
                slide_pill_text: Color32::from_rgb(217, 239, 178),
            },
            AppTheme::Light => Self {
                app_background: Color32::from_rgb(236, 233, 225),
                top_bar: Color32::from_rgb(249, 247, 241),
                panel: Color32::from_rgb(255, 252, 245),
                panel_elevated: Color32::from_rgb(248, 244, 235),
                panel_stroke: Color32::from_rgb(205, 196, 181),
                inset: Color32::from_rgb(244, 240, 231),
                inset_stroke: Color32::from_rgb(210, 201, 187),
                accent: Color32::from_rgb(18, 130, 109),
                accent_soft: Color32::from_rgb(205, 236, 226),
                text_primary: Color32::from_rgb(34, 42, 48),
                text_secondary: Color32::from_rgb(100, 109, 116),
                warning: Color32::from_rgb(170, 104, 24),
                danger: Color32::from_rgb(186, 63, 63),
                slide_surface: Color32::from_rgb(221, 216, 206),
                inactive_pill: Color32::from_rgb(226, 222, 213),
                inactive_pill_text: Color32::from_rgb(82, 91, 99),
                active_pill_text: Color32::from_rgb(24, 91, 78),
                unsaved_pill: Color32::from_rgb(255, 225, 176),
                unsaved_pill_text: Color32::from_rgb(118, 74, 19),
                talk_pill: Color32::from_rgb(213, 231, 241),
                talk_pill_text: Color32::from_rgb(35, 80, 107),
                slide_pill: Color32::from_rgb(219, 235, 201),
                slide_pill_text: Color32::from_rgb(64, 93, 43),
            },
            AppTheme::ProjectorSafe => Self {
                app_background: Color32::from_rgb(0, 0, 0),
                top_bar: Color32::from_rgb(0, 0, 0),
                panel: Color32::from_rgb(8, 8, 8),
                panel_elevated: Color32::from_rgb(16, 16, 16),
                panel_stroke: Color32::from_rgb(64, 64, 64),
                inset: Color32::from_rgb(0, 0, 0),
                inset_stroke: Color32::from_rgb(54, 54, 54),
                accent: Color32::from_rgb(255, 214, 94),
                accent_soft: Color32::from_rgb(86, 69, 18),
                text_primary: Color32::from_rgb(248, 248, 248),
                text_secondary: Color32::from_rgb(196, 196, 196),
                warning: Color32::from_rgb(255, 214, 94),
                danger: Color32::from_rgb(255, 96, 96),
                slide_surface: Color32::BLACK,
                inactive_pill: Color32::from_rgb(34, 34, 34),
                inactive_pill_text: Color32::from_rgb(210, 210, 210),
                active_pill_text: Color32::from_rgb(255, 239, 177),
                unsaved_pill: Color32::from_rgb(86, 35, 35),
                unsaved_pill_text: Color32::from_rgb(255, 206, 206),
                talk_pill: Color32::from_rgb(33, 48, 65),
                talk_pill_text: Color32::from_rgb(211, 233, 255),
                slide_pill: Color32::from_rgb(58, 49, 23),
                slide_pill_text: Color32::from_rgb(255, 234, 166),
            },
        }
    }
}

pub(super) fn theme_label(theme: AppTheme) -> &'static str {
    match theme {
        AppTheme::Dark => "Dark",
        AppTheme::Light => "Light",
        AppTheme::ProjectorSafe => "Projector-safe",
    }
}

pub(super) fn install_visual_style(ctx: &Context, theme: AppTheme) {
    let palette = ThemePalette::for_theme(theme);
    let mut style = (*ctx.style()).clone();
    let mut visuals = match theme {
        AppTheme::Light => Visuals::light(),
        AppTheme::Dark | AppTheme::ProjectorSafe => Visuals::dark(),
    };

    visuals.panel_fill = palette.app_background;
    visuals.window_fill = palette.panel;
    visuals.faint_bg_color = palette.panel_elevated;
    visuals.extreme_bg_color = palette.slide_surface;
    visuals.code_bg_color = palette.inset;
    visuals.warn_fg_color = palette.warning;
    visuals.error_fg_color = palette.danger;
    visuals.hyperlink_color = palette.accent;
    visuals.selection.bg_fill = palette.accent_soft;
    visuals.selection.stroke = Stroke::new(1.0, palette.accent);
    visuals.window_shadow = egui::epaint::Shadow {
        offset: [0, 16],
        blur: 32,
        spread: 0,
        color: Color32::from_rgba_unmultiplied(0, 0, 0, 96),
    };
    visuals.widgets.noninteractive.bg_fill = palette.panel_elevated;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, palette.panel_stroke);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, palette.text_secondary);
    visuals.widgets.inactive.bg_fill = palette.inset;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, palette.inset_stroke);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, palette.text_primary);
    visuals.widgets.hovered.bg_fill = palette.panel_elevated;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, palette.accent);
    visuals.widgets.active.bg_fill = palette.accent_soft;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, palette.accent);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, palette.text_primary);
    visuals.widgets.open.bg_fill = palette.panel_elevated;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0, palette.accent);

    style.visuals = visuals;
    style.spacing.item_spacing = Vec2::new(8.0, 8.0);
    style.spacing.button_padding = Vec2::new(12.0, 7.0);
    style.spacing.window_margin = egui::Margin::same(14);
    style.spacing.menu_margin = egui::Margin::symmetric(10, 8);
    style.spacing.indent = 16.0;
    style.spacing.slider_width = 170.0;
    style.text_styles.insert(
        egui::TextStyle::Heading,
        FontId::new(21.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Body,
        FontId::new(14.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Button,
        FontId::new(13.5, FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Monospace,
        FontId::new(13.0, FontFamily::Monospace),
    );

    ctx.set_style(style);
}

pub(super) fn card_frame(theme: AppTheme) -> egui::Frame {
    let palette = ThemePalette::for_theme(theme);
    egui::Frame::new()
        .fill(palette.panel)
        .stroke(Stroke::new(1.0, palette.panel_stroke))
        .corner_radius(14.0)
        .inner_margin(egui::Margin::same(14))
        .outer_margin(egui::Margin::same(2))
}

pub(super) fn inset_frame(theme: AppTheme) -> egui::Frame {
    let palette = ThemePalette::for_theme(theme);
    egui::Frame::new()
        .fill(palette.inset)
        .stroke(Stroke::new(1.0, palette.inset_stroke))
        .corner_radius(12.0)
        .inner_margin(egui::Margin::same(8))
}

pub(super) fn section_title(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    title: &str,
    subtitle: impl Into<Option<&'static str>>,
) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(title)
                .font(FontId::new(16.0, FontFamily::Proportional))
                .strong()
                .color(palette.text_primary),
        );
        if let Some(subtitle) = subtitle.into() {
            ui.label(
                RichText::new(subtitle)
                    .small()
                    .color(palette.text_secondary),
            );
        }
    });
}

pub(super) fn pill_text(text: impl Into<String>, fill: Color32, text_color: Color32) -> RichText {
    RichText::new(format!(" {} ", text.into()))
        .color(text_color)
        .background_color(fill)
        .font(FontId::new(12.0, FontFamily::Proportional))
}

pub(super) fn status_pill(ui: &mut egui::Ui, palette: ThemePalette, label: &str, active: bool) {
    let (fill, text_color) = if active {
        (palette.accent_soft, palette.active_pill_text)
    } else {
        (palette.inactive_pill, palette.inactive_pill_text)
    };
    ui.label(pill_text(label, fill, text_color));
}

pub(super) fn quiet_label(ui: &mut egui::Ui, palette: ThemePalette, text: impl Into<String>) {
    ui.label(RichText::new(text.into()).color(palette.text_secondary));
}
