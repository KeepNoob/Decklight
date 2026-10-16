use super::pdfium_renderer::render_page_bitmap_from_path;
use crate::config::{NotesLayout, NotesMode, NotesSide, SplitAxis};
use crate::document::{NotesDetectionResult, PageBitmap, PageView};
use anyhow::Result;
use std::path::Path;

const AUTO_CONFIDENCE_MIN: f32 = 0.12;
const MODE_CONFIDENCE_MIN: f32 = 0.20;
const LAYOUT_DETECTION_RENDER_WIDTH: usize = 1280;

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

#[derive(Debug, Clone, Copy, Default)]
struct LumaTotals {
    luma_sum: f64,
    ink: usize,
    dark: usize,
}

impl LumaTotals {
    fn add_pixel(&mut self, luma: f32) {
        self.luma_sum += f64::from(luma);
        if luma < 220.0 {
            self.ink += 1;
        }
        if luma < 120.0 {
            self.dark += 1;
        }
    }

    fn add(&mut self, other: LumaTotals) {
        self.luma_sum += other.luma_sum;
        self.ink += other.ink;
        self.dark += other.dark;
    }
}

/// Luma of every pixel plus per-column and per-row totals, computed in one pass so each split
/// candidate sums a few totals instead of rescanning the whole bitmap.
struct LumaProfile {
    width: usize,
    height: usize,
    luma: Vec<f32>,
    columns: Vec<LumaTotals>,
    rows: Vec<LumaTotals>,
}

impl LumaProfile {
    fn from_bitmap(bitmap: &PageBitmap) -> Option<Self> {
        let width = bitmap.width;
        let height = bitmap.height;
        let pixel_count = width.checked_mul(height)?;
        if width == 0 || height == 0 || bitmap.rgba.len() < pixel_count.checked_mul(4)? {
            return None;
        }

        let luma: Vec<f32> = bitmap
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .take(pixel_count)
            .map(|pixel| {
                0.2126 * f32::from(pixel[0])
                    + 0.7152 * f32::from(pixel[1])
                    + 0.0722 * f32::from(pixel[2])
            })
            .collect();
        let mut columns = vec![LumaTotals::default(); width];
        let mut rows = vec![LumaTotals::default(); height];
        for (row_totals, row) in rows.iter_mut().zip(luma.chunks_exact(width)) {
            for (column_totals, &pixel_luma) in columns.iter_mut().zip(row) {
                column_totals.add_pixel(pixel_luma);
                row_totals.add_pixel(pixel_luma);
            }
        }

        Some(Self {
            width,
            height,
            luma,
            columns,
            rows,
        })
    }

    /// Clamps `start..end` the same way for both axes: never empty unless `start` is at the end.
    fn clamp_range(start: usize, end: usize, length: usize) -> (usize, usize) {
        let clamped_start = start.min(length);
        let clamped_end = end.min(length).max(clamped_start + 1).min(length);
        (clamped_start, clamped_end)
    }

    fn stats(totals: &[LumaTotals], cross_length: usize) -> RegionStats {
        let mut sum = LumaTotals::default();
        for line in totals {
            sum.add(*line);
        }
        let safe_count = (totals.len() * cross_length).max(1) as f64;
        RegionStats {
            mean_luma: (sum.luma_sum / safe_count) as f32,
            ink_density: (sum.ink as f64 / safe_count) as f32,
            dark_density: (sum.dark as f64 / safe_count) as f32,
        }
    }

    fn column_stats(&self, from_x: usize, to_x: usize) -> RegionStats {
        let (start, end) = Self::clamp_range(from_x, to_x, self.width);
        Self::stats(&self.columns[start..end], self.height)
    }

    fn row_stats(&self, from_y: usize, to_y: usize) -> RegionStats {
        let (start, end) = Self::clamp_range(from_y, to_y, self.height);
        Self::stats(&self.rows[start..end], self.width)
    }

    fn seam_contrast_vertical(&self, split_x: usize) -> f32 {
        if self.width < 3 || split_x == 0 || split_x >= self.width {
            return 0.0;
        }

        let diff_sum: f32 = self
            .luma
            .chunks_exact(self.width)
            .map(|row| (row[split_x - 1] - row[split_x]).abs())
            .sum();
        (diff_sum / self.height as f32 / 255.0).clamp(0.0, 1.0)
    }

    fn seam_contrast_horizontal(&self, split_y: usize) -> f32 {
        if self.height < 3 || split_y == 0 || split_y >= self.height {
            return 0.0;
        }

        let above = &self.luma[(split_y - 1) * self.width..split_y * self.width];
        let below = &self.luma[split_y * self.width..(split_y + 1) * self.width];
        let diff_sum: f32 = above
            .iter()
            .zip(below)
            .map(|(top, bottom)| (top - bottom).abs())
            .sum();
        (diff_sum / self.width as f32 / 255.0).clamp(0.0, 1.0)
    }
}

fn analyze_vertical_candidate(profile: &LumaProfile, split_ratio: f32) -> SplitCandidate {
    let width = profile.width;
    let height = profile.height;
    let split_x = ((width as f32) * split_ratio).round() as usize;
    let left = profile.column_stats(0, split_x);
    let right = profile.column_stats(split_x, width);
    let seam = profile.seam_contrast_vertical(split_x);
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

fn analyze_horizontal_candidate(profile: &LumaProfile, split_ratio: f32) -> SplitCandidate {
    let width = profile.width;
    let height = profile.height;
    let split_y = ((height as f32) * split_ratio).round() as usize;
    let top = profile.row_stats(0, split_y);
    let bottom = profile.row_stats(split_y, height);
    let seam = profile.seam_contrast_horizontal(split_y);
    let bg_delta = ((top.mean_luma - bottom.mean_luma).abs() / 255.0).clamp(0.0, 1.0);
    let ink_delta = (top.ink_density - bottom.ink_density).abs().clamp(0.0, 1.0);

    let mut confidence = 0.45 * bg_delta + 0.35 * seam + 0.20 * ink_delta;
    let aspect = height as f32 / width.max(1) as f32;
    if aspect > 1.2 {
        confidence += ((aspect - 1.2) * 0.18).clamp(0.0, 0.2);
    }

    let top_score =
        ((255.0 - top.mean_luma) / 255.0) * 0.65 + top.ink_density * 0.25 + top.dark_density * 0.10;
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

fn detect_layout_from_profile(profile: &LumaProfile) -> Option<SplitCandidate> {
    if profile.width < 20 || profile.height < 20 {
        return None;
    }

    let candidates = [0.35_f32, 0.40, 0.45, 0.50, 0.55, 0.60, 0.65];
    let mut best: Option<SplitCandidate> = None;

    for ratio in candidates {
        let vertical = analyze_vertical_candidate(profile, ratio);
        if best.is_none_or(|existing| vertical.confidence > existing.confidence) {
            best = Some(vertical);
        }

        let horizontal = analyze_horizontal_candidate(profile, ratio);
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
    profile: &LumaProfile,
    axis: SplitAxis,
    split_ratio: f32,
) -> SplitCandidate {
    match axis {
        SplitAxis::Vertical => analyze_vertical_candidate(profile, split_ratio),
        SplitAxis::Horizontal => analyze_horizontal_candidate(profile, split_ratio),
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
    profile: &LumaProfile,
    prior: ModeLayoutPrior,
) -> Option<SplitCandidate> {
    if profile.width < 20 || profile.height < 20 {
        return None;
    }

    let mut best: Option<(SplitCandidate, f32)> = None;
    for split_ratio in prior.split_ratios.iter().copied() {
        let candidate = analyze_axis_candidate(profile, prior.axis, split_ratio);
        let score = prior_weighted_score(candidate, prior);
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

    let profile = LumaProfile::from_bitmap(bitmap);
    if mode == NotesMode::Auto {
        let candidate = profile.as_ref().and_then(detect_layout_from_profile);
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

    let Some(prior) = mode_prior(mode) else {
        return NotesDetectionResult {
            layout: None,
            confidence: 0.0,
            reason: "Unsupported notes mode.".to_owned(),
            warning: Some("Unsupported notes mode; using full page.".to_owned()),
        };
    };

    let prior_candidate = profile
        .as_ref()
        .and_then(|profile| detect_layout_with_prior(profile, prior));
    let generic_candidate = profile.as_ref().and_then(detect_layout_from_profile);
    let chosen_candidate = match (prior_candidate, generic_candidate) {
        (Some(prior_hit), Some(generic_hit)) => {
            let prior_score = prior_weighted_score(prior_hit, prior);
            if prior_score + 0.04 >= generic_hit.confidence {
                prior_hit
            } else {
                generic_hit
            }
        }
        (Some(prior_hit), None) => prior_hit,
        (None, Some(generic_hit)) => generic_hit,
        (None, None) => {
            let fallback = fallback_layout_for_prior(prior);
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
        let fallback = fallback_layout_for_prior(prior);
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

/// Renders the page and detects where its notes split is.
///
/// This renders a full page, so the app runs it on the render worker instead of the UI thread.
pub(crate) fn detect_notes_layout_from_path(
    path: &Path,
    index: usize,
    mode: NotesMode,
) -> Result<NotesDetectionResult> {
    let full = render_page_bitmap_from_path(
        path,
        index,
        PageView::Full,
        None,
        LAYOUT_DETECTION_RENDER_WIDTH,
    )?;
    Ok(detect_layout_for_mode(&full, mode))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_detection_falls_back_with_warning_on_low_confidence_bitmap() {
        let flat_bitmap = PageBitmap {
            width: 640,
            height: 480,
            rgba: vec![255_u8; 640 * 480 * 4],
        };

        let beamer = detect_layout_for_mode(&flat_bitmap, NotesMode::BeamerSecondScreen);
        let beamer_layout = beamer
            .layout
            .expect("beamer mode should return fallback layout");
        assert_eq!(beamer_layout.axis, SplitAxis::Vertical);
        assert_eq!(beamer_layout.notes_side, NotesSide::Right);
        assert!(beamer.warning.is_some());

        let libre = detect_layout_for_mode(&flat_bitmap, NotesMode::LibreOfficeNotes);
        let libre_layout = libre
            .layout
            .expect("libre mode should return fallback layout");
        assert_eq!(libre_layout.axis, SplitAxis::Horizontal);
        assert_eq!(libre_layout.notes_side, NotesSide::Bottom);
        assert!(libre.warning.is_some());
    }

    #[test]
    fn luma_profile_region_stats_match_a_direct_scan() {
        let width = 7;
        let height = 5;
        let rgba: Vec<u8> = (0..width * height)
            .flat_map(|pixel| {
                let value = ((pixel * 37) % 256) as u8;
                [value, value.wrapping_mul(3), 255 - value, 255]
            })
            .collect();
        let bitmap = PageBitmap {
            width,
            height,
            rgba: rgba.clone(),
        };
        let profile = LumaProfile::from_bitmap(&bitmap).expect("valid bitmap");
        let luma_at = |x: usize, y: usize| {
            let offset = (y * width + x) * 4;
            0.2126 * f32::from(rgba[offset])
                + 0.7152 * f32::from(rgba[offset + 1])
                + 0.0722 * f32::from(rgba[offset + 2])
        };

        let mut direct_sum = 0f32;
        let mut direct_ink = 0usize;
        for y in 0..height {
            for x in 2..width {
                let luma = luma_at(x, y);
                direct_sum += luma;
                direct_ink += usize::from(luma < 220.0);
            }
        }
        let pixel_count = ((width - 2) * height) as f32;
        let stats = profile.column_stats(2, width);
        assert!((stats.mean_luma - direct_sum / pixel_count).abs() < 1e-3);
        assert!((stats.ink_density - direct_ink as f32 / pixel_count).abs() < 1e-6);

        let direct_seam: f32 = (0..width)
            .map(|x| (luma_at(x, 2) - luma_at(x, 3)).abs())
            .sum::<f32>()
            / width as f32
            / 255.0;
        assert!((profile.seam_contrast_horizontal(3) - direct_seam).abs() < 1e-6);
        assert_eq!(profile.row_stats(height, height).mean_luma, 0.0);
    }

    #[test]
    fn short_bitmaps_do_not_build_a_luma_profile() {
        let bitmap = PageBitmap {
            width: 4,
            height: 4,
            rgba: vec![255; 12],
        };
        assert!(LumaProfile::from_bitmap(&bitmap).is_none());
    }
}
