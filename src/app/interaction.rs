use crate::document::PdfRect;
use eframe::egui::{Pos2, Rect, Vec2};
use std::collections::BTreeSet;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InteractionMode {
    Navigate,
    HighlightDraw,
    AnnotationEdit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HighlightTool {
    Draw,
    Eraser,
    Lasso,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SlideRegion {
    pub(super) min_x: f32,
    pub(super) max_x: f32,
    pub(super) min_y: f32,
    pub(super) max_y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SlideSurfaceSpace {
    FullPage,
    CroppedSlide,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct LaserSurfaceSample {
    pub(super) surface_rect: Rect,
    pub(super) pointer_pos: Option<Pos2>,
    pub(super) ctrl_down: bool,
    pub(super) primary_down: bool,
    pub(super) hovered: bool,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ZoomPanState {
    pub(super) zoom: f32,
    pub(super) pan: Vec2,
}

impl Default for ZoomPanState {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan: Vec2::ZERO,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ResizeHandle {
    NorthWest,
    NorthEast,
    SouthWest,
    SouthEast,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum TransformMode {
    Move,
    Resize(ResizeHandle),
}

#[derive(Debug, Clone)]
pub(super) struct ActiveTextTransform {
    pub(super) annotation_id: String,
    pub(super) start_pointer: (f32, f32),
    pub(super) start_rect: PdfRect,
    pub(super) mode: TransformMode,
}

#[derive(Debug, Clone)]
pub(super) struct SessionInkStroke {
    pub(super) annotation_id: String,
    pub(super) points: Vec<(f32, f32)>,
    pub(super) color_rgb: (f32, f32, f32),
    pub(super) width: f32,
    pub(super) opacity: f32,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct PenStrokeStyle {
    pub(super) color_rgb: (f32, f32, f32),
    pub(super) width: f32,
    pub(super) opacity: f32,
}

#[derive(Debug, Clone)]
pub(super) struct HighlightOverlaySnapshot<'a> {
    pub(super) strokes: &'a [SessionInkStroke],
    pub(super) selected_stroke_ids: &'a BTreeSet<String>,
    pub(super) stroke_in_progress: &'a [(f32, f32)],
    pub(super) stroke_in_progress_style: Option<PenStrokeStyle>,
    pub(super) active_highlight_tool: HighlightTool,
    pub(super) lasso_path_in_progress: &'a [(f32, f32)],
}

#[derive(Debug, Clone, Copy)]
pub(super) struct HighlightOverlaySurface {
    pub(super) image_rect: Rect,
    pub(super) clip_rect: Rect,
    pub(super) region: SlideRegion,
    pub(super) surface_space: SlideSurfaceSpace,
}

#[derive(Debug, Clone)]
pub(super) enum PendingUnsavedAction {
    Load(PathBuf),
    Close,
}
