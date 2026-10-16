use super::layout_detection::detect_notes_layout_from_path;
use super::pdfium_renderer::render_page_bitmap_from_path;
use super::pdfium_runtime::{close_pdfium_document, with_pdfium};
use crate::config::{NotesLayout, NotesMode};
use crate::document::{
    AnnotationMetadata, AnnotationPriority, AnnotationStatus, DocumentBackend, NewInkAnnotation,
    NewTextAnnotation, NotesDetectionResult, PageBitmap, PageGeometry, PageView, PdfRect,
    TextAnnotation,
};
use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use pdfium_render::prelude::{
    PdfBookmark, PdfColor, PdfDocument, PdfPage, PdfPageAnnotationCommon, PdfPageAnnotationType,
    PdfPageContentRegenerationStrategy, PdfPageObjectCommon, PdfPageObjectLineCap,
    PdfPageObjectLineJoin, PdfPageObjectsCommon, PdfPagePathObject, PdfPoints,
    PdfRect as PdfiumRect,
};
use std::collections::BTreeMap;
use std::ffi::c_int;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

const META_PREFIX: &str = "[[PDFPRESENTER_META:";
const META_SUFFIX: &str = "]]";
const MAX_OUTLINE_DEPTH: usize = 32;
const MAX_OUTLINE_ITEMS: usize = 65_536;

static SAVE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A PDF opened with PDFium for both rendering and annotation editing.
///
/// Edits go to a document this backend keeps open, while pages are rendered from the file on
/// disk, so unsaved annotations stay overlays until they are saved.
pub struct PdfiumBackend {
    path: PathBuf,
    /// The file `document` reads from. It differs from `path` only after a save wrote the edits
    /// to a temporary copy but could not move that copy over the original.
    document_source: PathBuf,
    document: Option<PdfDocument<'static>>,
    page_geometries: Vec<PageGeometry>,
    outline_paths_by_page: Option<Vec<Option<Vec<String>>>>,
    /// Annotation ids in each page's annotation order. PDFium addresses annotations by index,
    /// which shifts when one is deleted, so ids are tracked here and updated with every edit.
    annotation_ids_by_page: BTreeMap<usize, Vec<String>>,
    next_annotation_id: u64,
    modified_at: Option<SystemTime>,
    has_unsaved_changes: bool,
}

#[derive(Debug, Clone)]
struct OutlineEntry {
    page_index: usize,
    path: Vec<String>,
}

impl PdfiumBackend {
    pub fn open(path: &Path) -> Result<Self> {
        if !path.exists() {
            bail!("document not found: {}", path.display());
        }

        let metadata = fs::metadata(path)
            .with_context(|| format!("failed to read metadata for {}", path.display()))?;
        let (document, page_geometries, outline_paths_by_page) = with_pdfium(|session| {
            let document = session
                .pdfium()
                .load_pdf_from_file(path, None)
                .with_context(|| format!("failed to load PDF with PDFium: {}", path.display()))?;
            let page_geometries = read_page_geometries(&document)?;
            if page_geometries.is_empty() {
                bail!("document has no pages: {}", path.display());
            }
            let outline_paths_by_page =
                build_outline_paths_by_page(&document, page_geometries.len());
            Ok((document, page_geometries, outline_paths_by_page))
        })?;

        Ok(Self {
            path: path.to_path_buf(),
            document_source: path.to_path_buf(),
            document: Some(document),
            page_geometries,
            outline_paths_by_page,
            annotation_ids_by_page: BTreeMap::new(),
            next_annotation_id: 0,
            modified_at: metadata.modified().ok(),
            has_unsaved_changes: false,
        })
    }

    fn check_index(&self, index: usize) -> Result<()> {
        if index >= self.page_geometries.len() {
            bail!("page index out of bounds: {index}");
        }
        Ok(())
    }

    fn save_to(&mut self, target: &Path) -> Result<()> {
        let temporary = temporary_save_path(target)?;
        let document_slot = &mut self.document;
        let outcome = with_pdfium(|session| {
            let document = document_slot
                .as_ref()
                .ok_or_else(|| anyhow!("document is not open"))?;
            document
                .save_to_file(&temporary)
                .with_context(|| format!("failed to write {}", temporary.display()))?;

            // Both open documents read the old file lazily and Windows cannot replace a file
            // that is open, so close them before moving the saved copy into place.
            session.release_render_document();
            drop(document_slot.take());
            let replaced = fs::rename(&temporary, target);
            let reopen_from = if replaced.is_ok() {
                target
            } else {
                temporary.as_path()
            };
            let reopened = session
                .pdfium()
                .load_pdf_from_file(reopen_from, None)
                .with_context(|| format!("failed to reopen {}", reopen_from.display()))?;
            *document_slot = Some(reopened);
            Ok(replaced)
        });

        match outcome {
            Ok(Ok(())) => {
                self.remove_temporary_source();
                let metadata = fs::metadata(target)
                    .with_context(|| format!("failed to read metadata for {}", target.display()))?;
                self.path = target.to_path_buf();
                self.document_source = target.to_path_buf();
                self.modified_at = metadata.modified().ok();
                self.has_unsaved_changes = false;
                Ok(())
            }
            Ok(Err(rename_error)) => {
                // The edits now live only in the temporary copy, which stays open until a later
                // save succeeds.
                self.remove_temporary_source();
                self.document_source = temporary.clone();
                Err(rename_error).with_context(|| {
                    format!(
                        "failed to replace {} with the saved copy {}",
                        target.display(),
                        temporary.display()
                    )
                })
            }
            Err(error) => {
                if temporary.exists()
                    && let Err(remove_error) = fs::remove_file(&temporary)
                {
                    eprintln!(
                        "failed to remove partial save {}: {remove_error}",
                        temporary.display()
                    );
                }
                Err(error)
            }
        }
    }

    fn remove_temporary_source(&mut self) {
        if self.document_source == self.path {
            return;
        }
        if let Err(error) = fs::remove_file(&self.document_source) {
            eprintln!(
                "failed to remove temporary save {}: {error}",
                self.document_source.display()
            );
        }
        self.document_source = self.path.clone();
    }
}

impl Drop for PdfiumBackend {
    fn drop(&mut self) {
        if let Some(document) = self.document.take() {
            close_pdfium_document(document);
        }
        self.remove_temporary_source();
    }
}

fn read_page_geometries(document: &PdfDocument<'static>) -> Result<Vec<PageGeometry>> {
    let pages = document.pages();
    (0..pages.len())
        .map(|index| {
            let size = pages
                .page_size(index)
                .with_context(|| format!("failed to read size of page {}", index + 1))?;
            Ok(PageGeometry {
                width: size.width().value.abs().max(1.0),
                height: size.height().value.abs().max(1.0),
            })
        })
        .collect()
}

fn load_page(document: &PdfDocument<'static>, index: usize) -> Result<PdfPage<'static>> {
    let page_index = c_int::try_from(index)
        .map_err(|_| anyhow!("page index out of range for PDFium: {index}"))?;
    let mut page = document
        .pages()
        .get(page_index)
        .with_context(|| format!("failed to load page {}", index + 1))?;
    // Annotation edits must leave the page's own content stream untouched.
    page.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
    Ok(page)
}

fn temporary_save_path(target: &Path) -> Result<PathBuf> {
    let file_name = target
        .file_name()
        .ok_or_else(|| anyhow!("save path has no file name: {}", target.display()))?;
    let directory = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let sequence = SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Ok(directory.join(format!(
        ".{}.decklight-save-{}-{sequence}",
        file_name.to_string_lossy(),
        std::process::id()
    )))
}

fn allocate_annotation_id(next_annotation_id: &mut u64, page_index: usize) -> String {
    let id = format!("{page_index}:{next_annotation_id}");
    *next_annotation_id += 1;
    id
}

fn page_index_from_annotation_id(annotation_id: &str) -> Result<usize> {
    let (page, _) = annotation_id
        .split_once(':')
        .ok_or_else(|| anyhow!("invalid annotation id: {annotation_id}"))?;
    page.parse()
        .with_context(|| format!("invalid annotation id: {annotation_id}"))
}

fn annotation_ids_for_page<'ids>(
    ids_by_page: &'ids mut BTreeMap<usize, Vec<String>>,
    next_annotation_id: &mut u64,
    page_index: usize,
    annotation_count: usize,
) -> Result<&'ids mut Vec<String>> {
    let ids = ids_by_page.entry(page_index).or_insert_with(|| {
        (0..annotation_count)
            .map(|_| allocate_annotation_id(next_annotation_id, page_index))
            .collect()
    });
    if ids.len() != annotation_count {
        bail!(
            "annotations on page {} no longer match Decklight's record; reopen the document",
            page_index + 1
        );
    }
    Ok(ids)
}

fn annotation_position(ids: &[String], annotation_id: &str) -> Result<usize> {
    ids.iter()
        .position(|id| id == annotation_id)
        .ok_or_else(|| anyhow!("annotation not found: {annotation_id}"))
}

fn to_pdfium_rect(rect: PdfRect) -> PdfiumRect {
    PdfiumRect::new_from_values(rect.bottom, rect.left, rect.top, rect.right)
}

fn from_pdfium_rect(rect: PdfiumRect) -> PdfRect {
    PdfRect {
        left: rect.left().value,
        bottom: rect.bottom().value,
        right: rect.right().value,
        top: rect.top().value,
    }
    .normalized()
}

fn to_pdfium_color(color_rgb: (f32, f32, f32), opacity: f32) -> PdfColor {
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    PdfColor::new(
        channel(color_rgb.0),
        channel(color_rgb.1),
        channel(color_rgb.2),
        channel(opacity),
    )
}

fn build_outline_paths_by_page(
    document: &PdfDocument<'static>,
    page_count: usize,
) -> Option<Vec<Option<Vec<String>>>> {
    let mut entries = Vec::new();
    let mut visited_items = 0usize;
    collect_outline_entries(
        document.bookmarks().root(),
        &[],
        page_count,
        0,
        &mut visited_items,
        &mut entries,
    );
    if entries.is_empty() {
        return None;
    }

    entries.sort_by_key(|entry| entry.page_index);
    let mut cursor = 0usize;
    let mut active_path: Option<Vec<String>> = None;
    let mut paths = vec![None; page_count];
    for (page_index, slot) in paths.iter_mut().enumerate() {
        while let Some(entry) = entries.get(cursor)
            && entry.page_index <= page_index
        {
            active_path = Some(entry.path.clone());
            cursor += 1;
        }
        *slot = active_path.clone();
    }

    if paths.iter().all(Option::is_none) {
        None
    } else {
        Some(paths)
    }
}

fn collect_outline_entries(
    first: Option<PdfBookmark<'_>>,
    parent_path: &[String],
    page_count: usize,
    depth: usize,
    visited_items: &mut usize,
    entries: &mut Vec<OutlineEntry>,
) {
    if depth > MAX_OUTLINE_DEPTH {
        return;
    }

    let mut current = first;
    while let Some(bookmark) = current {
        // Malformed outlines can loop, so the walk is capped instead of trusting the links.
        if *visited_items >= MAX_OUTLINE_ITEMS {
            return;
        }
        *visited_items += 1;

        let mut path = parent_path.to_vec();
        if let Some(title) = bookmark.title() {
            let title = title.trim();
            if !title.is_empty() {
                path.push(title.to_owned());
            }
        }
        if !path.is_empty()
            && let Some(page_index) = bookmark_page_index(&bookmark, page_count)
        {
            entries.push(OutlineEntry {
                page_index,
                path: path.clone(),
            });
        }

        collect_outline_entries(
            bookmark.first_child(),
            &path,
            page_count,
            depth + 1,
            visited_items,
            entries,
        );
        current = bookmark.next_sibling();
    }
}

fn bookmark_page_index(bookmark: &PdfBookmark<'_>, page_count: usize) -> Option<usize> {
    let page_index = match bookmark.destination() {
        Some(destination) => destination.page_index().ok()?,
        None => {
            let action = bookmark.action()?;
            let local = action.as_local_destination_action()?;
            local.destination().ok()?.page_index().ok()?
        }
    };
    usize::try_from(page_index)
        .ok()
        .filter(|index| *index < page_count)
}

fn metadata_from_annotation(
    author: Option<String>,
    color: Option<PdfColor>,
    modified_at: Option<String>,
    contents_raw: &str,
) -> AnnotationMetadata {
    let mut metadata = AnnotationMetadata::default();

    if let Some(author) = author.map(|author| author.trim().to_owned())
        && !author.is_empty()
    {
        metadata.author = author;
    }
    if let Some(color) = color {
        metadata.color_rgb = (
            f32::from(color.red()) / 255.0,
            f32::from(color.green()) / 255.0,
            f32::from(color.blue()) / 255.0,
        );
    }
    if let Some(modified_at) = modified_at.map(|value| value.trim().to_owned())
        && !modified_at.is_empty()
    {
        metadata.modified_at = Some(modified_at);
    }

    if let Some(custom) = decode_custom_metadata_block(contents_raw) {
        if let Some(tags) = custom.get("tags") {
            metadata.tags = tags
                .split(',')
                .map(str::trim)
                .map(ToOwned::to_owned)
                .filter(|tag| !tag.is_empty())
                .collect();
        }
        if let Some(priority) = custom.get("priority") {
            metadata.priority = AnnotationPriority::parse(priority);
        }
        if let Some(status) = custom.get("status") {
            metadata.status = AnnotationStatus::parse(status);
        }
        if let Some(author) = custom.get("author")
            && !author.trim().is_empty()
        {
            metadata.author = author.trim().to_owned();
        }
    }

    metadata
}

fn encode_custom_metadata_block(metadata: &AnnotationMetadata) -> String {
    let mut tags = metadata
        .tags
        .iter()
        .map(|tag| tag.replace([';', ','], " ").trim().to_owned())
        .filter(|tag| !tag.is_empty())
        .collect::<Vec<_>>();
    tags.sort();
    tags.dedup();

    let payload = format!(
        "author={};tags={};priority={};status={}",
        metadata.author.replace(';', " ").trim(),
        tags.join(","),
        metadata.priority.as_str(),
        metadata.status.as_str()
    );
    format!("{META_PREFIX}{payload}{META_SUFFIX}")
}

fn compose_contents_with_metadata(note_text: &str, metadata: &AnnotationMetadata) -> String {
    let body = note_text.trim_end_matches('\n');
    if body.is_empty() {
        return encode_custom_metadata_block(metadata);
    }
    format!("{}\n{}", encode_custom_metadata_block(metadata), body)
}

fn decode_custom_metadata_block(contents: &str) -> Option<BTreeMap<String, String>> {
    let trimmed = contents.trim_start();
    if !trimmed.starts_with(META_PREFIX) {
        return None;
    }
    let end = trimmed.find(META_SUFFIX)?;
    let payload = &trimmed[META_PREFIX.len()..end];
    let mut decoded = BTreeMap::new();
    for pair in payload
        .split(';')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        decoded.insert(key.trim().to_ascii_lowercase(), value.trim().to_owned());
    }
    Some(decoded)
}

fn strip_custom_metadata_block(contents: &str) -> String {
    let trimmed = contents.trim_start();
    if !trimmed.starts_with(META_PREFIX) {
        return contents.to_owned();
    }
    let Some(end) = trimmed.find(META_SUFFIX) else {
        return contents.to_owned();
    };
    trimmed[end + META_SUFFIX.len()..]
        .trim_start_matches(['\n', '\r'])
        .to_owned()
}

fn sanitize_metadata(mut metadata: AnnotationMetadata) -> AnnotationMetadata {
    metadata.author = metadata.author.trim().to_owned();
    if metadata.author.is_empty() {
        metadata.author = "PDF Presenter".to_owned();
    }
    metadata.color_rgb = (
        metadata.color_rgb.0.clamp(0.0, 1.0),
        metadata.color_rgb.1.clamp(0.0, 1.0),
        metadata.color_rgb.2.clamp(0.0, 1.0),
    );
    metadata.tags = metadata
        .tags
        .into_iter()
        .map(|tag| tag.trim().replace([';', ','], " "))
        .filter(|tag| !tag.is_empty())
        .collect();
    metadata.tags.sort();
    metadata.tags.dedup();
    metadata
}

impl DocumentBackend for PdfiumBackend {
    fn source_path(&self) -> &Path {
        &self.path
    }

    fn page_count(&self) -> usize {
        self.page_geometries.len()
    }

    fn page_label(&self, index: usize) -> String {
        format!("{}", index + 1)
    }

    fn resolve_label(&self, label: &str) -> Option<usize> {
        let page_number = label.trim().parse::<usize>().ok()?;
        if page_number == 0 || page_number > self.page_count() {
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
        if let Some(result) = self.detect_notes_layout_without_render(index, mode) {
            return result;
        }
        detect_notes_layout_from_path(&self.path, index, mode)
    }

    fn detect_notes_layout_without_render(
        &self,
        index: usize,
        mode: NotesMode,
    ) -> Option<Result<NotesDetectionResult>> {
        if let Err(error) = self.check_index(index) {
            return Some(Err(error));
        }
        (mode == NotesMode::PdfAnnotations).then(|| {
            Ok(NotesDetectionResult {
                layout: None,
                confidence: 1.0,
                reason: "PDF annotations mode uses full-page rendering.".to_owned(),
                warning: None,
            })
        })
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
        self.page_geometries
            .get(index)
            .copied()
            .ok_or_else(|| anyhow!("page index out of bounds: {index}"))
    }

    fn list_text_annotations(&mut self, index: usize) -> Result<Vec<TextAnnotation>> {
        self.check_index(index)?;
        let document = self.document.as_ref();
        let ids_by_page = &mut self.annotation_ids_by_page;
        let next_annotation_id = &mut self.next_annotation_id;
        with_pdfium(|_session| {
            let document = document.ok_or_else(|| anyhow!("document is not open"))?;
            let page = load_page(document, index)?;
            let annotations = page.annotations();
            let ids =
                annotation_ids_for_page(ids_by_page, next_annotation_id, index, annotations.len())?;

            let mut listed = Vec::new();
            for (position, annotation) in annotations.iter().enumerate() {
                if !matches!(
                    annotation.annotation_type(),
                    PdfPageAnnotationType::Text | PdfPageAnnotationType::FreeText
                ) {
                    continue;
                }
                let id = ids
                    .get(position)
                    .cloned()
                    .ok_or_else(|| anyhow!("annotation {position} has no id"))?;
                let raw_contents = annotation.contents().unwrap_or_default();
                let metadata = sanitize_metadata(metadata_from_annotation(
                    annotation.creator(),
                    annotation.stroke_color().ok(),
                    annotation.modification_date(),
                    &raw_contents,
                ));
                let rect = annotation
                    .bounds()
                    .map(from_pdfium_rect)
                    .unwrap_or(PdfRect {
                        left: 0.0,
                        bottom: 0.0,
                        right: 100.0,
                        top: 50.0,
                    });
                listed.push(TextAnnotation {
                    id,
                    page_index: index,
                    rect,
                    contents: strip_custom_metadata_block(&raw_contents),
                    metadata,
                });
            }
            Ok(listed)
        })
    }

    fn add_text_annotation(
        &mut self,
        index: usize,
        annotation: NewTextAnnotation,
    ) -> Result<TextAnnotation> {
        self.check_index(index)?;
        let rect = annotation.rect.normalized();
        let mut metadata = sanitize_metadata(annotation.metadata);
        let stored_contents = compose_contents_with_metadata(&annotation.contents, &metadata);
        let document = self.document.as_ref();
        let ids_by_page = &mut self.annotation_ids_by_page;
        let next_annotation_id = &mut self.next_annotation_id;
        let id = with_pdfium(|_session| {
            let document = document.ok_or_else(|| anyhow!("document is not open"))?;
            let mut page = load_page(document, index)?;
            let ids = annotation_ids_for_page(
                ids_by_page,
                next_annotation_id,
                index,
                page.annotations().len(),
            )?;
            let mut created = page
                .annotations_mut()
                .create_text_annotation(&stored_contents)
                .context("failed to create text annotation")?;
            let id = allocate_annotation_id(next_annotation_id, index);
            ids.push(id.clone());

            created.set_bounds(to_pdfium_rect(rect))?;
            created.set_creator(&metadata.author)?;
            created.set_modification_date(Utc::now())?;
            created.set_stroke_color(to_pdfium_color(metadata.color_rgb, 1.0))?;
            created.set_is_printed(true)?;
            metadata.modified_at = created.modification_date();
            Ok(id)
        })?;
        self.has_unsaved_changes = true;

        Ok(TextAnnotation {
            id,
            page_index: index,
            rect,
            contents: annotation.contents,
            metadata,
        })
    }

    fn update_text_annotation(&mut self, annotation: &TextAnnotation) -> Result<()> {
        let index = page_index_from_annotation_id(&annotation.id)?;
        self.check_index(index)?;
        let rect = annotation.rect.normalized();
        let metadata = sanitize_metadata(annotation.metadata.clone());
        let stored_contents = compose_contents_with_metadata(&annotation.contents, &metadata);
        let document = self.document.as_ref();
        let ids_by_page = &mut self.annotation_ids_by_page;
        let next_annotation_id = &mut self.next_annotation_id;
        with_pdfium(|_session| {
            let document = document.ok_or_else(|| anyhow!("document is not open"))?;
            let mut page = load_page(document, index)?;
            let ids = annotation_ids_for_page(
                ids_by_page,
                next_annotation_id,
                index,
                page.annotations().len(),
            )?;
            let position = annotation_position(ids, &annotation.id)?;
            let mut target = page
                .annotations_mut()
                .get(position)
                .with_context(|| format!("failed to load annotation {}", annotation.id))?;
            target.set_bounds(to_pdfium_rect(rect))?;
            target.set_contents(&stored_contents)?;
            target.set_creator(&metadata.author)?;
            target.set_modification_date(Utc::now())?;
            target.set_stroke_color(to_pdfium_color(metadata.color_rgb, 1.0))?;
            Ok(())
        })?;
        self.has_unsaved_changes = true;
        Ok(())
    }

    fn delete_annotation(&mut self, annotation_id: &str) -> Result<()> {
        let index = page_index_from_annotation_id(annotation_id)?;
        self.check_index(index)?;
        let document = self.document.as_ref();
        let ids_by_page = &mut self.annotation_ids_by_page;
        let next_annotation_id = &mut self.next_annotation_id;
        with_pdfium(|_session| {
            let document = document.ok_or_else(|| anyhow!("document is not open"))?;
            let mut page = load_page(document, index)?;
            let ids = annotation_ids_for_page(
                ids_by_page,
                next_annotation_id,
                index,
                page.annotations().len(),
            )?;
            let position = annotation_position(ids, annotation_id)?;
            let annotations = page.annotations_mut();
            let target = annotations
                .get(position)
                .with_context(|| format!("failed to load annotation {annotation_id}"))?;
            annotations
                .delete_annotation(target)
                .with_context(|| format!("failed to delete annotation {annotation_id}"))?;
            ids.remove(position);
            Ok(())
        })?;
        self.has_unsaved_changes = true;
        Ok(())
    }

    fn add_ink_annotation(&mut self, index: usize, annotation: NewInkAnnotation) -> Result<String> {
        self.check_index(index)?;
        let strokes: Vec<&[(f32, f32)]> = annotation
            .strokes
            .iter()
            .map(|stroke| stroke.points.as_slice())
            .filter(|points| points.len() >= 2)
            .collect();
        if strokes.is_empty() {
            bail!("ink annotation requires at least one stroke with two points");
        }
        let rect = annotation.rect.normalized();
        let color = to_pdfium_color(annotation.color_rgb, annotation.opacity.clamp(0.05, 1.0));
        let width = PdfPoints::new(annotation.width.max(0.5));
        let document = self.document.as_ref();
        let ids_by_page = &mut self.annotation_ids_by_page;
        let next_annotation_id = &mut self.next_annotation_id;
        let id = with_pdfium(|_session| {
            let document = document.ok_or_else(|| anyhow!("document is not open"))?;
            let mut page = load_page(document, index)?;
            let ids = annotation_ids_for_page(
                ids_by_page,
                next_annotation_id,
                index,
                page.annotations().len(),
            )?;
            let mut ink = page
                .annotations_mut()
                .create_ink_annotation()
                .context("failed to create ink annotation")?;
            let id = allocate_annotation_id(next_annotation_id, index);
            ids.push(id.clone());

            ink.set_bounds(to_pdfium_rect(rect))?;
            ink.set_stroke_color(color)?;
            ink.set_is_printed(true)?;
            // The strokes are stored as the annotation's appearance, so every viewer draws them
            // the same way without regenerating it from an ink list.
            for points in &strokes {
                let Some(((first_x, first_y), rest)) = points.split_first() else {
                    continue;
                };
                let mut path = PdfPagePathObject::new(
                    document,
                    PdfPoints::new(*first_x),
                    PdfPoints::new(*first_y),
                    Some(color),
                    Some(width),
                    None,
                )?;
                for (x, y) in rest {
                    path.line_to(PdfPoints::new(*x), PdfPoints::new(*y))?;
                }
                path.set_line_cap(PdfPageObjectLineCap::Round)?;
                path.set_line_join(PdfPageObjectLineJoin::Round)?;
                ink.objects_mut().add_path_object(path)?;
            }
            Ok(id)
        })?;
        self.has_unsaved_changes = true;
        Ok(id)
    }

    fn has_unsaved_changes(&self) -> bool {
        self.has_unsaved_changes
    }

    fn save_in_place(&mut self) -> Result<()> {
        let target = self.path.clone();
        self.save_to(&target)
    }

    fn save_as(&mut self, path: &Path) -> Result<()> {
        self.save_to(path)
    }

    fn has_changed_on_disk(&self) -> Result<bool> {
        if self.has_unsaved_changes {
            return Ok(false);
        }

        let metadata = fs::metadata(&self.path)
            .with_context(|| format!("failed to read metadata for {}", self.path.display()))?;
        Ok(match (self.modified_at, metadata.modified().ok()) {
            (Some(previous), Some(current)) => current > previous,
            (None, Some(_)) => true,
            _ => false,
        })
    }

    fn outline_path_for_page(&self, index: usize) -> Option<Vec<String>> {
        self.outline_paths_by_page
            .as_ref()
            .and_then(|paths| paths.get(index))
            .cloned()
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{NotesSide, SplitAxis};
    use crate::document::InkStroke;
    use tempfile::tempdir;

    fn fixture_path() -> Option<PathBuf> {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Information Geometry.pdf");
        fixture.exists().then_some(fixture)
    }

    fn copy_fixture(fixture: &Path, directory: &Path, name: &str) -> PathBuf {
        let copied = directory.join(name);
        fs::copy(fixture, &copied).expect("copy fixture");
        copied
    }

    fn note(contents: &str) -> NewTextAnnotation {
        NewTextAnnotation {
            rect: PdfRect {
                left: 40.0,
                bottom: 40.0,
                right: 180.0,
                top: 120.0,
            },
            contents: contents.to_owned(),
            metadata: AnnotationMetadata::default(),
        }
    }

    fn red_ink() -> NewInkAnnotation {
        NewInkAnnotation {
            rect: PdfRect {
                left: 20.0,
                bottom: 20.0,
                right: 300.0,
                top: 200.0,
            },
            strokes: vec![InkStroke {
                points: vec![(30.0, 30.0), (290.0, 190.0)],
            }],
            color_rgb: (1.0, 0.0, 0.0),
            opacity: 1.0,
            width: 12.0,
        }
    }

    #[test]
    fn metadata_block_round_trip_extracts_note_text() {
        let metadata = AnnotationMetadata {
            author: "Alice".to_owned(),
            color_rgb: (0.2, 0.4, 0.8),
            modified_at: Some("D:20260101120000+0000".to_owned()),
            tags: vec!["tag1".to_owned(), "tag2".to_owned()],
            priority: AnnotationPriority::High,
            status: AnnotationStatus::InProgress,
        };
        let composed = compose_contents_with_metadata("hello world", &metadata);
        assert!(composed.starts_with(META_PREFIX));
        assert_eq!(strip_custom_metadata_block(&composed), "hello world");
        let decoded =
            decode_custom_metadata_block(&composed).expect("metadata block should decode");
        assert_eq!(decoded.get("priority").map(String::as_str), Some("high"));
        assert_eq!(
            decoded.get("status").map(String::as_str),
            Some("in_progress")
        );
    }

    #[test]
    fn malformed_metadata_block_is_not_decoded_or_stripped() {
        let malformed = format!("{META_PREFIX}author=Alice;status=done\nvisible note");
        assert!(decode_custom_metadata_block(&malformed).is_none());
        assert_eq!(strip_custom_metadata_block(&malformed), malformed);
    }

    #[test]
    fn metadata_from_annotation_uses_standard_fields() {
        let metadata = metadata_from_annotation(
            Some("  Bob  ".to_owned()),
            Some(PdfColor::new(51, 102, 153, 255)),
            Some("D:20260202120000+0000".to_owned()),
            "[[DECKLIGHT_META:author=Alice;status=in_progress\nnote body",
        );
        assert_eq!(metadata.author, "Bob");
        assert_eq!(metadata.color_rgb, (0.2, 0.4, 0.6));
        assert_eq!(
            metadata.modified_at.as_deref(),
            Some("D:20260202120000+0000")
        );
        assert_eq!(metadata.priority, AnnotationPriority::Medium);
        assert_eq!(metadata.status, AnnotationStatus::Open);
        assert!(metadata.tags.is_empty());
    }

    #[test]
    fn annotation_ids_encode_their_page() {
        let mut next_annotation_id = 7;
        let id = allocate_annotation_id(&mut next_annotation_id, 12);
        assert_eq!(page_index_from_annotation_id(&id).expect("valid id"), 12);
        assert_eq!(next_annotation_id, 8);
        assert!(page_index_from_annotation_id("not-an-id").is_err());
    }

    #[test]
    fn detects_layouts_and_outline_on_fixture_when_present() {
        let Some(fixture) = fixture_path() else {
            return;
        };

        let backend = PdfiumBackend::open(&fixture).expect("fixture should load");
        assert!(backend.page_count() > 1);
        for mode in [NotesMode::Auto, NotesMode::BeamerSecondScreen] {
            let layout = backend
                .detect_notes_layout(0, mode)
                .expect("detect notes layout")
                .layout
                .expect("fixture should provide a split");
            assert_eq!(layout.axis, SplitAxis::Vertical);
            assert_eq!(layout.notes_side, NotesSide::Right);
        }
        assert!(
            backend
                .detect_notes_layout_without_render(0, NotesMode::PdfAnnotations)
                .is_some()
        );
        assert!(
            backend
                .detect_notes_layout_without_render(0, NotesMode::Auto)
                .is_none()
        );
        for index in 0..backend.page_count() {
            if let Some(path) = backend.outline_path_for_page(index) {
                assert!(path.iter().all(|entry| !entry.is_empty()));
            }
        }
    }

    #[test]
    fn annotation_ids_stay_valid_after_deleting_an_earlier_annotation_when_fixture_present() {
        let Some(fixture) = fixture_path() else {
            return;
        };
        let temp = tempdir().expect("create temp directory");
        let copied = copy_fixture(&fixture, temp.path(), "ids.pdf");
        let mut backend = PdfiumBackend::open(&copied).expect("open copy");

        let first_ink = backend.add_ink_annotation(0, red_ink()).expect("add ink");
        let second_ink = backend.add_ink_annotation(0, red_ink()).expect("add ink");
        let mut text = backend
            .add_text_annotation(0, note("first draft"))
            .expect("add note");

        backend
            .delete_annotation(&first_ink)
            .expect("delete first ink");
        text.contents = "second draft".to_owned();
        backend
            .update_text_annotation(&text)
            .expect("update note after an earlier delete");
        backend
            .delete_annotation(&second_ink)
            .expect("delete second ink after an earlier delete");
        assert!(backend.delete_annotation(&first_ink).is_err());

        let listed = backend.list_text_annotations(0).expect("list notes");
        let updated = listed
            .iter()
            .find(|entry| entry.id == text.id)
            .expect("note keeps its id");
        assert_eq!(updated.contents, "second draft");
    }

    #[test]
    fn text_annotation_round_trip_save_reopen_when_fixture_present() {
        let Some(fixture) = fixture_path() else {
            return;
        };
        let temp = tempdir().expect("create temp directory");
        let copied = copy_fixture(&fixture, temp.path(), "roundtrip.pdf");

        let mut backend = PdfiumBackend::open(&copied).expect("open copy");
        let mut created = backend
            .add_text_annotation(0, note("todo: rehearse pacing"))
            .expect("create text annotation");
        assert!(backend.has_unsaved_changes());
        created.contents = "todo: rehearse pacing (updated)".to_owned();
        created.metadata.tags = vec!["timing".to_owned()];
        created.metadata.priority = AnnotationPriority::High;
        backend
            .update_text_annotation(&created)
            .expect("update text annotation");
        backend.save_in_place().expect("save in place");
        assert!(!backend.has_unsaved_changes());
        drop(backend);

        let mut reopened = PdfiumBackend::open(&copied).expect("reopen after save");
        let persisted = reopened
            .list_text_annotations(0)
            .expect("list after save")
            .into_iter()
            .find(|entry| entry.contents == "todo: rehearse pacing (updated)")
            .expect("saved annotation should persist");
        assert_eq!(persisted.metadata.tags, vec!["timing".to_owned()]);
        assert_eq!(persisted.metadata.priority, AnnotationPriority::High);
        assert!((persisted.rect.left - 40.0).abs() < 0.5);
        assert!((persisted.rect.top - 120.0).abs() < 0.5);

        reopened
            .delete_annotation(&persisted.id)
            .expect("delete annotation");
        reopened.save_in_place().expect("save after delete");
        drop(reopened);

        let mut verified = PdfiumBackend::open(&copied).expect("reopen after delete");
        assert!(
            verified
                .list_text_annotations(0)
                .expect("list after delete")
                .iter()
                .all(|entry| !entry.contents.contains("rehearse pacing"))
        );
    }

    #[test]
    fn saved_ink_renders_where_it_was_drawn_when_fixture_present() {
        let Some(fixture) = fixture_path() else {
            return;
        };
        let temp = tempdir().expect("create temp directory");
        let copied = copy_fixture(&fixture, temp.path(), "ink.pdf");
        let mut backend = PdfiumBackend::open(&copied).expect("open copy");
        let geometry = backend.page_geometry(0).expect("page geometry");
        let before = backend
            .render_page_bitmap(0, PageView::Full, None, 640)
            .expect("render before save");

        backend.add_ink_annotation(0, red_ink()).expect("add ink");
        backend.save_in_place().expect("save in place");
        let after = backend
            .render_page_bitmap(0, PageView::Full, None, 640)
            .expect("render after save");
        assert_ne!(before.rgba, after.rgba);

        // (160, 110) lies on the stroke from (30, 30) to (290, 190) in page space.
        let x = (160.0 / geometry.width * after.width as f32).round() as usize;
        let y =
            ((geometry.height - 110.0) / geometry.height * after.height as f32).round() as usize;
        let offset = (y * after.width + x) * 4;
        let pixel = &after.rgba[offset..offset + 3];
        assert!(
            pixel[0] > 200 && pixel[1] < 80 && pixel[2] < 80,
            "expected red ink at ({x}, {y}), found {pixel:?}"
        );
    }

    #[test]
    fn annotations_can_be_edited_on_a_pdf_with_a_broken_xref_when_fixture_present() {
        let Some(fixture) = fixture_path() else {
            return;
        };
        let temp = tempdir().expect("create temp directory");
        let broken = temp.path().join("broken-xref.pdf");
        let mut bytes = fs::read(&fixture).expect("read fixture");
        let marker = bytes
            .windows(b"startxref".len())
            .rposition(|window| window == b"startxref")
            .expect("fixture has startxref");
        // Pointing startxref past the real table forces a reader to rebuild the xref.
        bytes.truncate(marker);
        bytes.extend_from_slice(b"startxref\n12\n%%EOF\n");
        fs::write(&broken, bytes).expect("write broken copy");

        let mut backend = PdfiumBackend::open(&broken).expect("PDFium repairs the xref");
        backend
            .add_text_annotation(0, note("added to a repaired file"))
            .expect("add note");
        backend.save_in_place().expect("save repaired file");
        drop(backend);

        let mut reopened = PdfiumBackend::open(&broken).expect("reopen saved file");
        assert!(
            reopened
                .list_text_annotations(0)
                .expect("list notes")
                .iter()
                .any(|entry| entry.contents == "added to a repaired file")
        );
    }

    #[test]
    fn edits_survive_a_save_that_cannot_replace_the_target_when_fixture_present() {
        let Some(fixture) = fixture_path() else {
            return;
        };
        let temp = tempdir().expect("create temp directory");
        let copied = copy_fixture(&fixture, temp.path(), "kept.pdf");
        let blocked_target = temp.path().join("blocked");
        fs::create_dir(&blocked_target).expect("create directory");
        fs::write(blocked_target.join("occupied"), b"x").expect("fill directory");

        let mut backend = PdfiumBackend::open(&copied).expect("open copy");
        backend
            .add_text_annotation(0, note("kept through a failed save"))
            .expect("add note");
        assert!(backend.save_as(&blocked_target).is_err());
        assert!(backend.has_unsaved_changes());

        backend.save_in_place().expect("save to the original path");
        drop(backend);
        let leftovers: Vec<_> = fs::read_dir(temp.path())
            .expect("list temp directory")
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains("decklight-save")
            })
            .collect();
        assert!(leftovers.is_empty(), "temporary saves should be removed");

        let mut reopened = PdfiumBackend::open(&copied).expect("reopen");
        assert!(
            reopened
                .list_text_annotations(0)
                .expect("list notes")
                .iter()
                .any(|entry| entry.contents == "kept through a failed save")
        );
    }
}
