use crate::config::KeybindingConfig;
use eframe::egui::Key;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ShortcutAction {
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
    pub(super) const ALL: [Self; 36] = [
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

    pub(super) fn id(self) -> &'static str {
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

    pub(super) fn title(self) -> &'static str {
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

    pub(super) fn default_key(self) -> Key {
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
pub(super) struct ShortcutBindings {
    action_to_key: BTreeMap<ShortcutAction, Key>,
}

impl ShortcutBindings {
    pub(super) fn from_config(config: &KeybindingConfig) -> (Self, Vec<String>) {
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

    pub(super) fn key_for(&self, action: ShortcutAction) -> Option<Key> {
        self.action_to_key.get(&action).copied()
    }
}
