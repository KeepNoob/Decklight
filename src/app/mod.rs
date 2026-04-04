mod command;
mod state;

pub use command::AppCommand;
pub use state::PresentationState;

use crate::cache::PageCache;
use crate::config::{
    AppConfig, AutoAdvanceDocumentConfig, KeybindingConfig, NotesLayout, NotesLayoutOverrideConfig,
    NotesMode, NotesSide, PenPreset, SplitAxis,
};
use crate::document::{
    AnnotationMetadata, AnnotationPriority, AnnotationStatus, DocumentBackend, NewInkAnnotation,
    NewTextAnnotation, PageBitmap, PageGeometry, PageView, PdfRect, TextAnnotation, open_document,
};
use anyhow::{Result, anyhow};
use chrono::Local;
use eframe::egui::{
    self, Align, Align2, Color32, Context, FontFamily, FontId, Key, Modifiers, Pos2, Rect,
    RichText, Sense, Stroke, TextureHandle, TextureOptions, Vec2, ViewportBuilder, ViewportId,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub fn run(initial_source: Option<PathBuf>) -> Result<()> {
    let (config, config_path) = AppConfig::load_or_create()?;
    let native_options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_title("PDF Presenter - Presenter")
            .with_inner_size([1150.0, 760.0]),
        ..Default::default()
    };

    let app_creator = move |cc: &eframe::CreationContext<'_>| {
        Ok::<Box<dyn eframe::App>, Box<dyn std::error::Error + Send + Sync>>(Box::new(
            PresenterApp::new(
                cc,
                config.clone(),
                config_path.clone(),
                initial_source.clone(),
            ),
        ))
    };

    eframe::run_native("PDF Presenter", native_options, Box::new(app_creator))
        .map_err(|error| anyhow!(error.to_string()))
}

struct PresenterApp {
    config: AppConfig,
    config_path: PathBuf,
    state: PresentationState,
    document: Option<Box<dyn DocumentBackend>>,
    source_path_input: String,
    go_to_input: String,
    open_error: Option<String>,
    status_message: Option<String>,
    last_reload_check: Instant,
    projector_viewport: ViewportId,
    page_cache: PageCache<PageBitmap>,
    projector_texture: Option<TextureHandle>,
    projector_texture_key: Option<usize>,
    current_texture: Option<TextureHandle>,
    current_texture_key: Option<usize>,
    presenter_texture: Option<TextureHandle>,
    presenter_texture_key: Option<usize>,
    notes_texture: Option<TextureHandle>,
    notes_texture_key: Option<usize>,
    next_texture: Option<TextureHandle>,
    next_texture_key: Option<usize>,
    current_document_fingerprint: Option<String>,
    manual_layout_override: NotesLayoutOverrideConfig,
    resolved_layout: Option<NotesLayout>,
    resolved_layout_key: Option<usize>,
    layout_reason: Option<String>,
    layout_warning: Option<String>,
    deck_overview: DeckOverviewState,
    overview_textures: BTreeMap<usize, TextureHandle>,
    overview_texture_keys: BTreeMap<usize, usize>,
    annotation_edit_enabled: bool,
    pending_text_annotation: String,
    text_annotations: Vec<TextAnnotation>,
    original_text_annotations: BTreeMap<String, TextAnnotation>,
    text_annotation_cache_index: Option<usize>,
    selected_annotation_id: Option<String>,
    active_text_transform: Option<ActiveTextTransform>,
    stroke_in_progress: Vec<(f32, f32)>,
    stroke_in_progress_style: Option<PenStrokeStyle>,
    lasso_path_in_progress: Vec<(f32, f32)>,
    selected_stroke_ids: BTreeSet<String>,
    active_highlight_tool: HighlightTool,
    unsaved_strokes: BTreeMap<usize, Vec<SessionInkStroke>>,
    stroke_undo_stack: BTreeMap<usize, Vec<SessionInkStroke>>,
    eraser_erased_in_drag: BTreeSet<String>,
    eraser_removed_this_drag: bool,
    laser_position: Option<(f32, f32)>,
    temporary_laser_active: bool,
    laser_persistent_seen_this_frame: bool,
    laser_primary_down_seen_this_frame: bool,
    auto_override_editor_secs: u32,
    auto_override_editor_index: Option<usize>,
    timing_breakdown: TimingBreakdownState,
    shortcut_bindings: ShortcutBindings,
    keybinding_editor: BTreeMap<String, String>,
    keybinding_editor_dirty: bool,
    keybinding_warning: Option<String>,
    unsaved_toast_until: Option<Instant>,
    unsaved_toast_text: Option<String>,
    non_highlight_unsaved_changes: bool,
    pending_unsaved_action: Option<PendingUnsavedAction>,
    allow_close_without_guard: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InteractionMode {
    Navigate,
    HighlightDraw,
    AnnotationEdit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HighlightTool {
    Draw,
    Eraser,
    Lasso,
}

#[derive(Debug, Clone, Copy)]
struct SlideRegion {
    min_x: f32,
    max_x: f32,
    min_y: f32,
    max_y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlideSurfaceSpace {
    FullPage,
    CroppedSlide,
}

#[derive(Debug, Clone, Copy)]
struct LaserSurfaceSample {
    rect: Rect,
    pointer_pos: Option<Pos2>,
    ctrl_down: bool,
    primary_down: bool,
    hovered: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResizeHandle {
    NorthWest,
    NorthEast,
    SouthWest,
    SouthEast,
}

#[derive(Debug, Clone, Copy)]
enum TransformMode {
    Move,
    Resize(ResizeHandle),
}

#[derive(Debug, Clone)]
struct ActiveTextTransform {
    annotation_id: String,
    start_pointer: (f32, f32),
    start_rect: PdfRect,
    mode: TransformMode,
}

#[derive(Debug, Clone)]
struct SessionInkStroke {
    annotation_id: String,
    points: Vec<(f32, f32)>,
    color_rgb: (f32, f32, f32),
    width: f32,
    opacity: f32,
}

#[derive(Debug, Clone, Copy)]
struct PenStrokeStyle {
    color_rgb: (f32, f32, f32),
    width: f32,
    opacity: f32,
}

#[derive(Debug, Clone)]
struct HighlightOverlaySnapshot {
    strokes: Vec<SessionInkStroke>,
    selected_stroke_ids: BTreeSet<String>,
    stroke_in_progress: Vec<(f32, f32)>,
    stroke_in_progress_style: Option<PenStrokeStyle>,
    active_highlight_tool: HighlightTool,
    lasso_path_in_progress: Vec<(f32, f32)>,
}

#[derive(Debug, Clone)]
enum PendingUnsavedAction {
    Load(PathBuf),
    Close,
}

#[derive(Debug, Clone)]
struct DeckOverviewState {
    is_open: bool,
    selected_index: usize,
    thumbnail_scale: f32,
    commit_pending: Option<usize>,
}

#[derive(Debug, Clone, Default)]
struct TimingBreakdownState {
    is_open: bool,
}

impl TimingBreakdownState {
    fn toggle(&mut self) {
        self.is_open = !self.is_open;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TimingBreakdownRow {
    SectionTotal {
        depth: usize,
        title: String,
        duration: Duration,
    },
    Slide {
        depth: usize,
        index: usize,
        label: String,
        duration: Duration,
        is_current: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TimingBreakdownModel {
    total: Duration,
    has_sections: bool,
    rows: Vec<TimingBreakdownRow>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ShortcutAction {
    ToggleDeckOverview,
    ToggleTimingBreakdown,
    Next,
    NextPageDown,
    NextSpace,
    Previous,
    PreviousPageUp,
    First,
    Last,
    SwapScreens,
    ToggleLabelMode,
    ToggleLaser,
    ToggleAutoAdvance,
    ToggleHighlight,
    ToggleAnnotationEdit,
    ToggleProjectorFullscreen,
    ResetTimer,
    GoTo,
    OverviewCancel,
    OverviewCommit,
    OverviewUp,
    OverviewDown,
    UndoStroke,
    RedoStroke,
    ClearCurrentStroke,
    ClearSlideHighlights,
    DuplicateSelectedNote,
    DeleteSelectedNote,
    ApplyTransform,
    RevertTransform,
    PreviousPenPreset,
    NextPenPreset,
    ToggleEraser,
    ToggleLassoSelect,
    DeleteSelectedStrokes,
    ApplyPresetToSelectedStrokes,
}

impl ShortcutAction {
    const ALL: [Self; 36] = [
        Self::ToggleDeckOverview,
        Self::ToggleTimingBreakdown,
        Self::Next,
        Self::NextPageDown,
        Self::NextSpace,
        Self::Previous,
        Self::PreviousPageUp,
        Self::First,
        Self::Last,
        Self::SwapScreens,
        Self::ToggleLabelMode,
        Self::ToggleLaser,
        Self::ToggleAutoAdvance,
        Self::ToggleHighlight,
        Self::ToggleAnnotationEdit,
        Self::ToggleProjectorFullscreen,
        Self::ResetTimer,
        Self::GoTo,
        Self::OverviewCancel,
        Self::OverviewCommit,
        Self::OverviewUp,
        Self::OverviewDown,
        Self::UndoStroke,
        Self::RedoStroke,
        Self::ClearCurrentStroke,
        Self::ClearSlideHighlights,
        Self::DuplicateSelectedNote,
        Self::DeleteSelectedNote,
        Self::ApplyTransform,
        Self::RevertTransform,
        Self::PreviousPenPreset,
        Self::NextPenPreset,
        Self::ToggleEraser,
        Self::ToggleLassoSelect,
        Self::DeleteSelectedStrokes,
        Self::ApplyPresetToSelectedStrokes,
    ];

    fn id(self) -> &'static str {
        match self {
            Self::ToggleDeckOverview => "toggle_deck_overview",
            Self::ToggleTimingBreakdown => "toggle_timing_breakdown",
            Self::Next => "next",
            Self::NextPageDown => "next_page_down",
            Self::NextSpace => "next_space",
            Self::Previous => "prev",
            Self::PreviousPageUp => "prev_page_up",
            Self::First => "first",
            Self::Last => "last",
            Self::SwapScreens => "swap_screens",
            Self::ToggleLabelMode => "toggle_label_mode",
            Self::ToggleLaser => "toggle_laser",
            Self::ToggleAutoAdvance => "toggle_auto_advance",
            Self::ToggleHighlight => "toggle_highlight",
            Self::ToggleAnnotationEdit => "toggle_annotation_edit",
            Self::ToggleProjectorFullscreen => "toggle_projector_fullscreen",
            Self::ResetTimer => "reset_timer",
            Self::GoTo => "goto",
            Self::OverviewCancel => "overview_cancel",
            Self::OverviewCommit => "overview_commit",
            Self::OverviewUp => "overview_up",
            Self::OverviewDown => "overview_down",
            Self::UndoStroke => "undo_stroke",
            Self::RedoStroke => "redo_stroke",
            Self::ClearCurrentStroke => "clear_current_stroke",
            Self::ClearSlideHighlights => "clear_slide_highlights",
            Self::DuplicateSelectedNote => "duplicate_selected_note",
            Self::DeleteSelectedNote => "delete_selected_note",
            Self::ApplyTransform => "apply_transform",
            Self::RevertTransform => "revert_transform",
            Self::PreviousPenPreset => "prev_pen_preset",
            Self::NextPenPreset => "next_pen_preset",
            Self::ToggleEraser => "toggle_eraser",
            Self::ToggleLassoSelect => "toggle_lasso_select",
            Self::DeleteSelectedStrokes => "delete_selected_strokes",
            Self::ApplyPresetToSelectedStrokes => "apply_preset_to_selected_strokes",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::ToggleDeckOverview => "Deck Overview Toggle",
            Self::ToggleTimingBreakdown => "Timing Breakdown Toggle",
            Self::Next => "Next Slide",
            Self::NextPageDown => "Next (PageDown)",
            Self::NextSpace => "Next (Space)",
            Self::Previous => "Previous Slide",
            Self::PreviousPageUp => "Previous (PageUp)",
            Self::First => "First Slide",
            Self::Last => "Last Slide",
            Self::SwapScreens => "Swap Screens",
            Self::ToggleLabelMode => "Toggle Label Mode",
            Self::ToggleLaser => "Toggle Laser",
            Self::ToggleAutoAdvance => "Toggle Auto-Advance",
            Self::ToggleHighlight => "Toggle Highlight",
            Self::ToggleAnnotationEdit => "Toggle Annotation Edit",
            Self::ToggleProjectorFullscreen => "Toggle Projector Fullscreen",
            Self::ResetTimer => "Reset Timer",
            Self::GoTo => "Go To",
            Self::OverviewCancel => "Overview Cancel",
            Self::OverviewCommit => "Overview Commit",
            Self::OverviewUp => "Overview Up",
            Self::OverviewDown => "Overview Down",
            Self::UndoStroke => "Undo Stroke",
            Self::RedoStroke => "Redo Stroke",
            Self::ClearCurrentStroke => "Clear Current Stroke",
            Self::ClearSlideHighlights => "Clear Slide Highlights",
            Self::DuplicateSelectedNote => "Duplicate Selected Note",
            Self::DeleteSelectedNote => "Delete Selected Note",
            Self::ApplyTransform => "Apply Transform",
            Self::RevertTransform => "Revert Transform",
            Self::PreviousPenPreset => "Previous Pen Preset",
            Self::NextPenPreset => "Next Pen Preset",
            Self::ToggleEraser => "Toggle Eraser",
            Self::ToggleLassoSelect => "Toggle Lasso Select",
            Self::DeleteSelectedStrokes => "Delete Selected Strokes",
            Self::ApplyPresetToSelectedStrokes => "Apply Preset To Selected Strokes",
        }
    }

    fn default_key(self) -> Key {
        match self {
            Self::ToggleDeckOverview => Key::D,
            Self::ToggleTimingBreakdown => Key::B,
            Self::Next => Key::ArrowRight,
            Self::NextPageDown => Key::PageDown,
            Self::NextSpace => Key::Space,
            Self::Previous => Key::ArrowLeft,
            Self::PreviousPageUp => Key::PageUp,
            Self::First => Key::Home,
            Self::Last => Key::End,
            Self::SwapScreens => Key::S,
            Self::ToggleLabelMode => Key::J,
            Self::ToggleLaser => Key::L,
            Self::ToggleAutoAdvance => Key::T,
            Self::ToggleHighlight => Key::H,
            Self::ToggleAnnotationEdit => Key::A,
            Self::ToggleProjectorFullscreen => Key::F,
            Self::ResetTimer => Key::R,
            Self::GoTo => Key::G,
            Self::OverviewCancel => Key::Escape,
            Self::OverviewCommit => Key::Enter,
            Self::OverviewUp => Key::ArrowUp,
            Self::OverviewDown => Key::ArrowDown,
            Self::UndoStroke => Key::U,
            Self::RedoStroke => Key::Y,
            Self::ClearCurrentStroke => Key::X,
            Self::ClearSlideHighlights => Key::C,
            Self::DuplicateSelectedNote => Key::N,
            Self::DeleteSelectedNote => Key::Backspace,
            Self::ApplyTransform => Key::P,
            Self::RevertTransform => Key::V,
            Self::PreviousPenPreset => Key::Q,
            Self::NextPenPreset => Key::W,
            Self::ToggleEraser => Key::E,
            Self::ToggleLassoSelect => Key::M,
            Self::DeleteSelectedStrokes => Key::Delete,
            Self::ApplyPresetToSelectedStrokes => Key::K,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct ShortcutBindings {
    action_to_key: BTreeMap<ShortcutAction, Key>,
}

impl ShortcutBindings {
    fn from_config(config: &KeybindingConfig) -> (Self, Vec<String>) {
        let mut bindings = Self {
            action_to_key: BTreeMap::new(),
        };
        let mut warnings = Vec::new();
        let mut used_keys: BTreeMap<Key, ShortcutAction> = BTreeMap::new();

        let known_ids: BTreeSet<&'static str> = ShortcutAction::ALL
            .into_iter()
            .map(ShortcutAction::id)
            .collect();
        for configured in config.bindings.keys() {
            if !known_ids.contains(configured.as_str()) {
                warnings.push(format!("Unknown shortcut action '{configured}' ignored."));
            }
        }

        for action in ShortcutAction::ALL {
            let configured = config.bindings.get(action.id()).map(String::as_str);
            let parsed_configured = match configured {
                Some(raw) => match Self::parse_single_key(raw) {
                    Some(key) => Some(key),
                    None => {
                        warnings.push(format!(
                            "Invalid key '{}' for '{}'; using default '{}'.",
                            raw,
                            action.id(),
                            action.default_key().name()
                        ));
                        None
                    }
                },
                None => None,
            };

            let preferred_key = parsed_configured.unwrap_or_else(|| action.default_key());
            let mut final_key = preferred_key;

            if let Some(existing_action) = used_keys.get(&preferred_key).copied() {
                if preferred_key != action.default_key() {
                    let fallback = action.default_key();
                    if used_keys.contains_key(&fallback) {
                        warnings.push(format!(
                            "Key '{}' for '{}' conflicts with '{}'; action left unbound.",
                            preferred_key.name(),
                            action.id(),
                            existing_action.id()
                        ));
                        continue;
                    }
                    warnings.push(format!(
                        "Key '{}' for '{}' conflicts with '{}'; falling back to '{}'.",
                        preferred_key.name(),
                        action.id(),
                        existing_action.id(),
                        fallback.name()
                    ));
                    final_key = fallback;
                } else {
                    warnings.push(format!(
                        "Key '{}' for '{}' conflicts with '{}'; action left unbound.",
                        preferred_key.name(),
                        action.id(),
                        existing_action.id()
                    ));
                    continue;
                }
            }

            used_keys.insert(final_key, action);
            bindings.action_to_key.insert(action, final_key);
        }

        (bindings, warnings)
    }

    fn parse_single_key(raw: &str) -> Option<Key> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.contains("ctrl")
            || lower.contains("shift")
            || lower.contains("alt")
            || lower.contains("cmd")
            || trimmed.contains('+')
        {
            return None;
        }
        Key::from_name(trimmed)
    }

    fn key_for(&self, action: ShortcutAction) -> Option<Key> {
        self.action_to_key.get(&action).copied()
    }
}

impl Default for DeckOverviewState {
    fn default() -> Self {
        Self {
            is_open: false,
            selected_index: 0,
            thumbnail_scale: 1.0,
            commit_pending: None,
        }
    }
}

impl DeckOverviewState {
    fn open(&mut self, current_index: usize) {
        self.is_open = true;
        self.selected_index = current_index;
    }

    fn toggle(&mut self, current_index: usize) {
        if self.is_open {
            self.is_open = false;
        } else {
            self.open(current_index);
        }
    }

    fn cancel(&mut self) {
        self.is_open = false;
        self.commit_pending = None;
    }

    fn move_selection(
        &mut self,
        total_slides: usize,
        columns: usize,
        delta_col: i32,
        delta_row: i32,
    ) {
        if total_slides == 0 {
            self.selected_index = 0;
            return;
        }
        let cols = columns.max(1);
        let mut row = self.selected_index / cols;
        let mut col = self.selected_index % cols;

        if delta_col < 0 {
            col = col.saturating_sub(delta_col.unsigned_abs() as usize);
        } else if delta_col > 0 {
            col = col.saturating_add(delta_col as usize);
        }

        if delta_row < 0 {
            row = row.saturating_sub(delta_row.unsigned_abs() as usize);
        } else if delta_row > 0 {
            row = row.saturating_add(delta_row as usize);
        }

        let mut next = row.saturating_mul(cols).saturating_add(col);
        if next >= total_slides {
            next = total_slides - 1;
        }
        self.selected_index = next;
    }

    fn commit(&mut self) {
        self.commit_pending = Some(self.selected_index);
        self.is_open = false;
    }

    fn take_commit(&mut self) -> Option<usize> {
        self.commit_pending.take()
    }
}

impl PresenterApp {
    fn new(
        _cc: &eframe::CreationContext<'_>,
        config: AppConfig,
        config_path: PathBuf,
        initial_source: Option<PathBuf>,
    ) -> Self {
        let now = Instant::now();
        let auto_default_interval = config.auto_advance_default_interval_secs;
        let initial_highlight_tool = if config.pen_toolkit.eraser_enabled {
            HighlightTool::Eraser
        } else {
            HighlightTool::Draw
        };
        let (shortcut_bindings, shortcut_warnings) =
            ShortcutBindings::from_config(&config.keybindings);
        let keybinding_editor = Self::build_keybinding_editor(&config.keybindings);
        let keybinding_warning =
            (!shortcut_warnings.is_empty()).then(|| shortcut_warnings.join(" "));
        let mut app = Self {
            page_cache: PageCache::new(config.cache_size),
            state: PresentationState::with_labels(1, vec!["1".to_owned()], now),
            config,
            config_path,
            document: None,
            source_path_input: String::new(),
            go_to_input: String::new(),
            open_error: None,
            status_message: Some("Ready. Enter a PDF path and click Load.".to_owned()),
            last_reload_check: now,
            projector_viewport: ViewportId::from_hash_of("projector_window"),
            projector_texture: None,
            projector_texture_key: None,
            current_texture: None,
            current_texture_key: None,
            presenter_texture: None,
            presenter_texture_key: None,
            notes_texture: None,
            notes_texture_key: None,
            next_texture: None,
            next_texture_key: None,
            current_document_fingerprint: None,
            manual_layout_override: NotesLayoutOverrideConfig::default(),
            resolved_layout: None,
            resolved_layout_key: None,
            layout_reason: None,
            layout_warning: None,
            deck_overview: DeckOverviewState::default(),
            overview_textures: BTreeMap::new(),
            overview_texture_keys: BTreeMap::new(),
            annotation_edit_enabled: false,
            pending_text_annotation: String::new(),
            text_annotations: Vec::new(),
            original_text_annotations: BTreeMap::new(),
            text_annotation_cache_index: None,
            selected_annotation_id: None,
            active_text_transform: None,
            stroke_in_progress: Vec::new(),
            stroke_in_progress_style: None,
            lasso_path_in_progress: Vec::new(),
            selected_stroke_ids: BTreeSet::new(),
            active_highlight_tool: initial_highlight_tool,
            unsaved_strokes: BTreeMap::new(),
            stroke_undo_stack: BTreeMap::new(),
            eraser_erased_in_drag: BTreeSet::new(),
            eraser_removed_this_drag: false,
            laser_position: None,
            temporary_laser_active: false,
            laser_persistent_seen_this_frame: false,
            laser_primary_down_seen_this_frame: false,
            auto_override_editor_secs: auto_default_interval,
            auto_override_editor_index: None,
            timing_breakdown: TimingBreakdownState::default(),
            shortcut_bindings,
            keybinding_editor,
            keybinding_editor_dirty: false,
            keybinding_warning,
            unsaved_toast_until: None,
            unsaved_toast_text: None,
            non_highlight_unsaved_changes: false,
            pending_unsaved_action: None,
            allow_close_without_guard: false,
        };
        app.state.projector_fullscreen = app.config.startup_content_fullscreen;

        if let Some(path) = initial_source {
            app.source_path_input = path.display().to_string();
            if let Err(error) = app.load_document(path) {
                app.open_error = Some(error.to_string());
            }
        }

        app
    }

    fn build_keybinding_editor(config: &KeybindingConfig) -> BTreeMap<String, String> {
        let mut editor = KeybindingConfig::default().bindings;
        for action in ShortcutAction::ALL {
            if let Some(raw) = config.bindings.get(action.id()) {
                editor.insert(action.id().to_owned(), raw.clone());
            }
        }
        editor
    }

    fn rebuild_shortcut_bindings_from_config(&mut self) {
        let (shortcut_bindings, warnings) = ShortcutBindings::from_config(&self.config.keybindings);
        self.shortcut_bindings = shortcut_bindings;
        self.keybinding_warning = (!warnings.is_empty()).then(|| warnings.join(" "));
    }

    fn apply_keybinding_editor(&mut self) {
        self.config.keybindings.bindings = self.keybinding_editor.clone();
        self.rebuild_shortcut_bindings_from_config();
        self.persist_config();
        self.keybinding_editor_dirty = false;
        self.status_message = Some("Keybindings updated.".to_owned());
    }

    fn render_keybindings_editor(&mut self, ui: &mut egui::Ui) {
        ui.collapsing("Keybindings", |ui| {
            ui.label("Set one key per action (e.g. ArrowRight, PageDown, Space, G).");
            ui.label("Ctrl+S remains reserved for Save.");
            ui.add_space(4.0);

            egui::ScrollArea::vertical()
                .max_height(240.0)
                .show(ui, |ui| {
                    egui::Grid::new("keybinding_editor_grid")
                        .num_columns(3)
                        .spacing(Vec2::new(10.0, 6.0))
                        .striped(true)
                        .show(ui, |ui| {
                            ui.strong("Action");
                            ui.strong("Key");
                            ui.strong("Default");
                            ui.end_row();

                            for action in ShortcutAction::ALL {
                                ui.label(action.title());
                                let entry = self
                                    .keybinding_editor
                                    .entry(action.id().to_owned())
                                    .or_insert_with(|| action.default_key().name().to_owned());
                                let changed = ui.text_edit_singleline(entry).changed();
                                if changed {
                                    self.keybinding_editor_dirty = true;
                                }
                                ui.monospace(action.default_key().name());
                                ui.end_row();
                            }
                        });
                });

            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        self.keybinding_editor_dirty,
                        egui::Button::new("Apply Keybindings"),
                    )
                    .clicked()
                {
                    self.apply_keybinding_editor();
                }

                if ui.button("Reset Defaults").clicked() {
                    self.keybinding_editor = KeybindingConfig::default().bindings;
                    self.keybinding_editor_dirty = true;
                    self.apply_keybinding_editor();
                }
            });
        });
    }

    fn shortcut_consume(&self, ctx: &egui::Context, action: ShortcutAction) -> bool {
        let Some(key) = self.shortcut_bindings.key_for(action) else {
            return false;
        };
        ctx.input_mut(|input| input.consume_key(Modifiers::NONE, key))
    }

    fn active_pen_preset(&self) -> PenPreset {
        let Some(preset) = self
            .config
            .pen_toolkit
            .presets
            .get(self.config.pen_toolkit.active_preset_index)
            .cloned()
        else {
            return PenPreset {
                name: "Yellow".to_owned(),
                color_rgb: (1.0, 0.92, 0.15),
                width: 3.0,
                opacity: 0.90,
            };
        };
        preset
    }

    fn set_active_pen_preset(&mut self, index: usize) {
        if self.config.pen_toolkit.presets.is_empty() {
            return;
        }
        let clamped = index.min(self.config.pen_toolkit.presets.len().saturating_sub(1));
        self.config.pen_toolkit.active_preset_index = clamped;
        self.persist_config();
        let preset = self.active_pen_preset();
        self.status_message = Some(format!("Pen preset: {}", preset.name));
    }

    fn cycle_pen_preset(&mut self, direction: i32) {
        let count = self.config.pen_toolkit.presets.len();
        if count == 0 {
            return;
        }
        let current = self.config.pen_toolkit.active_preset_index;
        let next = if direction >= 0 {
            (current + 1) % count
        } else if current == 0 {
            count - 1
        } else {
            current - 1
        };
        self.set_active_pen_preset(next);
    }

    fn sanitize_pen_style_inputs(color_rgb: (f32, f32, f32), width: f32) -> ((f32, f32, f32), f32) {
        (
            (
                color_rgb.0.clamp(0.0, 1.0),
                color_rgb.1.clamp(0.0, 1.0),
                color_rgb.2.clamp(0.0, 1.0),
            ),
            width.clamp(0.5, 24.0),
        )
    }

    fn update_active_pen_preset_style(
        &mut self,
        color_rgb: Option<(f32, f32, f32)>,
        width: Option<f32>,
    ) {
        let count = self.config.pen_toolkit.presets.len();
        if count == 0 {
            return;
        }
        let active_index = self.config.pen_toolkit.active_preset_index.min(count - 1);
        let Some(active) = self.config.pen_toolkit.presets.get_mut(active_index) else {
            return;
        };

        let mut next_color = active.color_rgb;
        let mut next_width = active.width;
        if let Some(color) = color_rgb {
            next_color = color;
        }
        if let Some(value) = width {
            next_width = value;
        }
        let (next_color, next_width) = Self::sanitize_pen_style_inputs(next_color, next_width);
        if active.color_rgb == next_color && (active.width - next_width).abs() <= f32::EPSILON {
            return;
        }

        active.color_rgb = next_color;
        active.width = next_width;
        self.config.pen_toolkit.sanitize();
        self.persist_config();
    }

    fn set_highlight_tool(&mut self, tool: HighlightTool) {
        self.active_highlight_tool = tool;
        self.config.pen_toolkit.eraser_enabled = tool == HighlightTool::Eraser;
        self.stroke_in_progress.clear();
        self.stroke_in_progress_style = None;
        self.lasso_path_in_progress.clear();
        if tool != HighlightTool::Lasso {
            self.selected_stroke_ids.clear();
        }
        self.eraser_erased_in_drag.clear();
        self.eraser_removed_this_drag = false;
        self.persist_config();
    }

    fn toggle_eraser_mode(&mut self) {
        if self.active_highlight_tool == HighlightTool::Eraser {
            self.set_highlight_tool(HighlightTool::Draw);
        } else {
            self.set_highlight_tool(HighlightTool::Eraser);
        }
        self.status_message = Some(format!(
            "Eraser {}",
            if self.active_highlight_tool == HighlightTool::Eraser {
                "enabled"
            } else {
                "disabled"
            }
        ));
    }

    fn toggle_lasso_mode(&mut self) {
        if self.active_highlight_tool == HighlightTool::Lasso {
            self.set_highlight_tool(HighlightTool::Draw);
            self.status_message = Some("Lasso disabled.".to_owned());
        } else {
            self.set_highlight_tool(HighlightTool::Lasso);
            self.status_message = Some("Lasso enabled.".to_owned());
        }
    }

    fn color32_from_rgb_opacity(color_rgb: (f32, f32, f32), opacity: f32) -> Color32 {
        Color32::from_rgba_unmultiplied(
            (color_rgb.0.clamp(0.0, 1.0) * 255.0).round() as u8,
            (color_rgb.1.clamp(0.0, 1.0) * 255.0).round() as u8,
            (color_rgb.2.clamp(0.0, 1.0) * 255.0).round() as u8,
            (opacity.clamp(0.05, 1.0) * 255.0).round() as u8,
        )
    }

    fn constrain_straight_line(anchor: (f32, f32), current: (f32, f32)) -> (f32, f32) {
        let dx = current.0 - anchor.0;
        let dy = current.1 - anchor.1;
        if dx.abs() >= dy.abs() {
            (current.0, anchor.1)
        } else {
            (anchor.0, current.1)
        }
    }

    fn point_in_polygon(point: (f32, f32), polygon: &[(f32, f32)]) -> bool {
        if polygon.len() < 3 {
            return false;
        }
        let mut inside = false;
        let mut j = polygon.len() - 1;
        for i in 0..polygon.len() {
            let (xi, yi) = polygon[i];
            let (xj, yj) = polygon[j];
            let intersects = ((yi > point.1) != (yj > point.1))
                && (point.0 < (xj - xi) * (point.1 - yi) / ((yj - yi).abs().max(1e-6)) + xi);
            if intersects {
                inside = !inside;
            }
            j = i;
        }
        inside
    }

    fn orientation(a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> f32 {
        (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
    }

    fn on_segment(a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
        c.0 >= a.0.min(b.0) - 1e-6
            && c.0 <= a.0.max(b.0) + 1e-6
            && c.1 >= a.1.min(b.1) - 1e-6
            && c.1 <= a.1.max(b.1) + 1e-6
    }

    fn segments_intersect(a1: (f32, f32), a2: (f32, f32), b1: (f32, f32), b2: (f32, f32)) -> bool {
        let o1 = Self::orientation(a1, a2, b1);
        let o2 = Self::orientation(a1, a2, b2);
        let o3 = Self::orientation(b1, b2, a1);
        let o4 = Self::orientation(b1, b2, a2);

        if o1.abs() < 1e-6 && Self::on_segment(a1, a2, b1) {
            return true;
        }
        if o2.abs() < 1e-6 && Self::on_segment(a1, a2, b2) {
            return true;
        }
        if o3.abs() < 1e-6 && Self::on_segment(b1, b2, a1) {
            return true;
        }
        if o4.abs() < 1e-6 && Self::on_segment(b1, b2, a2) {
            return true;
        }

        (o1 > 0.0) != (o2 > 0.0) && (o3 > 0.0) != (o4 > 0.0)
    }

    fn bounds_for_points(points: &[(f32, f32)]) -> Option<(f32, f32, f32, f32)> {
        let first = points.first()?;
        let mut min_x = first.0;
        let mut max_x = first.0;
        let mut min_y = first.1;
        let mut max_y = first.1;
        for point in points.iter().skip(1) {
            min_x = min_x.min(point.0);
            max_x = max_x.max(point.0);
            min_y = min_y.min(point.1);
            max_y = max_y.max(point.1);
        }
        Some((min_x, max_x, min_y, max_y))
    }

    fn bounds_overlap(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> bool {
        !(a.1 < b.0 || b.1 < a.0 || a.3 < b.2 || b.3 < a.2)
    }

    fn stroke_selected_by_lasso(stroke_points: &[(f32, f32)], polygon: &[(f32, f32)]) -> bool {
        if stroke_points.len() < 2 || polygon.len() < 3 {
            return false;
        }
        if stroke_points
            .iter()
            .any(|point| Self::point_in_polygon(*point, polygon))
        {
            return true;
        }

        let mut poly_segments = Vec::with_capacity(polygon.len());
        for i in 0..polygon.len() {
            let j = (i + 1) % polygon.len();
            poly_segments.push((polygon[i], polygon[j]));
        }
        for segment in stroke_points.windows(2) {
            for (p1, p2) in &poly_segments {
                if Self::segments_intersect(segment[0], segment[1], *p1, *p2) {
                    return true;
                }
            }
        }
        false
    }

    fn compute_lasso_selection_for_strokes(
        strokes: &[SessionInkStroke],
        polygon: &[(f32, f32)],
    ) -> BTreeSet<String> {
        let Some(poly_bounds) = Self::bounds_for_points(polygon) else {
            return BTreeSet::new();
        };
        strokes
            .iter()
            .filter_map(|stroke| {
                let stroke_bounds = Self::bounds_for_points(&stroke.points)?;
                if !Self::bounds_overlap(stroke_bounds, poly_bounds) {
                    return None;
                }
                Self::stroke_selected_by_lasso(&stroke.points, polygon)
                    .then(|| stroke.annotation_id.clone())
            })
            .collect()
    }

    fn finish_lasso_selection(&mut self, index: usize) {
        if self.lasso_path_in_progress.len() < 3 {
            self.lasso_path_in_progress.clear();
            self.selected_stroke_ids.clear();
            return;
        }
        let selected = self
            .unsaved_strokes
            .get(&index)
            .map(|strokes| {
                Self::compute_lasso_selection_for_strokes(strokes, &self.lasso_path_in_progress)
            })
            .unwrap_or_default();
        self.selected_stroke_ids = selected;
        self.lasso_path_in_progress.clear();
        self.status_message = Some(format!(
            "Lasso selected {} stroke(s).",
            self.selected_stroke_ids.len()
        ));
    }

    fn clear_selected_strokes_selection(&mut self) {
        self.selected_stroke_ids.clear();
        self.lasso_path_in_progress.clear();
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let typing_in_text_field = ctx.wants_keyboard_input();

        if !typing_in_text_field && self.shortcut_consume(ctx, ShortcutAction::ToggleDeckOverview) {
            self.deck_overview.toggle(self.state.current_index());
        }
        if !typing_in_text_field
            && self.shortcut_consume(ctx, ShortcutAction::ToggleTimingBreakdown)
        {
            self.timing_breakdown.toggle();
        }

        if self.deck_overview.is_open {
            let total = self.state.total_slides();
            let cols = self.config.overview_columns as usize;
            if self.shortcut_consume(ctx, ShortcutAction::OverviewCancel) {
                self.deck_overview.cancel();
                return;
            }
            if self.shortcut_consume(ctx, ShortcutAction::OverviewCommit) {
                self.deck_overview.commit();
                return;
            }
            if self.shortcut_consume(ctx, ShortcutAction::Previous) {
                self.deck_overview.move_selection(total, cols, -1, 0);
            }
            if self.shortcut_consume(ctx, ShortcutAction::Next) {
                self.deck_overview.move_selection(total, cols, 1, 0);
            }
            if self.shortcut_consume(ctx, ShortcutAction::OverviewUp) {
                self.deck_overview.move_selection(total, cols, 0, -1);
            }
            if self.shortcut_consume(ctx, ShortcutAction::OverviewDown) {
                self.deck_overview.move_selection(total, cols, 0, 1);
            }
            if self.shortcut_consume(ctx, ShortcutAction::First) {
                self.deck_overview.selected_index = 0;
            }
            if self.shortcut_consume(ctx, ShortcutAction::Last) {
                self.deck_overview.selected_index = self.state.total_slides().saturating_sub(1);
            }
            return;
        }

        let next_pressed = !typing_in_text_field
            && (self.shortcut_consume(ctx, ShortcutAction::Next)
                || self.shortcut_consume(ctx, ShortcutAction::NextPageDown)
                || self.shortcut_consume(ctx, ShortcutAction::NextSpace));
        if next_pressed {
            self.apply_command(AppCommand::NextSlide);
        }

        let prev_pressed = !typing_in_text_field
            && (self.shortcut_consume(ctx, ShortcutAction::Previous)
                || self.shortcut_consume(ctx, ShortcutAction::PreviousPageUp));
        if prev_pressed {
            self.apply_command(AppCommand::PreviousSlide);
        }

        if !typing_in_text_field && self.shortcut_consume(ctx, ShortcutAction::First) {
            self.apply_command(AppCommand::FirstSlide);
        }
        if !typing_in_text_field && self.shortcut_consume(ctx, ShortcutAction::Last) {
            self.apply_command(AppCommand::LastSlide);
        }
        if ctx.input_mut(|input| input.consume_key(Modifiers::CTRL, Key::S)) {
            self.save_document_in_place();
            return;
        }
        if !typing_in_text_field && self.shortcut_consume(ctx, ShortcutAction::SwapScreens) {
            self.apply_command(AppCommand::SwapScreens);
        }
        if !typing_in_text_field && self.shortcut_consume(ctx, ShortcutAction::ToggleLabelMode) {
            self.apply_command(AppCommand::ToggleLabelMode);
        }
        if !typing_in_text_field && self.shortcut_consume(ctx, ShortcutAction::ToggleLaser) {
            self.apply_command(AppCommand::ToggleLaser);
            if !self.state.laser_enabled {
                self.laser_position = None;
            }
        }
        if !typing_in_text_field && self.shortcut_consume(ctx, ShortcutAction::ToggleAutoAdvance) {
            self.apply_command(AppCommand::ToggleAutoAdvance);
        }
        let highlight_toggle =
            !typing_in_text_field && self.shortcut_consume(ctx, ShortcutAction::ToggleHighlight);
        if highlight_toggle {
            self.apply_command(AppCommand::ToggleHighlight);
            if self.state.highlight_enabled {
                self.annotation_edit_enabled = false;
            } else {
                self.stroke_in_progress.clear();
                self.stroke_in_progress_style = None;
            }
        }
        let annotation_toggle = !typing_in_text_field
            && self.shortcut_consume(ctx, ShortcutAction::ToggleAnnotationEdit);
        if annotation_toggle {
            self.annotation_edit_enabled = !self.annotation_edit_enabled;
            if self.annotation_edit_enabled {
                self.state.highlight_enabled = false;
                self.stroke_in_progress.clear();
                self.stroke_in_progress_style = None;
            }
        }
        if !typing_in_text_field
            && self.shortcut_consume(ctx, ShortcutAction::ToggleProjectorFullscreen)
        {
            self.apply_command(AppCommand::ToggleProjectorFullscreen);
            self.persist_config();
        }
        if !typing_in_text_field && self.shortcut_consume(ctx, ShortcutAction::ResetTimer) {
            self.apply_command(AppCommand::ResetTimer);
        }
        if !typing_in_text_field && self.shortcut_consume(ctx, ShortcutAction::GoTo) {
            self.submit_go_to();
        }
        if !typing_in_text_field && self.shortcut_consume(ctx, ShortcutAction::UndoStroke) {
            self.undo_last_stroke(self.presenter_display_index());
        }
        if !typing_in_text_field && self.shortcut_consume(ctx, ShortcutAction::RedoStroke) {
            self.redo_last_stroke(self.presenter_display_index());
        }
        if !typing_in_text_field && self.shortcut_consume(ctx, ShortcutAction::ClearCurrentStroke) {
            self.clear_current_stroke();
        }
        if !typing_in_text_field && self.shortcut_consume(ctx, ShortcutAction::ClearSlideHighlights)
        {
            self.clear_slide_highlights(self.presenter_display_index());
        }
        if !typing_in_text_field
            && self.shortcut_consume(ctx, ShortcutAction::DuplicateSelectedNote)
        {
            self.duplicate_selected_note(self.presenter_display_index());
        }
        if !typing_in_text_field && self.shortcut_consume(ctx, ShortcutAction::DeleteSelectedNote) {
            self.delete_selected_note(self.presenter_display_index());
        }
        if !typing_in_text_field
            && self.shortcut_consume(ctx, ShortcutAction::ApplyTransform)
            && self.selected_transform_dirty()
        {
            self.commit_selected_annotation_update("Annotation transform applied (unsaved).");
        }
        if !typing_in_text_field && self.shortcut_consume(ctx, ShortcutAction::RevertTransform) {
            self.revert_selected_transform();
        }
        if !typing_in_text_field && self.shortcut_consume(ctx, ShortcutAction::PreviousPenPreset) {
            self.cycle_pen_preset(-1);
        }
        if !typing_in_text_field && self.shortcut_consume(ctx, ShortcutAction::NextPenPreset) {
            self.cycle_pen_preset(1);
        }
        if !typing_in_text_field && self.shortcut_consume(ctx, ShortcutAction::ToggleEraser) {
            self.toggle_eraser_mode();
        }
        if !typing_in_text_field && self.shortcut_consume(ctx, ShortcutAction::ToggleLassoSelect) {
            self.toggle_lasso_mode();
        }
        if !typing_in_text_field
            && self.shortcut_consume(ctx, ShortcutAction::DeleteSelectedStrokes)
        {
            self.delete_selected_strokes(self.presenter_display_index());
        }
        if !typing_in_text_field
            && self.shortcut_consume(ctx, ShortcutAction::ApplyPresetToSelectedStrokes)
        {
            self.apply_active_preset_to_selected_strokes(self.presenter_display_index());
        }
    }

    fn document_fingerprint(path: &Path) -> String {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let normalized = canonical.to_string_lossy().to_lowercase();
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in normalized.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        format!("{hash:016x}")
    }

    fn apply_saved_override_for_current_document(&mut self) {
        let Some(fingerprint) = self.current_document_fingerprint.as_ref() else {
            self.manual_layout_override = NotesLayoutOverrideConfig::default();
            return;
        };

        self.manual_layout_override = self
            .config
            .notes_overrides
            .get(fingerprint)
            .cloned()
            .unwrap_or_default();
    }

    fn sync_override_to_config(&mut self) {
        let Some(fingerprint) = self.current_document_fingerprint.clone() else {
            return;
        };

        if self.manual_layout_override.enabled {
            let mut override_copy = self.manual_layout_override.clone();
            override_copy.sanitize();
            self.config
                .notes_overrides
                .insert(fingerprint, override_copy.clone());
            self.manual_layout_override = override_copy;
        } else {
            self.config.notes_overrides.remove(&fingerprint);
        }

        self.persist_config();
        self.invalidate_render_cache();
    }

    fn apply_auto_advance_profile_for_current_document(&mut self) {
        let Some(fingerprint) = self.current_document_fingerprint.as_ref() else {
            let now = Instant::now();
            self.state.set_auto_advance_profile(
                self.config.auto_advance_default_interval_secs,
                BTreeMap::new(),
                now,
            );
            self.state
                .apply(AppCommand::SetAutoAdvanceEnabled(false), now);
            self.auto_override_editor_secs = self.state.auto_advance_global_interval_secs();
            return;
        };

        let profile = self.config.auto_advance_per_document.get(fingerprint);
        let global = profile
            .map(|config| config.global_interval_secs)
            .unwrap_or(self.config.auto_advance_default_interval_secs);
        let overrides = profile
            .map(|config| config.slide_overrides_secs.clone())
            .unwrap_or_default();

        let now = Instant::now();
        self.state.set_auto_advance_profile(global, overrides, now);
        self.state
            .apply(AppCommand::SetAutoAdvanceEnabled(false), now);
        self.auto_override_editor_secs = self.state.auto_advance_global_interval_secs();
    }

    fn sync_auto_advance_profile_to_config(&mut self) {
        let Some(fingerprint) = self.current_document_fingerprint.clone() else {
            return;
        };

        let (global, overrides) = self.state.auto_advance_profile();
        self.config.auto_advance_per_document.insert(
            fingerprint,
            AutoAdvanceDocumentConfig {
                global_interval_secs: global.clamp(1, 3600),
                slide_overrides_secs: overrides
                    .into_iter()
                    .map(|(index, secs)| (index, secs.clamp(1, 3600)))
                    .collect(),
            },
        );
        self.persist_config();
    }

    fn active_override_layout(&self) -> Option<NotesLayout> {
        if self.manual_layout_override.enabled {
            Some(self.manual_layout_override.as_layout())
        } else {
            None
        }
    }

    fn reset_layout_resolution(&mut self) {
        self.resolved_layout = None;
        self.resolved_layout_key = None;
        self.layout_reason = None;
        self.layout_warning = None;
    }

    fn load_document(&mut self, path: PathBuf) -> Result<()> {
        let backend = open_document(&path)?;
        let fingerprint = Self::document_fingerprint(backend.source_path());
        let labels = Self::labels_from_backend(backend.as_ref());
        let total_slides = backend.page_count();
        self.state
            .update_document(total_slides, labels, Instant::now());
        self.deck_overview.selected_index = self.state.current_index();
        self.state.projector_fullscreen = self.config.startup_content_fullscreen;
        self.document = Some(backend);
        self.current_document_fingerprint = Some(fingerprint);
        self.apply_saved_override_for_current_document();
        self.apply_auto_advance_profile_for_current_document();
        self.annotation_edit_enabled = false;
        self.pending_text_annotation.clear();
        self.text_annotations.clear();
        self.original_text_annotations.clear();
        self.text_annotation_cache_index = None;
        self.selected_annotation_id = None;
        self.active_text_transform = None;
        self.stroke_in_progress.clear();
        self.stroke_in_progress_style = None;
        self.unsaved_strokes.clear();
        self.stroke_undo_stack.clear();
        self.eraser_erased_in_drag.clear();
        self.eraser_removed_this_drag = false;
        self.laser_position = None;
        self.temporary_laser_active = false;
        self.laser_persistent_seen_this_frame = false;
        self.laser_primary_down_seen_this_frame = false;
        self.auto_override_editor_index = None;
        self.timing_breakdown.is_open = false;
        self.unsaved_toast_until = None;
        self.unsaved_toast_text = None;
        self.non_highlight_unsaved_changes = false;
        self.pending_unsaved_action = None;
        self.allow_close_without_guard = false;
        self.invalidate_render_cache();
        self.status_message = Some(format!(
            "Loaded {} slide(s) from {}",
            total_slides,
            path.display()
        ));
        self.open_error = None;
        Ok(())
    }

    fn labels_from_backend(backend: &dyn DocumentBackend) -> Vec<String> {
        (0..backend.page_count())
            .map(|index| backend.page_label(index))
            .collect()
    }

    fn apply_command(&mut self, command: AppCommand) {
        let now = Instant::now();
        let previous_index = self.state.current_index();
        let command_for_status = command.clone();
        if self.state.apply(command, now) {
            let changed_slide = self.state.current_index() != previous_index;
            if changed_slide {
                self.text_annotation_cache_index = None;
                self.selected_annotation_id = None;
                self.active_text_transform = None;
                self.stroke_in_progress.clear();
                self.stroke_in_progress_style = None;
                self.auto_override_editor_index = None;
                self.status_message = Some(format!(
                    "Slide {}/{}",
                    self.state.current_index() + 1,
                    self.state.total_slides()
                ));
                return;
            }

            match command_for_status {
                AppCommand::ToggleAutoAdvance | AppCommand::SetAutoAdvanceEnabled(_) => {
                    self.status_message = Some(format!(
                        "Auto-advance {}",
                        if self.state.auto_advance_enabled() {
                            "enabled"
                        } else {
                            "disabled"
                        }
                    ));
                }
                AppCommand::SetAutoAdvanceGlobalInterval(secs) => {
                    self.status_message = Some(format!(
                        "Auto-advance global interval set to {}s",
                        secs.clamp(1, 3600)
                    ));
                }
                AppCommand::SetAutoAdvanceSlideOverride { index, secs } => {
                    self.status_message = Some(format!(
                        "Slide {} auto-advance override set to {}s",
                        index + 1,
                        secs.clamp(1, 3600)
                    ));
                }
                AppCommand::ClearAutoAdvanceSlideOverride(index) => {
                    self.status_message =
                        Some(format!("Slide {} auto-advance override cleared", index + 1));
                }
                _ => {}
            }
        }
    }

    fn run_auto_advance_tick(&mut self) {
        if self.document.is_none() || self.deck_overview.is_open {
            return;
        }

        let now = Instant::now();
        if self.state.maybe_auto_advance(now) {
            self.text_annotation_cache_index = None;
            self.selected_annotation_id = None;
            self.active_text_transform = None;
            self.stroke_in_progress.clear();
            self.stroke_in_progress_style = None;
            self.auto_override_editor_index = None;
            self.status_message = Some(format!(
                "Auto-advance to slide {}/{}",
                self.state.current_index() + 1,
                self.state.total_slides()
            ));
        }
    }

    fn interaction_mode(&self) -> InteractionMode {
        if self.annotation_edit_enabled {
            InteractionMode::AnnotationEdit
        } else if self.state.highlight_enabled {
            InteractionMode::HighlightDraw
        } else {
            InteractionMode::Navigate
        }
    }

    fn document_dirty(&self) -> bool {
        self.non_highlight_unsaved_changes || !self.unsaved_strokes.is_empty()
    }

    fn has_unsaved_annotation_changes(&self) -> bool {
        self.document_dirty() || !self.stroke_in_progress.is_empty()
    }

    fn show_unsaved_edit_feedback(&mut self, message: &str) {
        self.status_message = Some(message.to_owned());
        self.unsaved_toast_text = Some(message.to_owned());
        self.unsaved_toast_until = Some(Instant::now() + Duration::from_millis(2500));
    }

    fn mark_non_highlight_unsaved(&mut self) {
        self.non_highlight_unsaved_changes = true;
    }

    fn request_load_document(&mut self, path: PathBuf) {
        if self.has_unsaved_annotation_changes() {
            self.pending_unsaved_action = Some(PendingUnsavedAction::Load(path));
            self.status_message = Some(
                "Unsaved annotation changes detected. Choose Save, Discard, or Cancel.".to_owned(),
            );
            return;
        }

        if let Err(error) = self.load_document(path) {
            self.open_error = Some(error.to_string());
        }
    }

    fn execute_pending_unsaved_action(&mut self, ctx: &Context, action: PendingUnsavedAction) {
        match action {
            PendingUnsavedAction::Load(path) => {
                if let Err(error) = self.load_document(path) {
                    self.open_error = Some(error.to_string());
                }
            }
            PendingUnsavedAction::Close => {
                self.allow_close_without_guard = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn render_unsaved_changes_dialog(&mut self, ctx: &Context) {
        let Some(action) = self.pending_unsaved_action.clone() else {
            return;
        };

        egui::Window::new("Unsaved Annotation Changes")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("You have unsaved annotation changes.");
                ui.label("Choose what to do before continuing:");
                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        self.save_document_in_place();
                        if !self.has_unsaved_annotation_changes() {
                            self.pending_unsaved_action = None;
                            self.execute_pending_unsaved_action(ctx, action.clone());
                        }
                    }

                    if ui.button("Discard").clicked() {
                        self.pending_unsaved_action = None;
                        self.execute_pending_unsaved_action(ctx, action.clone());
                    }

                    if ui.button("Cancel").clicked() {
                        self.pending_unsaved_action = None;
                    }
                });
            });
    }

    fn render_unsaved_toast(&mut self, ctx: &Context) {
        let Some(until) = self.unsaved_toast_until else {
            return;
        };
        if Instant::now() > until {
            self.unsaved_toast_until = None;
            self.unsaved_toast_text = None;
            return;
        }
        let Some(message) = self.unsaved_toast_text.as_ref() else {
            return;
        };
        egui::Area::new("unsaved_edit_toast".into())
            .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-16.0, -16.0))
            .show(ctx, |ui| {
                let frame = egui::Frame::new()
                    .fill(Color32::from_rgba_unmultiplied(12, 14, 20, 220))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(255, 208, 86)))
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::same(8));
                frame.show(ui, |ui| {
                    ui.colored_label(Color32::from_rgb(255, 235, 140), message);
                });
            });
    }

    fn save_document_in_place(&mut self) {
        let Some(document) = self.document.as_mut() else {
            self.open_error = Some("No document loaded.".to_owned());
            return;
        };

        match document.save_in_place() {
            Ok(()) => {
                self.unsaved_strokes.clear();
                self.stroke_undo_stack.clear();
                self.stroke_in_progress.clear();
                self.stroke_in_progress_style = None;
                self.eraser_erased_in_drag.clear();
                self.eraser_removed_this_drag = false;
                self.non_highlight_unsaved_changes = false;
                self.invalidate_render_cache();
                self.status_message = Some("Saved PDF with annotations.".to_owned());
            }
            Err(error) => {
                self.open_error = Some(format!("Failed to save PDF: {error}"));
            }
        }
    }

    fn refresh_text_annotations_for_index(&mut self, index: usize) {
        if self.text_annotation_cache_index == Some(index) {
            return;
        }

        let Some(document) = self.document.as_ref() else {
            self.text_annotations.clear();
            self.original_text_annotations.clear();
            self.text_annotation_cache_index = None;
            self.selected_annotation_id = None;
            self.active_text_transform = None;
            return;
        };

        match document.list_text_annotations(index) {
            Ok(annotations) => {
                self.original_text_annotations = annotations
                    .iter()
                    .map(|annotation| (annotation.id.clone(), annotation.clone()))
                    .collect();
                self.text_annotations = annotations;
                self.text_annotation_cache_index = Some(index);
                if let Some(selected) = self.selected_annotation_id.as_ref()
                    && !self
                        .text_annotations
                        .iter()
                        .any(|entry| &entry.id == selected)
                {
                    self.selected_annotation_id = None;
                }
                self.active_text_transform = None;
            }
            Err(error) => {
                self.open_error = Some(format!("Failed to load text annotations: {error}"));
                self.text_annotations.clear();
                self.original_text_annotations.clear();
                self.text_annotation_cache_index = None;
                self.selected_annotation_id = None;
                self.active_text_transform = None;
            }
        }
    }

    fn slide_region(layout: Option<NotesLayout>) -> SlideRegion {
        let Some(layout) = layout else {
            return SlideRegion {
                min_x: 0.0,
                max_x: 1.0,
                min_y: 0.0,
                max_y: 1.0,
            };
        };

        let split = layout.split_ratio.clamp(0.0, 1.0);
        match layout.axis {
            SplitAxis::Vertical => {
                if layout.notes_side == NotesSide::Left {
                    SlideRegion {
                        min_x: split,
                        max_x: 1.0,
                        min_y: 0.0,
                        max_y: 1.0,
                    }
                } else {
                    SlideRegion {
                        min_x: 0.0,
                        max_x: split,
                        min_y: 0.0,
                        max_y: 1.0,
                    }
                }
            }
            SplitAxis::Horizontal => {
                if layout.notes_side == NotesSide::Top {
                    SlideRegion {
                        min_x: 0.0,
                        max_x: 1.0,
                        min_y: 0.0,
                        max_y: 1.0 - split,
                    }
                } else {
                    SlideRegion {
                        min_x: 0.0,
                        max_x: 1.0,
                        min_y: split,
                        max_y: 1.0,
                    }
                }
            }
        }
    }

    fn view_fraction(layout: Option<NotesLayout>, view: PageView) -> f32 {
        let Some(layout) = layout else {
            return 1.0;
        };
        if view == PageView::Full {
            return 1.0;
        }

        let split = layout.split_ratio.clamp(0.0, 1.0);
        match layout.axis {
            SplitAxis::Vertical => {
                let notes_fraction = if layout.notes_side == NotesSide::Left {
                    split
                } else {
                    1.0 - split
                };
                if view == PageView::NotesOnly {
                    notes_fraction
                } else {
                    1.0 - notes_fraction
                }
            }
            SplitAxis::Horizontal => {
                let notes_fraction = if layout.notes_side == NotesSide::Top {
                    split
                } else {
                    1.0 - split
                };
                if view == PageView::NotesOnly {
                    notes_fraction
                } else {
                    1.0 - notes_fraction
                }
            }
        }
    }

    fn has_visible_region(layout: Option<NotesLayout>, view: PageView) -> bool {
        Self::view_fraction(layout, view) > 0.001
    }

    fn screen_to_page_normalized(
        rect: Rect,
        region: SlideRegion,
        point: Pos2,
        space: SlideSurfaceSpace,
    ) -> Option<(f32, f32)> {
        if rect.width() <= 1.0 || rect.height() <= 1.0 || !rect.contains(point) {
            return None;
        }
        let x_local = ((point.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        let y_local_down = ((point.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
        let y_local_up = 1.0 - y_local_down;
        match space {
            SlideSurfaceSpace::CroppedSlide => {
                let x = region.min_x + x_local * (region.max_x - region.min_x);
                let y = region.min_y + y_local_up * (region.max_y - region.min_y);
                Some((x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)))
            }
            SlideSurfaceSpace::FullPage => {
                if x_local < region.min_x
                    || x_local > region.max_x
                    || y_local_up < region.min_y
                    || y_local_up > region.max_y
                {
                    return None;
                }
                Some((x_local, y_local_up))
            }
        }
    }

    fn page_normalized_to_screen(
        rect: Rect,
        region: SlideRegion,
        point: (f32, f32),
        space: SlideSurfaceSpace,
    ) -> Option<Pos2> {
        let (x_local, y_local_up) = match space {
            SlideSurfaceSpace::CroppedSlide => {
                let width = (region.max_x - region.min_x).max(0.0001);
                let height = (region.max_y - region.min_y).max(0.0001);
                let x_local = (point.0 - region.min_x) / width;
                let y_local_up = (point.1 - region.min_y) / height;
                (x_local, y_local_up)
            }
            SlideSurfaceSpace::FullPage => (point.0, point.1),
        };
        if !(0.0..=1.0).contains(&x_local) || !(0.0..=1.0).contains(&y_local_up) {
            return None;
        }
        let x = rect.left() + x_local * rect.width();
        let y = rect.top() + (1.0 - y_local_up) * rect.height();
        Some(Pos2::new(x, y))
    }

    fn capture_laser_sample(ctx: &Context, response: &egui::Response) -> LaserSurfaceSample {
        let pointer_pos = response.interact_pointer_pos().or_else(|| {
            response
                .hovered()
                .then(|| ctx.input(|input| input.pointer.hover_pos()))
                .flatten()
        });

        LaserSurfaceSample {
            rect: response.rect,
            pointer_pos,
            ctrl_down: ctx.input(|input| input.modifiers.ctrl),
            primary_down: ctx.input(|input| input.pointer.primary_down()),
            hovered: response.hovered(),
        }
    }

    fn finalize_laser_state(
        laser_enabled: bool,
        persistent_seen_this_frame: bool,
        temporary_active: bool,
        primary_down: bool,
    ) -> (bool, bool) {
        let temporary_next = temporary_active && primary_down;
        let visible = (laser_enabled && persistent_seen_this_frame) || temporary_next;
        (temporary_next, visible)
    }

    fn consume_laser_sample(
        &mut self,
        sample: LaserSurfaceSample,
        region: SlideRegion,
        space: SlideSurfaceSpace,
    ) {
        if sample.primary_down {
            self.laser_primary_down_seen_this_frame = true;
        }
        if let Some(pointer) = sample.pointer_pos
            && let Some(normalized) =
                Self::screen_to_page_normalized(sample.rect, region, pointer, space)
        {
            if sample.ctrl_down && sample.primary_down {
                self.temporary_laser_active = true;
                self.laser_position = Some(normalized);
            }

            if self.state.laser_enabled && sample.hovered {
                self.laser_position = Some(normalized);
                self.laser_persistent_seen_this_frame = true;
            }
        }
    }

    fn finish_laser_frame(&mut self, ctx: &Context) {
        let primary_down = self.laser_primary_down_seen_this_frame
            || ctx.input(|input| input.pointer.primary_down());
        let (temporary_next, visible) = Self::finalize_laser_state(
            self.state.laser_enabled,
            self.laser_persistent_seen_this_frame,
            self.temporary_laser_active,
            primary_down,
        );
        self.temporary_laser_active = temporary_next;
        if !visible {
            self.laser_position = None;
        }
    }

    fn draw_laser_overlay(
        ui: &egui::Ui,
        image_rect: Rect,
        laser_position: Option<(f32, f32)>,
        region: SlideRegion,
        space: SlideSurfaceSpace,
    ) {
        let Some(page_position) = laser_position else {
            return;
        };
        let Some(screen_position) =
            Self::page_normalized_to_screen(image_rect, region, page_position, space)
        else {
            return;
        };
        let painter = ui.painter_at(image_rect);
        painter.circle_filled(
            screen_position,
            14.0,
            Color32::from_rgba_unmultiplied(255, 60, 60, 45),
        );
        painter.circle_filled(
            screen_position,
            5.0,
            Color32::from_rgba_unmultiplied(255, 70, 70, 140),
        );
        painter.circle_filled(screen_position, 2.0, Color32::from_rgb(255, 245, 245));
    }

    fn default_text_annotation_rect(geometry: PageGeometry, region: SlideRegion) -> PdfRect {
        let region_width = (region.max_x - region.min_x).max(0.1);
        let region_height = (region.max_y - region.min_y).max(0.1);
        let left = geometry.width * (region.min_x + 0.05 * region_width);
        let bottom = geometry.height * (region.min_y + 0.75 * region_height);
        let note_width = (geometry.width * region_width * 0.35).clamp(90.0, 320.0);
        let note_height = (geometry.height * region_height * 0.16).clamp(48.0, 140.0);

        PdfRect {
            left,
            bottom,
            right: (left + note_width).min(geometry.width),
            top: (bottom + note_height).min(geometry.height),
        }
        .normalized()
    }

    fn add_text_annotation_for_current_slide(&mut self, index: usize, layout: Option<NotesLayout>) {
        let contents = self.pending_text_annotation.trim().to_owned();
        if contents.is_empty() {
            return;
        }

        let Some(document) = self.document.as_ref() else {
            self.open_error = Some("No document loaded.".to_owned());
            return;
        };
        let geometry = match document.page_geometry(index) {
            Ok(geometry) => geometry,
            Err(error) => {
                self.open_error = Some(format!("Failed to read page geometry: {error}"));
                return;
            }
        };

        let region = Self::slide_region(layout);
        let rect = Self::default_text_annotation_rect(geometry, region);
        let Some(document) = self.document.as_mut() else {
            return;
        };

        let metadata = AnnotationMetadata {
            author: "PDF Presenter".to_owned(),
            color_rgb: (1.0, 1.0, 0.0),
            ..Default::default()
        };
        match document.add_text_annotation(
            index,
            NewTextAnnotation {
                rect,
                contents,
                metadata,
            },
        ) {
            Ok(created) => {
                self.pending_text_annotation.clear();
                self.text_annotation_cache_index = None;
                self.refresh_text_annotations_for_index(index);
                self.selected_annotation_id = Some(created.id);
                self.mark_non_highlight_unsaved();
                self.show_unsaved_edit_feedback("Text annotation added (unsaved).");
            }
            Err(error) => {
                self.open_error = Some(format!("Failed to add text annotation: {error}"));
            }
        }
    }

    fn persist_ink_stroke_with_style(
        &mut self,
        index: usize,
        points_normalized: &[(f32, f32)],
        color_rgb: (f32, f32, f32),
        width: f32,
        opacity: f32,
    ) -> Result<String> {
        if points_normalized.len() < 2 {
            anyhow::bail!("ink annotation requires at least two points");
        }

        let Some(document) = self.document.as_ref() else {
            anyhow::bail!("no document loaded");
        };
        let geometry = document
            .page_geometry(index)
            .map_err(|error| anyhow!("Failed to read page geometry: {error}"))?;

        let mut min_x = f32::MAX;
        let mut min_y = f32::MAX;
        let mut max_x = f32::MIN;
        let mut max_y = f32::MIN;
        let mut points = Vec::with_capacity(points_normalized.len());

        for (x, y) in points_normalized {
            let page_x = x.clamp(0.0, 1.0) * geometry.width;
            let page_y = y.clamp(0.0, 1.0) * geometry.height;
            min_x = min_x.min(page_x);
            min_y = min_y.min(page_y);
            max_x = max_x.max(page_x);
            max_y = max_y.max(page_y);
            points.push((page_x, page_y));
        }

        if points.len() < 2 {
            anyhow::bail!("ink annotation requires at least one stroke with two points");
        }

        let padding = 4.0;
        let rect = PdfRect {
            left: (min_x - padding).max(0.0),
            bottom: (min_y - padding).max(0.0),
            right: (max_x + padding).min(geometry.width),
            top: (max_y + padding).min(geometry.height),
        };

        let Some(document) = self.document.as_mut() else {
            anyhow::bail!("no document loaded");
        };
        document
            .add_ink_annotation(
                index,
                NewInkAnnotation {
                    rect,
                    strokes: vec![crate::document::InkStroke { points }],
                    color_rgb,
                    opacity,
                    width,
                },
            )
            .map_err(|error| anyhow!("Failed to add ink annotation: {error}"))
    }

    fn finalize_stroke(&mut self, index: usize) {
        if self.stroke_in_progress.len() < 2 {
            self.stroke_in_progress.clear();
            self.stroke_in_progress_style = None;
            return;
        }

        let points_normalized = std::mem::take(&mut self.stroke_in_progress);
        let style = self.stroke_in_progress_style.take().unwrap_or_else(|| {
            let preset = self.active_pen_preset();
            PenStrokeStyle {
                color_rgb: preset.color_rgb,
                width: preset.width,
                opacity: preset.opacity,
            }
        });

        match self.persist_ink_stroke_with_style(
            index,
            &points_normalized,
            style.color_rgb,
            style.width,
            style.opacity,
        ) {
            Ok(annotation_id) => {
                self.unsaved_strokes
                    .entry(index)
                    .or_default()
                    .push(SessionInkStroke {
                        annotation_id,
                        points: points_normalized,
                        color_rgb: style.color_rgb,
                        width: style.width,
                        opacity: style.opacity,
                    });
                self.stroke_undo_stack.remove(&index);
                self.show_unsaved_edit_feedback("Ink highlight added (unsaved).");
            }
            Err(error) => {
                self.open_error = Some(error.to_string());
            }
        }
    }

    fn clear_current_stroke(&mut self) {
        if self.stroke_in_progress.is_empty() {
            return;
        }
        self.stroke_in_progress.clear();
        self.stroke_in_progress_style = None;
        self.show_unsaved_edit_feedback("Current stroke cleared.");
    }

    fn undo_last_stroke(&mut self, index: usize) {
        let popped = {
            let Some(strokes) = self.unsaved_strokes.get_mut(&index) else {
                return;
            };
            let Some(popped) = strokes.pop() else {
                return;
            };
            let should_remove = strokes.is_empty();
            (popped, should_remove)
        };

        if popped.1 {
            self.unsaved_strokes.remove(&index);
        }

        let stroke = popped.0;
        self.selected_stroke_ids.remove(&stroke.annotation_id);
        let Some(document) = self.document.as_mut() else {
            return;
        };
        if let Err(error) = document.delete_annotation(&stroke.annotation_id) {
            self.open_error = Some(format!("Failed to undo stroke: {error}"));
            self.unsaved_strokes.entry(index).or_default().push(stroke);
            return;
        }

        self.stroke_undo_stack
            .entry(index)
            .or_default()
            .push(stroke);
        self.show_unsaved_edit_feedback("Stroke undone (unsaved).");
    }

    fn redo_last_stroke(&mut self, index: usize) {
        let stroke = {
            let Some(stack) = self.stroke_undo_stack.get_mut(&index) else {
                return;
            };
            let Some(stroke) = stack.pop() else {
                return;
            };
            if stack.is_empty() {
                self.stroke_undo_stack.remove(&index);
            }
            stroke
        };

        match self.persist_ink_stroke_with_style(
            index,
            &stroke.points,
            stroke.color_rgb,
            stroke.width,
            stroke.opacity,
        ) {
            Ok(annotation_id) => {
                self.unsaved_strokes
                    .entry(index)
                    .or_default()
                    .push(SessionInkStroke {
                        annotation_id,
                        points: stroke.points,
                        color_rgb: stroke.color_rgb,
                        width: stroke.width,
                        opacity: stroke.opacity,
                    });
                self.show_unsaved_edit_feedback("Stroke redone (unsaved).");
            }
            Err(error) => {
                self.open_error = Some(error.to_string());
            }
        }
    }

    fn clear_slide_highlights(&mut self, index: usize) {
        let Some(strokes) = self.unsaved_strokes.remove(&index) else {
            return;
        };

        let Some(document) = self.document.as_mut() else {
            return;
        };

        let mut failed = Vec::new();
        for stroke in strokes {
            if let Err(error) = document.delete_annotation(&stroke.annotation_id) {
                self.open_error = Some(format!("Failed to clear highlight stroke: {error}"));
                failed.push(stroke);
            }
        }
        if !failed.is_empty() {
            self.unsaved_strokes.insert(index, failed);
            return;
        }

        self.stroke_undo_stack.remove(&index);
        self.stroke_in_progress.clear();
        self.stroke_in_progress_style = None;
        self.selected_stroke_ids.clear();
        self.lasso_path_in_progress.clear();
        self.show_unsaved_edit_feedback("Slide highlights cleared (unsaved).");
    }

    fn delete_selected_strokes(&mut self, index: usize) {
        if self.selected_stroke_ids.is_empty() {
            return;
        }
        let selected_ids = self.selected_stroke_ids.clone();
        let Some(document) = self.document.as_mut() else {
            return;
        };

        let mut removed_ids = BTreeSet::new();
        for annotation_id in selected_ids {
            match document.delete_annotation(&annotation_id) {
                Ok(()) => {
                    removed_ids.insert(annotation_id);
                }
                Err(error) => {
                    self.open_error = Some(format!("Failed to delete selected stroke: {error}"));
                }
            }
        }
        if removed_ids.is_empty() {
            return;
        }

        let mut remove_empty_entry = false;
        if let Some(strokes) = self.unsaved_strokes.get_mut(&index) {
            strokes.retain(|stroke| !removed_ids.contains(&stroke.annotation_id));
            remove_empty_entry = strokes.is_empty();
        }
        if remove_empty_entry {
            self.unsaved_strokes.remove(&index);
        }

        self.stroke_undo_stack.remove(&index);
        self.selected_stroke_ids.clear();
        self.lasso_path_in_progress.clear();
        self.show_unsaved_edit_feedback("Selected strokes deleted (unsaved).");
    }

    fn apply_active_preset_to_selected_strokes(&mut self, index: usize) {
        if self.selected_stroke_ids.is_empty() {
            return;
        }
        let Some(strokes) = self.unsaved_strokes.get(&index) else {
            return;
        };
        let selected: Vec<SessionInkStroke> = strokes
            .iter()
            .filter(|stroke| self.selected_stroke_ids.contains(&stroke.annotation_id))
            .cloned()
            .collect();
        if selected.is_empty() {
            return;
        }

        let preset = self.active_pen_preset();
        let mut replacements = BTreeMap::new();
        for stroke in selected {
            let delete_result = self
                .document
                .as_mut()
                .map(|document| document.delete_annotation(&stroke.annotation_id));
            let Some(delete_result) = delete_result else {
                return;
            };
            if let Err(error) = delete_result {
                self.open_error = Some(format!("Failed to restyle selected stroke: {error}"));
                continue;
            }
            match self.persist_ink_stroke_with_style(
                index,
                &stroke.points,
                preset.color_rgb,
                preset.width,
                preset.opacity,
            ) {
                Ok(new_id) => {
                    replacements.insert(
                        stroke.annotation_id.clone(),
                        SessionInkStroke {
                            annotation_id: new_id,
                            points: stroke.points,
                            color_rgb: preset.color_rgb,
                            width: preset.width,
                            opacity: preset.opacity,
                        },
                    );
                }
                Err(error) => {
                    self.open_error = Some(error.to_string());
                }
            }
        }
        if replacements.is_empty() {
            return;
        }

        if let Some(strokes) = self.unsaved_strokes.get_mut(&index) {
            for stroke in strokes.iter_mut() {
                if let Some(updated) = replacements.get(&stroke.annotation_id) {
                    *stroke = updated.clone();
                }
            }
        }

        self.stroke_undo_stack.remove(&index);
        self.selected_stroke_ids.clear();
        self.lasso_path_in_progress.clear();
        self.show_unsaved_edit_feedback("Applied active preset to selected strokes (unsaved).");
    }

    fn duplicate_selected_note(&mut self, index: usize) {
        let Some(selected) = self.selected_annotation().cloned() else {
            return;
        };
        let Some(document) = self.document.as_ref() else {
            return;
        };
        let geometry = match document.page_geometry(index) {
            Ok(geometry) => geometry,
            Err(error) => {
                self.open_error = Some(format!("Failed to read page geometry: {error}"));
                return;
            }
        };

        let offset = (
            16.0 / geometry.width.max(1.0),
            16.0 / geometry.height.max(1.0),
        );
        let normalized = Self::pdf_rect_to_normalized(selected.rect, geometry);
        let shifted = Self::transform_rect(normalized, (0.0, 0.0), offset, TransformMode::Move);
        let shifted_rect = Self::normalized_to_pdf_rect(shifted, geometry);

        let mut contents = selected.contents.clone();
        if self.text_annotations.iter().any(|entry| {
            entry.page_index == selected.page_index
                && entry.rect == shifted_rect
                && entry.contents == contents
        }) {
            contents.push_str(" (copy)");
        }

        let Some(document) = self.document.as_mut() else {
            return;
        };
        match document.add_text_annotation(
            index,
            NewTextAnnotation {
                rect: shifted_rect,
                contents,
                metadata: selected.metadata,
            },
        ) {
            Ok(created) => {
                self.text_annotation_cache_index = None;
                self.refresh_text_annotations_for_index(index);
                self.selected_annotation_id = Some(created.id);
                self.mark_non_highlight_unsaved();
                self.show_unsaved_edit_feedback("Text annotation duplicated (unsaved).");
            }
            Err(error) => {
                self.open_error = Some(format!("Failed to duplicate annotation: {error}"));
            }
        }
    }

    fn delete_selected_note(&mut self, index: usize) {
        let selected_id = self.selected_annotation_id.clone();
        if let Some(selected_id) = selected_id.as_ref()
            && let Some(document) = self.document.as_mut()
        {
            match document.delete_annotation(selected_id) {
                Ok(()) => {
                    self.text_annotation_cache_index = None;
                    self.refresh_text_annotations_for_index(index);
                    self.selected_annotation_id = None;
                    self.active_text_transform = None;
                    self.mark_non_highlight_unsaved();
                    self.show_unsaved_edit_feedback("Text annotation deleted (unsaved).");
                }
                Err(error) => {
                    self.open_error = Some(format!("Failed to delete annotation: {error}"));
                }
            }
        }
    }

    fn point_segment_distance(point: Pos2, start: Pos2, end: Pos2) -> f32 {
        let vx = end.x - start.x;
        let vy = end.y - start.y;
        let wx = point.x - start.x;
        let wy = point.y - start.y;

        let c1 = vx * wx + vy * wy;
        if c1 <= 0.0 {
            return point.distance(start);
        }
        let c2 = vx * vx + vy * vy;
        if c2 <= 0.000001 {
            return point.distance(start);
        }
        if c2 <= c1 {
            return point.distance(end);
        }
        let t = c1 / c2;
        let projection = Pos2::new(start.x + t * vx, start.y + t * vy);
        point.distance(projection)
    }

    fn stroke_hits_pointer(
        image_rect: Rect,
        region: SlideRegion,
        points: &[(f32, f32)],
        pointer: Pos2,
        radius_px: f32,
    ) -> bool {
        for segment in points.windows(2) {
            let Some(start) = Self::page_normalized_to_screen(
                image_rect,
                region,
                segment[0],
                SlideSurfaceSpace::CroppedSlide,
            ) else {
                continue;
            };
            let Some(end) = Self::page_normalized_to_screen(
                image_rect,
                region,
                segment[1],
                SlideSurfaceSpace::CroppedSlide,
            ) else {
                continue;
            };
            if Self::point_segment_distance(pointer, start, end) <= radius_px {
                return true;
            }
        }
        false
    }

    fn erase_strokes_at_pointer(
        &mut self,
        index: usize,
        image_rect: Rect,
        region: SlideRegion,
        pointer: Pos2,
    ) {
        let candidates: Vec<String> = self
            .unsaved_strokes
            .get(&index)
            .map(|strokes| {
                strokes
                    .iter()
                    .filter(|stroke| !self.eraser_erased_in_drag.contains(&stroke.annotation_id))
                    .filter(|stroke| {
                        Self::stroke_hits_pointer(image_rect, region, &stroke.points, pointer, 12.0)
                    })
                    .map(|stroke| stroke.annotation_id.clone())
                    .collect()
            })
            .unwrap_or_default();
        if candidates.is_empty() {
            return;
        }

        let Some(document) = self.document.as_mut() else {
            return;
        };

        let mut removed_ids = BTreeSet::new();
        for annotation_id in candidates {
            match document.delete_annotation(&annotation_id) {
                Ok(()) => {
                    removed_ids.insert(annotation_id.clone());
                    self.eraser_erased_in_drag.insert(annotation_id);
                }
                Err(error) => {
                    self.open_error = Some(format!("Failed to erase stroke: {error}"));
                }
            }
        }
        if removed_ids.is_empty() {
            return;
        }

        let mut remove_empty_entry = false;
        if let Some(strokes) = self.unsaved_strokes.get_mut(&index) {
            let mut removed_strokes = Vec::new();
            strokes.retain(|stroke| {
                if removed_ids.contains(&stroke.annotation_id) {
                    removed_strokes.push(stroke.clone());
                    false
                } else {
                    true
                }
            });
            if !removed_strokes.is_empty() {
                self.stroke_undo_stack
                    .entry(index)
                    .or_default()
                    .extend(removed_strokes);
                self.eraser_removed_this_drag = true;
            }
            remove_empty_entry = strokes.is_empty();
        }
        if remove_empty_entry {
            self.unsaved_strokes.remove(&index);
        }
    }

    fn handle_current_slide_interaction(
        &mut self,
        response: &egui::Response,
        index: usize,
        layout: Option<NotesLayout>,
    ) {
        if self.interaction_mode() != InteractionMode::HighlightDraw {
            self.stroke_in_progress.clear();
            self.stroke_in_progress_style = None;
            self.lasso_path_in_progress.clear();
            self.selected_stroke_ids.clear();
            self.eraser_erased_in_drag.clear();
            self.eraser_removed_this_drag = false;
            return;
        }

        let region = Self::slide_region(layout);
        match self.active_highlight_tool {
            HighlightTool::Eraser => {
                if response.drag_started() {
                    self.eraser_erased_in_drag.clear();
                    self.eraser_removed_this_drag = false;
                    self.stroke_in_progress.clear();
                    self.stroke_in_progress_style = None;
                    self.lasso_path_in_progress.clear();
                }
                if (response.drag_started() || response.dragged())
                    && let Some(pointer_pos) = response.interact_pointer_pos()
                {
                    self.erase_strokes_at_pointer(index, response.rect, region, pointer_pos);
                }
                if response.drag_stopped() {
                    self.eraser_erased_in_drag.clear();
                    if self.eraser_removed_this_drag {
                        self.show_unsaved_edit_feedback("Eraser removed stroke(s) (unsaved).");
                    }
                    self.eraser_removed_this_drag = false;
                }
            }
            HighlightTool::Lasso => {
                if response.drag_started() {
                    self.lasso_path_in_progress.clear();
                    self.stroke_in_progress.clear();
                    self.stroke_in_progress_style = None;
                }
                if (response.drag_started() || response.dragged())
                    && let Some(pointer_pos) = response.interact_pointer_pos()
                    && let Some(normalized) = Self::screen_to_page_normalized(
                        response.rect,
                        region,
                        pointer_pos,
                        SlideSurfaceSpace::CroppedSlide,
                    )
                {
                    let should_push = self.lasso_path_in_progress.last().is_none_or(|last| {
                        let dx = last.0 - normalized.0;
                        let dy = last.1 - normalized.1;
                        (dx * dx + dy * dy) > 0.00001
                    });
                    if should_push {
                        self.lasso_path_in_progress.push(normalized);
                    }
                }
                if response.drag_stopped() {
                    self.finish_lasso_selection(index);
                }
            }
            HighlightTool::Draw => {
                if response.drag_started() {
                    self.stroke_in_progress.clear();
                    self.lasso_path_in_progress.clear();
                    let preset = self.active_pen_preset();
                    self.stroke_in_progress_style = Some(PenStrokeStyle {
                        color_rgb: preset.color_rgb,
                        width: preset.width,
                        opacity: preset.opacity,
                    });
                }
                if (response.drag_started() || response.dragged())
                    && let Some(pointer_pos) = response.interact_pointer_pos()
                    && let Some(normalized) = Self::screen_to_page_normalized(
                        response.rect,
                        region,
                        pointer_pos,
                        SlideSurfaceSpace::CroppedSlide,
                    )
                {
                    let shift_down = response.ctx.input(|input| input.modifiers.shift);
                    let normalized = if shift_down {
                        self.stroke_in_progress
                            .first()
                            .copied()
                            .map(|anchor| Self::constrain_straight_line(anchor, normalized))
                            .unwrap_or(normalized)
                    } else {
                        normalized
                    };

                    if shift_down && !self.stroke_in_progress.is_empty() {
                        if self.stroke_in_progress.len() == 1 {
                            self.stroke_in_progress.push(normalized);
                        } else {
                            self.stroke_in_progress[1] = normalized;
                            self.stroke_in_progress.truncate(2);
                        }
                        return;
                    }

                    let should_push = self.stroke_in_progress.last().is_none_or(|last| {
                        let dx = last.0 - normalized.0;
                        let dy = last.1 - normalized.1;
                        (dx * dx + dy * dy) > 0.000005
                    });
                    if should_push {
                        self.stroke_in_progress.push(normalized);
                    }
                }
                if response.drag_stopped() {
                    self.finalize_stroke(index);
                    self.eraser_erased_in_drag.clear();
                    self.eraser_removed_this_drag = false;
                }
            }
        }
    }

    fn pdf_rect_to_normalized(rect: PdfRect, geometry: PageGeometry) -> PdfRect {
        PdfRect {
            left: (rect.left / geometry.width.max(1.0)).clamp(0.0, 1.0),
            bottom: (rect.bottom / geometry.height.max(1.0)).clamp(0.0, 1.0),
            right: (rect.right / geometry.width.max(1.0)).clamp(0.0, 1.0),
            top: (rect.top / geometry.height.max(1.0)).clamp(0.0, 1.0),
        }
        .normalized()
    }

    fn normalized_to_pdf_rect(rect: PdfRect, geometry: PageGeometry) -> PdfRect {
        PdfRect {
            left: rect.left.clamp(0.0, 1.0) * geometry.width,
            bottom: rect.bottom.clamp(0.0, 1.0) * geometry.height,
            right: rect.right.clamp(0.0, 1.0) * geometry.width,
            top: rect.top.clamp(0.0, 1.0) * geometry.height,
        }
        .normalized()
    }

    fn clamp_normalized_rect(rect: PdfRect) -> PdfRect {
        let mut rect = rect.normalized();
        let min_size = 0.02_f32;
        rect.left = rect.left.clamp(0.0, 1.0);
        rect.right = rect.right.clamp(0.0, 1.0);
        rect.bottom = rect.bottom.clamp(0.0, 1.0);
        rect.top = rect.top.clamp(0.0, 1.0);

        if rect.right - rect.left < min_size {
            let center = (rect.left + rect.right) * 0.5;
            rect.left = (center - min_size * 0.5).clamp(0.0, 1.0 - min_size);
            rect.right = (rect.left + min_size).clamp(min_size, 1.0);
        }
        if rect.top - rect.bottom < min_size {
            let center = (rect.bottom + rect.top) * 0.5;
            rect.bottom = (center - min_size * 0.5).clamp(0.0, 1.0 - min_size);
            rect.top = (rect.bottom + min_size).clamp(min_size, 1.0);
        }
        rect
    }

    fn transform_rect(
        start_rect: PdfRect,
        start_pointer: (f32, f32),
        current_pointer: (f32, f32),
        mode: TransformMode,
    ) -> PdfRect {
        let dx = current_pointer.0 - start_pointer.0;
        let dy = current_pointer.1 - start_pointer.1;
        let mut rect = start_rect;
        match mode {
            TransformMode::Move => {
                rect.left += dx;
                rect.right += dx;
                rect.bottom += dy;
                rect.top += dy;

                let width = rect.right - rect.left;
                let height = rect.top - rect.bottom;
                if rect.left < 0.0 {
                    rect.left = 0.0;
                    rect.right = width;
                }
                if rect.right > 1.0 {
                    rect.right = 1.0;
                    rect.left = 1.0 - width;
                }
                if rect.bottom < 0.0 {
                    rect.bottom = 0.0;
                    rect.top = height;
                }
                if rect.top > 1.0 {
                    rect.top = 1.0;
                    rect.bottom = 1.0 - height;
                }
            }
            TransformMode::Resize(handle) => match handle {
                ResizeHandle::NorthWest => {
                    rect.left += dx;
                    rect.top += dy;
                }
                ResizeHandle::NorthEast => {
                    rect.right += dx;
                    rect.top += dy;
                }
                ResizeHandle::SouthWest => {
                    rect.left += dx;
                    rect.bottom += dy;
                }
                ResizeHandle::SouthEast => {
                    rect.right += dx;
                    rect.bottom += dy;
                }
            },
        }
        Self::clamp_normalized_rect(rect)
    }

    fn annotation_screen_rect(
        image_rect: Rect,
        region: SlideRegion,
        annotation: &TextAnnotation,
        geometry: PageGeometry,
    ) -> Option<Rect> {
        let normalized = Self::pdf_rect_to_normalized(annotation.rect, geometry);
        let lower_left = Self::page_normalized_to_screen(
            image_rect,
            region,
            (normalized.left, normalized.bottom),
            SlideSurfaceSpace::CroppedSlide,
        )?;
        let upper_right = Self::page_normalized_to_screen(
            image_rect,
            region,
            (normalized.right, normalized.top),
            SlideSurfaceSpace::CroppedSlide,
        )?;
        Some(Rect::from_min_max(
            Pos2::new(
                lower_left.x.min(upper_right.x),
                lower_left.y.min(upper_right.y),
            ),
            Pos2::new(
                lower_left.x.max(upper_right.x),
                lower_left.y.max(upper_right.y),
            ),
        ))
    }

    fn detect_resize_handle(screen_rect: Rect, pointer: Pos2) -> Option<ResizeHandle> {
        let threshold = 10.0_f32;
        let corners = [
            (ResizeHandle::NorthWest, screen_rect.left_top()),
            (ResizeHandle::NorthEast, screen_rect.right_top()),
            (ResizeHandle::SouthWest, screen_rect.left_bottom()),
            (ResizeHandle::SouthEast, screen_rect.right_bottom()),
        ];
        corners
            .into_iter()
            .find_map(|(handle, pos)| (pos.distance(pointer) <= threshold).then_some(handle))
    }

    fn selected_annotation_by_id<'a>(
        selected_id: Option<&str>,
        annotations: &'a [TextAnnotation],
    ) -> Option<&'a TextAnnotation> {
        let selected_id = selected_id?;
        annotations.iter().find(|entry| entry.id == selected_id)
    }

    fn selected_transform_dirty_for(
        selected_id: Option<&str>,
        annotations: &[TextAnnotation],
        originals: &BTreeMap<String, TextAnnotation>,
    ) -> bool {
        let Some(selected) = Self::selected_annotation_by_id(selected_id, annotations) else {
            return false;
        };
        let Some(original) = originals.get(&selected.id) else {
            return false;
        };
        selected.rect != original.rect
    }

    fn selected_metadata_dirty_for(
        selected_id: Option<&str>,
        annotations: &[TextAnnotation],
        originals: &BTreeMap<String, TextAnnotation>,
    ) -> bool {
        let Some(selected) = Self::selected_annotation_by_id(selected_id, annotations) else {
            return false;
        };
        let Some(original) = originals.get(&selected.id) else {
            return false;
        };
        selected.contents != original.contents || selected.metadata != original.metadata
    }

    fn revert_transform_for_selected(
        selected_id: Option<&str>,
        annotations: &mut [TextAnnotation],
        originals: &BTreeMap<String, TextAnnotation>,
    ) -> bool {
        let Some(selected_id) = selected_id else {
            return false;
        };
        let Some(original) = originals.get(selected_id) else {
            return false;
        };
        let Some(selected) = annotations.iter_mut().find(|entry| entry.id == selected_id) else {
            return false;
        };
        selected.rect = original.rect;
        true
    }

    fn selected_annotation(&self) -> Option<&TextAnnotation> {
        Self::selected_annotation_by_id(
            self.selected_annotation_id.as_deref(),
            &self.text_annotations,
        )
    }

    fn revert_selected_transform(&mut self) {
        if Self::revert_transform_for_selected(
            self.selected_annotation_id.as_deref(),
            &mut self.text_annotations,
            &self.original_text_annotations,
        ) {
            self.active_text_transform = None;
            self.show_unsaved_edit_feedback("Annotation transform reverted.");
        }
    }

    fn selected_transform_dirty(&self) -> bool {
        Self::selected_transform_dirty_for(
            self.selected_annotation_id.as_deref(),
            &self.text_annotations,
            &self.original_text_annotations,
        )
    }

    fn selected_metadata_dirty(&self) -> bool {
        Self::selected_metadata_dirty_for(
            self.selected_annotation_id.as_deref(),
            &self.text_annotations,
            &self.original_text_annotations,
        )
    }

    fn commit_selected_annotation_update(&mut self, status_text: &str) {
        let Some(selected_id) = self.selected_annotation_id.clone() else {
            return;
        };
        let Some(annotation) = self
            .text_annotations
            .iter()
            .find(|entry| entry.id == selected_id)
            .cloned()
        else {
            return;
        };
        let Some(document) = self.document.as_mut() else {
            return;
        };

        match document.update_text_annotation(&annotation) {
            Ok(()) => {
                let annotation_id = annotation.id.clone();
                let page_index = annotation.page_index;
                self.original_text_annotations
                    .insert(annotation_id.clone(), annotation);
                self.selected_annotation_id = Some(annotation_id);
                self.text_annotation_cache_index = None;
                self.refresh_text_annotations_for_index(page_index);
                self.active_text_transform = None;
                self.mark_non_highlight_unsaved();
                self.show_unsaved_edit_feedback(status_text);
            }
            Err(error) => {
                self.open_error = Some(format!("Failed to update annotation: {error}"));
            }
        }
    }

    fn parse_tags(raw: &str) -> Vec<String> {
        let mut tags = raw
            .split(',')
            .map(str::trim)
            .map(ToOwned::to_owned)
            .filter(|tag| !tag.is_empty())
            .collect::<Vec<_>>();
        tags.sort();
        tags.dedup();
        tags
    }

    fn handle_annotation_interaction(
        &mut self,
        response: &egui::Response,
        index: usize,
        layout: Option<NotesLayout>,
    ) {
        if self.interaction_mode() != InteractionMode::AnnotationEdit {
            self.active_text_transform = None;
            return;
        }

        let Some(document) = self.document.as_ref() else {
            return;
        };
        let Ok(geometry) = document.page_geometry(index) else {
            return;
        };
        let region = Self::slide_region(layout);

        if response.clicked()
            && let Some(pointer) = response.interact_pointer_pos()
        {
            let mut selected = None;
            for annotation in self.text_annotations.iter().rev() {
                if let Some(rect) =
                    Self::annotation_screen_rect(response.rect, region, annotation, geometry)
                    && rect.contains(pointer)
                {
                    selected = Some(annotation.id.clone());
                    break;
                }
            }
            self.selected_annotation_id = selected;
        }

        let pointer_pos = response
            .interact_pointer_pos()
            .or_else(|| response.ctx.input(|input| input.pointer.interact_pos()));
        let primary_down = response.ctx.input(|input| input.pointer.primary_down());
        let pointer_on_surface = pointer_pos.is_some_and(|pointer| response.rect.contains(pointer));

        if self.active_text_transform.is_none()
            && primary_down
            && pointer_on_surface
            && let Some(pointer) = pointer_pos
        {
            let mut picked: Option<(String, TransformMode, PdfRect)> = None;
            let selected_first = self.selected_annotation().into_iter();
            for annotation in selected_first.chain(self.text_annotations.iter().rev()) {
                let Some(rect) =
                    Self::annotation_screen_rect(response.rect, region, annotation, geometry)
                else {
                    continue;
                };
                if let Some(handle) = Self::detect_resize_handle(rect, pointer) {
                    picked = Some((
                        annotation.id.clone(),
                        TransformMode::Resize(handle),
                        Self::pdf_rect_to_normalized(annotation.rect, geometry),
                    ));
                    break;
                }
                if rect.contains(pointer) {
                    picked = Some((
                        annotation.id.clone(),
                        TransformMode::Move,
                        Self::pdf_rect_to_normalized(annotation.rect, geometry),
                    ));
                    break;
                }
            }

            if let Some((annotation_id, mode, start_rect)) = picked
                && let Some(start_pointer) = Self::screen_to_page_normalized(
                    response.rect,
                    region,
                    pointer,
                    SlideSurfaceSpace::CroppedSlide,
                )
            {
                self.selected_annotation_id = Some(annotation_id.clone());
                self.active_text_transform = Some(ActiveTextTransform {
                    annotation_id,
                    start_pointer,
                    start_rect,
                    mode,
                });
            }
        }

        if primary_down
            && let Some(pointer) = pointer_pos
            && let Some(active) = self.active_text_transform.as_ref()
            && let Some(current_pointer) = Self::screen_to_page_normalized(
                response.rect,
                region,
                pointer,
                SlideSurfaceSpace::CroppedSlide,
            )
        {
            let updated_normalized = Self::transform_rect(
                active.start_rect,
                active.start_pointer,
                current_pointer,
                active.mode,
            );
            let updated_pdf = Self::normalized_to_pdf_rect(updated_normalized, geometry);
            if let Some(annotation) = self
                .text_annotations
                .iter_mut()
                .find(|entry| entry.id == active.annotation_id)
            {
                annotation.rect = updated_pdf;
            }
        }

        if !primary_down {
            self.active_text_transform = None;
        }
    }

    fn highlight_overlay_snapshot_for_index(&self, index: usize) -> HighlightOverlaySnapshot {
        let stroke_in_progress_style = self.stroke_in_progress_style.or_else(|| {
            if self.active_highlight_tool != HighlightTool::Draw {
                None
            } else {
                let preset = self.active_pen_preset();
                Some(PenStrokeStyle {
                    color_rgb: preset.color_rgb,
                    width: preset.width,
                    opacity: preset.opacity,
                })
            }
        });

        HighlightOverlaySnapshot {
            strokes: self
                .unsaved_strokes
                .get(&index)
                .cloned()
                .unwrap_or_default(),
            selected_stroke_ids: self.selected_stroke_ids.clone(),
            stroke_in_progress: self.stroke_in_progress.clone(),
            stroke_in_progress_style,
            active_highlight_tool: self.active_highlight_tool,
            lasso_path_in_progress: self.lasso_path_in_progress.clone(),
        }
    }

    fn draw_highlight_overlay_from_snapshot(
        ui: &egui::Ui,
        image_rect: Rect,
        region: SlideRegion,
        surface_space: SlideSurfaceSpace,
        overlay: &HighlightOverlaySnapshot,
        show_selected_strokes: bool,
        show_lasso: bool,
    ) {
        let painter = ui.painter_at(image_rect);

        for stroke in &overlay.strokes {
            let stroke_style = Stroke::new(
                stroke.width,
                Self::color32_from_rgb_opacity(stroke.color_rgb, stroke.opacity),
            );
            for segment in stroke.points.windows(2) {
                let Some(start) =
                    Self::page_normalized_to_screen(image_rect, region, segment[0], surface_space)
                else {
                    continue;
                };
                let Some(end) =
                    Self::page_normalized_to_screen(image_rect, region, segment[1], surface_space)
                else {
                    continue;
                };
                painter.line_segment([start, end], stroke_style);
            }

            if show_selected_strokes && overlay.selected_stroke_ids.contains(&stroke.annotation_id)
            {
                let selected_style = Stroke::new(4.0, Color32::from_rgb(90, 220, 255));
                for segment in stroke.points.windows(2) {
                    let Some(start) = Self::page_normalized_to_screen(
                        image_rect,
                        region,
                        segment[0],
                        surface_space,
                    ) else {
                        continue;
                    };
                    let Some(end) = Self::page_normalized_to_screen(
                        image_rect,
                        region,
                        segment[1],
                        surface_space,
                    ) else {
                        continue;
                    };
                    painter.line_segment([start, end], selected_style);
                }
            }
        }

        let in_progress_style = overlay.stroke_in_progress_style.map(|style| {
            Stroke::new(
                style.width,
                Self::color32_from_rgb_opacity(style.color_rgb, style.opacity),
            )
        });
        for segment in overlay.stroke_in_progress.windows(2) {
            let Some(stroke_style) = in_progress_style else {
                break;
            };
            let Some(start) =
                Self::page_normalized_to_screen(image_rect, region, segment[0], surface_space)
            else {
                continue;
            };
            let Some(end) =
                Self::page_normalized_to_screen(image_rect, region, segment[1], surface_space)
            else {
                continue;
            };
            painter.line_segment([start, end], stroke_style);
        }

        if show_lasso
            && overlay.active_highlight_tool == HighlightTool::Lasso
            && overlay.lasso_path_in_progress.len() >= 2
        {
            let lasso_style = Stroke::new(2.0, Color32::from_rgb(120, 220, 255));
            for segment in overlay.lasso_path_in_progress.windows(2) {
                let Some(start) =
                    Self::page_normalized_to_screen(image_rect, region, segment[0], surface_space)
                else {
                    continue;
                };
                let Some(end) =
                    Self::page_normalized_to_screen(image_rect, region, segment[1], surface_space)
                else {
                    continue;
                };
                painter.line_segment([start, end], lasso_style);
            }
            if let (Some(first), Some(last)) = (
                overlay.lasso_path_in_progress.first().copied(),
                overlay.lasso_path_in_progress.last().copied(),
            ) && let (Some(first_screen), Some(last_screen)) = (
                Self::page_normalized_to_screen(image_rect, region, first, surface_space),
                Self::page_normalized_to_screen(image_rect, region, last, surface_space),
            ) {
                painter.line_segment([last_screen, first_screen], lasso_style);
            }
        }
    }

    fn draw_annotation_overlays(
        &self,
        ui: &egui::Ui,
        image_rect: Rect,
        index: usize,
        layout: Option<NotesLayout>,
    ) {
        let region = Self::slide_region(layout);
        let overlay = self.highlight_overlay_snapshot_for_index(index);
        Self::draw_highlight_overlay_from_snapshot(
            ui,
            image_rect,
            region,
            SlideSurfaceSpace::CroppedSlide,
            &overlay,
            true,
            true,
        );

        let painter = ui.painter_at(image_rect);

        if self.interaction_mode() != InteractionMode::AnnotationEdit {
            return;
        }
        let Some(document) = self.document.as_ref() else {
            return;
        };
        let Ok(geometry) = document.page_geometry(index) else {
            return;
        };

        for annotation in &self.text_annotations {
            let Some(rect) = Self::annotation_screen_rect(image_rect, region, annotation, geometry)
            else {
                continue;
            };
            let selected = self
                .selected_annotation_id
                .as_ref()
                .is_some_and(|id| id == &annotation.id);
            let color = if selected {
                Color32::from_rgb(120, 220, 255)
            } else {
                Color32::from_rgba_unmultiplied(180, 210, 235, 180)
            };
            painter.rect_stroke(
                rect,
                2.0,
                Stroke::new(2.0, color),
                egui::StrokeKind::Outside,
            );
            if selected {
                let handle_color = Color32::from_rgb(120, 220, 255);
                let handles = [
                    rect.left_top(),
                    rect.right_top(),
                    rect.left_bottom(),
                    rect.right_bottom(),
                ];
                for pos in handles {
                    let handle_rect = Rect::from_center_size(pos, Vec2::new(8.0, 8.0));
                    painter.rect_filled(handle_rect, 1.0, handle_color);
                }
            }
        }
    }

    fn draw_badge(ui: &egui::Ui, rect: Rect, text: &str, fill: Color32, text_color: Color32) {
        ui.painter().rect_filled(rect, 6.0, fill);
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            text,
            FontId::new(11.0, FontFamily::Proportional),
            text_color,
        );
    }

    fn draw_unsaved_badge_on_preview(&self, ui: &egui::Ui, image_rect: Rect) {
        if !self.has_unsaved_annotation_changes() {
            return;
        }
        let badge_rect = Rect::from_min_size(
            image_rect.min + Vec2::new(10.0, 10.0),
            Vec2::new(78.0, 24.0),
        );
        Self::draw_badge(
            ui,
            badge_rect,
            "UNSAVED",
            Color32::from_rgba_unmultiplied(120, 26, 26, 220),
            Color32::from_rgb(255, 220, 220),
        );
    }

    fn render_pen_toolkit_controls(&mut self, ui: &mut egui::Ui, compact: bool, index: usize) {
        let presets = self.config.pen_toolkit.presets.clone();
        let active_index = self.config.pen_toolkit.active_preset_index;
        let draw_selected = self.active_highlight_tool == HighlightTool::Draw;
        let lasso_selected = self.active_highlight_tool == HighlightTool::Lasso;
        let eraser_selected = self.active_highlight_tool == HighlightTool::Eraser;

        ui.horizontal_wrapped(|ui| {
            if !compact {
                ui.label("Tool:");
            }
            if ui
                .selectable_label(draw_selected, if compact { "Draw" } else { "Draw (H)" })
                .clicked()
            {
                self.set_highlight_tool(HighlightTool::Draw);
            }
            if ui
                .selectable_label(lasso_selected, if compact { "Lasso" } else { "Lasso (M)" })
                .clicked()
            {
                self.toggle_lasso_mode();
            }
            if ui
                .selectable_label(
                    eraser_selected,
                    if compact { "Eraser" } else { "Eraser (E)" },
                )
                .clicked()
            {
                self.toggle_eraser_mode();
            }
        });

        ui.horizontal_wrapped(|ui| {
            if !compact {
                ui.label("Pen:");
            }
            for (preset_index, preset) in presets.iter().enumerate() {
                let selected = preset_index == active_index;
                let button_text = if compact {
                    format!(
                        "{}{}",
                        preset_index + 1,
                        preset.name.chars().next().unwrap_or('P')
                    )
                } else {
                    preset.name.clone()
                };
                if ui.selectable_label(selected, button_text).clicked() {
                    self.set_active_pen_preset(preset_index);
                }
            }
        });

        let mut preset = self.active_pen_preset();
        let style_color = Self::color32_from_rgb_opacity(preset.color_rgb, preset.opacity);
        ui.horizontal(|ui| {
            ui.colored_label(style_color, "Sample");
            ui.label(format!(
                "{}  w:{:.1}  op:{:.2}",
                preset.name, preset.width, preset.opacity
            ));
            if !self.selected_stroke_ids.is_empty() {
                ui.label(format!("selected: {}", self.selected_stroke_ids.len()));
            }
        });

        let color_swatches: [(&str, (f32, f32, f32)); 8] = [
            ("Yellow", (1.0, 0.92, 0.15)),
            ("Red", (0.95, 0.30, 0.25)),
            ("Blue", (0.30, 0.60, 1.0)),
            ("Green", (0.20, 0.82, 0.40)),
            ("Orange", (0.98, 0.58, 0.18)),
            ("Purple", (0.66, 0.44, 0.95)),
            ("White", (0.96, 0.96, 0.96)),
            ("Black", (0.10, 0.10, 0.10)),
        ];
        ui.horizontal_wrapped(|ui| {
            ui.label(if compact { "Clr:" } else { "Color:" });
            for (name, rgb) in color_swatches {
                let selected = (preset.color_rgb.0 - rgb.0).abs() < 0.01
                    && (preset.color_rgb.1 - rgb.1).abs() < 0.01
                    && (preset.color_rgb.2 - rgb.2).abs() < 0.01;
                let stroke = if selected {
                    Stroke::new(2.0, Color32::WHITE)
                } else {
                    Stroke::new(1.0, Color32::from_gray(30))
                };
                let response = ui
                    .add(
                        egui::Button::new("")
                            .fill(Self::color32_from_rgb_opacity(rgb, 1.0))
                            .stroke(stroke)
                            .min_size(Vec2::new(16.0, 16.0)),
                    )
                    .on_hover_text(name);
                if response.clicked() {
                    self.update_active_pen_preset_style(Some(rgb), None);
                    preset = self.active_pen_preset();
                }
            }

            let mut rgb = [preset.color_rgb.0, preset.color_rgb.1, preset.color_rgb.2];
            if egui::color_picker::color_edit_button_rgb(ui, &mut rgb).changed() {
                self.update_active_pen_preset_style(Some((rgb[0], rgb[1], rgb[2])), None);
                preset = self.active_pen_preset();
            }
        });

        let quick_widths = [2.0_f32, 4.0_f32, 6.0_f32];
        let mut width = preset.width;
        ui.horizontal_wrapped(|ui| {
            ui.label(if compact { "W:" } else { "Thickness:" });
            for quick in quick_widths {
                let selected = (width - quick).abs() < 0.05;
                if ui
                    .selectable_label(
                        selected,
                        if compact {
                            format!("{quick:.0}")
                        } else {
                            format!("{quick:.0}px")
                        },
                    )
                    .clicked()
                {
                    width = quick;
                    self.update_active_pen_preset_style(None, Some(width));
                }
            }
            let slider = egui::Slider::new(&mut width, 0.5..=24.0).text(if compact {
                "w"
            } else {
                "Stroke"
            });
            if ui.add(slider).changed() {
                self.update_active_pen_preset_style(None, Some(width));
            }
        });

        if !self.selected_stroke_ids.is_empty() {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button(if compact {
                        "Preset->Sel"
                    } else {
                        "Apply Preset (K)"
                    })
                    .clicked()
                {
                    self.apply_active_preset_to_selected_strokes(index);
                }
                if ui
                    .button(if compact {
                        "Delete Sel"
                    } else {
                        "Delete Selected (Del)"
                    })
                    .clicked()
                {
                    self.delete_selected_strokes(index);
                }
                if ui
                    .button(if compact {
                        "Clear Sel"
                    } else {
                        "Clear Selection"
                    })
                    .clicked()
                {
                    self.clear_selected_strokes_selection();
                }
            });
        }
    }

    fn render_quick_actions_overlay(&mut self, ui: &mut egui::Ui, image_rect: Rect, index: usize) {
        let mode = self.interaction_mode();
        if mode == InteractionMode::Navigate {
            return;
        }

        let has_unsaved_strokes = self
            .unsaved_strokes
            .get(&index)
            .is_some_and(|entries| !entries.is_empty());
        let has_redo = self
            .stroke_undo_stack
            .get(&index)
            .is_some_and(|entries| !entries.is_empty());
        let has_current_stroke = !self.stroke_in_progress.is_empty();
        let selected_exists = self.selected_annotation().is_some();
        let transform_dirty = self.selected_transform_dirty();
        let note_actions_enabled = mode == InteractionMode::AnnotationEdit && selected_exists;
        let can_save = self.has_unsaved_annotation_changes();

        let overlay_id = egui::Id::new("quick_actions_overlay");
        let overlay_width = (image_rect.width() - 20.0).clamp(220.0, 560.0);
        let overlay_offset = Vec2::new(16.0, -16.0);
        egui::Area::new(overlay_id)
            .order(egui::Order::Foreground)
            .anchor(Align2::LEFT_BOTTOM, overlay_offset)
            .show(ui.ctx(), |ui| {
                ui.set_width(overlay_width);
                ui.set_max_width(overlay_width);
                let frame = egui::Frame::new()
                    .fill(Color32::from_rgba_unmultiplied(18, 22, 30, 218))
                    .stroke(Stroke::new(
                        1.0,
                        Color32::from_rgba_unmultiplied(90, 100, 120, 220),
                    ))
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::same(6));
                frame.show(ui, |ui| {
                    self.render_pen_toolkit_controls(ui, true, index);
                    ui.separator();
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .add_enabled(has_unsaved_strokes, egui::Button::new("Undo"))
                            .clicked()
                        {
                            self.undo_last_stroke(index);
                        }
                        if ui
                            .add_enabled(has_redo, egui::Button::new("Redo"))
                            .clicked()
                        {
                            self.redo_last_stroke(index);
                        }
                        if ui
                            .add_enabled(has_current_stroke, egui::Button::new("Clear Current"))
                            .clicked()
                        {
                            self.clear_current_stroke();
                        }
                        if ui
                            .add_enabled(has_unsaved_strokes, egui::Button::new("Clear Slide"))
                            .clicked()
                        {
                            self.clear_slide_highlights(index);
                        }
                        if ui
                            .add_enabled(can_save, egui::Button::new("Save Now"))
                            .clicked()
                        {
                            self.save_document_in_place();
                        }
                        if ui
                            .add_enabled(note_actions_enabled, egui::Button::new("Duplicate"))
                            .clicked()
                        {
                            self.duplicate_selected_note(index);
                        }
                        if ui
                            .add_enabled(note_actions_enabled, egui::Button::new("Delete"))
                            .clicked()
                        {
                            self.delete_selected_note(index);
                        }
                        if ui
                            .add_enabled(
                                mode == InteractionMode::AnnotationEdit && transform_dirty,
                                egui::Button::new("Apply"),
                            )
                            .clicked()
                        {
                            self.commit_selected_annotation_update(
                                "Annotation transform applied (unsaved).",
                            );
                        }
                        if ui
                            .add_enabled(
                                mode == InteractionMode::AnnotationEdit && transform_dirty,
                                egui::Button::new("Revert"),
                            )
                            .clicked()
                        {
                            self.revert_selected_transform();
                        }
                    });
                });
            });
    }

    fn submit_go_to(&mut self) {
        let raw = self.go_to_input.trim();
        if raw.is_empty() {
            return;
        }

        if self.state.use_slide_labels() {
            self.apply_command(AppCommand::GoToLabel(raw.to_owned()));
            return;
        }

        if let Ok(page_number) = raw.parse::<usize>()
            && page_number >= 1
        {
            self.apply_command(AppCommand::GoToIndex(page_number - 1));
            return;
        }

        if let Some(document) = self.document.as_ref()
            && let Some(index) = document.resolve_label(raw)
        {
            self.apply_command(AppCommand::GoToIndex(index));
            return;
        }

        self.open_error = Some(format!("Unable to go to slide: {raw}"));
    }

    fn refresh_document_if_changed(&mut self) {
        if self.last_reload_check.elapsed() < Duration::from_secs(2) {
            return;
        }

        self.last_reload_check = Instant::now();
        if self.has_unsaved_annotation_changes() {
            return;
        }
        let Some(document) = self.document.as_mut() else {
            return;
        };

        match document.reload_if_changed() {
            Ok(true) => {
                let current = self.state.current_index();
                let auto_enabled = self.state.auto_advance_enabled();
                let (global_interval, overrides) = self.state.auto_advance_profile();
                let labels = Self::labels_from_backend(document.as_ref());
                self.state
                    .update_document(document.page_count(), labels, Instant::now());
                self.state
                    .set_auto_advance_profile(global_interval, overrides, Instant::now());
                self.state.apply(
                    AppCommand::SetAutoAdvanceEnabled(auto_enabled),
                    Instant::now(),
                );
                let capped = current.min(self.state.total_slides() - 1);
                self.apply_command(AppCommand::GoToIndex(capped));
                self.unsaved_strokes.clear();
                self.stroke_undo_stack.clear();
                self.stroke_in_progress.clear();
                self.stroke_in_progress_style = None;
                self.eraser_erased_in_drag.clear();
                self.eraser_removed_this_drag = false;
                self.non_highlight_unsaved_changes = false;
                self.invalidate_render_cache();
                self.status_message = Some("PDF reloaded after file change.".to_owned());
            }
            Ok(false) => {}
            Err(error) => {
                self.open_error = Some(format!("Failed to auto-reload document: {error}"));
            }
        }
    }

    fn persist_config(&mut self) {
        if let Err(error) = self.config.save_to_path(&self.config_path) {
            self.open_error = Some(format!("Failed to save config: {error}"));
        }
    }

    fn invalidate_render_cache(&mut self) {
        self.page_cache = PageCache::new(self.config.cache_size);
        self.projector_texture_key = None;
        self.current_texture_key = None;
        self.presenter_texture_key = None;
        self.notes_texture_key = None;
        self.next_texture_key = None;
        self.overview_textures.clear();
        self.overview_texture_keys.clear();
        self.reset_layout_resolution();
        self.text_annotation_cache_index = None;
        self.original_text_annotations.clear();
        self.active_text_transform = None;
    }

    fn mode_id(mode: NotesMode) -> usize {
        match mode {
            NotesMode::Auto => 0,
            NotesMode::PdfAnnotations => 1,
            NotesMode::BeamerSecondScreen => 2,
            NotesMode::LibreOfficeNotes => 3,
        }
    }

    fn view_id(view: PageView) -> usize {
        match view {
            PageView::Full => 0,
            PageView::SlideOnly => 1,
            PageView::NotesOnly => 2,
        }
    }

    fn axis_id(axis: SplitAxis) -> usize {
        match axis {
            SplitAxis::Vertical => 1,
            SplitAxis::Horizontal => 2,
        }
    }

    fn side_id(side: NotesSide) -> usize {
        match side {
            NotesSide::Left => 1,
            NotesSide::Right => 2,
            NotesSide::Top => 3,
            NotesSide::Bottom => 4,
        }
    }

    fn layout_cache_id(layout: Option<NotesLayout>) -> usize {
        let Some(layout) = layout else {
            return 0;
        };

        let ratio = (layout.split_ratio.clamp(0.0, 1.0) * 1000.0).round() as usize;
        let confidence = (layout.confidence.clamp(0.0, 1.0) * 1000.0).round() as usize;
        let mut key = 17usize;
        key = key
            .wrapping_mul(131)
            .wrapping_add(Self::axis_id(layout.axis));
        key = key
            .wrapping_mul(131)
            .wrapping_add(Self::side_id(layout.notes_side));
        key = key.wrapping_mul(131).wrapping_add(ratio);
        key = key.wrapping_mul(131).wrapping_add(confidence);
        key
    }

    fn layout_resolution_key(&self, index: usize) -> usize {
        let mut key = 19usize;
        key = key.wrapping_mul(131).wrapping_add(index);
        key = key
            .wrapping_mul(131)
            .wrapping_add(Self::mode_id(self.config.notes_mode));
        key = key.wrapping_mul(131).wrapping_add(
            (self.config.notes_confidence_threshold.clamp(0.05, 0.95) * 1000.0).round() as usize,
        );
        key = key
            .wrapping_mul(131)
            .wrapping_add(Self::layout_cache_id(self.active_override_layout()));
        key
    }

    fn bitmap_cache_key(
        index: usize,
        mode: NotesMode,
        view: PageView,
        layout: Option<NotesLayout>,
        target_width: usize,
    ) -> usize {
        let mut key = 23usize;
        key = key.wrapping_mul(131).wrapping_add(index);
        key = key.wrapping_mul(131).wrapping_add(Self::mode_id(mode));
        key = key.wrapping_mul(131).wrapping_add(Self::view_id(view));
        key = key
            .wrapping_mul(131)
            .wrapping_add(Self::layout_cache_id(layout));
        key = key
            .wrapping_mul(131)
            .wrapping_add(target_width.clamp(120, 4096));
        key
    }

    fn get_or_render_bitmap(
        &mut self,
        index: usize,
        view: PageView,
        layout: Option<NotesLayout>,
        target_width: usize,
    ) -> Result<(usize, PageBitmap)> {
        let key = Self::bitmap_cache_key(index, self.config.notes_mode, view, layout, target_width);
        if let Some(bitmap) = self.page_cache.get(key) {
            return Ok((key, bitmap.clone()));
        }

        let document = self
            .document
            .as_ref()
            .ok_or_else(|| anyhow!("no document loaded"))?;
        let bitmap = document.render_page_bitmap(index, view, layout, target_width)?;
        self.page_cache.put(key, bitmap.clone());

        Ok((key, bitmap))
    }

    fn color_image(bitmap: &PageBitmap) -> egui::ColorImage {
        egui::ColorImage::from_rgba_unmultiplied([bitmap.width, bitmap.height], &bitmap.rgba)
    }

    fn resolve_layout_for_slide(&mut self, index: usize) -> Option<NotesLayout> {
        let resolution_key = self.layout_resolution_key(index);
        if self.resolved_layout_key == Some(resolution_key) {
            return self.resolved_layout;
        }

        self.resolved_layout_key = Some(resolution_key);
        self.layout_warning = None;

        if let Some(layout) = self.active_override_layout() {
            self.resolved_layout = Some(layout);
            self.layout_reason = Some("Manual layout override is active.".to_owned());
            return self.resolved_layout;
        }

        let Some(document) = self.document.as_ref() else {
            self.resolved_layout = None;
            self.layout_reason = Some("No document loaded.".to_owned());
            return None;
        };

        match document.detect_notes_layout(index, self.config.notes_mode) {
            Ok(result) => {
                self.layout_warning = result.warning.clone();
                self.layout_reason = Some(result.reason.clone());
                let resolved = if self.config.notes_mode == NotesMode::Auto {
                    match result.layout {
                        Some(layout)
                            if result.confidence >= self.config.notes_confidence_threshold =>
                        {
                            Some(layout)
                        }
                        _ => {
                            self.layout_warning = Some(format!(
                                "Auto split confidence {:.2} below threshold {:.2}; using full page.",
                                result.confidence, self.config.notes_confidence_threshold
                            ));
                            None
                        }
                    }
                } else {
                    result.layout
                };
                self.resolved_layout = resolved;
                resolved
            }
            Err(error) => {
                self.layout_warning = Some(format!("Layout detection failed: {error}"));
                self.layout_reason = Some("Falling back to full-page rendering.".to_owned());
                self.resolved_layout = None;
                None
            }
        }
    }

    fn refresh_textures(&mut self, ctx: &Context) {
        let Some(_) = self.document else {
            return;
        };
        let projector_index = self.state.current_index();
        let presenter_index = if self.deck_overview.is_open {
            self.deck_overview
                .selected_index
                .min(self.state.total_slides().saturating_sub(1))
        } else {
            projector_index
        };
        let next_index = (presenter_index + 1).min(self.state.total_slides().saturating_sub(1));
        let projector_layout = self.resolve_layout_for_slide(projector_index);
        let presenter_layout = self.resolve_layout_for_slide(presenter_index);
        let next_layout = self.resolve_layout_for_slide(next_index);

        if Self::has_visible_region(projector_layout, PageView::SlideOnly) {
            match self.get_or_render_bitmap(
                projector_index,
                PageView::SlideOnly,
                projector_layout,
                1920,
            ) {
                Ok((key, bitmap)) => {
                    if self.projector_texture_key != Some(key) {
                        let image = Self::color_image(&bitmap);
                        if let Some(texture) = self.projector_texture.as_mut() {
                            texture.set(image, TextureOptions::LINEAR);
                        } else {
                            self.projector_texture = Some(ctx.load_texture(
                                "projector_page",
                                image,
                                TextureOptions::LINEAR,
                            ));
                        }
                        self.projector_texture_key = Some(key);
                    }
                }
                Err(error) => {
                    self.open_error = Some(format!("Projector render failed: {error}"));
                }
            }
        } else {
            self.projector_texture = None;
            self.projector_texture_key = None;
        }

        if Self::has_visible_region(presenter_layout, PageView::SlideOnly) {
            match self.get_or_render_bitmap(
                presenter_index,
                PageView::SlideOnly,
                presenter_layout,
                1440,
            ) {
                Ok((key, bitmap)) => {
                    if self.current_texture_key != Some(key) {
                        let image = Self::color_image(&bitmap);
                        if let Some(texture) = self.current_texture.as_mut() {
                            texture.set(image, TextureOptions::LINEAR);
                        } else {
                            self.current_texture = Some(ctx.load_texture(
                                "current_page",
                                image,
                                TextureOptions::LINEAR,
                            ));
                        }
                        self.current_texture_key = Some(key);
                    }
                }
                Err(error) => {
                    self.open_error = Some(format!("Current slide render failed: {error}"));
                }
            }
        } else {
            self.current_texture = None;
            self.current_texture_key = None;
        }

        match self.get_or_render_bitmap(presenter_index, PageView::Full, presenter_layout, 1440) {
            Ok((key, bitmap)) => {
                if self.presenter_texture_key != Some(key) {
                    let image = Self::color_image(&bitmap);
                    if let Some(texture) = self.presenter_texture.as_mut() {
                        texture.set(image, TextureOptions::LINEAR);
                    } else {
                        self.presenter_texture =
                            Some(ctx.load_texture("presenter_page", image, TextureOptions::LINEAR));
                    }
                    self.presenter_texture_key = Some(key);
                }
            }
            Err(error) => {
                self.open_error = Some(format!("Presenter render failed: {error}"));
            }
        }

        if next_index != presenter_index
            && Self::has_visible_region(next_layout, PageView::SlideOnly)
        {
            match self.get_or_render_bitmap(next_index, PageView::SlideOnly, next_layout, 1024) {
                Ok((key, bitmap)) => {
                    if self.next_texture_key != Some(key) {
                        let image = Self::color_image(&bitmap);
                        if let Some(texture) = self.next_texture.as_mut() {
                            texture.set(image, TextureOptions::LINEAR);
                        } else {
                            self.next_texture =
                                Some(ctx.load_texture("next_page", image, TextureOptions::LINEAR));
                        }
                        self.next_texture_key = Some(key);
                    }
                }
                Err(error) => {
                    self.open_error = Some(format!("Next slide render failed: {error}"));
                }
            }
        } else {
            self.next_texture = None;
            self.next_texture_key = None;
        }

        if Self::has_visible_region(presenter_layout, PageView::NotesOnly) {
            match self.get_or_render_bitmap(
                presenter_index,
                PageView::NotesOnly,
                presenter_layout,
                1440,
            ) {
                Ok((key, bitmap)) => {
                    if self.notes_texture_key != Some(key) {
                        let image = Self::color_image(&bitmap);
                        if let Some(texture) = self.notes_texture.as_mut() {
                            texture.set(image, TextureOptions::LINEAR);
                        } else {
                            self.notes_texture =
                                Some(ctx.load_texture("notes_page", image, TextureOptions::LINEAR));
                        }
                        self.notes_texture_key = Some(key);
                    }
                }
                Err(error) => {
                    self.open_error = Some(format!("Notes render failed: {error}"));
                }
            }
        } else {
            self.notes_texture = None;
            self.notes_texture_key = None;
        }
    }

    fn format_duration(duration: Duration) -> String {
        let seconds = duration.as_secs();
        let hours = seconds / 3600;
        let minutes = (seconds % 3600) / 60;
        let secs = seconds % 60;
        format!("{hours:02}:{minutes:02}:{secs:02}")
    }

    fn format_countdown(duration: Duration) -> String {
        let seconds = duration.as_secs();
        let minutes = seconds / 60;
        let secs = seconds % 60;
        format!("{minutes:02}:{secs:02}")
    }

    fn longest_common_prefix_len(left: &[String], right: &[String]) -> usize {
        let mut count = 0usize;
        for (left_item, right_item) in left.iter().zip(right.iter()) {
            if left_item != right_item {
                break;
            }
            count += 1;
        }
        count
    }

    fn build_timing_breakdown_model(
        durations: &[Duration],
        labels: &[String],
        current_index: usize,
        section_paths: Option<&[Option<Vec<String>>]>,
    ) -> TimingBreakdownModel {
        let total = durations
            .iter()
            .copied()
            .fold(Duration::ZERO, |acc, value| acc + value);
        let normalized_paths: Vec<Vec<String>> = (0..durations.len())
            .map(|index| {
                section_paths
                    .and_then(|paths| paths.get(index))
                    .cloned()
                    .flatten()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|segment| segment.trim().to_owned())
                    .filter(|segment| !segment.is_empty())
                    .collect()
            })
            .collect();
        let has_sections = normalized_paths.iter().any(|path| !path.is_empty());

        let mut rows = Vec::new();
        if has_sections {
            let mut section_totals: BTreeMap<Vec<String>, Duration> = BTreeMap::new();
            for (index, path) in normalized_paths.iter().enumerate() {
                for depth in 1..=path.len() {
                    let prefix = path[..depth].to_vec();
                    let entry = section_totals.entry(prefix).or_default();
                    *entry += durations[index];
                }
            }

            let mut previous_path: Vec<String> = Vec::new();
            for (index, duration) in durations.iter().copied().enumerate() {
                let path = &normalized_paths[index];
                let common_depth = Self::longest_common_prefix_len(&previous_path, path);
                for depth in (common_depth + 1)..=path.len() {
                    let prefix = path[..depth].to_vec();
                    let section_total = section_totals
                        .get(&prefix)
                        .copied()
                        .unwrap_or(Duration::ZERO);
                    rows.push(TimingBreakdownRow::SectionTotal {
                        depth: depth - 1,
                        title: path[depth - 1].clone(),
                        duration: section_total,
                    });
                }

                let fallback_label = (index + 1).to_string();
                let label = labels.get(index).unwrap_or(&fallback_label).clone();
                rows.push(TimingBreakdownRow::Slide {
                    depth: path.len(),
                    index,
                    label,
                    duration,
                    is_current: index == current_index,
                });
                previous_path = path.clone();
            }
        } else {
            for (index, duration) in durations.iter().copied().enumerate() {
                let fallback_label = (index + 1).to_string();
                let label = labels.get(index).unwrap_or(&fallback_label).clone();
                rows.push(TimingBreakdownRow::Slide {
                    depth: 0,
                    index,
                    label,
                    duration,
                    is_current: index == current_index,
                });
            }
        }

        TimingBreakdownModel {
            total,
            has_sections,
            rows,
        }
    }

    fn timing_breakdown_model(&self, now: Instant) -> TimingBreakdownModel {
        let durations = self.state.timing_breakdown(now);
        let labels: Vec<String> = (0..durations.len())
            .map(|index| self.state.label_at(index).unwrap_or("").to_owned())
            .collect();
        let section_paths = self.document.as_ref().map(|document| {
            (0..durations.len())
                .map(|index| document.outline_path_for_page(index))
                .collect::<Vec<_>>()
        });

        Self::build_timing_breakdown_model(
            &durations,
            &labels,
            self.state.current_index(),
            section_paths.as_deref(),
        )
    }

    fn render_timing_breakdown(&mut self, ctx: &Context) {
        if !self.timing_breakdown.is_open {
            return;
        }

        let model = self.timing_breakdown_model(Instant::now());
        let mut open = self.timing_breakdown.is_open;
        egui::Window::new("Timing Breakdown (B)")
            .open(&mut open)
            .resizable(true)
            .default_width(560.0)
            .default_height(680.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(format!("Total: {}", Self::format_duration(model.total)));
                    ui.separator();
                    ui.label(format!(
                        "Current slide: {} / {}",
                        self.state.current_index() + 1,
                        self.state.total_slides()
                    ));
                });
                if !model.has_sections {
                    ui.label("Outline unavailable: showing flat per-slide timing.");
                }
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for row in &model.rows {
                        match row {
                            TimingBreakdownRow::SectionTotal {
                                depth,
                                title,
                                duration,
                            } => {
                                ui.horizontal(|ui| {
                                    ui.add_space((*depth as f32) * 14.0);
                                    ui.colored_label(
                                        Color32::from_rgb(140, 195, 255),
                                        format!("Section: {title}"),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(Align::Center),
                                        |ui| {
                                            ui.monospace(Self::format_duration(*duration));
                                        },
                                    );
                                });
                            }
                            TimingBreakdownRow::Slide {
                                depth,
                                index,
                                label,
                                duration,
                                is_current,
                            } => {
                                ui.horizontal(|ui| {
                                    ui.add_space((*depth as f32) * 14.0 + 8.0);
                                    let slide_text = format!("Slide {} ({label})", index + 1);
                                    if *is_current {
                                        ui.colored_label(Color32::LIGHT_GREEN, slide_text);
                                    } else {
                                        ui.label(slide_text);
                                    }
                                    ui.with_layout(
                                        egui::Layout::right_to_left(Align::Center),
                                        |ui| {
                                            ui.monospace(Self::format_duration(*duration));
                                        },
                                    );
                                });
                            }
                        }
                    }
                });
            });
        self.timing_breakdown.is_open = open;
    }

    fn notes_text(&self) -> String {
        if self.document.is_none() {
            return "Load a document to show notes.".to_owned();
        }
        self.layout_reason
            .clone()
            .unwrap_or_else(|| "Waiting for layout detection.".to_owned())
    }

    fn presenter_display_index(&self) -> usize {
        if self.deck_overview.is_open {
            self.deck_overview
                .selected_index
                .min(self.state.total_slides().saturating_sub(1))
        } else {
            self.state.current_index()
        }
    }

    fn get_or_render_overview_texture(
        &mut self,
        ctx: &Context,
        index: usize,
        layout: Option<NotesLayout>,
    ) -> Option<TextureHandle> {
        let target_width = self.config.overview_thumbnail_width;
        let cache_key = Self::bitmap_cache_key(
            index,
            self.config.notes_mode,
            PageView::SlideOnly,
            layout,
            target_width,
        );

        if self.overview_texture_keys.get(&index).copied() == Some(cache_key) {
            return self.overview_textures.get(&index).cloned();
        }

        match self.get_or_render_bitmap(index, PageView::SlideOnly, layout, target_width) {
            Ok((_, bitmap)) => {
                let image = Self::color_image(&bitmap);
                if let Some(texture) = self.overview_textures.get_mut(&index) {
                    texture.set(image, TextureOptions::LINEAR);
                } else {
                    let texture = ctx.load_texture(
                        format!("overview_thumb_{index}"),
                        image,
                        TextureOptions::LINEAR,
                    );
                    self.overview_textures.insert(index, texture);
                }
                self.overview_texture_keys.insert(index, cache_key);
                self.overview_textures.get(&index).cloned()
            }
            Err(error) => {
                self.open_error = Some(format!("Overview thumbnail render failed: {error}"));
                None
            }
        }
    }

    fn render_deck_overview(&mut self, ctx: &Context) {
        if !self.deck_overview.is_open {
            return;
        }

        let mut open = self.deck_overview.is_open;
        let total = self.state.total_slides();
        let columns = self.config.overview_columns as usize;
        let thumb_layout = self.resolve_layout_for_slide(self.presenter_display_index());

        egui::Window::new("Deck Overview (D)")
            .open(&mut open)
            .resizable(true)
            .default_width(1050.0)
            .default_height(720.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(format!(
                        "Selected: {} / {}",
                        self.deck_overview.selected_index + 1,
                        total
                    ));
                    ui.separator();
                    ui.label("Arrow keys move, Enter jump, Esc cancel.");
                    ui.separator();
                    let scale_changed = ui
                        .add(
                            egui::Slider::new(&mut self.deck_overview.thumbnail_scale, 0.6..=1.8)
                                .text("Thumb scale"),
                        )
                        .changed();
                    if scale_changed {
                        self.deck_overview.thumbnail_scale =
                            self.deck_overview.thumbnail_scale.clamp(0.6, 1.8);
                    }
                });

                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let mut row_start = 0usize;
                    while row_start < total {
                        ui.horizontal(|ui| {
                            for col in 0..columns {
                                let index = row_start + col;
                                if index >= total {
                                    break;
                                }
                                let selected = index == self.deck_overview.selected_index;
                                let frame = egui::Frame::group(ui.style()).stroke(egui::Stroke {
                                    width: if selected { 2.0 } else { 1.0 },
                                    color: if selected {
                                        Color32::LIGHT_GREEN
                                    } else {
                                        Color32::DARK_GRAY
                                    },
                                });
                                frame.show(ui, |ui| {
                                    ui.vertical(|ui| {
                                        if let Some(texture) = self.get_or_render_overview_texture(
                                            ctx,
                                            index,
                                            thumb_layout,
                                        ) {
                                            let base_width = self.config.overview_thumbnail_width
                                                as f32
                                                * self.deck_overview.thumbnail_scale;
                                            let size = texture.size_vec2();
                                            let ratio =
                                                if size.x > 0.0 { size.y / size.x } else { 1.0 };
                                            let target = Vec2::new(base_width, base_width * ratio);
                                            let response = ui.add(
                                                egui::Image::from_texture((texture.id(), target))
                                                    .sense(egui::Sense::click()),
                                            );
                                            if response.clicked() {
                                                self.deck_overview.selected_index = index;
                                                self.deck_overview.commit();
                                            }
                                        } else {
                                            ui.label("Rendering...");
                                        }
                                        ui.label(format!("Slide {}", index + 1));
                                    });
                                });
                            }
                        });
                        ui.add_space(6.0);
                        row_start += columns.max(1);
                    }
                });
            });

        self.deck_overview.is_open = open;
    }

    fn fit_size(image_size: Vec2, available_size: Vec2) -> Vec2 {
        if image_size.x <= 0.0 || image_size.y <= 0.0 {
            return available_size;
        }
        let sx = available_size.x / image_size.x;
        let sy = available_size.y / image_size.y;
        let scale = sx.min(sy).max(0.01);
        image_size * scale
    }

    fn render_projector_view(&mut self, ctx: &egui::Context) {
        let title = if self.state.screen_roles_swapped {
            "PDF Presenter - Presenter (Swapped)"
        } else {
            "PDF Presenter - Projector"
        };

        let viewport = ViewportBuilder::default()
            .with_title(title)
            .with_inner_size([1280.0, 720.0])
            .with_fullscreen(self.state.projector_fullscreen);

        let texture = if self.state.screen_roles_swapped {
            self.presenter_texture.clone()
        } else {
            self.projector_texture.clone()
        };
        let displayed_index = if self.state.screen_roles_swapped {
            self.presenter_display_index()
        } else {
            self.state.current_index()
        };
        let displayed_layout = self.resolve_layout_for_slide(displayed_index);
        let surface_space = if self.state.screen_roles_swapped {
            SlideSurfaceSpace::FullPage
        } else {
            SlideSurfaceSpace::CroppedSlide
        };
        let auto_badge_text = if self.state.auto_advance_enabled() {
            let remaining = self
                .state
                .auto_advance_remaining(Instant::now())
                .map(Self::format_countdown)
                .unwrap_or_else(|| "--:--".to_owned());
            Some(format!("AUTO {remaining}"))
        } else {
            None
        };
        let show_dirty_badge = self.has_unsaved_annotation_changes();
        let slide_region = Self::slide_region(displayed_layout);
        let highlight_overlay = self.highlight_overlay_snapshot_for_index(displayed_index);
        let mut projector_laser_sample: Option<LaserSurfaceSample> = None;
        let laser_position = self.laser_position;

        ctx.show_viewport_immediate(self.projector_viewport, viewport, move |ctx, _class| {
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(Color32::from_rgb(12, 14, 20)))
                .show(ctx, |ui| {
                    if let Some(texture) = texture.as_ref() {
                        let available = ui.available_size();
                        let target = PresenterApp::fit_size(texture.size_vec2(), available);
                        ui.centered_and_justified(|ui| {
                            let response = ui.add(
                                egui::Image::from_texture((texture.id(), target))
                                    .sense(Sense::click_and_drag()),
                            );
                            projector_laser_sample =
                                Some(PresenterApp::capture_laser_sample(ctx, &response));
                            PresenterApp::draw_highlight_overlay_from_snapshot(
                                ui,
                                response.rect,
                                slide_region,
                                surface_space,
                                &highlight_overlay,
                                false,
                                false,
                            );
                            PresenterApp::draw_laser_overlay(
                                ui,
                                response.rect,
                                laser_position,
                                slide_region,
                                surface_space,
                            );
                        });
                    } else {
                        ui.vertical_centered(|ui| {
                            ui.add_space(40.0);
                            ui.label(
                                RichText::new("No slide rendered yet")
                                    .font(FontId::new(28.0, FontFamily::Proportional))
                                    .color(Color32::LIGHT_GRAY),
                            );
                        });
                    }

                    if let Some(badge_text) = auto_badge_text.as_ref() {
                        let badge_rect = Rect::from_min_size(
                            ui.min_rect().min + Vec2::new(12.0, 12.0),
                            Vec2::new(92.0, 24.0),
                        );
                        PresenterApp::draw_badge(
                            ui,
                            badge_rect,
                            badge_text,
                            Color32::from_rgba_unmultiplied(0, 0, 0, 175),
                            Color32::from_rgb(170, 255, 170),
                        );
                    }
                    if show_dirty_badge {
                        let dirty_rect = Rect::from_min_size(
                            ui.min_rect().min + Vec2::new(12.0, 42.0),
                            Vec2::new(92.0, 24.0),
                        );
                        PresenterApp::draw_badge(
                            ui,
                            dirty_rect,
                            "UNSAVED",
                            Color32::from_rgba_unmultiplied(120, 26, 26, 220),
                            Color32::from_rgb(255, 220, 220),
                        );
                    }
                });
        });

        if let Some(sample) = projector_laser_sample {
            self.consume_laser_sample(sample, slide_region, surface_space);
        }
    }

    fn render_presenter_view(&mut self, ctx: &egui::Context) {
        let talk_elapsed = Self::format_duration(self.state.talk_elapsed(Instant::now()));
        let slide_elapsed = Self::format_duration(self.state.current_slide_elapsed(Instant::now()));
        let wall_clock = Local::now().format("%H:%M:%S").to_string();
        let display_index = self.presenter_display_index();
        let display_layout = self.resolve_layout_for_slide(display_index);
        let interaction_mode = self.interaction_mode();
        let current_override_secs = self.state.auto_advance_slide_override_secs(display_index);
        let effective_interval_secs = self
            .state
            .auto_advance_effective_interval_secs(display_index);
        if self.auto_override_editor_index != Some(display_index) {
            self.auto_override_editor_secs =
                current_override_secs.unwrap_or(effective_interval_secs);
            self.auto_override_editor_index = Some(display_index);
        }
        self.auto_override_editor_secs = self.auto_override_editor_secs.clamp(1, 3600);

        if interaction_mode == InteractionMode::AnnotationEdit {
            self.refresh_text_annotations_for_index(display_index);
        }

        egui::TopBottomPanel::top("top_controls").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("PDF Path:");
                let path_response = ui.text_edit_singleline(&mut self.source_path_input);
                let load_clicked = ui.button("Load").clicked();
                let enter_pressed =
                    path_response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
                if load_clicked || enter_pressed {
                    let source = PathBuf::from(self.source_path_input.trim());
                    if source.exists() {
                        self.request_load_document(source);
                    } else {
                        self.open_error = Some("Path does not exist.".to_owned());
                    }
                }
            });

            ui.horizontal(|ui| {
                if ui.button("Prev").clicked() {
                    self.apply_command(AppCommand::PreviousSlide);
                }
                if ui.button("Next").clicked() {
                    self.apply_command(AppCommand::NextSlide);
                }
                if ui.button("First").clicked() {
                    self.apply_command(AppCommand::FirstSlide);
                }
                if ui.button("Last").clicked() {
                    self.apply_command(AppCommand::LastSlide);
                }
                if ui.button("Swap Screens (S)").clicked() {
                    self.apply_command(AppCommand::SwapScreens);
                }
                if ui.button("Toggle Fullscreen (F)").clicked() {
                    self.apply_command(AppCommand::ToggleProjectorFullscreen);
                    self.persist_config();
                }
                if ui.button("Save (Ctrl+S)").clicked() {
                    self.save_document_in_place();
                }
                if ui.button("Timing (B)").clicked() {
                    self.timing_breakdown.toggle();
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let available = ui.available_size();
            let left_width = available.x * 0.60;
            let right_width = (available.x - left_width - 8.0).max(280.0);

            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(
                    Vec2::new(left_width.max(320.0), available.y),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.group(|ui| {
                            ui.heading("Notes");
                            ui.label(self.notes_text());
                            if let Some(warning) = &self.layout_warning {
                                ui.colored_label(Color32::YELLOW, warning);
                            }
                            let mut override_changed = false;
                            ui.horizontal_wrapped(|ui| {
                                override_changed |= ui
                                    .checkbox(
                                        &mut self.manual_layout_override.enabled,
                                        "Manual split override",
                                    )
                                    .changed();
                                if ui
                                    .add_enabled(
                                        !self.manual_layout_override.enabled,
                                        egui::Button::new("Use Manual"),
                                    )
                                    .clicked()
                                {
                                    self.manual_layout_override.enabled = true;
                                    override_changed = true;
                                }
                                if ui
                                    .add_enabled(
                                        self.manual_layout_override.enabled,
                                        egui::Button::new("Auto"),
                                    )
                                    .clicked()
                                {
                                    self.manual_layout_override.enabled = false;
                                    override_changed = true;
                                }
                            });

                            let manual_enabled = self.manual_layout_override.enabled;
                            ui.horizontal_wrapped(|ui| {
                                ui.label("Axis:");
                                ui.add_enabled_ui(manual_enabled, |ui| {
                                    egui::ComboBox::from_id_salt("notes_manual_axis_compact")
                                        .selected_text(match self.manual_layout_override.axis {
                                            SplitAxis::Vertical => "Vertical",
                                            SplitAxis::Horizontal => "Horizontal",
                                        })
                                        .show_ui(ui, |ui| {
                                            override_changed |= ui
                                                .selectable_value(
                                                    &mut self.manual_layout_override.axis,
                                                    SplitAxis::Vertical,
                                                    "Vertical",
                                                )
                                                .changed();
                                            override_changed |= ui
                                                .selectable_value(
                                                    &mut self.manual_layout_override.axis,
                                                    SplitAxis::Horizontal,
                                                    "Horizontal",
                                                )
                                                .changed();
                                        });
                                });

                                ui.label("Side:");
                                let side_label = match self.manual_layout_override.notes_side {
                                    NotesSide::Left => "Left",
                                    NotesSide::Right => "Right",
                                    NotesSide::Top => "Top",
                                    NotesSide::Bottom => "Bottom",
                                };
                                ui.add_enabled_ui(manual_enabled, |ui| {
                                    egui::ComboBox::from_id_salt("notes_manual_side_compact")
                                        .selected_text(side_label)
                                        .show_ui(ui, |ui| match self.manual_layout_override.axis {
                                            SplitAxis::Vertical => {
                                                override_changed |= ui
                                                    .selectable_value(
                                                        &mut self.manual_layout_override.notes_side,
                                                        NotesSide::Left,
                                                        "Left",
                                                    )
                                                    .changed();
                                                override_changed |= ui
                                                    .selectable_value(
                                                        &mut self.manual_layout_override.notes_side,
                                                        NotesSide::Right,
                                                        "Right",
                                                    )
                                                    .changed();
                                            }
                                            SplitAxis::Horizontal => {
                                                override_changed |= ui
                                                    .selectable_value(
                                                        &mut self.manual_layout_override.notes_side,
                                                        NotesSide::Top,
                                                        "Top",
                                                    )
                                                    .changed();
                                                override_changed |= ui
                                                    .selectable_value(
                                                        &mut self.manual_layout_override.notes_side,
                                                        NotesSide::Bottom,
                                                        "Bottom",
                                                    )
                                                    .changed();
                                            }
                                        });
                                });

                                ui.label("Split:");
                                let mut split_percent =
                                    (self.manual_layout_override.split_ratio * 100.0).round() as i32;
                                if ui
                                    .add_enabled(
                                        manual_enabled,
                                        egui::DragValue::new(&mut split_percent)
                                            .range(0..=100)
                                            .speed(0.5)
                                            .suffix("%"),
                                    )
                                    .changed()
                                {
                                    self.manual_layout_override.split_ratio =
                                        (split_percent as f32 / 100.0).clamp(0.0, 1.0);
                                    override_changed = true;
                                }
                            });

                            if !manual_enabled {
                                ui.small("Enable manual split to set Axis / Side / Split %.");
                            }

                            if override_changed {
                                self.manual_layout_override.sanitize();
                                self.sync_override_to_config();
                            }
                            ui.separator();
                            if let Some(texture) = self.notes_texture.as_ref() {
                                let max = ui.available_size_before_wrap();
                                let target = Self::fit_size(texture.size_vec2(), max);
                                ui.image((texture.id(), target));
                            } else {
                                ui.label("No notes region detected.");
                            }
                        });
                    },
                );

                ui.allocate_ui_with_layout(
                    Vec2::new(right_width, available.y),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        let right_available = ui.available_size_before_wrap();
                        let preview_height = (right_available.y * 0.32).max(160.0);
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {

                        ui.group(|ui| {
                            ui.heading("Current slide");
                            if let Some(texture) = self.current_texture.as_ref() {
                                let max = Vec2::new(ui.available_width(), preview_height);
                                let target = Self::fit_size(texture.size_vec2(), max);
                                let response = ui.add(
                                    egui::Image::from_texture((texture.id(), target))
                                        .sense(Sense::click_and_drag()),
                                );
                                self.handle_current_slide_interaction(
                                    &response,
                                    display_index,
                                    display_layout,
                                );
                                self.handle_annotation_interaction(
                                    &response,
                                    display_index,
                                    display_layout,
                                );
                                let laser_sample = Self::capture_laser_sample(ctx, &response);
                                self.consume_laser_sample(
                                    laser_sample,
                                    Self::slide_region(display_layout),
                                    SlideSurfaceSpace::CroppedSlide,
                                );
                                self.draw_annotation_overlays(
                                    ui,
                                    response.rect,
                                    display_index,
                                    display_layout,
                                );
                                self.render_quick_actions_overlay(ui, response.rect, display_index);
                                self.draw_unsaved_badge_on_preview(ui, response.rect);
                                Self::draw_laser_overlay(
                                    ui,
                                    response.rect,
                                    self.laser_position,
                                    Self::slide_region(display_layout),
                                    SlideSurfaceSpace::CroppedSlide,
                                );
                            } else {
                                ui.label("No rendered current slide.");
                            }
                        });

                        ui.add_space(6.0);
                        ui.group(|ui| {
                            ui.heading("Next slide");
                            if let Some(texture) = self.next_texture.as_ref() {
                                let max = Vec2::new(ui.available_width(), preview_height);
                                let target = Self::fit_size(texture.size_vec2(), max);
                                ui.image((texture.id(), target));
                            } else {
                                ui.label("No next slide.");
                            }
                        });

                        ui.add_space(6.0);
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                ui.label("Go To");
                                ui.text_edit_singleline(&mut self.go_to_input);
                                if ui.button("Go (G)").clicked() {
                                    self.submit_go_to();
                                }
                            });
                            ui.label(if self.state.use_slide_labels() {
                                "Mode: slide label (J to toggle)"
                            } else {
                                "Mode: page number (J to toggle)"
                            });
                            ui.separator();
                            ui.label(format!(
                                "{} / {}",
                                display_index + 1,
                                self.state.total_slides()
                            ));
                            ui.label(format!(
                                "Label: {}",
                                self.state.label_at(display_index).unwrap_or("N/A")
                            ));
                            ui.label(format!(
                                "Next: {}",
                                self.state
                                    .next_label_from(display_index)
                                    .unwrap_or("End of deck")
                            ));
                            ui.label(format!("Talk Time: {talk_elapsed}"));
                            ui.label(format!("Slide Time: {slide_elapsed}"));
                            ui.label(format!("Wall Clock: {wall_clock}"));

                            ui.separator();
                            ui.heading("Auto-Advance");
                            ui.horizontal(|ui| {
                                let enabled = self.state.auto_advance_enabled();
                                if ui
                                    .button(if enabled {
                                        "Disable (T)"
                                    } else {
                                        "Enable (T)"
                                    })
                                    .clicked()
                                {
                                    self.apply_command(AppCommand::ToggleAutoAdvance);
                                }

                                let mut global_secs = self.state.auto_advance_global_interval_secs();
                                let global_changed = ui
                                    .add(
                                        egui::DragValue::new(&mut global_secs)
                                            .range(1..=3600)
                                            .speed(0.5)
                                            .suffix(" s"),
                                    )
                                    .changed();
                                if global_changed {
                                    self.apply_command(AppCommand::SetAutoAdvanceGlobalInterval(
                                        global_secs,
                                    ));
                                    self.sync_auto_advance_profile_to_config();
                                }
                            });

                            ui.horizontal(|ui| {
                                ui.label(format!("Slide {} override:", display_index + 1));
                                ui.add(
                                    egui::DragValue::new(&mut self.auto_override_editor_secs)
                                        .range(1..=3600)
                                        .speed(0.5)
                                        .suffix(" s"),
                                );
                                if ui.button("Set").clicked() {
                                    self.apply_command(AppCommand::SetAutoAdvanceSlideOverride {
                                        index: display_index,
                                        secs: self.auto_override_editor_secs,
                                    });
                                    self.sync_auto_advance_profile_to_config();
                                    self.auto_override_editor_secs =
                                        self.state.auto_advance_effective_interval_secs(display_index);
                                }
                                if ui.button("Clear").clicked() {
                                    self.apply_command(AppCommand::ClearAutoAdvanceSlideOverride(
                                        display_index,
                                    ));
                                    self.sync_auto_advance_profile_to_config();
                                    self.auto_override_editor_secs =
                                        self.state.auto_advance_effective_interval_secs(display_index);
                                }
                            });

                            let effective = self.state.auto_advance_effective_interval_secs(display_index);
                            let countdown = self
                                .state
                                .auto_advance_remaining(Instant::now())
                                .map(Self::format_duration)
                                .unwrap_or_else(|| "--:--:--".to_owned());
                            ui.label(format!("Effective interval: {effective}s"));
                            ui.label(format!("Next advance in: {countdown}"));
                        });

                        ui.add_space(6.0);
                        ui.group(|ui| {
                            ui.heading("Annotations");
                            ui.label("Laser: hold Ctrl+click/drag for temporary pointer, or press L for persistent pointer.");
                            ui.horizontal(|ui| {
                                let draw_selected =
                                    self.interaction_mode() == InteractionMode::HighlightDraw;
                                if ui
                                    .selectable_label(draw_selected, "Draw Highlight (H)")
                                    .clicked()
                                {
                                    if !draw_selected {
                                        self.state.highlight_enabled = true;
                                        self.annotation_edit_enabled = false;
                                    } else {
                                        self.state.highlight_enabled = false;
                                        self.stroke_in_progress.clear();
                                        self.stroke_in_progress_style = None;
                                    }
                                }

                                let edit_selected =
                                    self.interaction_mode() == InteractionMode::AnnotationEdit;
                                if ui
                                    .selectable_label(edit_selected, "Edit Notes (A)")
                                    .clicked()
                                {
                                    self.annotation_edit_enabled = !edit_selected;
                                    if self.annotation_edit_enabled {
                                        self.state.highlight_enabled = false;
                                        self.stroke_in_progress.clear();
                                        self.stroke_in_progress_style = None;
                                    }
                                }
                            });
                            ui.add_space(4.0);
                            self.render_pen_toolkit_controls(ui, false, display_index);

                            if self.document_dirty() {
                                ui.colored_label(
                                    Color32::from_rgb(255, 220, 90),
                                    "Unsaved annotation changes",
                                );
                            }

                            if self.interaction_mode() == InteractionMode::AnnotationEdit {
                                ui.separator();
                                ui.horizontal(|ui| {
                                    ui.label("New note:");
                                    ui.text_edit_singleline(&mut self.pending_text_annotation);
                                    if ui.button("Add").clicked() {
                                        self.add_text_annotation_for_current_slide(
                                            display_index,
                                            display_layout,
                                        );
                                    }
                                });

                                if self.text_annotations.is_empty() {
                                    ui.label("No text annotations on this slide.");
                                } else {
                                    for annotation in &mut self.text_annotations {
                                        ui.separator();
                                        ui.horizontal(|ui| {
                                            let selected = self
                                                .selected_annotation_id
                                                .as_ref()
                                                .is_some_and(|id| id == &annotation.id);
                                            if ui
                                                .selectable_label(
                                                    selected,
                                                    format!("ID {}", annotation.id),
                                                )
                                                .clicked()
                                            {
                                                self.selected_annotation_id =
                                                    Some(annotation.id.clone());
                                            }
                                            ui.label(format!(
                                                "Rect {:.0},{:.0}-{:.0},{:.0}",
                                                annotation.rect.left,
                                                annotation.rect.bottom,
                                                annotation.rect.right,
                                                annotation.rect.top
                                            ));
                                        });
                                        ui.label(format!(
                                            "Status: {} | Priority: {}",
                                            annotation.metadata.status.as_str(),
                                            annotation.metadata.priority.as_str()
                                        ));
                                    }

                                    let selected_index = self
                                        .selected_annotation_id
                                        .as_ref()
                                        .and_then(|id| {
                                            self.text_annotations
                                                .iter()
                                                .position(|entry| &entry.id == id)
                                        });
                                    if let Some(selected_index) = selected_index {
                                        ui.separator();
                                        ui.label("Selected annotation details:");
                                        {
                                            let annotation = &mut self.text_annotations[selected_index];
                                            ui.label("Text:");
                                            ui.text_edit_multiline(&mut annotation.contents);

                                            ui.horizontal(|ui| {
                                                ui.label("Author:");
                                                ui.text_edit_singleline(&mut annotation.metadata.author);
                                            });

                                            ui.horizontal(|ui| {
                                                ui.label("Color:");
                                                let mut rgb = [
                                                    annotation.metadata.color_rgb.0,
                                                    annotation.metadata.color_rgb.1,
                                                    annotation.metadata.color_rgb.2,
                                                ];
                                                if egui::color_picker::color_edit_button_rgb(ui, &mut rgb)
                                                    .changed()
                                                {
                                                    annotation.metadata.color_rgb =
                                                        (rgb[0], rgb[1], rgb[2]);
                                                }
                                            });

                                            ui.horizontal(|ui| {
                                                ui.label("Tags:");
                                                let mut tags_csv = annotation.metadata.tags.join(", ");
                                                if ui.text_edit_singleline(&mut tags_csv).changed() {
                                                    annotation.metadata.tags = Self::parse_tags(&tags_csv);
                                                }
                                            });

                                            ui.horizontal(|ui| {
                                                ui.label("Priority:");
                                                egui::ComboBox::from_id_salt(format!(
                                                    "priority_{}",
                                                    annotation.id
                                                ))
                                                .selected_text(annotation.metadata.priority.as_str())
                                                .show_ui(ui, |ui| {
                                                    ui.selectable_value(
                                                        &mut annotation.metadata.priority,
                                                        AnnotationPriority::Low,
                                                        "low",
                                                    );
                                                    ui.selectable_value(
                                                        &mut annotation.metadata.priority,
                                                        AnnotationPriority::Medium,
                                                        "medium",
                                                    );
                                                    ui.selectable_value(
                                                        &mut annotation.metadata.priority,
                                                        AnnotationPriority::High,
                                                        "high",
                                                    );
                                                });
                                            });

                                            ui.horizontal(|ui| {
                                                ui.label("Status:");
                                                egui::ComboBox::from_id_salt(format!(
                                                    "status_{}",
                                                    annotation.id
                                                ))
                                                .selected_text(annotation.metadata.status.as_str())
                                                .show_ui(ui, |ui| {
                                                    ui.selectable_value(
                                                        &mut annotation.metadata.status,
                                                        AnnotationStatus::Open,
                                                        "open",
                                                    );
                                                    ui.selectable_value(
                                                        &mut annotation.metadata.status,
                                                        AnnotationStatus::InProgress,
                                                        "in_progress",
                                                    );
                                                    ui.selectable_value(
                                                        &mut annotation.metadata.status,
                                                        AnnotationStatus::Done,
                                                        "done",
                                                    );
                                                });
                                            });

                                            ui.label(format!(
                                                "Modified: {}",
                                                annotation
                                                    .metadata
                                                    .modified_at
                                                    .clone()
                                                    .unwrap_or_else(|| "N/A".to_owned())
                                            ));
                                        }

                                        let transform_dirty = self.selected_transform_dirty();
                                        let metadata_dirty = self.selected_metadata_dirty();
                                        if transform_dirty {
                                            ui.colored_label(
                                                Color32::from_rgb(180, 220, 255),
                                                "Geometry changed on canvas. Apply or revert before save.",
                                            );
                                        }

                                        ui.horizontal(|ui| {
                                            if ui
                                                .add_enabled(
                                                    transform_dirty,
                                                    egui::Button::new("Apply Transform"),
                                                )
                                                .clicked()
                                            {
                                                self.commit_selected_annotation_update(
                                                    "Annotation transform applied (unsaved).",
                                                );
                                            }

                                            if ui
                                                .add_enabled(
                                                    transform_dirty,
                                                    egui::Button::new("Revert Transform"),
                                                )
                                                .clicked()
                                            {
                                                self.revert_selected_transform();
                                            }

                                            if ui
                                                .add_enabled(
                                                    transform_dirty || metadata_dirty,
                                                    egui::Button::new("Update Selected"),
                                                )
                                                .clicked()
                                            {
                                                self.commit_selected_annotation_update(
                                                    "Text annotation updated (unsaved).",
                                                );
                                            }

                                            if ui.button("Delete Selected").clicked() {
                                                self.delete_selected_note(display_index);
                                            }
                                        });
                                    }
                                }
                            } else if self.interaction_mode() == InteractionMode::HighlightDraw {
                                ui.label(
                                    "Drag to draw highlights. Hold Shift for straight lines. Toggle eraser (E) to remove session strokes. Save to write changes to file.",
                                );
                            }
                        });
                        ui.add_space(6.0);
                        self.render_keybindings_editor(ui);
                        });
                    },
                );
            });

            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Notes Mode:");
                let mut changed = false;
                egui::ComboBox::from_id_salt("notes_mode")
                    .selected_text(match self.config.notes_mode {
                        NotesMode::Auto => "Auto",
                        NotesMode::PdfAnnotations => "PDF annotations",
                        NotesMode::BeamerSecondScreen => "Beamer second screen",
                        NotesMode::LibreOfficeNotes => "LibreOffice notes",
                    })
                    .show_ui(ui, |ui| {
                        changed |= ui
                            .selectable_value(&mut self.config.notes_mode, NotesMode::Auto, "Auto")
                            .changed();
                        changed |= ui
                            .selectable_value(
                                &mut self.config.notes_mode,
                                NotesMode::PdfAnnotations,
                                "PDF annotations",
                            )
                            .changed();
                        changed |= ui
                            .selectable_value(
                                &mut self.config.notes_mode,
                                NotesMode::BeamerSecondScreen,
                                "Beamer second screen",
                            )
                            .changed();
                        changed |= ui
                            .selectable_value(
                                &mut self.config.notes_mode,
                                NotesMode::LibreOfficeNotes,
                                "LibreOffice notes",
                            )
                            .changed();
                    });
                if changed {
                    self.invalidate_render_cache();
                    self.persist_config();
                }

                ui.separator();
                let threshold_changed = ui
                    .add(
                        egui::Slider::new(&mut self.config.notes_confidence_threshold, 0.05..=0.95)
                            .text("Auto split threshold"),
                    )
                    .changed();
                if threshold_changed {
                    self.invalidate_render_cache();
                    self.persist_config();
                }

                ui.separator();
                let slider_response = ui.add(
                    egui::Slider::new(&mut self.config.presenter_layout.preview_slides, 1..=16)
                        .text("Preview slides"),
                );
                if slider_response.changed() {
                    self.persist_config();
                }

                ui.separator();
                let overview_width_changed = ui
                    .add(
                        egui::Slider::new(&mut self.config.overview_thumbnail_width, 120..=800)
                            .text("Overview thumb width"),
                    )
                    .changed();
                if overview_width_changed {
                    self.config.overview_thumbnail_width =
                        self.config.overview_thumbnail_width.clamp(120, 800);
                    self.invalidate_render_cache();
                    self.persist_config();
                }

                let overview_cols_changed = ui
                    .add(
                        egui::Slider::new(&mut self.config.overview_columns, 2..=8)
                            .text("Overview cols"),
                    )
                    .changed();
                if overview_cols_changed {
                    self.config.overview_columns = self.config.overview_columns.clamp(2, 8);
                    self.persist_config();
                }
            });

            ui.horizontal(|ui| {
                let enabled_changed = ui
                    .checkbox(
                        &mut self.manual_layout_override.enabled,
                        "Manual split override",
                    )
                    .changed();

                let mut override_changed = false;
                if self.manual_layout_override.enabled {
                    ui.label("Axis:");
                    egui::ComboBox::from_id_salt("manual_axis")
                        .selected_text(match self.manual_layout_override.axis {
                            SplitAxis::Vertical => "Vertical",
                            SplitAxis::Horizontal => "Horizontal",
                        })
                        .show_ui(ui, |ui| {
                            override_changed |= ui
                                .selectable_value(
                                    &mut self.manual_layout_override.axis,
                                    SplitAxis::Vertical,
                                    "Vertical",
                                )
                                .changed();
                            override_changed |= ui
                                .selectable_value(
                                    &mut self.manual_layout_override.axis,
                                    SplitAxis::Horizontal,
                                    "Horizontal",
                                )
                                .changed();
                        });

                    ui.label("Notes Side:");
                    let side_label = match self.manual_layout_override.notes_side {
                        NotesSide::Left => "Left",
                        NotesSide::Right => "Right",
                        NotesSide::Top => "Top",
                        NotesSide::Bottom => "Bottom",
                    };
                    egui::ComboBox::from_id_salt("manual_notes_side")
                        .selected_text(side_label)
                        .show_ui(ui, |ui| match self.manual_layout_override.axis {
                            SplitAxis::Vertical => {
                                override_changed |= ui
                                    .selectable_value(
                                        &mut self.manual_layout_override.notes_side,
                                        NotesSide::Left,
                                        "Left",
                                    )
                                    .changed();
                                override_changed |= ui
                                    .selectable_value(
                                        &mut self.manual_layout_override.notes_side,
                                        NotesSide::Right,
                                        "Right",
                                    )
                                    .changed();
                            }
                            SplitAxis::Horizontal => {
                                override_changed |= ui
                                    .selectable_value(
                                        &mut self.manual_layout_override.notes_side,
                                        NotesSide::Top,
                                        "Top",
                                    )
                                    .changed();
                                override_changed |= ui
                                    .selectable_value(
                                        &mut self.manual_layout_override.notes_side,
                                        NotesSide::Bottom,
                                        "Bottom",
                                    )
                                    .changed();
                            }
                        });

                    override_changed |= ui
                        .add(
                            egui::Slider::new(
                                &mut self.manual_layout_override.split_ratio,
                                0.0..=1.0,
                            )
                            .text("Split ratio"),
                        )
                        .changed();
                }

                if enabled_changed || override_changed {
                    self.manual_layout_override.sanitize();
                    self.sync_override_to_config();
                }
            });

            ui.horizontal(|ui| {
                ui.label(format!(
                    "Laser: {}",
                    if self.state.laser_enabled {
                        "On"
                    } else {
                        "Off"
                    }
                ));
                ui.label(format!(
                    "Highlight: {}",
                    if self.state.highlight_enabled {
                        "On"
                    } else {
                        "Off"
                    }
                ));
                ui.label(format!(
                    "Annotation Edit: {}",
                    if self.annotation_edit_enabled {
                        "On"
                    } else {
                        "Off"
                    }
                ));
            });

            if let Some(status) = &self.status_message {
                ui.separator();
                ui.colored_label(Color32::LIGHT_GREEN, status);
            }
            if let Some(warning) = &self.keybinding_warning {
                ui.colored_label(Color32::from_rgb(255, 220, 90), warning);
            }
            if let Some(error) = &self.open_error {
                ui.colored_label(Color32::LIGHT_RED, error);
            }
        });

        self.render_deck_overview(ctx);
        self.render_timing_breakdown(ctx);
    }

    fn apply_overview_commit(&mut self) {
        if let Some(target) = self.deck_overview.take_commit() {
            self.apply_command(AppCommand::GoToIndex(target));
            self.deck_overview.selected_index = self.state.current_index();
        }
    }
}

impl eframe::App for PresenterApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.laser_persistent_seen_this_frame = false;
        self.laser_primary_down_seen_this_frame = false;
        let close_requested = ctx.input(|input| input.viewport().close_requested());
        if close_requested {
            if self.allow_close_without_guard {
                self.allow_close_without_guard = false;
            } else if self.has_unsaved_annotation_changes() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                if self.pending_unsaved_action.is_none() {
                    self.pending_unsaved_action = Some(PendingUnsavedAction::Close);
                }
            }
        }
        self.handle_shortcuts(ctx);
        self.apply_overview_commit();
        self.refresh_document_if_changed();
        self.run_auto_advance_tick();
        self.refresh_textures(ctx);

        let root_title = if self.state.screen_roles_swapped {
            "PDF Presenter - Projector (Swapped)"
        } else {
            "PDF Presenter - Presenter"
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(root_title.to_owned()));

        self.render_presenter_view(ctx);
        self.render_projector_view(ctx);
        self.render_unsaved_changes_dialog(ctx);
        self.render_unsaved_toast(ctx);
        self.finish_laser_frame(ctx);
        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod overview_tests {
    use super::{
        DeckOverviewState, PresenterApp, ResizeHandle, ShortcutAction, ShortcutBindings,
        SlideSurfaceSpace, TimingBreakdownRow, TimingBreakdownState, TransformMode,
    };
    use crate::app::{AppCommand, PresentationState};
    use crate::config::{KeybindingConfig, NotesLayout, NotesSide, SplitAxis};
    use crate::document::{AnnotationMetadata, PdfRect, TextAnnotation};
    use eframe::egui::{Key, Pos2, Rect};
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    fn sample_text_annotation(id: &str, rect: PdfRect) -> TextAnnotation {
        TextAnnotation {
            id: id.to_owned(),
            page_index: 0,
            rect,
            contents: "note".to_owned(),
            metadata: AnnotationMetadata::default(),
        }
    }

    #[test]
    fn overview_selection_moves_with_grid_bounds() {
        let mut overview = DeckOverviewState::default();
        overview.open(0);
        overview.move_selection(10, 4, 1, 0);
        assert_eq!(overview.selected_index, 1);
        overview.move_selection(10, 4, 0, 1);
        assert_eq!(overview.selected_index, 5);
        overview.move_selection(10, 4, -10, 0);
        assert_eq!(overview.selected_index, 4);
        overview.move_selection(10, 4, 10, 10);
        assert_eq!(overview.selected_index, 9);
    }

    #[test]
    fn overview_commit_and_cancel_work() {
        let mut overview = DeckOverviewState::default();
        overview.open(3);
        overview.commit();
        assert!(!overview.is_open);
        assert_eq!(overview.take_commit(), Some(3));
        assert_eq!(overview.take_commit(), None);

        overview.open(2);
        overview.cancel();
        assert!(!overview.is_open);
        assert_eq!(overview.take_commit(), None);
    }

    #[test]
    fn slide_region_for_vertical_and_horizontal_layouts() {
        let vertical = PresenterApp::slide_region(Some(NotesLayout {
            axis: SplitAxis::Vertical,
            split_ratio: 0.40,
            notes_side: NotesSide::Right,
            confidence: 1.0,
        }));
        assert!((vertical.min_x - 0.0).abs() < 0.0001);
        assert!((vertical.max_x - 0.40).abs() < 0.0001);

        let horizontal = PresenterApp::slide_region(Some(NotesLayout {
            axis: SplitAxis::Horizontal,
            split_ratio: 0.30,
            notes_side: NotesSide::Top,
            confidence: 1.0,
        }));
        assert!((horizontal.min_y - 0.0).abs() < 0.0001);
        assert!((horizontal.max_y - 0.70).abs() < 0.0001);
    }

    #[test]
    fn coordinate_mapping_supports_cropped_and_full_spaces() {
        let rect = Rect::from_min_max(Pos2::new(100.0, 50.0), Pos2::new(500.0, 250.0));
        let region = PresenterApp::slide_region(Some(NotesLayout {
            axis: SplitAxis::Vertical,
            split_ratio: 0.4,
            notes_side: NotesSide::Right,
            confidence: 1.0,
        }));

        let pointer = Pos2::new(300.0, 150.0);
        let page_pos = PresenterApp::screen_to_page_normalized(
            rect,
            region,
            pointer,
            SlideSurfaceSpace::CroppedSlide,
        )
        .expect("cropped space should map");
        assert!((page_pos.0 - 0.2).abs() < 0.01);
        assert!((page_pos.1 - 0.5).abs() < 0.01);

        let screen_back = PresenterApp::page_normalized_to_screen(
            rect,
            region,
            page_pos,
            SlideSurfaceSpace::CroppedSlide,
        )
        .expect("cropped reverse mapping should succeed");
        assert!((screen_back.x - pointer.x).abs() < 1.0);
        assert!((screen_back.y - pointer.y).abs() < 1.0);

        let full_inside = PresenterApp::screen_to_page_normalized(
            rect,
            region,
            Pos2::new(180.0, 150.0),
            SlideSurfaceSpace::FullPage,
        )
        .expect("point in slide region should map");
        assert!((full_inside.0 - 0.2).abs() < 0.01);
        assert!(
            PresenterApp::screen_to_page_normalized(
                rect,
                region,
                Pos2::new(480.0, 150.0),
                SlideSurfaceSpace::FullPage
            )
            .is_none(),
            "point in notes region should be rejected for full-page interaction"
        );
    }

    #[test]
    fn laser_visibility_transitions_are_consistent() {
        let (temp_next, visible) = PresenterApp::finalize_laser_state(true, true, false, false);
        assert!(!temp_next);
        assert!(visible);

        let (temp_next, visible) = PresenterApp::finalize_laser_state(false, false, true, true);
        assert!(temp_next);
        assert!(visible);

        let (temp_next, visible) = PresenterApp::finalize_laser_state(false, false, true, false);
        assert!(!temp_next);
        assert!(!visible);
    }

    #[test]
    fn timing_breakdown_groups_rows_when_outline_paths_exist() {
        let durations = vec![
            Duration::from_secs(10),
            Duration::from_secs(20),
            Duration::from_secs(30),
            Duration::from_secs(5),
        ];
        let labels = vec![
            "1".to_owned(),
            "2".to_owned(),
            "3".to_owned(),
            "4".to_owned(),
        ];
        let section_paths = vec![
            Some(vec!["Intro".to_owned()]),
            Some(vec!["Intro".to_owned(), "Scope".to_owned()]),
            Some(vec!["Intro".to_owned(), "Scope".to_owned()]),
            Some(vec!["Q&A".to_owned()]),
        ];

        let model = PresenterApp::build_timing_breakdown_model(
            &durations,
            &labels,
            2,
            Some(&section_paths),
        );
        assert!(model.has_sections);
        assert_eq!(model.total, Duration::from_secs(65));
        assert!(model.rows.iter().any(|row| {
            matches!(
                row,
                TimingBreakdownRow::SectionTotal { title, duration, .. }
                    if title == "Intro" && *duration == Duration::from_secs(60)
            )
        }));
        assert!(model.rows.iter().any(|row| {
            matches!(
                row,
                TimingBreakdownRow::Slide { index, is_current, .. } if *index == 2 && *is_current
            )
        }));
    }

    #[test]
    fn timing_breakdown_falls_back_to_flat_rows_without_outline() {
        let durations = vec![Duration::from_secs(4), Duration::from_secs(9)];
        let labels = vec!["1".to_owned(), "2".to_owned()];
        let model = PresenterApp::build_timing_breakdown_model(&durations, &labels, 1, None);

        assert!(!model.has_sections);
        assert_eq!(model.rows.len(), 2);
        assert!(
            model
                .rows
                .iter()
                .all(|row| matches!(row, TimingBreakdownRow::Slide { .. }))
        );
    }

    #[test]
    fn timing_breakdown_window_toggle_is_stable() {
        let mut state = TimingBreakdownState::default();
        assert!(!state.is_open);
        state.toggle();
        assert!(state.is_open);
        state.toggle();
        assert!(!state.is_open);
    }

    #[test]
    fn timing_breakdown_model_updates_with_elapsed_time() {
        let now = Instant::now();
        let mut state = PresentationState::with_labels(2, vec!["1".into(), "2".into()], now);
        state.apply(AppCommand::NextSlide, now + Duration::from_secs(3));

        let labels = vec!["1".to_owned(), "2".to_owned()];
        let early = PresenterApp::build_timing_breakdown_model(
            &state.timing_breakdown(now + Duration::from_secs(6)),
            &labels,
            state.current_index(),
            None,
        );
        let late = PresenterApp::build_timing_breakdown_model(
            &state.timing_breakdown(now + Duration::from_secs(12)),
            &labels,
            state.current_index(),
            None,
        );

        let early_slide_2 = early
            .rows
            .iter()
            .find_map(|row| match row {
                TimingBreakdownRow::Slide {
                    index, duration, ..
                } if *index == 1 => Some(*duration),
                _ => None,
            })
            .expect("slide 2 should be present");
        let late_slide_2 = late
            .rows
            .iter()
            .find_map(|row| match row {
                TimingBreakdownRow::Slide {
                    index, duration, ..
                } if *index == 1 => Some(*duration),
                _ => None,
            })
            .expect("slide 2 should be present");

        assert!(late_slide_2 > early_slide_2);
    }

    #[test]
    fn shortcut_bindings_use_defaults_when_missing() {
        let (bindings, warnings) = ShortcutBindings::from_config(&KeybindingConfig {
            bindings: BTreeMap::new(),
        });
        assert!(warnings.is_empty());
        assert_eq!(
            bindings.key_for(ShortcutAction::Next),
            Some(Key::ArrowRight)
        );
        assert_eq!(bindings.key_for(ShortcutAction::SwapScreens), Some(Key::S));
        assert_eq!(
            bindings.key_for(ShortcutAction::ToggleTimingBreakdown),
            Some(Key::B)
        );
        assert_eq!(bindings.key_for(ShortcutAction::UndoStroke), Some(Key::U));
        assert_eq!(bindings.key_for(ShortcutAction::RedoStroke), Some(Key::Y));
        assert_eq!(
            bindings.key_for(ShortcutAction::DeleteSelectedNote),
            Some(Key::Backspace)
        );
        assert_eq!(
            bindings.key_for(ShortcutAction::PreviousPenPreset),
            Some(Key::Q)
        );
        assert_eq!(
            bindings.key_for(ShortcutAction::NextPenPreset),
            Some(Key::W)
        );
        assert_eq!(bindings.key_for(ShortcutAction::ToggleEraser), Some(Key::E));
        assert_eq!(
            bindings.key_for(ShortcutAction::ToggleLassoSelect),
            Some(Key::M)
        );
        assert_eq!(
            bindings.key_for(ShortcutAction::DeleteSelectedStrokes),
            Some(Key::Delete)
        );
        assert_eq!(
            bindings.key_for(ShortcutAction::ApplyPresetToSelectedStrokes),
            Some(Key::K)
        );
    }

    #[test]
    fn shortcut_bindings_apply_valid_override() {
        let mut raw = BTreeMap::new();
        raw.insert("toggle_deck_overview".to_owned(), "O".to_owned());
        let (bindings, warnings) =
            ShortcutBindings::from_config(&KeybindingConfig { bindings: raw });
        assert!(warnings.is_empty());
        assert_eq!(
            bindings.key_for(ShortcutAction::ToggleDeckOverview),
            Some(Key::O)
        );
    }

    #[test]
    fn shortcut_bindings_invalid_values_fall_back_to_default() {
        let mut raw = BTreeMap::new();
        raw.insert("swap_screens".to_owned(), "Ctrl+S".to_owned());
        let (bindings, warnings) =
            ShortcutBindings::from_config(&KeybindingConfig { bindings: raw });
        assert!(
            warnings.iter().any(|entry| entry.contains("Invalid key")),
            "invalid shortcut should emit warning"
        );
        assert_eq!(bindings.key_for(ShortcutAction::SwapScreens), Some(Key::S));
    }

    #[test]
    fn shortcut_bindings_conflicts_keep_first_binding() {
        let mut raw = BTreeMap::new();
        raw.insert("next".to_owned(), "X".to_owned());
        raw.insert("prev".to_owned(), "X".to_owned());
        let (bindings, warnings) =
            ShortcutBindings::from_config(&KeybindingConfig { bindings: raw });
        assert_eq!(bindings.key_for(ShortcutAction::Next), Some(Key::X));
        assert_eq!(
            bindings.key_for(ShortcutAction::Previous),
            Some(Key::ArrowLeft)
        );
        assert!(
            warnings.iter().any(|entry| entry.contains("conflicts")),
            "conflict should emit warning"
        );
    }

    #[test]
    fn transform_rect_move_clamps_to_page_bounds() {
        let start = crate::document::PdfRect {
            left: 0.8,
            bottom: 0.8,
            right: 0.95,
            top: 0.95,
        };
        let moved =
            PresenterApp::transform_rect(start, (0.8, 0.8), (1.2, 1.2), TransformMode::Move);
        assert!(moved.right <= 1.0);
        assert!(moved.top <= 1.0);
    }

    #[test]
    fn transform_rect_resize_preserves_minimum_size() {
        let start = crate::document::PdfRect {
            left: 0.4,
            bottom: 0.4,
            right: 0.6,
            top: 0.6,
        };
        let resized = PresenterApp::transform_rect(
            start,
            (0.4, 0.4),
            (0.59, 0.59),
            TransformMode::Resize(ResizeHandle::SouthWest),
        );
        assert!(resized.right - resized.left >= 0.0195);
        assert!(resized.top - resized.bottom >= 0.0195);
    }

    #[test]
    fn parse_tags_deduplicates_and_trims() {
        let parsed = PresenterApp::parse_tags("alpha, beta,alpha , , gamma");
        assert_eq!(parsed, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn straight_line_constrain_prefers_dominant_axis() {
        let anchor = (0.2, 0.3);
        let mostly_horizontal = PresenterApp::constrain_straight_line(anchor, (0.8, 0.4));
        assert!((mostly_horizontal.0 - 0.8).abs() < 0.0001);
        assert!((mostly_horizontal.1 - 0.3).abs() < 0.0001);

        let mostly_vertical = PresenterApp::constrain_straight_line(anchor, (0.25, 0.9));
        assert!((mostly_vertical.0 - 0.2).abs() < 0.0001);
        assert!((mostly_vertical.1 - 0.9).abs() < 0.0001);
    }

    #[test]
    fn pen_style_sanitize_clamps_color_and_width() {
        let (color, width) = PresenterApp::sanitize_pen_style_inputs((1.5, -0.3, 0.4), 42.0);
        assert_eq!(color, (1.0, 0.0, 0.4));
        assert_eq!(width, 24.0);

        let (color, width) = PresenterApp::sanitize_pen_style_inputs((-0.4, 0.5, 1.6), 0.1);
        assert_eq!(color, (0.0, 0.5, 1.0));
        assert_eq!(width, 0.5);
    }

    #[test]
    fn lasso_point_in_polygon_detects_inside_and_outside() {
        let polygon = vec![(0.2, 0.2), (0.8, 0.2), (0.8, 0.8), (0.2, 0.8)];
        assert!(PresenterApp::point_in_polygon((0.5, 0.5), &polygon));
        assert!(!PresenterApp::point_in_polygon((0.95, 0.5), &polygon));
    }

    #[test]
    fn lasso_segments_intersect_detects_crossing_and_disjoint() {
        assert!(PresenterApp::segments_intersect(
            (0.0, 0.0),
            (1.0, 1.0),
            (0.0, 1.0),
            (1.0, 0.0),
        ));
        assert!(!PresenterApp::segments_intersect(
            (0.0, 0.0),
            (0.4, 0.0),
            (0.0, 1.0),
            (0.4, 1.0),
        ));
    }

    #[test]
    fn lasso_stroke_selection_uses_inside_or_boundary_intersection() {
        let polygon = vec![(0.3, 0.3), (0.7, 0.3), (0.7, 0.7), (0.3, 0.7)];
        let inside_stroke = vec![(0.4, 0.4), (0.6, 0.6)];
        let crossing_stroke = vec![(0.2, 0.5), (0.8, 0.5)];
        let outside_stroke = vec![(0.05, 0.05), (0.15, 0.15)];

        assert!(PresenterApp::stroke_selected_by_lasso(
            &inside_stroke,
            &polygon
        ));
        assert!(PresenterApp::stroke_selected_by_lasso(
            &crossing_stroke,
            &polygon
        ));
        assert!(!PresenterApp::stroke_selected_by_lasso(
            &outside_stroke,
            &polygon
        ));
    }

    #[test]
    fn selected_transform_dirty_detects_rect_only_changes() {
        let base = sample_text_annotation(
            "11:0",
            PdfRect {
                left: 0.1,
                bottom: 0.2,
                right: 0.3,
                top: 0.4,
            },
        );

        let mut originals = BTreeMap::new();
        originals.insert(base.id.clone(), base.clone());

        let mut metadata_changed = base.clone();
        metadata_changed.contents = "edited text".to_owned();
        metadata_changed.metadata.author = "Alice".to_owned();
        assert!(!PresenterApp::selected_transform_dirty_for(
            Some("11:0"),
            &[metadata_changed],
            &originals
        ));

        let mut moved = base.clone();
        moved.rect.left = 0.15;
        assert!(PresenterApp::selected_transform_dirty_for(
            Some("11:0"),
            &[moved],
            &originals
        ));
    }

    #[test]
    fn selected_metadata_dirty_detects_note_or_metadata_edits() {
        let base = sample_text_annotation(
            "12:0",
            PdfRect {
                left: 0.2,
                bottom: 0.2,
                right: 0.4,
                top: 0.45,
            },
        );
        let mut originals = BTreeMap::new();
        originals.insert(base.id.clone(), base.clone());

        assert!(!PresenterApp::selected_metadata_dirty_for(
            Some("12:0"),
            std::slice::from_ref(&base),
            &originals
        ));

        let mut note_changed = base.clone();
        note_changed.contents = "updated".to_owned();
        assert!(PresenterApp::selected_metadata_dirty_for(
            Some("12:0"),
            &[note_changed],
            &originals
        ));

        let mut meta_changed = base.clone();
        meta_changed.metadata.tags = vec!["todo".to_owned()];
        assert!(PresenterApp::selected_metadata_dirty_for(
            Some("12:0"),
            &[meta_changed],
            &originals
        ));
    }

    #[test]
    fn revert_transform_for_selected_restores_original_rect() {
        let original = sample_text_annotation(
            "13:0",
            PdfRect {
                left: 0.3,
                bottom: 0.3,
                right: 0.5,
                top: 0.6,
            },
        );
        let mut moved = original.clone();
        moved.rect = PdfRect {
            left: 0.6,
            bottom: 0.6,
            right: 0.85,
            top: 0.9,
        };

        let mut current = vec![moved];
        let mut originals = BTreeMap::new();
        originals.insert(original.id.clone(), original.clone());

        assert!(PresenterApp::revert_transform_for_selected(
            Some("13:0"),
            &mut current,
            &originals
        ));
        assert_eq!(current[0].rect, original.rect);
        assert!(!PresenterApp::revert_transform_for_selected(
            Some("missing"),
            &mut current,
            &originals
        ));
    }
}
