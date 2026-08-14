use super::interaction::{
    ActiveTextTransform, HighlightTool, PenStrokeStyle, PendingUnsavedAction, SessionInkStroke,
    ZoomPanState,
};
use super::overview::DeckOverviewState;
use super::render_cache::CachedTexture;
use super::render_worker::RenderWorker;
use super::shortcuts::ShortcutBindings;
use super::state::PresentationState;
use super::timing::TimingBreakdownState;
use crate::cache::PageCache;
use crate::config::{AppConfig, NotesLayout, NotesLayoutOverrideConfig};
use crate::document::{DocumentBackend, TextAnnotation};
use eframe::egui::{TextureHandle, ViewportId};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Instant;

#[derive(Debug, Clone)]
pub(super) struct CachedResolvedLayout {
    pub(super) layout: Option<NotesLayout>,
    pub(super) reason: Option<String>,
    pub(super) warning: Option<String>,
}

pub(super) struct PresenterApp {
    pub(super) config: AppConfig,
    pub(super) config_path: PathBuf,
    pub(super) state: PresentationState,
    pub(super) document: Option<Box<dyn DocumentBackend>>,
    pub(super) source_path_input: String,
    pub(super) go_to_input: String,
    pub(super) open_error: Option<String>,
    pub(super) status_message: Option<String>,
    pub(super) last_reload_check: Instant,
    pub(super) non_critical_texture_refresh_after: Option<Instant>,
    pub(super) render_worker: RenderWorker,
    pub(super) render_document_generation: u64,
    pub(super) projector_viewport: ViewportId,
    pub(super) single_monitor_zoom_share: bool,
    pub(super) texture_cache: PageCache<CachedTexture>,
    pub(super) projector_texture: Option<TextureHandle>,
    pub(super) projector_texture_key: Option<usize>,
    pub(super) projector_low_res_texture_key: Option<usize>,
    pub(super) projector_zoom_pan: ZoomPanState,
    pub(super) current_texture: Option<TextureHandle>,
    pub(super) current_texture_key: Option<usize>,
    pub(super) current_low_res_texture_key: Option<usize>,
    pub(super) presenter_zoom_pan: ZoomPanState,
    pub(super) presenter_texture: Option<TextureHandle>,
    pub(super) presenter_texture_key: Option<usize>,
    pub(super) presenter_texture_pending: bool,
    pub(super) presenter_texture_low_res: bool,
    pub(super) presenter_low_res_texture_key: Option<usize>,
    pub(super) notes_texture: Option<TextureHandle>,
    pub(super) notes_texture_key: Option<usize>,
    pub(super) notes_texture_pending: bool,
    pub(super) notes_texture_low_res: bool,
    pub(super) notes_low_res_texture_key: Option<usize>,
    pub(super) next_texture: Option<TextureHandle>,
    pub(super) next_texture_key: Option<usize>,
    pub(super) next_texture_pending: bool,
    pub(super) next_texture_low_res: bool,
    pub(super) next_low_res_texture_key: Option<usize>,
    pub(super) current_document_fingerprint: Option<String>,
    pub(super) manual_layout_override: NotesLayoutOverrideConfig,
    pub(super) resolved_layout: Option<NotesLayout>,
    pub(super) resolved_layout_key: Option<usize>,
    pub(super) resolved_layout_cache: BTreeMap<usize, CachedResolvedLayout>,
    pub(super) layout_reason: Option<String>,
    pub(super) layout_warning: Option<String>,
    pub(super) deck_overview: DeckOverviewState,
    pub(super) annotation_edit_enabled: bool,
    pub(super) pending_text_annotation: String,
    pub(super) text_annotations: Vec<TextAnnotation>,
    pub(super) original_text_annotations: BTreeMap<String, TextAnnotation>,
    pub(super) text_annotation_cache_index: Option<usize>,
    pub(super) selected_annotation_id: Option<String>,
    pub(super) active_text_transform: Option<ActiveTextTransform>,
    pub(super) stroke_in_progress: Vec<(f32, f32)>,
    pub(super) stroke_in_progress_style: Option<PenStrokeStyle>,
    pub(super) lasso_path_in_progress: Vec<(f32, f32)>,
    pub(super) selected_stroke_ids: BTreeSet<String>,
    pub(super) active_highlight_tool: HighlightTool,
    pub(super) unsaved_strokes: BTreeMap<usize, Vec<SessionInkStroke>>,
    pub(super) stroke_undo_stack: BTreeMap<usize, Vec<SessionInkStroke>>,
    pub(super) eraser_erased_in_drag: BTreeSet<String>,
    pub(super) eraser_removed_this_drag: bool,
    pub(super) laser_position: Option<(f32, f32)>,
    pub(super) temporary_laser_active: bool,
    pub(super) laser_persistent_seen_this_frame: bool,
    pub(super) laser_primary_down_seen_this_frame: bool,
    pub(super) auto_override_editor_secs: u32,
    pub(super) auto_override_editor_index: Option<usize>,
    pub(super) timing_breakdown: TimingBreakdownState,
    pub(super) performance_panel_open: bool,
    pub(super) shortcut_bindings: ShortcutBindings,
    pub(super) keybinding_editor: BTreeMap<String, String>,
    pub(super) keybinding_editor_dirty: bool,
    pub(super) keybinding_warning: Option<String>,
    pub(super) unsaved_toast_until: Option<Instant>,
    pub(super) unsaved_toast_text: Option<String>,
    pub(super) non_highlight_unsaved_changes: bool,
    pub(super) pending_unsaved_action: Option<PendingUnsavedAction>,
    pub(super) allow_close_without_guard: bool,
}
