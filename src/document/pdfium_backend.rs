use super::pdfium_runtime::with_pdfium;
use crate::config::{NotesLayout, NotesMode, NotesSide, SplitAxis};
use crate::document::{
    DocumentBackend, NewInkAnnotation, NewTextAnnotation, NotesDetectionResult, PageBitmap,
    PageGeometry, PageView, TextAnnotation,
};
use anyhow::{Context, Result, bail};
use pdfium_render::prelude::PdfRenderConfig;
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

        let page_count = with_pdfium(|pdfium| {
            let document = pdfium
                .load_pdf_from_file(path, None)
                .with_context(|| format!("failed to load PDF with PDFium: {}", path.display()))?;
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

    fn crop_vertical(
        rgba: &[u8],
        width: usize,
        height: usize,
        from_x: usize,
        to_x: usize,
    ) -> PageBitmap {
        if width == 0 || height == 0 {
            return PageBitmap {
                width: 1,
                height: 1,
                rgba: vec![255_u8; 4],
            };
        }

        let left = from_x.min(width - 1);
        let right = to_x.min(width).max(left + 1).min(width);
        let cropped_width = right - left;
        let mut cropped = vec![0_u8; cropped_width * height * 4];

        for row in 0..height {
            let src_start = row * width * 4 + left * 4;
            let src_end = row * width * 4 + right * 4;
            let dst_start = row * cropped_width * 4;
            let dst_end = dst_start + cropped_width * 4;
            cropped[dst_start..dst_end].copy_from_slice(&rgba[src_start..src_end]);
        }

        PageBitmap {
            width: cropped_width,
            height,
            rgba: cropped,
        }
    }

    fn crop_horizontal(
        rgba: &[u8],
        width: usize,
        height: usize,
        from_y: usize,
        to_y: usize,
    ) -> PageBitmap {
        if width == 0 || height == 0 {
            return PageBitmap {
                width: 1,
                height: 1,
                rgba: vec![255_u8; 4],
            };
        }

        let top = from_y.min(height - 1);
        let bottom = to_y.min(height).max(top + 1).min(height);
        let cropped_height = bottom - top;
        let mut cropped = vec![0_u8; width * cropped_height * 4];

        for row in 0..cropped_height {
            let src_row = top + row;
            let src_start = src_row * width * 4;
            let src_end = src_start + width * 4;
            let dst_start = row * width * 4;
            let dst_end = dst_start + width * 4;
            cropped[dst_start..dst_end].copy_from_slice(&rgba[src_start..src_end]);
        }

        PageBitmap {
            width,
            height: cropped_height,
            rgba: cropped,
        }
    }

    fn crop_by_layout(full: &PageBitmap, layout: NotesLayout, view: PageView) -> PageBitmap {
        if view == PageView::Full {
            return full.clone();
        }

        let split_ratio = layout.split_ratio.clamp(0.0, 1.0);
        match layout.axis {
            SplitAxis::Vertical => {
                let split_x = ((full.width as f32) * split_ratio).round() as usize;
                let (notes_from, notes_to) = match layout.notes_side {
                    NotesSide::Left => (0usize, split_x),
                    NotesSide::Right => (split_x, full.width),
                    NotesSide::Top | NotesSide::Bottom => (split_x, full.width),
                };
                let (slide_from, slide_to) = if matches!(layout.notes_side, NotesSide::Left) {
                    (split_x, full.width)
                } else {
                    (0usize, split_x)
                };

                match view {
                    PageView::SlideOnly => Self::crop_vertical(
                        &full.rgba,
                        full.width,
                        full.height,
                        slide_from,
                        slide_to,
                    ),
                    PageView::NotesOnly => Self::crop_vertical(
                        &full.rgba,
                        full.width,
                        full.height,
                        notes_from,
                        notes_to,
                    ),
                    PageView::Full => full.clone(),
                }
            }
            SplitAxis::Horizontal => {
                let split_y = ((full.height as f32) * split_ratio).round() as usize;
                let (notes_from, notes_to) = match layout.notes_side {
                    NotesSide::Top => (0usize, split_y),
                    NotesSide::Bottom => (split_y, full.height),
                    NotesSide::Left | NotesSide::Right => (split_y, full.height),
                };
                let (slide_from, slide_to) = if matches!(layout.notes_side, NotesSide::Top) {
                    (split_y, full.height)
                } else {
                    (0usize, split_y)
                };

                match view {
                    PageView::SlideOnly => Self::crop_horizontal(
                        &full.rgba,
                        full.width,
                        full.height,
                        slide_from,
                        slide_to,
                    ),
                    PageView::NotesOnly => Self::crop_horizontal(
                        &full.rgba,
                        full.width,
                        full.height,
                        notes_from,
                        notes_to,
                    ),
                    PageView::Full => full.clone(),
                }
            }
        }
    }

    fn view_scale_factor(view: PageView, layout: Option<NotesLayout>) -> f32 {
        let Some(layout) = layout else {
            return 1.0;
        };
        if view == PageView::Full {
            return 1.0;
        }

        let split = layout.split_ratio.clamp(0.0, 1.0);
        let (fraction_x, fraction_y) = match layout.axis {
            SplitAxis::Vertical => {
                let notes_fraction = if layout.notes_side == NotesSide::Left {
                    split
                } else {
                    1.0 - split
                };
                let slide_fraction = 1.0 - notes_fraction;
                let selected = if view == PageView::NotesOnly {
                    notes_fraction
                } else {
                    slide_fraction
                };
                (selected, 1.0)
            }
            SplitAxis::Horizontal => {
                let notes_fraction = if layout.notes_side == NotesSide::Top {
                    split
                } else {
                    1.0 - split
                };
                let slide_fraction = 1.0 - notes_fraction;
                let selected = if view == PageView::NotesOnly {
                    notes_fraction
                } else {
                    slide_fraction
                };
                (1.0, selected)
            }
        };

        let min_fraction = fraction_x.min(fraction_y).clamp(0.1, 1.0);
        (1.0 / min_fraction).clamp(1.0, 3.0)
    }

    fn effective_render_width(
        target_width: usize,
        view: PageView,
        layout: Option<NotesLayout>,
    ) -> usize {
        let base = target_width.clamp(200, 4096) as f32;
        let scale = Self::view_scale_factor(view, layout);
        (base * scale).round().clamp(200.0, 4096.0) as usize
    }

    fn render_full_page(&self, index: usize, target_width: usize) -> Result<PageBitmap> {
        with_pdfium(|pdfium| {
            let document = pdfium
                .load_pdf_from_file(&self.path, None)
                .with_context(|| {
                    format!("failed to load PDF with PDFium: {}", self.path.display())
                })?;

            let page = document.pages().get(self.check_index(index)?)?;
            let render_config = PdfRenderConfig::new()
                .set_target_width(target_width.clamp(200, 4096) as i32)
                .set_maximum_height(4096)
                .render_annotations(true)
                .render_form_data(true);
            let rendered = page.render_with_config(&render_config)?;

            Ok(PageBitmap {
                width: rendered.width() as usize,
                height: rendered.height() as usize,
                rgba: rendered.as_rgba_bytes(),
            })
        })
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

    fn render_page_bitmap(
        &self,
        index: usize,
        view: PageView,
        layout: Option<NotesLayout>,
        target_width: usize,
    ) -> Result<PageBitmap> {
        let full_target_width = Self::effective_render_width(target_width, view, layout);
        let full = self.render_full_page(index, full_target_width)?;
        if let Some(layout) = layout {
            return Ok(Self::crop_by_layout(&full, layout, view));
        }
        Ok(full)
    }

    fn page_geometry(&self, index: usize) -> Result<PageGeometry> {
        with_pdfium(|pdfium| {
            let document = pdfium
                .load_pdf_from_file(&self.path, None)
                .with_context(|| {
                    format!("failed to load PDF with PDFium: {}", self.path.display())
                })?;
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

    fn reload_if_changed(&mut self) -> Result<bool> {
        let metadata = std::fs::metadata(&self.path)
            .with_context(|| format!("failed to read metadata for {}", self.path.display()))?;
        let modified = metadata.modified().ok();
        let changed = match (self.modified_at, modified) {
            (Some(previous), Some(current)) => current > previous,
            (None, Some(_)) => true,
            _ => false,
        };

        if !changed {
            return Ok(false);
        }

        let reloaded = Self::open(&self.path)?;
        self.page_count = reloaded.page_count;
        self.modified_at = reloaded.modified_at;
        Ok(true)
    }
}
