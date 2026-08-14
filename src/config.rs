use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NotesMode {
    #[default]
    Auto,
    PdfAnnotations,
    BeamerSecondScreen,
    LibreOfficeNotes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AppTheme {
    #[default]
    Dark,
    Light,
    ProjectorSafe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SplitAxis {
    #[default]
    Vertical,
    Horizontal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NotesSide {
    Left,
    #[default]
    Right,
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NotesLayout {
    pub axis: SplitAxis,
    pub split_ratio: f32,
    pub notes_side: NotesSide,
    pub confidence: f32,
}

impl Default for NotesLayout {
    fn default() -> Self {
        Self {
            axis: SplitAxis::Vertical,
            split_ratio: 0.5,
            notes_side: NotesSide::Right,
            confidence: 1.0,
        }
    }
}

impl NotesLayout {
    pub fn sanitize(&mut self) {
        self.split_ratio = self.split_ratio.clamp(0.0, 1.0);
        self.confidence = self.confidence.clamp(0.0, 1.0);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotesLayoutOverrideConfig {
    pub enabled: bool,
    pub axis: SplitAxis,
    pub split_ratio: f32,
    pub notes_side: NotesSide,
}

impl Default for NotesLayoutOverrideConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            axis: SplitAxis::Vertical,
            split_ratio: 0.5,
            notes_side: NotesSide::Right,
        }
    }
}

impl NotesLayoutOverrideConfig {
    pub fn sanitize(&mut self) {
        self.split_ratio = self.split_ratio.clamp(0.0, 1.0);
        self.notes_side = match self.axis {
            SplitAxis::Vertical => match self.notes_side {
                NotesSide::Left | NotesSide::Right => self.notes_side,
                NotesSide::Top | NotesSide::Bottom => NotesSide::Right,
            },
            SplitAxis::Horizontal => match self.notes_side {
                NotesSide::Top | NotesSide::Bottom => self.notes_side,
                NotesSide::Left | NotesSide::Right => NotesSide::Bottom,
            },
        };
    }

    pub fn as_layout(&self) -> NotesLayout {
        let mut layout = NotesLayout {
            axis: self.axis,
            split_ratio: self.split_ratio,
            notes_side: self.notes_side,
            confidence: 1.0,
        };
        layout.sanitize();
        layout
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenterLayoutConfig {
    pub preview_slides: u8,
}

impl Default for PresenterLayoutConfig {
    fn default() -> Self {
        Self { preview_slides: 1 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PenPreset {
    pub name: String,
    pub color_rgb: (f32, f32, f32),
    pub width: f32,
    pub opacity: f32,
}

impl PenPreset {
    fn sanitize(&mut self) {
        self.name = self.name.trim().to_owned();
        if self.name.is_empty() {
            self.name = "Preset".to_owned();
        }
        self.color_rgb = (
            self.color_rgb.0.clamp(0.0, 1.0),
            self.color_rgb.1.clamp(0.0, 1.0),
            self.color_rgb.2.clamp(0.0, 1.0),
        );
        self.width = self.width.clamp(0.5, 24.0);
        self.opacity = self.opacity.clamp(0.05, 1.0);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PenToolkitConfig {
    pub presets: Vec<PenPreset>,
    pub active_preset_index: usize,
    pub eraser_enabled: bool,
}

impl Default for PenToolkitConfig {
    fn default() -> Self {
        Self {
            presets: vec![
                PenPreset {
                    name: "Yellow".to_owned(),
                    color_rgb: (1.0, 0.92, 0.15),
                    width: 3.0,
                    opacity: 0.90,
                },
                PenPreset {
                    name: "Red".to_owned(),
                    color_rgb: (0.95, 0.30, 0.25),
                    width: 4.0,
                    opacity: 0.85,
                },
                PenPreset {
                    name: "Blue".to_owned(),
                    color_rgb: (0.30, 0.60, 1.00),
                    width: 2.5,
                    opacity: 0.80,
                },
            ],
            active_preset_index: 0,
            eraser_enabled: false,
        }
    }
}

impl PenToolkitConfig {
    pub fn sanitize(&mut self) {
        for preset in &mut self.presets {
            preset.sanitize();
        }
        if self.presets.is_empty() {
            *self = Self::default();
            return;
        }
        self.active_preset_index = self.active_preset_index.min(self.presets.len() - 1);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeybindingConfig {
    pub bindings: BTreeMap<String, String>,
}

impl Default for KeybindingConfig {
    fn default() -> Self {
        let mut bindings = BTreeMap::new();
        bindings.insert("toggle_deck_overview".to_owned(), "D".to_owned());
        bindings.insert("toggle_timing_breakdown".to_owned(), "B".to_owned());
        bindings.insert("next".to_owned(), "ArrowRight".to_owned());
        bindings.insert("next_page_down".to_owned(), "PageDown".to_owned());
        bindings.insert("next_space".to_owned(), "Space".to_owned());
        bindings.insert("prev".to_owned(), "ArrowLeft".to_owned());
        bindings.insert("prev_page_up".to_owned(), "PageUp".to_owned());
        bindings.insert("first".to_owned(), "Home".to_owned());
        bindings.insert("last".to_owned(), "End".to_owned());
        bindings.insert("goto".to_owned(), "G".to_owned());
        bindings.insert("toggle_label_mode".to_owned(), "J".to_owned());
        bindings.insert("swap_screens".to_owned(), "S".to_owned());
        bindings.insert("toggle_laser".to_owned(), "L".to_owned());
        bindings.insert("toggle_auto_advance".to_owned(), "T".to_owned());
        bindings.insert("toggle_highlight".to_owned(), "H".to_owned());
        bindings.insert("toggle_annotation_edit".to_owned(), "A".to_owned());
        bindings.insert("toggle_projector_fullscreen".to_owned(), "F".to_owned());
        bindings.insert("reset_timer".to_owned(), "R".to_owned());
        bindings.insert("overview_cancel".to_owned(), "Escape".to_owned());
        bindings.insert("overview_commit".to_owned(), "Enter".to_owned());
        bindings.insert("overview_up".to_owned(), "ArrowUp".to_owned());
        bindings.insert("overview_down".to_owned(), "ArrowDown".to_owned());
        bindings.insert("undo_stroke".to_owned(), "U".to_owned());
        bindings.insert("redo_stroke".to_owned(), "Y".to_owned());
        bindings.insert("clear_current_stroke".to_owned(), "X".to_owned());
        bindings.insert("clear_slide_highlights".to_owned(), "C".to_owned());
        bindings.insert("duplicate_selected_note".to_owned(), "N".to_owned());
        bindings.insert("delete_selected_note".to_owned(), "Backspace".to_owned());
        bindings.insert("apply_transform".to_owned(), "P".to_owned());
        bindings.insert("revert_transform".to_owned(), "V".to_owned());
        bindings.insert("prev_pen_preset".to_owned(), "Q".to_owned());
        bindings.insert("next_pen_preset".to_owned(), "W".to_owned());
        bindings.insert("toggle_eraser".to_owned(), "E".to_owned());
        bindings.insert("toggle_lasso_select".to_owned(), "M".to_owned());
        bindings.insert("delete_selected_strokes".to_owned(), "Delete".to_owned());
        bindings.insert(
            "apply_preset_to_selected_strokes".to_owned(),
            "K".to_owned(),
        );
        Self { bindings }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoAdvanceDocumentConfig {
    pub global_interval_secs: u32,
    pub slide_overrides_secs: BTreeMap<usize, u32>,
}

impl Default for AutoAdvanceDocumentConfig {
    fn default() -> Self {
        Self {
            global_interval_secs: 12,
            slide_overrides_secs: BTreeMap::new(),
        }
    }
}

impl AutoAdvanceDocumentConfig {
    pub fn sanitize(&mut self) {
        self.global_interval_secs = self.global_interval_secs.clamp(1, 3600);
        for secs in self.slide_overrides_secs.values_mut() {
            *secs = (*secs).clamp(1, 3600);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub config_version: u16,
    pub theme: AppTheme,
    pub cache_size: usize,
    pub cache_budget_mb: usize,
    pub projector_render_width: usize,
    pub presenter_render_width: usize,
    pub next_render_width: usize,
    pub notes_render_width: usize,
    pub startup_content_fullscreen: bool,
    pub notes_mode: NotesMode,
    pub notes_confidence_threshold: f32,
    pub notes_overrides: BTreeMap<String, NotesLayoutOverrideConfig>,
    pub overview_thumbnail_width: usize,
    pub overview_columns: u8,
    pub auto_advance_default_interval_secs: u32,
    pub auto_advance_per_document: BTreeMap<String, AutoAdvanceDocumentConfig>,
    pub keybindings: KeybindingConfig,
    pub presenter_layout: PresenterLayoutConfig,
    pub pen_toolkit: PenToolkitConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            config_version: 4,
            theme: AppTheme::Dark,
            cache_size: 80,
            cache_budget_mb: 256,
            projector_render_width: 1280,
            presenter_render_width: 960,
            next_render_width: 640,
            notes_render_width: 768,
            startup_content_fullscreen: true,
            notes_mode: NotesMode::Auto,
            notes_confidence_threshold: 0.35,
            notes_overrides: BTreeMap::new(),
            overview_thumbnail_width: 200,
            overview_columns: 4,
            auto_advance_default_interval_secs: 12,
            auto_advance_per_document: BTreeMap::new(),
            keybindings: KeybindingConfig::default(),
            presenter_layout: PresenterLayoutConfig::default(),
            pen_toolkit: PenToolkitConfig::default(),
        }
    }
}

impl AppConfig {
    pub fn load_or_create() -> Result<(Self, PathBuf)> {
        let config_path = Self::default_path()?;
        if config_path.exists() {
            return Ok((Self::load_from_path(&config_path)?, config_path));
        }

        let config = Self::default();
        config.save_to_path(&config_path)?;
        Ok((config, config_path))
    }

    pub fn load_from_path(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read config file: {}", path.display()))?;
        let mut config: AppConfig =
            toml::from_str(&raw).with_context(|| format!("invalid TOML at {}", path.display()))?;
        config.sanitize();
        Ok(config)
    }

    pub fn save_to_path(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create config directory: {}", parent.display())
            })?;
        }
        let encoded =
            toml::to_string_pretty(self).context("failed to encode app config as TOML")?;
        fs::write(path, encoded)
            .with_context(|| format!("failed to write config file: {}", path.display()))?;
        Ok(())
    }

    fn sanitize(&mut self) {
        self.cache_size = self.cache_size.max(1);
        self.cache_budget_mb = self.cache_budget_mb.clamp(64, 8192);
        self.projector_render_width = self.projector_render_width.clamp(640, 4096);
        self.presenter_render_width = self.presenter_render_width.clamp(640, 4096);
        self.next_render_width = self.next_render_width.clamp(640, 4096);
        self.notes_render_width = self.notes_render_width.clamp(640, 4096);
        self.presenter_layout.preview_slides = self.presenter_layout.preview_slides.clamp(1, 16);
        self.notes_confidence_threshold = self.notes_confidence_threshold.clamp(0.05, 0.95);
        self.overview_thumbnail_width = self.overview_thumbnail_width.clamp(120, 800);
        self.overview_columns = self.overview_columns.clamp(2, 8);
        self.auto_advance_default_interval_secs =
            self.auto_advance_default_interval_secs.clamp(1, 3600);
        for override_config in self.notes_overrides.values_mut() {
            override_config.sanitize();
        }
        for config in self.auto_advance_per_document.values_mut() {
            config.sanitize();
        }
        self.pen_toolkit.sanitize();
    }

    fn default_path() -> Result<PathBuf> {
        let project_dirs = ProjectDirs::from("com", "decklight", "decklight")
            .context("unable to resolve project configuration directory")?;
        Ok(project_dirs.config_dir().join("config.toml"))
    }
}

#[cfg(test)]
mod tests {
    use super::{AppConfig, AppTheme, KeybindingConfig};
    use tempfile::tempdir;

    #[test]
    fn round_trips_config_toml() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("config.toml");
        let config = AppConfig::default();

        config.save_to_path(&path).expect("save config");
        let loaded = AppConfig::load_from_path(&path).expect("load config");

        assert_eq!(loaded, config);
    }

    #[test]
    fn defaults_use_lower_render_cache_settings() {
        let config = AppConfig::default();

        assert_eq!(config.cache_size, 80);
        assert_eq!(config.cache_budget_mb, 256);
        assert_eq!(config.projector_render_width, 1280);
        assert_eq!(config.presenter_render_width, 960);
        assert_eq!(config.next_render_width, 640);
        assert_eq!(config.notes_render_width, 768);
        assert_eq!(config.overview_thumbnail_width, 200);
    }

    #[test]
    fn sanitizes_out_of_range_values() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("config.toml");
        let raw = r#"
config_version = 1
cache_size = 0
cache_budget_mb = 12
projector_render_width = 100
presenter_render_width = 99999
next_render_width = 123
notes_render_width = 7000
startup_content_fullscreen = true
notes_mode = "auto"
notes_confidence_threshold = 1.5
overview_thumbnail_width = 32
overview_columns = 20
auto_advance_default_interval_secs = 0

[keybindings]
bindings = {}

[presenter_layout]
preview_slides = 32

[pen_toolkit]
active_preset_index = 88
eraser_enabled = false

[[pen_toolkit.presets]]
name = ""
color_rgb = [1.5, -1.0, 0.5]
width = 0.1
opacity = 2.0

[notes_overrides.demo]
enabled = true
axis = "vertical"
split_ratio = 2.0
notes_side = "right"

[auto_advance_per_document.deck_a]
global_interval_secs = 99999

[auto_advance_per_document.deck_a.slide_overrides_secs]
0 = 0
3 = 7200
"#;

        std::fs::write(&path, raw).expect("write raw config");
        let loaded = AppConfig::load_from_path(&path).expect("load config");

        assert_eq!(loaded.cache_size, 1);
        assert_eq!(loaded.theme, AppTheme::Dark);
        assert_eq!(loaded.cache_budget_mb, 64);
        assert_eq!(loaded.projector_render_width, 640);
        assert_eq!(loaded.presenter_render_width, 4096);
        assert_eq!(loaded.next_render_width, 640);
        assert_eq!(loaded.notes_render_width, 4096);
        assert_eq!(loaded.presenter_layout.preview_slides, 16);
        assert_eq!(loaded.notes_confidence_threshold, 0.95);
        assert_eq!(loaded.overview_thumbnail_width, 120);
        assert_eq!(loaded.overview_columns, 8);
        assert_eq!(loaded.auto_advance_default_interval_secs, 1);
        assert_eq!(loaded.pen_toolkit.active_preset_index, 0);
        assert_eq!(loaded.pen_toolkit.presets.len(), 1);
        assert_eq!(loaded.pen_toolkit.presets[0].name, "Preset");
        assert_eq!(loaded.pen_toolkit.presets[0].color_rgb, (1.0, 0.0, 0.5));
        assert_eq!(loaded.pen_toolkit.presets[0].width, 0.5);
        assert_eq!(loaded.pen_toolkit.presets[0].opacity, 1.0);
        let demo = loaded
            .notes_overrides
            .get("demo")
            .cloned()
            .unwrap_or_default();
        assert!(demo.enabled);
        assert_eq!(demo.split_ratio, 1.0);
        let auto = loaded
            .auto_advance_per_document
            .get("deck_a")
            .cloned()
            .unwrap_or_default();
        assert_eq!(auto.global_interval_secs, 3600);
        assert_eq!(auto.slide_overrides_secs.get(&0).copied(), Some(1));
        assert_eq!(auto.slide_overrides_secs.get(&3).copied(), Some(3600));
    }

    #[test]
    fn pen_toolkit_defaults_when_presets_missing() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("config.toml");
        let raw = r#"
config_version = 4
cache_size = 80
cache_budget_mb = 256
projector_render_width = 1280
presenter_render_width = 960
next_render_width = 640
notes_render_width = 768
startup_content_fullscreen = true
notes_mode = "auto"
notes_confidence_threshold = 0.35
overview_thumbnail_width = 200
overview_columns = 4
auto_advance_default_interval_secs = 12

[keybindings]
bindings = {}

[presenter_layout]
preview_slides = 1

[pen_toolkit]
active_preset_index = 2
eraser_enabled = true
presets = []
"#;

        std::fs::write(&path, raw).expect("write raw config");
        let loaded = AppConfig::load_from_path(&path).expect("load config");

        assert_eq!(loaded.pen_toolkit.active_preset_index, 0);
        assert!(!loaded.pen_toolkit.presets.is_empty());
        assert_eq!(loaded.pen_toolkit.presets[0].name, "Yellow");
        assert!(!loaded.pen_toolkit.eraser_enabled);
    }

    #[test]
    fn keybinding_defaults_include_lasso_actions() {
        let defaults = KeybindingConfig::default().bindings;
        assert_eq!(
            defaults.get("toggle_lasso_select").map(String::as_str),
            Some("M")
        );
        assert_eq!(
            defaults.get("delete_selected_strokes").map(String::as_str),
            Some("Delete")
        );
        assert_eq!(
            defaults
                .get("apply_preset_to_selected_strokes")
                .map(String::as_str),
            Some("K")
        );
    }
}
