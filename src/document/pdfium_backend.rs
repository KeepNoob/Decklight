use super::pdfium_renderer::render_page_bitmap_from_path;
use super::pdfium_runtime::with_pdfium_document;
use crate::config::{NotesLayout, NotesMode, NotesSide, SplitAxis};
use crate::document::{
    DocumentBackend, NewInkAnnotation, NewTextAnnotation, NotesDetectionResult, PageBitmap,
    PageGeometry, PageView, TextAnnotation,
};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const READ_ONLY_MESSAGE: &str =
    "annotation editing is unavailable for PDFs loaded via PDFium fallback";

pub struct PdfiumFallbackBackend {
    path: PathBuf,
    page_count: usize,
    modified_at: Option<SystemTime>,
}

impl PdfiumFallbackBackend {
    pub fn open(path: &Path) -> Result<Self> {
        if !path.exists() {
            bail!("document not found: {}", path.display());
        }

        let page_count = with_pdfium_document(path, |document| {
            let page_count = document.pages().len() as usize;
            if page_count == 0 {
                bail!("document has no pages: {}", path.display());
            }
            Ok(page_count)
        })?;

        let metadata = std::fs::metadata(path)
            .with_context(|| format!("failed to read metadata for {}", path.display()))?;

        Ok(Self {
            path: path.to_path_buf(),
            page_count,
            modified_at: metadata.modified().ok(),
        })
    }

    fn check_index(&self, index: usize) -> Result<i32> {
        if index >= self.page_count {
            bail!("page index out of bounds: {index}");
        }
        index
            .try_into()
            .map_err(|_| anyhow::anyhow!("page index out of range for PDFium: {index}"))
    }

    fn detect_layout_from_mode(mode: NotesMode) -> NotesDetectionResult {
        match mode {
            NotesMode::PdfAnnotations => NotesDetectionResult {
                layout: None,
                confidence: 1.0,
                reason: "PDF annotations mode uses full-page rendering.".to_owned(),
                warning: None,
            },
            NotesMode::Auto => NotesDetectionResult {
                layout: None,
                confidence: 0.0,
                reason: "PDFium fallback backend: auto split detection unavailable.".to_owned(),
                warning: Some(
                    "Using full-page rendering; set a manual split if needed.".to_owned(),
                ),
            },
            NotesMode::BeamerSecondScreen => NotesDetectionResult {
                layout: Some(NotesLayout {
                    axis: SplitAxis::Vertical,
                    split_ratio: 0.5,
                    notes_side: NotesSide::Right,
                    confidence: 0.25,
                }),
                confidence: 0.25,
                reason: "PDFium fallback backend: using Beamer default split.".to_owned(),
                warning: Some(
                    "Validate split manually; automatic detection is unavailable.".to_owned(),
                ),
            },
            NotesMode::LibreOfficeNotes => NotesDetectionResult {
                layout: Some(NotesLayout {
                    axis: SplitAxis::Horizontal,
                    split_ratio: 0.45,
                    notes_side: NotesSide::Bottom,
                    confidence: 0.25,
                }),
                confidence: 0.25,
                reason: "PDFium fallback backend: using LibreOffice default split.".to_owned(),
                warning: Some(
                    "Validate split manually; automatic detection is unavailable.".to_owned(),
                ),
            },
        }
    }
}

impl DocumentBackend for PdfiumFallbackBackend {
    fn source_path(&self) -> &Path {
        &self.path
    }

    fn page_count(&self) -> usize {
        self.page_count
    }

    fn page_label(&self, index: usize) -> String {
        format!("{}", index + 1)
    }

    fn resolve_label(&self, label: &str) -> Option<usize> {
        let page_number = label.trim().parse::<usize>().ok()?;
        if page_number == 0 || page_number > self.page_count {
            return None;
        }
        Some(page_number - 1)
    }

    fn notes_for_page(&self, index: usize, mode: NotesMode) -> Option<String> {
        match self.detect_notes_layout(index, mode) {
            Ok(result) => {
                let warning_suffix = result
                    .warning
                    .as_ref()
                    .map(|warning| format!("; warning: {warning}"))
                    .unwrap_or_default();
                Some(format!(
                    "{} (confidence: {:.2}{warning_suffix})",
                    result.reason, result.confidence
                ))
            }
            Err(_) => None,
        }
    }

    fn detect_notes_layout(&self, index: usize, mode: NotesMode) -> Result<NotesDetectionResult> {
        self.check_index(index)?;
        Ok(Self::detect_layout_from_mode(mode))
    }

    fn detect_notes_layout_without_render(
        &self,
        index: usize,
        mode: NotesMode,
    ) -> Option<Result<NotesDetectionResult>> {
        Some(self.detect_notes_layout(index, mode))
    }

    fn render_page_bitmap(
        &self,
        index: usize,
        view: PageView,
        layout: Option<NotesLayout>,
        target_width: usize,
    ) -> Result<PageBitmap> {
        render_page_bitmap_from_path(&self.path, index, view, layout, target_width)
    }

    fn page_geometry(&self, index: usize) -> Result<PageGeometry> {
        with_pdfium_document(&self.path, |document| {
            let page = document.pages().get(self.check_index(index)?)?;

            Ok(PageGeometry {
                width: page.width().value.abs().max(1.0),
                height: page.height().value.abs().max(1.0),
            })
        })
    }

    fn list_text_annotations(&self, _index: usize) -> Result<Vec<TextAnnotation>> {
        Ok(Vec::new())
    }

    fn add_text_annotation(
        &mut self,
        _index: usize,
        _annotation: NewTextAnnotation,
    ) -> Result<TextAnnotation> {
        bail!("{READ_ONLY_MESSAGE}");
    }

    fn update_text_annotation(&mut self, _annotation: &TextAnnotation) -> Result<()> {
        bail!("{READ_ONLY_MESSAGE}");
    }

    fn delete_annotation(&mut self, _annotation_id: &str) -> Result<()> {
        bail!("{READ_ONLY_MESSAGE}");
    }

    fn add_ink_annotation(
        &mut self,
        _index: usize,
        _annotation: NewInkAnnotation,
    ) -> Result<String> {
        bail!("{READ_ONLY_MESSAGE}");
    }

    fn has_unsaved_changes(&self) -> bool {
        false
    }

    fn save_in_place(&mut self) -> Result<()> {
        bail!("{READ_ONLY_MESSAGE}");
    }

    fn save_as(&mut self, _path: &Path) -> Result<()> {
        bail!("{READ_ONLY_MESSAGE}");
    }

    fn has_changed_on_disk(&self) -> Result<bool> {
        let metadata = std::fs::metadata(&self.path)
            .with_context(|| format!("failed to read metadata for {}", self.path.display()))?;
        Ok(match (self.modified_at, metadata.modified().ok()) {
            (Some(previous), Some(current)) => current > previous,
            (None, Some(_)) => true,
            _ => false,
        })
    }
}
