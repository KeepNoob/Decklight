use crate::config::{NotesLayout, NotesMode, NotesSide, SplitAxis};
use crate::document::{
    AnnotationMetadata, AnnotationPriority, AnnotationStatus, DocumentBackend, NewInkAnnotation,
    NewTextAnnotation, NotesDetectionResult, PageBitmap, PageGeometry, PageView, PdfRect,
    TextAnnotation,
};
use anyhow::{Context, Result, bail};
use chrono::Local;
use lopdf::{Dictionary, Document, Object, ObjectId, StringFormat};
use pdfium_auto::bind_pdfium_silent;
use pdfium_render::prelude::PdfRenderConfig;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub struct LopdfBackend {
    path: PathBuf,
    document: Document,
    page_count: usize,
    outline_paths_by_page: Option<Vec<Option<Vec<String>>>>,
    modified_at: Option<SystemTime>,
    has_unsaved_changes: bool,
}

#[derive(Debug, Clone, Copy)]
struct SplitCandidate {
    axis: SplitAxis,
    split_ratio: f32,
    notes_side: NotesSide,
    confidence: f32,
}

#[derive(Debug, Clone, Copy)]
struct ModeLayoutPrior {
    mode_name: &'static str,
    axis: SplitAxis,
    notes_side: NotesSide,
    split_ratios: &'static [f32],
    fallback_split_ratio: f32,
}

#[derive(Debug, Clone, Copy)]
struct RegionStats {
    mean_luma: f32,
    ink_density: f32,
    dark_density: f32,
}

#[derive(Debug, Clone)]
struct OutlineEntry {
    page_index: usize,
    path: Vec<String>,
}

const AUTO_CONFIDENCE_MIN: f32 = 0.12;
const MODE_CONFIDENCE_MIN: f32 = 0.20;
const META_PREFIX: &str = "[[PDFPRESENTER_META:";
const META_SUFFIX: &str = "]]";

impl LopdfBackend {
    pub fn open(path: &Path) -> Result<Self> {
        if !path.exists() {
            bail!("document not found: {}", path.display());
        }

        let document =
            Document::load(path).with_context(|| format!("failed to load {}", path.display()))?;
        let page_count = document.get_pages().len();
        if page_count == 0 {
            bail!("document has no pages: {}", path.display());
        }

        let metadata = std::fs::metadata(path)
            .with_context(|| format!("failed to read metadata for {}", path.display()))?;
        let outline_paths_by_page = Self::build_outline_paths_by_page(&document, page_count);
        Ok(Self {
            path: path.to_path_buf(),
            document,
            page_count,
            outline_paths_by_page,
            modified_at: metadata.modified().ok(),
            has_unsaved_changes: false,
        })
    }

    fn reload(&mut self) -> Result<()> {
        self.document = Document::load(&self.path)
            .with_context(|| format!("failed to reload {}", self.path.display()))?;
        self.page_count = self.document.get_pages().len();
        self.outline_paths_by_page =
            Self::build_outline_paths_by_page(&self.document, self.page_count);
        self.has_unsaved_changes = false;
        Ok(())
    }

    fn decode_pdf_text(bytes: &[u8]) -> String {
        if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
            let utf16_units: Vec<u16> = bytes[2..]
                .chunks_exact(2)
                .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
                .collect();
            if let Ok(decoded) = String::from_utf16(&utf16_units) {
                let trimmed = decoded.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_owned();
                }
            }
        }

        String::from_utf8_lossy(bytes).trim().to_owned()
    }

    fn outline_root_first_item(document: &Document) -> Option<ObjectId> {
        let root_id = document.trailer.get(b"Root").ok()?.as_reference().ok()?;
        let catalog = document.get_dictionary(root_id).ok()?;
        let outlines_dict = match catalog.get(b"Outlines").ok()? {
            Object::Reference(id) => document.get_dictionary(*id).ok()?.clone(),
            Object::Dictionary(dict) => dict.clone(),
            _ => return None,
        };
        outlines_dict.get(b"First").ok()?.as_reference().ok()
    }

    fn outline_title_from_dict(dict: &Dictionary) -> Option<String> {
        let title_bytes = dict.get(b"Title").ok()?.as_str().ok()?;
        let title = Self::decode_pdf_text(title_bytes);
        if title.is_empty() { None } else { Some(title) }
    }

    fn destination_page_index_from_object(
        document: &Document,
        destination: &Object,
        page_index_by_id: &BTreeMap<ObjectId, usize>,
        page_count: usize,
        depth: u8,
    ) -> Option<usize> {
        if depth > 8 {
            return None;
        }

        match destination {
            Object::Reference(id) => {
                let resolved = document.get_object(*id).ok()?;
                Self::destination_page_index_from_object(
                    document,
                    resolved,
                    page_index_by_id,
                    page_count,
                    depth + 1,
                )
            }
            Object::Array(values) => {
                let first = values.first()?;
                if let Ok(page_id) = first.as_reference() {
                    return page_index_by_id.get(&page_id).copied();
                }

                let index = first.as_i64().ok()?;
                if index.is_negative() {
                    return None;
                }
                let index = index as usize;
                (index < page_count).then_some(index)
            }
            Object::Integer(index) => {
                if index.is_negative() {
                    return None;
                }
                let index = *index as usize;
                (index < page_count).then_some(index)
            }
            _ => None,
        }
    }

    fn outline_target_page_index(
        document: &Document,
        item: &Dictionary,
        page_index_by_id: &BTreeMap<ObjectId, usize>,
        page_count: usize,
    ) -> Option<usize> {
        if let Ok(destination) = item.get(b"Dest")
            && let Some(index) = Self::destination_page_index_from_object(
                document,
                destination,
                page_index_by_id,
                page_count,
                0,
            )
        {
            return Some(index);
        }

        let action_dict = match item.get(b"A").ok()? {
            Object::Reference(id) => document.get_dictionary(*id).ok()?.clone(),
            Object::Dictionary(dict) => dict.clone(),
            _ => return None,
        };

        if let Ok(action) = action_dict.get(b"S").and_then(Object::as_name)
            && action != b"GoTo"
        {
            return None;
        }

        let destination = action_dict.get(b"D").ok()?;
        Self::destination_page_index_from_object(
            document,
            destination,
            page_index_by_id,
            page_count,
            0,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_outline_entries(
        document: &Document,
        first_item_id: ObjectId,
        parent_path: &[String],
        page_index_by_id: &BTreeMap<ObjectId, usize>,
        page_count: usize,
        visited: &mut BTreeSet<ObjectId>,
        entries: &mut Vec<OutlineEntry>,
        depth: usize,
    ) {
        if depth > 32 {
            return;
        }

        let mut current = Some(first_item_id);
        let mut siblings_seen = 0usize;
        while let Some(item_id) = current {
            if siblings_seen > 16_384 {
                break;
            }
            siblings_seen += 1;

            if !visited.insert(item_id) {
                break;
            }

            let Ok(item) = document.get_dictionary(item_id) else {
                break;
            };

            let mut current_path = parent_path.to_vec();
            if let Some(title) = Self::outline_title_from_dict(item) {
                current_path.push(title);
            }
            if !current_path.is_empty()
                && let Some(page_index) =
                    Self::outline_target_page_index(document, item, page_index_by_id, page_count)
            {
                entries.push(OutlineEntry {
                    page_index,
                    path: current_path.clone(),
                });
            }

            if let Ok(child_first) = item.get(b"First").and_then(Object::as_reference) {
                Self::collect_outline_entries(
                    document,
                    child_first,
                    &current_path,
                    page_index_by_id,
                    page_count,
                    visited,
                    entries,
                    depth + 1,
                );
            }

            current = item.get(b"Next").and_then(Object::as_reference).ok();
        }
    }

    fn build_outline_paths_by_page(
        document: &Document,
        page_count: usize,
    ) -> Option<Vec<Option<Vec<String>>>> {
        if page_count == 0 {
            return None;
        }

        let page_index_by_id: BTreeMap<ObjectId, usize> = document
            .get_pages()
            .values()
            .copied()
            .enumerate()
            .map(|(index, page_id)| (page_id, index))
            .collect();
        if page_index_by_id.is_empty() {
            return None;
        }

        let first_item_id = Self::outline_root_first_item(document)?;
        let mut visited = BTreeSet::new();
        let mut entries = Vec::new();
        Self::collect_outline_entries(
            document,
            first_item_id,
            &[],
            &page_index_by_id,
            page_count,
            &mut visited,
            &mut entries,
            0,
        );
        if entries.is_empty() {
            return None;
        }

        entries.sort_by_key(|entry| entry.page_index);
        let mut cursor = 0usize;
        let mut active_path: Option<Vec<String>> = None;
        let mut paths = vec![None; page_count];
        for (page_index, slot) in paths.iter_mut().enumerate() {
            while cursor < entries.len() && entries[cursor].page_index <= page_index {
                active_path = Some(entries[cursor].path.clone());
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

    fn now_pdf_timestamp() -> String {
        Local::now().format("D:%Y%m%d%H%M%S%z").to_string()
    }

    fn metadata_from_pdf_dict(dict: &Dictionary, contents_raw: &str) -> AnnotationMetadata {
        let mut metadata = AnnotationMetadata::default();

        if let Ok(author) = dict.get(b"T").and_then(Object::as_str) {
            let decoded = Self::decode_pdf_text(author);
            if !decoded.is_empty() {
                metadata.author = decoded;
            }
        }

        if let Ok(color) = dict.get(b"C").and_then(Object::as_array)
            && color.len() >= 3
        {
            let red = color[0].as_float().unwrap_or(1.0).clamp(0.0, 1.0);
            let green = color[1].as_float().unwrap_or(1.0).clamp(0.0, 1.0);
            let blue = color[2].as_float().unwrap_or(0.0).clamp(0.0, 1.0);
            metadata.color_rgb = (red, green, blue);
        }

        if let Ok(modified) = dict.get(b"M").and_then(Object::as_str) {
            let decoded = Self::decode_pdf_text(modified);
            if !decoded.is_empty() {
                metadata.modified_at = Some(decoded);
            }
        }

        if let Ok(subject) = dict
            .get(b"Subj")
            .and_then(Object::as_str)
            .or_else(|_| dict.get(b"Subject").and_then(Object::as_str))
        {
            let decoded = Self::decode_pdf_text(subject);
            for token in decoded
                .split(';')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
            {
                if let Some(value) = token.strip_prefix("status=") {
                    metadata.status = AnnotationStatus::parse(value);
                } else if let Some(value) = token.strip_prefix("priority=") {
                    metadata.priority = AnnotationPriority::parse(value);
                }
            }
        }

        if let Some(custom) = Self::decode_custom_metadata_block(contents_raw) {
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
            return Self::encode_custom_metadata_block(metadata);
        }
        format!("{}\n{}", Self::encode_custom_metadata_block(metadata), body)
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
        let mut rest = trimmed[end + META_SUFFIX.len()..].to_owned();
        while rest.starts_with('\n') || rest.starts_with('\r') {
            rest = rest[1..].to_owned();
        }
        rest
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

    fn page_id_from_index(&self, index: usize) -> Result<ObjectId> {
        if index >= self.page_count {
            bail!("page index out of bounds: {index}");
        }
        self.document
            .get_pages()
            .values()
            .copied()
            .nth(index)
            .ok_or_else(|| anyhow::anyhow!("failed to resolve page id for index {index}"))
    }

    fn parse_rect(object: &Object) -> Result<PdfRect> {
        let values = object.as_array()?;
        if values.len() < 4 {
            bail!("annotation rectangle requires four coordinates");
        }
        Ok(PdfRect {
            left: values[0].as_float()?,
            bottom: values[1].as_float()?,
            right: values[2].as_float()?,
            top: values[3].as_float()?,
        }
        .normalized())
    }

    fn media_box_for_page(&self, page_id: ObjectId) -> Result<PdfRect> {
        let mut cursor = page_id;
        for _ in 0..128 {
            let dict = self.document.get_dictionary(cursor)?;
            if let Ok(media_box) = dict.get(b"MediaBox") {
                return Self::parse_rect(media_box);
            }

            let Ok(parent_id) = dict.get(b"Parent").and_then(Object::as_reference) else {
                break;
            };
            cursor = parent_id;
        }

        // Fallback to Letter-size points when page dictionaries don't expose a media box.
        Ok(PdfRect {
            left: 0.0,
            bottom: 0.0,
            right: 612.0,
            top: 792.0,
        })
    }

    fn annotation_id(object_id: ObjectId) -> String {
        format!("{}:{}", object_id.0, object_id.1)
    }

    fn parse_annotation_id(raw: &str) -> Result<ObjectId> {
        let Some((object, generation)) = raw.trim().split_once(':') else {
            bail!("invalid annotation id: {raw}");
        };
        let object = object
            .parse::<u32>()
            .with_context(|| format!("invalid annotation object id: {raw}"))?;
        let generation = generation
            .parse::<u16>()
            .with_context(|| format!("invalid annotation generation id: {raw}"))?;
        Ok((object, generation))
    }

    fn page_annotation_ids(&self, page_id: ObjectId) -> Result<Vec<ObjectId>> {
        let page = self.document.get_dictionary(page_id)?;
        let annots = match page.get(b"Annots") {
            Ok(value) => value,
            Err(_) => return Ok(Vec::new()),
        };

        let list_object = match annots {
            Object::Reference(id) => self.document.get_object(*id)?,
            other => other,
        };
        let array = list_object.as_array()?;
        Ok(array
            .iter()
            .filter_map(|entry| entry.as_reference().ok())
            .collect())
    }

    fn append_annotation_to_page(
        &mut self,
        page_id: ObjectId,
        annotation_id: ObjectId,
    ) -> Result<()> {
        enum AnnotsTarget {
            Missing,
            InlineArray,
            ReferencedArray(ObjectId),
            Unsupported,
        }

        let target = {
            let page = self.document.get_dictionary(page_id)?;
            match page.get(b"Annots") {
                Ok(Object::Array(_)) => AnnotsTarget::InlineArray,
                Ok(Object::Reference(id)) => AnnotsTarget::ReferencedArray(*id),
                Ok(_) => AnnotsTarget::Unsupported,
                Err(_) => AnnotsTarget::Missing,
            }
        };

        match target {
            AnnotsTarget::Missing | AnnotsTarget::Unsupported => {
                let page = self.document.get_object_mut(page_id)?.as_dict_mut()?;
                page.set("Annots", vec![Object::Reference(annotation_id)]);
            }
            AnnotsTarget::InlineArray => {
                let page = self.document.get_object_mut(page_id)?.as_dict_mut()?;
                let annots = page.get_mut(b"Annots")?.as_array_mut()?;
                annots.push(Object::Reference(annotation_id));
            }
            AnnotsTarget::ReferencedArray(array_id) => {
                let annots = self.document.get_object_mut(array_id)?.as_array_mut()?;
                annots.push(Object::Reference(annotation_id));
            }
        }

        Ok(())
    }

    fn save_to_path(&mut self, path: &Path) -> Result<()> {
        self.document
            .save(path)
            .with_context(|| format!("failed to save {}", path.display()))?;
        let metadata = std::fs::metadata(path)
            .with_context(|| format!("failed to read metadata for {}", path.display()))?;
        self.path = path.to_path_buf();
        self.modified_at = metadata.modified().ok();
        self.has_unsaved_changes = false;
        Ok(())
    }

    fn luma(r: u8, g: u8, b: u8) -> f32 {
        0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32
    }

    fn region_stats(
        rgba: &[u8],
        width: usize,
        height: usize,
        x0: usize,
        x1: usize,
        y0: usize,
        y1: usize,
    ) -> RegionStats {
        let clamped_x0 = x0.min(width);
        let clamped_x1 = x1.min(width).max(clamped_x0 + 1).min(width);
        let clamped_y0 = y0.min(height);
        let clamped_y1 = y1.min(height).max(clamped_y0 + 1).min(height);

        let mut count = 0usize;
        let mut luma_sum = 0f32;
        let mut ink = 0usize;
        let mut dark = 0usize;

        for y in clamped_y0..clamped_y1 {
            for x in clamped_x0..clamped_x1 {
                let offset = (y * width + x) * 4;
                let lum = Self::luma(rgba[offset], rgba[offset + 1], rgba[offset + 2]);
                luma_sum += lum;
                if lum < 220.0 {
                    ink += 1;
                }
                if lum < 120.0 {
                    dark += 1;
                }
                count += 1;
            }
        }

        let safe_count = count.max(1) as f32;
        RegionStats {
            mean_luma: luma_sum / safe_count,
            ink_density: ink as f32 / safe_count,
            dark_density: dark as f32 / safe_count,
        }
    }

    fn seam_contrast_vertical(rgba: &[u8], width: usize, height: usize, split_x: usize) -> f32 {
        if width < 3 || split_x == 0 || split_x >= width {
            return 0.0;
        }

        let mut diff_sum = 0f32;
        for y in 0..height {
            let left = (y * width + (split_x - 1)) * 4;
            let right = (y * width + split_x) * 4;
            let l = Self::luma(rgba[left], rgba[left + 1], rgba[left + 2]);
            let r = Self::luma(rgba[right], rgba[right + 1], rgba[right + 2]);
            diff_sum += (l - r).abs();
        }
        (diff_sum / height.max(1) as f32 / 255.0).clamp(0.0, 1.0)
    }

    fn seam_contrast_horizontal(rgba: &[u8], width: usize, height: usize, split_y: usize) -> f32 {
        if height < 3 || split_y == 0 || split_y >= height {
            return 0.0;
        }

        let mut diff_sum = 0f32;
        for x in 0..width {
            let top = ((split_y - 1) * width + x) * 4;
            let bottom = (split_y * width + x) * 4;
            let t = Self::luma(rgba[top], rgba[top + 1], rgba[top + 2]);
            let b = Self::luma(rgba[bottom], rgba[bottom + 1], rgba[bottom + 2]);
            diff_sum += (t - b).abs();
        }
        (diff_sum / width.max(1) as f32 / 255.0).clamp(0.0, 1.0)
    }

    fn analyze_vertical_candidate(
        rgba: &[u8],
        width: usize,
        height: usize,
        split_ratio: f32,
    ) -> SplitCandidate {
        let split_x = ((width as f32) * split_ratio).round() as usize;
        let left = Self::region_stats(rgba, width, height, 0, split_x, 0, height);
        let right = Self::region_stats(rgba, width, height, split_x, width, 0, height);
        let seam = Self::seam_contrast_vertical(rgba, width, height, split_x);
        let bg_delta = ((left.mean_luma - right.mean_luma).abs() / 255.0).clamp(0.0, 1.0);
        let ink_delta = (left.ink_density - right.ink_density).abs().clamp(0.0, 1.0);

        let mut confidence = 0.45 * bg_delta + 0.35 * seam + 0.20 * ink_delta;
        let aspect = width as f32 / height.max(1) as f32;
        if aspect > 1.2 {
            confidence += ((aspect - 1.2) * 0.18).clamp(0.0, 0.2);
        }
        // Dual-screen note pages are commonly wide and split near center.
        // Bias confidence upward for that geometry so auto mode is less conservative.
        let centered_split = 1.0 - ((split_ratio - 0.5).abs() / 0.25).clamp(0.0, 1.0);
        if aspect > 1.45 {
            confidence += 0.12 * centered_split;
        }
        if aspect > 1.35 && seam > 0.04 && bg_delta > 0.03 {
            confidence += 0.08;
        }

        let left_score = ((255.0 - left.mean_luma) / 255.0) * 0.65
            + left.ink_density * 0.25
            + left.dark_density * 0.10;
        let right_score = ((255.0 - right.mean_luma) / 255.0) * 0.65
            + right.ink_density * 0.25
            + right.dark_density * 0.10;

        let notes_side = if (right_score - left_score).abs() < 0.08 {
            // Ambiguous split: default to right-side notes for dual-screen layouts.
            NotesSide::Right
        } else if right_score > left_score {
            NotesSide::Right
        } else {
            NotesSide::Left
        };

        SplitCandidate {
            axis: SplitAxis::Vertical,
            split_ratio,
            notes_side,
            confidence: confidence.clamp(0.0, 1.0),
        }
    }

    fn analyze_horizontal_candidate(
        rgba: &[u8],
        width: usize,
        height: usize,
        split_ratio: f32,
    ) -> SplitCandidate {
        let split_y = ((height as f32) * split_ratio).round() as usize;
        let top = Self::region_stats(rgba, width, height, 0, width, 0, split_y);
        let bottom = Self::region_stats(rgba, width, height, 0, width, split_y, height);
        let seam = Self::seam_contrast_horizontal(rgba, width, height, split_y);
        let bg_delta = ((top.mean_luma - bottom.mean_luma).abs() / 255.0).clamp(0.0, 1.0);
        let ink_delta = (top.ink_density - bottom.ink_density).abs().clamp(0.0, 1.0);

        let mut confidence = 0.45 * bg_delta + 0.35 * seam + 0.20 * ink_delta;
        let aspect = height as f32 / width.max(1) as f32;
        if aspect > 1.2 {
            confidence += ((aspect - 1.2) * 0.18).clamp(0.0, 0.2);
        }

        let top_score = ((255.0 - top.mean_luma) / 255.0) * 0.65
            + top.ink_density * 0.25
            + top.dark_density * 0.10;
        let bottom_score = ((255.0 - bottom.mean_luma) / 255.0) * 0.65
            + bottom.ink_density * 0.25
            + bottom.dark_density * 0.10;

        let notes_side = if (bottom_score - top_score).abs() < 0.08 {
            // Ambiguous split: default to bottom notes for horizontal layouts.
            NotesSide::Bottom
        } else if bottom_score > top_score {
            NotesSide::Bottom
        } else {
            NotesSide::Top
        };

        SplitCandidate {
            axis: SplitAxis::Horizontal,
            split_ratio,
            notes_side,
            confidence: confidence.clamp(0.0, 1.0),
        }
    }

    fn detect_layout_from_bitmap(bitmap: &PageBitmap) -> Option<SplitCandidate> {
        if bitmap.width < 20 || bitmap.height < 20 {
            return None;
        }

        let candidates = [0.35_f32, 0.40, 0.45, 0.50, 0.55, 0.60, 0.65];
        let mut best: Option<SplitCandidate> = None;

        for ratio in candidates {
            let vertical =
                Self::analyze_vertical_candidate(&bitmap.rgba, bitmap.width, bitmap.height, ratio);
            if best.is_none_or(|existing| vertical.confidence > existing.confidence) {
                best = Some(vertical);
            }

            let horizontal = Self::analyze_horizontal_candidate(
                &bitmap.rgba,
                bitmap.width,
                bitmap.height,
                ratio,
            );
            if best.is_none_or(|existing| horizontal.confidence > existing.confidence) {
                best = Some(horizontal);
            }
        }

        best
    }

    fn mode_prior(mode: NotesMode) -> Option<ModeLayoutPrior> {
        match mode {
            NotesMode::BeamerSecondScreen => Some(ModeLayoutPrior {
                mode_name: "Beamer second-screen",
                axis: SplitAxis::Vertical,
                notes_side: NotesSide::Right,
                split_ratios: &[0.42, 0.46, 0.50, 0.54, 0.58],
                fallback_split_ratio: 0.50,
            }),
            NotesMode::LibreOfficeNotes => Some(ModeLayoutPrior {
                mode_name: "LibreOffice notes",
                axis: SplitAxis::Horizontal,
                notes_side: NotesSide::Bottom,
                split_ratios: &[0.36, 0.40, 0.45, 0.50, 0.56, 0.62],
                fallback_split_ratio: 0.45,
            }),
            NotesMode::Auto | NotesMode::PdfAnnotations => None,
        }
    }

    fn fallback_layout_for_prior(prior: ModeLayoutPrior) -> NotesLayout {
        let mut layout = NotesLayout {
            axis: prior.axis,
            split_ratio: prior.fallback_split_ratio,
            notes_side: prior.notes_side,
            confidence: 0.25,
        };
        layout.sanitize();
        layout
    }

    fn analyze_axis_candidate(
        bitmap: &PageBitmap,
        axis: SplitAxis,
        split_ratio: f32,
    ) -> SplitCandidate {
        match axis {
            SplitAxis::Vertical => Self::analyze_vertical_candidate(
                &bitmap.rgba,
                bitmap.width,
                bitmap.height,
                split_ratio,
            ),
            SplitAxis::Horizontal => Self::analyze_horizontal_candidate(
                &bitmap.rgba,
                bitmap.width,
                bitmap.height,
                split_ratio,
            ),
        }
    }

    fn prior_weighted_score(candidate: SplitCandidate, prior: ModeLayoutPrior) -> f32 {
        let side_bonus = if candidate.notes_side == prior.notes_side {
            0.18
        } else {
            -0.10
        };
        let ratio_bonus = (1.0
            - ((candidate.split_ratio - prior.fallback_split_ratio).abs() / 0.30).clamp(0.0, 1.0))
            * 0.08;
        candidate.confidence + side_bonus + ratio_bonus
    }

    fn detect_layout_with_prior(
        bitmap: &PageBitmap,
        prior: ModeLayoutPrior,
    ) -> Option<SplitCandidate> {
        if bitmap.width < 20 || bitmap.height < 20 {
            return None;
        }

        let mut best: Option<(SplitCandidate, f32)> = None;
        for split_ratio in prior.split_ratios.iter().copied() {
            let candidate = Self::analyze_axis_candidate(bitmap, prior.axis, split_ratio);
            let score = Self::prior_weighted_score(candidate, prior);
            if best.is_none_or(|(_, existing_score)| score > existing_score) {
                best = Some((candidate, score));
            }
        }

        best.map(|(candidate, _)| candidate)
    }

    fn detect_layout_for_mode(bitmap: &PageBitmap, mode: NotesMode) -> NotesDetectionResult {
        if mode == NotesMode::PdfAnnotations {
            return NotesDetectionResult {
                layout: None,
                confidence: 1.0,
                reason: "PDF annotations mode uses full-page rendering.".to_owned(),
                warning: None,
            };
        }

        if mode == NotesMode::Auto {
            let candidate = Self::detect_layout_from_bitmap(bitmap);
            if let Some(candidate) = candidate {
                let mut layout = NotesLayout {
                    axis: candidate.axis,
                    split_ratio: candidate.split_ratio,
                    notes_side: candidate.notes_side,
                    confidence: candidate.confidence,
                };
                layout.sanitize();
                if candidate.confidence < AUTO_CONFIDENCE_MIN {
                    return NotesDetectionResult {
                        layout: None,
                        confidence: candidate.confidence,
                        reason: "Auto mode: no reliable notes split detected.".to_owned(),
                        warning: None,
                    };
                }
                return NotesDetectionResult {
                    layout: Some(layout),
                    confidence: candidate.confidence,
                    reason: "Detected notes split candidate from page raster analysis.".to_owned(),
                    warning: None,
                };
            }
            return NotesDetectionResult {
                layout: None,
                confidence: 0.0,
                reason: "Auto mode: unable to infer notes split.".to_owned(),
                warning: None,
            };
        }

        let Some(prior) = Self::mode_prior(mode) else {
            return NotesDetectionResult {
                layout: None,
                confidence: 0.0,
                reason: "Unsupported notes mode.".to_owned(),
                warning: Some("Unsupported notes mode; using full page.".to_owned()),
            };
        };

        let prior_candidate = Self::detect_layout_with_prior(bitmap, prior);
        let generic_candidate = Self::detect_layout_from_bitmap(bitmap);
        let chosen_candidate = match (prior_candidate, generic_candidate) {
            (Some(prior_hit), Some(generic_hit)) => {
                let prior_score = Self::prior_weighted_score(prior_hit, prior);
                if prior_score + 0.04 >= generic_hit.confidence {
                    prior_hit
                } else {
                    generic_hit
                }
            }
            (Some(prior_hit), None) => prior_hit,
            (None, Some(generic_hit)) => generic_hit,
            (None, None) => {
                let fallback = Self::fallback_layout_for_prior(prior);
                let warning = format!(
                    "{} mode: split detection failed; applied fallback {:?}/{:?} layout.",
                    prior.mode_name, prior.axis, prior.notes_side
                );
                return NotesDetectionResult {
                    layout: Some(fallback),
                    confidence: 0.0,
                    reason: warning.clone(),
                    warning: Some(warning),
                };
            }
        };

        if chosen_candidate.confidence < MODE_CONFIDENCE_MIN {
            let fallback = Self::fallback_layout_for_prior(prior);
            let warning = format!(
                "{} mode: low-confidence split ({:.2}); applied fallback {:?}/{:?} layout.",
                prior.mode_name, chosen_candidate.confidence, prior.axis, prior.notes_side
            );
            return NotesDetectionResult {
                layout: Some(fallback),
                confidence: chosen_candidate.confidence,
                reason: warning.clone(),
                warning: Some(warning),
            };
        }

        let mut layout = NotesLayout {
            axis: chosen_candidate.axis,
            split_ratio: chosen_candidate.split_ratio,
            notes_side: chosen_candidate.notes_side,
            confidence: chosen_candidate.confidence,
        };
        layout.sanitize();
        NotesDetectionResult {
            layout: Some(layout),
            confidence: chosen_candidate.confidence,
            reason: format!(
                "{} mode: detected split from prior-guided raster analysis.",
                prior.mode_name
            ),
            warning: None,
        }
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
        let pdfium = bind_pdfium_silent().map_err(|error| {
            anyhow::anyhow!("failed to bind PDFium runtime (first run may need network): {error}")
        })?;

        let document = pdfium
            .load_pdf_from_file(&self.path, None)
            .with_context(|| format!("failed to load PDF with PDFium: {}", self.path.display()))?;
        let page_index: u16 = index
            .try_into()
            .map_err(|_| anyhow::anyhow!("page index out of range for PDFium: {}", index))?;
        let page = document.pages().get(page_index)?;

        let render_config = PdfRenderConfig::new()
            .set_target_width(target_width.clamp(200, 4096) as i32)
            // Use a high max-height cap so split-region supersampling does not get downscaled.
            .set_maximum_height(4096)
            .render_annotations(true)
            .render_form_data(true);

        let rendered = page.render_with_config(&render_config)?;
        let width = rendered.width() as usize;
        let height = rendered.height() as usize;

        Ok(PageBitmap {
            width,
            height,
            rgba: rendered.as_rgba_bytes(),
        })
    }
}

impl DocumentBackend for LopdfBackend {
    fn outline_path_for_page(&self, index: usize) -> Option<Vec<String>> {
        self.outline_paths_by_page
            .as_ref()
            .and_then(|paths| paths.get(index))
            .cloned()
            .flatten()
    }

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
        if mode == NotesMode::PdfAnnotations {
            return Ok(NotesDetectionResult {
                layout: None,
                confidence: 1.0,
                reason: "PDF annotations mode uses full-page rendering.".to_owned(),
                warning: None,
            });
        }
        let full = self.render_full_page(index, 1280)?;
        Ok(Self::detect_layout_for_mode(&full, mode))
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
        let page_id = self.page_id_from_index(index)?;
        let media_box = self.media_box_for_page(page_id)?;
        Ok(PageGeometry {
            width: (media_box.right - media_box.left).abs().max(1.0),
            height: (media_box.top - media_box.bottom).abs().max(1.0),
        })
    }

    fn list_text_annotations(&self, index: usize) -> Result<Vec<TextAnnotation>> {
        let page_id = self.page_id_from_index(index)?;
        let mut annotations = Vec::new();

        for annotation_id in self.page_annotation_ids(page_id)? {
            let Ok(dict) = self.document.get_dictionary(annotation_id) else {
                continue;
            };

            let Ok(subtype) = dict.get(b"Subtype").and_then(Object::as_name) else {
                continue;
            };
            if subtype != b"Text" && subtype != b"FreeText" {
                continue;
            }

            let rect = dict
                .get(b"Rect")
                .ok()
                .and_then(|value| Self::parse_rect(value).ok())
                .unwrap_or(PdfRect {
                    left: 0.0,
                    bottom: 0.0,
                    right: 100.0,
                    top: 50.0,
                });
            let raw_contents = dict
                .get(b"Contents")
                .ok()
                .and_then(|value| value.as_str().ok())
                .map(|bytes| String::from_utf8_lossy(bytes).to_string())
                .unwrap_or_default();
            let metadata =
                Self::sanitize_metadata(Self::metadata_from_pdf_dict(dict, &raw_contents));
            let contents = Self::strip_custom_metadata_block(&raw_contents);

            annotations.push(TextAnnotation {
                id: Self::annotation_id(annotation_id),
                page_index: index,
                rect,
                contents,
                metadata,
            });
        }

        Ok(annotations)
    }

    fn add_text_annotation(
        &mut self,
        index: usize,
        annotation: NewTextAnnotation,
    ) -> Result<TextAnnotation> {
        let page_id = self.page_id_from_index(index)?;
        let rect = annotation.rect.normalized();
        let mut metadata = Self::sanitize_metadata(annotation.metadata);
        metadata.modified_at = Some(Self::now_pdf_timestamp());
        let stored_contents = Self::compose_contents_with_metadata(&annotation.contents, &metadata);

        let mut dict = Dictionary::new();
        dict.set("Type", "Annot");
        dict.set("Subtype", "Text");
        dict.set(
            "Rect",
            vec![
                rect.left.into(),
                rect.bottom.into(),
                rect.right.into(),
                rect.top.into(),
            ],
        );
        dict.set(
            "Contents",
            Object::String(stored_contents.into_bytes(), StringFormat::Literal),
        );
        dict.set("T", Object::string_literal(metadata.author.clone()));
        dict.set(
            "M",
            Object::string_literal(
                metadata
                    .modified_at
                    .clone()
                    .unwrap_or_else(Self::now_pdf_timestamp),
            ),
        );
        dict.set("Name", "Comment");
        dict.set(
            "C",
            vec![
                metadata.color_rgb.0.into(),
                metadata.color_rgb.1.into(),
                metadata.color_rgb.2.into(),
            ],
        );
        dict.set(
            "Subj",
            Object::string_literal(format!(
                "status={};priority={}",
                metadata.status.as_str(),
                metadata.priority.as_str()
            )),
        );
        dict.set("F", 4i64);
        dict.set("Open", false);
        dict.set("P", page_id);

        let annotation_id = self.document.add_object(dict);
        self.append_annotation_to_page(page_id, annotation_id)?;
        self.has_unsaved_changes = true;

        Ok(TextAnnotation {
            id: Self::annotation_id(annotation_id),
            page_index: index,
            rect,
            contents: annotation.contents,
            metadata,
        })
    }

    fn update_text_annotation(&mut self, annotation: &TextAnnotation) -> Result<()> {
        let annotation_id = Self::parse_annotation_id(&annotation.id)?;
        let rect = annotation.rect.normalized();
        let mut metadata = Self::sanitize_metadata(annotation.metadata.clone());
        metadata.modified_at = Some(Self::now_pdf_timestamp());
        let stored_contents = Self::compose_contents_with_metadata(&annotation.contents, &metadata);
        let dict = self.document.get_object_mut(annotation_id)?.as_dict_mut()?;
        dict.set(
            "Rect",
            vec![
                rect.left.into(),
                rect.bottom.into(),
                rect.right.into(),
                rect.top.into(),
            ],
        );
        dict.set(
            "Contents",
            Object::String(stored_contents.into_bytes(), StringFormat::Literal),
        );
        dict.set("T", Object::string_literal(metadata.author));
        dict.set(
            "M",
            Object::string_literal(metadata.modified_at.unwrap_or_else(Self::now_pdf_timestamp)),
        );
        dict.set(
            "C",
            vec![
                metadata.color_rgb.0.into(),
                metadata.color_rgb.1.into(),
                metadata.color_rgb.2.into(),
            ],
        );
        dict.set(
            "Subj",
            Object::string_literal(format!(
                "status={};priority={}",
                metadata.status.as_str(),
                metadata.priority.as_str()
            )),
        );
        self.has_unsaved_changes = true;
        Ok(())
    }

    fn delete_annotation(&mut self, annotation_id: &str) -> Result<()> {
        let object_id = Self::parse_annotation_id(annotation_id)?;
        let pages: Vec<ObjectId> = self.document.get_pages().values().copied().collect();
        for page_id in pages {
            let target = {
                let Ok(page) = self.document.get_dictionary(page_id) else {
                    continue;
                };
                match page.get(b"Annots") {
                    Ok(Object::Reference(array_id)) => Some((true, *array_id)),
                    Ok(Object::Array(_)) => Some((false, page_id)),
                    _ => None,
                }
            };

            let Some((is_reference_array, target_id)) = target else {
                continue;
            };
            if is_reference_array {
                if let Ok(array) = self
                    .document
                    .get_object_mut(target_id)
                    .and_then(Object::as_array_mut)
                {
                    array.retain(|entry| entry.as_reference().ok() != Some(object_id));
                }
            } else if let Ok(page) = self
                .document
                .get_object_mut(target_id)
                .and_then(Object::as_dict_mut)
                && let Ok(array) = page.get_mut(b"Annots").and_then(Object::as_array_mut)
            {
                array.retain(|entry| entry.as_reference().ok() != Some(object_id));
            }
        }

        self.document.remove_object(&object_id)?;
        self.has_unsaved_changes = true;
        Ok(())
    }

    fn add_ink_annotation(&mut self, index: usize, annotation: NewInkAnnotation) -> Result<String> {
        let page_id = self.page_id_from_index(index)?;
        let rect = annotation.rect.normalized();

        let mut ink_list = Vec::new();
        for stroke in annotation.strokes {
            if stroke.points.len() < 2 {
                continue;
            }
            let mut encoded = Vec::with_capacity(stroke.points.len() * 2);
            for (x, y) in stroke.points {
                encoded.push(x.into());
                encoded.push(y.into());
            }
            if encoded.len() >= 4 {
                ink_list.push(Object::Array(encoded));
            }
        }
        if ink_list.is_empty() {
            bail!("ink annotation requires at least one stroke with two points");
        }

        let (red, green, blue) = annotation.color_rgb;
        let width = annotation.width.max(0.5);
        let mut border_style = Dictionary::new();
        border_style.set("W", width);
        border_style.set("S", "S");

        let mut dict = Dictionary::new();
        dict.set("Type", "Annot");
        dict.set("Subtype", "Ink");
        dict.set(
            "Rect",
            vec![
                rect.left.into(),
                rect.bottom.into(),
                rect.right.into(),
                rect.top.into(),
            ],
        );
        dict.set("InkList", Object::Array(ink_list));
        dict.set(
            "C",
            vec![
                red.clamp(0.0, 1.0).into(),
                green.clamp(0.0, 1.0).into(),
                blue.clamp(0.0, 1.0).into(),
            ],
        );
        dict.set("CA", annotation.opacity.clamp(0.05, 1.0));
        dict.set("Border", vec![0.into(), 0.into(), width.into()]);
        dict.set("BS", border_style);
        dict.set("F", 4i64);
        dict.set("P", page_id);

        let annotation_id = self.document.add_object(dict);
        self.append_annotation_to_page(page_id, annotation_id)?;
        self.has_unsaved_changes = true;

        Ok(Self::annotation_id(annotation_id))
    }

    fn has_unsaved_changes(&self) -> bool {
        self.has_unsaved_changes
    }

    fn save_in_place(&mut self) -> Result<()> {
        let target = self.path.clone();
        self.save_to_path(&target)
    }

    fn save_as(&mut self, path: &Path) -> Result<()> {
        self.save_to_path(path)
    }

    fn reload_if_changed(&mut self) -> Result<bool> {
        if self.has_unsaved_changes {
            return Ok(false);
        }

        let metadata = std::fs::metadata(&self.path)
            .with_context(|| format!("failed to read metadata for {}", self.path.display()))?;
        let modified = metadata.modified().ok();
        let changed = match (self.modified_at, modified) {
            (Some(previous), Some(current)) => current > previous,
            (None, Some(_)) => true,
            _ => false,
        };

        if changed {
            self.reload()?;
            self.modified_at = modified;
            return Ok(true);
        }

        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::LopdfBackend;
    use crate::config::{NotesLayout, NotesMode, NotesSide, SplitAxis};
    use crate::document::{
        AnnotationMetadata, AnnotationPriority, AnnotationStatus, DocumentBackend,
        NewTextAnnotation, PdfRect,
    };
    use crate::document::{PageBitmap, PageView};
    use lopdf::{Dictionary, Document, Object};
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn loads_information_geometry_fixture_when_present() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Information Geometry.pdf");
        if !fixture.exists() {
            return;
        }

        let backend = LopdfBackend::open(&fixture).expect("fixture should load");
        assert!(
            backend.page_count() > 1,
            "fixture should contain multiple pages"
        );
        let detected = backend
            .detect_notes_layout(0, NotesMode::Auto)
            .expect("detect notes layout");
        let layout = detected
            .layout
            .expect("fixture should provide a detected split candidate");
        assert_eq!(
            layout.axis,
            SplitAxis::Vertical,
            "fixture expected vertical split"
        );
        assert_eq!(
            layout.notes_side,
            NotesSide::Right,
            "fixture expected notes on right side"
        );
    }

    #[test]
    fn text_annotation_round_trip_save_reopen_when_fixture_present() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Information Geometry.pdf");
        if !fixture.exists() {
            return;
        }

        let temp = tempdir().expect("create temp directory");
        let copied_pdf = temp.path().join("roundtrip.pdf");
        std::fs::copy(&fixture, &copied_pdf).expect("copy fixture");

        let mut backend = LopdfBackend::open(&copied_pdf).expect("open copied fixture");
        let created = backend
            .add_text_annotation(
                0,
                NewTextAnnotation {
                    rect: PdfRect {
                        left: 40.0,
                        bottom: 40.0,
                        right: 180.0,
                        top: 120.0,
                    },
                    contents: "todo: rehearse pacing".to_owned(),
                    metadata: AnnotationMetadata::default(),
                },
            )
            .expect("create text annotation");
        assert!(
            backend.has_unsaved_changes(),
            "backend should be dirty after add"
        );

        let mut updated = created.clone();
        updated.contents = "todo: rehearse pacing (updated)".to_owned();
        backend
            .update_text_annotation(&updated)
            .expect("update text annotation");
        backend.save_in_place().expect("save in place");
        assert!(
            !backend.has_unsaved_changes(),
            "backend should be clean after save"
        );

        let mut reopened = LopdfBackend::open(&copied_pdf).expect("reopen after save");
        let listed = reopened
            .list_text_annotations(0)
            .expect("list text annotations after save");
        let persisted = listed
            .iter()
            .find(|entry| entry.id == created.id)
            .expect("saved annotation should persist");
        assert!(
            persisted.contents.contains("updated"),
            "updated annotation contents should persist"
        );

        reopened
            .delete_annotation(&created.id)
            .expect("delete annotation");
        reopened.save_in_place().expect("save after delete");

        let verified = LopdfBackend::open(&copied_pdf).expect("reopen after delete");
        let remaining = verified
            .list_text_annotations(0)
            .expect("list text annotations after delete");
        assert!(
            remaining.iter().all(|entry| entry.id != created.id),
            "deleted annotation should not exist"
        );
    }

    #[test]
    fn split_views_request_higher_source_resolution() {
        let vertical_half = NotesLayout {
            axis: SplitAxis::Vertical,
            split_ratio: 0.5,
            notes_side: NotesSide::Right,
            confidence: 1.0,
        };

        let slide_width =
            LopdfBackend::effective_render_width(1920, PageView::SlideOnly, Some(vertical_half));
        let full_width =
            LopdfBackend::effective_render_width(1920, PageView::Full, Some(vertical_half));
        assert_eq!(full_width, 1920);
        assert_eq!(slide_width, 3840);

        let horizontal_half = NotesLayout {
            axis: SplitAxis::Horizontal,
            split_ratio: 0.5,
            notes_side: NotesSide::Bottom,
            confidence: 1.0,
        };
        let horizontal_scaled =
            LopdfBackend::effective_render_width(1280, PageView::NotesOnly, Some(horizontal_half));
        assert_eq!(horizontal_scaled, 2560);
    }

    #[test]
    fn decode_pdf_text_handles_ascii_and_utf16be() {
        assert_eq!(LopdfBackend::decode_pdf_text(b"Section 1"), "Section 1");

        let utf16be = [0xFE, 0xFF, 0x00, 0x43, 0x00, 0x68, 0x00, 0x31];
        assert_eq!(LopdfBackend::decode_pdf_text(&utf16be), "Ch1");
    }

    #[test]
    fn destination_page_index_parser_is_defensive() {
        let document = Document::with_version("1.7");
        let page_map = BTreeMap::new();

        let valid = Object::Array(vec![Object::Integer(2), Object::Name(b"Fit".to_vec())]);
        assert_eq!(
            LopdfBackend::destination_page_index_from_object(&document, &valid, &page_map, 4, 0),
            Some(2)
        );

        let out_of_range = Object::Array(vec![Object::Integer(9)]);
        assert_eq!(
            LopdfBackend::destination_page_index_from_object(
                &document,
                &out_of_range,
                &page_map,
                4,
                0,
            ),
            None
        );

        let negative = Object::Array(vec![Object::Integer(-1)]);
        assert_eq!(
            LopdfBackend::destination_page_index_from_object(&document, &negative, &page_map, 4, 0),
            None
        );

        let unsupported = Object::Name(b"chapter-one".to_vec());
        assert_eq!(
            LopdfBackend::destination_page_index_from_object(
                &document,
                &unsupported,
                &page_map,
                4,
                0,
            ),
            None
        );
    }

    #[test]
    fn outline_path_lookup_is_resilient_when_fixture_present() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Information Geometry.pdf");
        if !fixture.exists() {
            return;
        }

        let backend = LopdfBackend::open(&fixture).expect("fixture should load");
        for index in 0..backend.page_count() {
            if let Some(path) = backend.outline_path_for_page(index) {
                assert!(
                    !path.is_empty(),
                    "outline path entries should not be empty strings"
                );
            }
        }
    }

    #[test]
    fn beamer_mode_resolves_vertical_right_on_fixture_when_present() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Information Geometry.pdf");
        if !fixture.exists() {
            return;
        }

        let backend = LopdfBackend::open(&fixture).expect("fixture should load");
        let detected = backend
            .detect_notes_layout(0, NotesMode::BeamerSecondScreen)
            .expect("detect beamer notes layout");
        let layout = detected
            .layout
            .expect("beamer mode should resolve a layout");
        assert_eq!(layout.axis, SplitAxis::Vertical);
        assert_eq!(layout.notes_side, NotesSide::Right);
    }

    #[test]
    fn mode_detection_falls_back_with_warning_on_low_confidence_bitmap() {
        let flat_bitmap = PageBitmap {
            width: 640,
            height: 480,
            rgba: vec![255_u8; 640 * 480 * 4],
        };

        let beamer =
            LopdfBackend::detect_layout_for_mode(&flat_bitmap, NotesMode::BeamerSecondScreen);
        let beamer_layout = beamer
            .layout
            .expect("beamer mode should return fallback layout");
        assert_eq!(beamer_layout.axis, SplitAxis::Vertical);
        assert_eq!(beamer_layout.notes_side, NotesSide::Right);
        assert!(beamer.warning.is_some());

        let libre = LopdfBackend::detect_layout_for_mode(&flat_bitmap, NotesMode::LibreOfficeNotes);
        let libre_layout = libre
            .layout
            .expect("libre mode should return fallback layout");
        assert_eq!(libre_layout.axis, SplitAxis::Horizontal);
        assert_eq!(libre_layout.notes_side, NotesSide::Bottom);
        assert!(libre.warning.is_some());
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
        let composed = LopdfBackend::compose_contents_with_metadata("hello world", &metadata);
        assert!(composed.starts_with(super::META_PREFIX));
        let stripped = LopdfBackend::strip_custom_metadata_block(&composed);
        assert_eq!(stripped, "hello world");
        let decoded = LopdfBackend::decode_custom_metadata_block(&composed)
            .expect("metadata block should decode");
        assert_eq!(decoded.get("priority").map(String::as_str), Some("high"));
        assert_eq!(
            decoded.get("status").map(String::as_str),
            Some("in_progress")
        );
    }

    #[test]
    fn malformed_metadata_block_is_not_decoded_or_stripped() {
        let malformed = format!(
            "{}author=Alice;status=done\nvisible note",
            super::META_PREFIX
        );
        assert!(
            LopdfBackend::decode_custom_metadata_block(&malformed).is_none(),
            "unterminated metadata blocks should be ignored"
        );
        assert_eq!(
            LopdfBackend::strip_custom_metadata_block(&malformed),
            malformed
        );
    }

    #[test]
    fn metadata_from_pdf_dict_uses_partial_standard_fields() {
        let mut dict = Dictionary::new();
        dict.set("T", Object::string_literal("  Bob  "));
        dict.set("C", vec![0.2.into(), 0.4.into(), 0.6.into()]);
        dict.set(
            "Subj",
            Object::string_literal("status=done;priority=high;ignored=value"),
        );
        dict.set("M", Object::string_literal("D:20260202120000+0000"));

        let metadata = LopdfBackend::metadata_from_pdf_dict(
            &dict,
            "[[DECKLIGHT_META:author=Alice;status=in_progress\nnote body",
        );
        assert_eq!(metadata.author, "Bob");
        assert_eq!(metadata.color_rgb, (0.2, 0.4, 0.6));
        assert_eq!(
            metadata.modified_at.as_deref(),
            Some("D:20260202120000+0000")
        );
        assert_eq!(metadata.priority, AnnotationPriority::High);
        assert_eq!(metadata.status, AnnotationStatus::Done);
        assert!(metadata.tags.is_empty());
    }
}
