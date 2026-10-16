mod layout_detection;
mod pdfium_backend;
mod pdfium_renderer;
mod pdfium_runtime;

use crate::cache::CacheWeight;
use crate::config::{NotesLayout, NotesMode};
use anyhow::Result;
pub(crate) use layout_detection::detect_notes_layout_from_path;
pub use pdfium_backend::PdfiumBackend;
pub(crate) use pdfium_renderer::render_page_bitmap_from_path;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct PageBitmap {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

impl CacheWeight for PageBitmap {
    fn cache_weight_bytes(&self) -> usize {
        self.rgba.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageView {
    Full,
    SlideOnly,
    NotesOnly,
}

#[derive(Debug, Clone)]
pub struct NotesDetectionResult {
    pub layout: Option<NotesLayout>,
    pub confidence: f32,
    pub reason: String,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PdfRect {
    pub left: f32,
    pub bottom: f32,
    pub right: f32,
    pub top: f32,
}

impl PdfRect {
    pub fn normalized(self) -> Self {
        let left = self.left.min(self.right);
        let right = self.left.max(self.right);
        let bottom = self.bottom.min(self.top);
        let top = self.bottom.max(self.top);
        Self {
            left,
            bottom,
            right,
            top,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageGeometry {
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextAnnotation {
    pub id: String,
    pub page_index: usize,
    pub rect: PdfRect,
    pub contents: String,
    pub metadata: AnnotationMetadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationPriority {
    Low,
    Medium,
    High,
}

impl AnnotationPriority {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "low" => Self::Low,
            "high" => Self::High,
            _ => Self::Medium,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationStatus {
    Open,
    InProgress,
    Done,
}

impl AnnotationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::InProgress => "in_progress",
            Self::Done => "done",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "done" => Self::Done,
            "in_progress" | "in-progress" | "progress" => Self::InProgress,
            _ => Self::Open,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationMetadata {
    pub author: String,
    pub color_rgb: (f32, f32, f32),
    pub modified_at: Option<String>,
    pub tags: Vec<String>,
    pub priority: AnnotationPriority,
    pub status: AnnotationStatus,
}

impl Default for AnnotationMetadata {
    fn default() -> Self {
        Self {
            author: "PDF Presenter".to_owned(),
            color_rgb: (1.0, 1.0, 0.0),
            modified_at: None,
            tags: Vec::new(),
            priority: AnnotationPriority::Medium,
            status: AnnotationStatus::Open,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct InkStroke {
    pub points: Vec<(f32, f32)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewTextAnnotation {
    pub rect: PdfRect,
    pub contents: String,
    pub metadata: AnnotationMetadata,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewInkAnnotation {
    pub rect: PdfRect,
    pub strokes: Vec<InkStroke>,
    pub color_rgb: (f32, f32, f32),
    pub opacity: f32,
    pub width: f32,
}

/// Implementations are `Send` so documents can be opened on a background thread.
pub trait DocumentBackend: Send {
    fn source_path(&self) -> &Path;
    fn page_count(&self) -> usize;
    fn page_label(&self, index: usize) -> String;
    fn resolve_label(&self, label: &str) -> Option<usize>;
    fn notes_for_page(&self, index: usize, mode: NotesMode) -> Option<String>;
    fn detect_notes_layout(&self, index: usize, mode: NotesMode) -> Result<NotesDetectionResult>;
    /// Returns the notes layout when it can be determined without rendering the page.
    /// `None` means detection needs a render, which [`detect_notes_layout_from_path`] performs
    /// off the UI thread.
    fn detect_notes_layout_without_render(
        &self,
        index: usize,
        mode: NotesMode,
    ) -> Option<Result<NotesDetectionResult>>;
    fn render_page_bitmap(
        &self,
        index: usize,
        view: PageView,
        layout: Option<NotesLayout>,
        target_width: usize,
    ) -> Result<PageBitmap>;
    fn page_geometry(&self, index: usize) -> Result<PageGeometry>;
    /// Takes `&mut self` because listing assigns ids to annotations seen for the first time.
    fn list_text_annotations(&mut self, index: usize) -> Result<Vec<TextAnnotation>>;
    fn add_text_annotation(
        &mut self,
        index: usize,
        annotation: NewTextAnnotation,
    ) -> Result<TextAnnotation>;
    fn update_text_annotation(&mut self, annotation: &TextAnnotation) -> Result<()>;
    fn delete_annotation(&mut self, annotation_id: &str) -> Result<()>;
    fn add_ink_annotation(&mut self, index: usize, annotation: NewInkAnnotation) -> Result<String>;
    fn has_unsaved_changes(&self) -> bool;
    fn save_in_place(&mut self) -> Result<()>;
    fn save_as(&mut self, path: &Path) -> Result<()>;
    /// Reports whether the file was modified externally since it was opened or saved.
    fn has_changed_on_disk(&self) -> Result<bool>;
    fn outline_path_for_page(&self, _index: usize) -> Option<Vec<String>> {
        None
    }
}

pub fn open_document(path: impl AsRef<Path>) -> Result<Box<dyn DocumentBackend>> {
    Ok(Box::new(PdfiumBackend::open(path.as_ref())?))
}

#[cfg(test)]
mod tests {
    use super::{AnnotationPriority, AnnotationStatus, NotesLayout, PageView, open_document};
    use std::path::PathBuf;

    #[test]
    fn priority_parser_is_case_insensitive_with_medium_fallback() {
        assert_eq!(AnnotationPriority::parse("LOW"), AnnotationPriority::Low);
        assert_eq!(
            AnnotationPriority::parse(" high "),
            AnnotationPriority::High
        );
        assert_eq!(
            AnnotationPriority::parse("unknown"),
            AnnotationPriority::Medium
        );
    }

    #[test]
    fn status_parser_accepts_common_variants_and_defaults_open() {
        assert_eq!(AnnotationStatus::parse("done"), AnnotationStatus::Done);
        assert_eq!(
            AnnotationStatus::parse("in-progress"),
            AnnotationStatus::InProgress
        );
        assert_eq!(
            AnnotationStatus::parse("IN_PROGRESS"),
            AnnotationStatus::InProgress
        );
        assert_eq!(
            AnnotationStatus::parse("progress"),
            AnnotationStatus::InProgress
        );
        assert_eq!(AnnotationStatus::parse("todo"), AnnotationStatus::Open);
    }

    #[test]
    fn open_document_renders_the_secondary_fixture_when_present() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("978-4-431-55978-8.pdf");
        if !fixture.exists() {
            return;
        }

        let backend = open_document(&fixture).expect("fixture should open");
        assert!(backend.page_count() > 1, "backend should expose page count");

        let rendered = backend
            .render_page_bitmap(0, PageView::Full, None::<NotesLayout>, 1000)
            .expect("backend should render first page");
        assert!(rendered.width > 0);
        assert!(rendered.height > 0);
    }
}
