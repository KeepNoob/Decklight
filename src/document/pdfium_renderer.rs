use super::pdfium_runtime::with_pdfium;
use crate::config::{NotesLayout, NotesSide, SplitAxis};
use crate::document::{PageBitmap, PageView};
use anyhow::{Context, Result, bail};
use pdfium_render::prelude::{PdfBitmap, PdfBitmapFormat, PdfRenderConfig};
use std::path::Path;

const MIN_RENDER_WIDTH: usize = 200;
const MAX_RENDER_SIZE: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RenderBounds {
    left: usize,
    top: usize,
    right: usize,
    bottom: usize,
}

impl RenderBounds {
    fn width(self) -> usize {
        self.right - self.left
    }

    fn height(self) -> usize {
        self.bottom - self.top
    }
}

pub(crate) fn effective_render_width(
    target_width: usize,
    view: PageView,
    layout: Option<NotesLayout>,
) -> usize {
    let base = target_width.clamp(MIN_RENDER_WIDTH, MAX_RENDER_SIZE) as f32;
    let scale = view_scale_factor(view, layout);
    (base * scale)
        .round()
        .clamp(MIN_RENDER_WIDTH as f32, MAX_RENDER_SIZE as f32) as usize
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

fn axis_bounds(length: usize, from: usize, to: usize) -> (usize, usize) {
    if length == 0 {
        return (0, 1);
    }

    let start = from.min(length - 1);
    let end = to.min(length).max(start + 1).min(length);
    (start, end)
}

fn split_bounds(
    full_width: usize,
    full_height: usize,
    layout: NotesLayout,
    view: PageView,
) -> RenderBounds {
    match layout.axis {
        SplitAxis::Vertical => {
            let split_x =
                ((full_width as f32) * layout.split_ratio.clamp(0.0, 1.0)).round() as usize;
            let (notes_from, notes_to) = match layout.notes_side {
                NotesSide::Left => (0usize, split_x),
                NotesSide::Right => (split_x, full_width),
                NotesSide::Top | NotesSide::Bottom => (split_x, full_width),
            };
            let (slide_from, slide_to) = if matches!(layout.notes_side, NotesSide::Left) {
                (split_x, full_width)
            } else {
                (0usize, split_x)
            };
            let (from, to) = match view {
                PageView::SlideOnly => (slide_from, slide_to),
                PageView::NotesOnly => (notes_from, notes_to),
                PageView::Full => (0, full_width),
            };
            let (left, right) = axis_bounds(full_width, from, to);
            RenderBounds {
                left,
                top: 0,
                right,
                bottom: full_height,
            }
        }
        SplitAxis::Horizontal => {
            let split_y =
                ((full_height as f32) * layout.split_ratio.clamp(0.0, 1.0)).round() as usize;
            let (notes_from, notes_to) = match layout.notes_side {
                NotesSide::Top => (0usize, split_y),
                NotesSide::Bottom => (split_y, full_height),
                NotesSide::Left | NotesSide::Right => (split_y, full_height),
            };
            let (slide_from, slide_to) = if matches!(layout.notes_side, NotesSide::Top) {
                (split_y, full_height)
            } else {
                (0usize, split_y)
            };
            let (from, to) = match view {
                PageView::SlideOnly => (slide_from, slide_to),
                PageView::NotesOnly => (notes_from, notes_to),
                PageView::Full => (0, full_height),
            };
            let (top, bottom) = axis_bounds(full_height, from, to);
            RenderBounds {
                left: 0,
                top,
                right: full_width,
                bottom,
            }
        }
    }
}

fn full_output_dimensions(
    page_width: f32,
    page_height: f32,
    target_width: usize,
) -> Result<(usize, usize)> {
    if !page_width.is_finite()
        || !page_height.is_finite()
        || page_width <= 0.0
        || page_height <= 0.0
    {
        bail!("PDFium returned invalid page dimensions {page_width}x{page_height}");
    }

    let target_width = target_width.clamp(MIN_RENDER_WIDTH, MAX_RENDER_SIZE) as f32;
    let mut width_scale = target_width / page_width;
    let mut height_scale = width_scale;
    if page_height * height_scale > MAX_RENDER_SIZE as f32 {
        height_scale = MAX_RENDER_SIZE as f32 / page_height;
        width_scale = height_scale;
    }

    let width = (page_width * width_scale).round() as usize;
    let height = (page_height * height_scale).round() as usize;
    if width == 0 || height == 0 {
        bail!("PDFium returned an unrenderable page size {page_width}x{page_height}");
    }

    Ok((width, height))
}

fn full_page_render_config(target_width: usize) -> PdfRenderConfig {
    PdfRenderConfig::new()
        .set_target_width(target_width.clamp(MIN_RENDER_WIDTH, MAX_RENDER_SIZE) as i32)
        .set_maximum_height(MAX_RENDER_SIZE as i32)
        .render_annotations(true)
        .render_form_data(true)
}

pub(crate) fn render_page_bitmap_from_path(
    path: &Path,
    index: usize,
    view: PageView,
    layout: Option<NotesLayout>,
    target_width: usize,
) -> Result<PageBitmap> {
    with_pdfium(|pdfium| {
        let document = pdfium
            .load_pdf_from_file(path, None)
            .with_context(|| format!("failed to load PDF with PDFium: {}", path.display()))?;
        let page_index: i32 = index
            .try_into()
            .map_err(|_| anyhow::anyhow!("page index out of range for PDFium: {index}"))?;
        let page = document
            .pages()
            .get(page_index)
            .with_context(|| format!("failed to load page {index} from {}", path.display()))?;

        let full_target_width = effective_render_width(target_width, view, layout);
        if let Some(layout) = layout
            && view != PageView::Full
        {
            let (full_width, full_height) =
                full_output_dimensions(page.width().value, page.height().value, full_target_width)?;
            let bounds = split_bounds(full_width, full_height, layout, view);
            let mut bitmap = PdfBitmap::empty(
                bounds.width() as i32,
                bounds.height() as i32,
                PdfBitmapFormat::default(),
            )?;
            let render_config = PdfRenderConfig::new()
                .set_fixed_size(full_width as i32, full_height as i32)
                .set_origin(-(bounds.left as i32), -(bounds.top as i32))
                .render_annotations(true)
                .render_form_data(true);
            page.render_into_bitmap_with_config(&mut bitmap, &render_config)?;

            return Ok(PageBitmap {
                width: bitmap.width() as usize,
                height: bitmap.height() as usize,
                rgba: bitmap.as_rgba_bytes(),
            });
        }

        let rendered = page.render_with_config(&full_page_render_config(full_target_width))?;
        Ok(PageBitmap {
            width: rendered.width() as usize,
            height: rendered.height() as usize,
            rgba: rendered.as_rgba_bytes(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{NotesSide, SplitAxis};

    fn layout(axis: SplitAxis, split_ratio: f32, notes_side: NotesSide) -> NotesLayout {
        NotesLayout {
            axis,
            split_ratio,
            notes_side,
            confidence: 1.0,
        }
    }

    #[test]
    fn effective_width_preserves_split_resolution_scaling_and_caps() {
        let vertical_half = layout(SplitAxis::Vertical, 0.5, NotesSide::Right);
        assert_eq!(
            effective_render_width(1920, PageView::SlideOnly, Some(vertical_half)),
            3840
        );
        assert_eq!(
            effective_render_width(1920, PageView::Full, Some(vertical_half)),
            1920
        );

        let narrow_region = layout(SplitAxis::Vertical, 0.1, NotesSide::Right);
        assert_eq!(
            effective_render_width(4096, PageView::NotesOnly, Some(narrow_region)),
            4096
        );
        assert_eq!(effective_render_width(100, PageView::Full, None), 200);
    }

    #[test]
    fn vertical_bounds_match_left_and_right_notes() {
        let left = layout(SplitAxis::Vertical, 0.25, NotesSide::Left);
        assert_eq!(
            split_bounds(1000, 800, left, PageView::NotesOnly),
            RenderBounds {
                left: 0,
                top: 0,
                right: 250,
                bottom: 800,
            }
        );
        assert_eq!(
            split_bounds(1000, 800, left, PageView::SlideOnly),
            RenderBounds {
                left: 250,
                top: 0,
                right: 1000,
                bottom: 800,
            }
        );

        let right = layout(SplitAxis::Vertical, 0.25, NotesSide::Right);
        assert_eq!(
            split_bounds(1000, 800, right, PageView::NotesOnly),
            RenderBounds {
                left: 250,
                top: 0,
                right: 1000,
                bottom: 800,
            }
        );
        assert_eq!(
            split_bounds(1000, 800, right, PageView::SlideOnly),
            RenderBounds {
                left: 0,
                top: 0,
                right: 250,
                bottom: 800,
            }
        );
    }

    #[test]
    fn vertical_bounds_keep_legacy_behavior_for_horizontal_sides() {
        for notes_side in [NotesSide::Top, NotesSide::Bottom] {
            let layout = layout(SplitAxis::Vertical, 0.4, notes_side);
            assert_eq!(
                split_bounds(1000, 800, layout, PageView::NotesOnly),
                RenderBounds {
                    left: 400,
                    top: 0,
                    right: 1000,
                    bottom: 800,
                }
            );
        }
    }

    #[test]
    fn horizontal_bounds_match_top_and_bottom_notes() {
        let top = layout(SplitAxis::Horizontal, 0.25, NotesSide::Top);
        assert_eq!(
            split_bounds(1000, 800, top, PageView::NotesOnly),
            RenderBounds {
                left: 0,
                top: 0,
                right: 1000,
                bottom: 200,
            }
        );
        assert_eq!(
            split_bounds(1000, 800, top, PageView::SlideOnly),
            RenderBounds {
                left: 0,
                top: 200,
                right: 1000,
                bottom: 800,
            }
        );

        let bottom = layout(SplitAxis::Horizontal, 0.25, NotesSide::Bottom);
        assert_eq!(
            split_bounds(1000, 800, bottom, PageView::NotesOnly),
            RenderBounds {
                left: 0,
                top: 200,
                right: 1000,
                bottom: 800,
            }
        );
        assert_eq!(
            split_bounds(1000, 800, bottom, PageView::SlideOnly),
            RenderBounds {
                left: 0,
                top: 0,
                right: 1000,
                bottom: 200,
            }
        );
    }

    #[test]
    fn horizontal_bounds_keep_legacy_behavior_for_vertical_sides() {
        for notes_side in [NotesSide::Left, NotesSide::Right] {
            let layout = layout(SplitAxis::Horizontal, 0.4, notes_side);
            assert_eq!(
                split_bounds(1000, 800, layout, PageView::NotesOnly),
                RenderBounds {
                    left: 0,
                    top: 320,
                    right: 1000,
                    bottom: 800,
                }
            );
        }
    }

    #[test]
    fn bounds_preserve_dimensions_and_clamping_at_edge_ratios() {
        let left = layout(SplitAxis::Vertical, 0.0, NotesSide::Left);
        let right = layout(SplitAxis::Vertical, 1.0, NotesSide::Right);
        let top = layout(SplitAxis::Horizontal, 0.0, NotesSide::Top);
        let bottom = layout(SplitAxis::Horizontal, 1.0, NotesSide::Bottom);

        assert_eq!(split_bounds(101, 73, left, PageView::NotesOnly).width(), 1);
        assert_eq!(
            split_bounds(101, 73, left, PageView::SlideOnly).width(),
            101
        );
        assert_eq!(split_bounds(101, 73, right, PageView::NotesOnly).width(), 1);
        assert_eq!(
            split_bounds(101, 73, right, PageView::SlideOnly).width(),
            101
        );
        assert_eq!(split_bounds(101, 73, top, PageView::NotesOnly).height(), 1);
        assert_eq!(split_bounds(101, 73, top, PageView::SlideOnly).height(), 73);
        assert_eq!(
            split_bounds(101, 73, bottom, PageView::NotesOnly).height(),
            1
        );
        assert_eq!(
            split_bounds(101, 73, bottom, PageView::SlideOnly).height(),
            73
        );

        for view in [PageView::SlideOnly, PageView::NotesOnly] {
            let bounds = split_bounds(101, 73, left, view);
            assert!(bounds.right <= 101);
            assert!(bounds.bottom <= 73);
            assert!(bounds.width() >= 1);
            assert!(bounds.height() >= 1);
        }
    }

    #[test]
    fn full_output_dimensions_match_target_scaling_and_height_cap() {
        assert_eq!(
            full_output_dimensions(600.0, 800.0, 2000).expect("valid page dimensions"),
            (2000, 2667)
        );
        assert_eq!(
            full_output_dimensions(100.0, 10_000.0, 200).expect("valid page dimensions"),
            (41, 4096)
        );
        assert_eq!(
            full_output_dimensions(600.0, 800.0, 5000).expect("valid page dimensions"),
            (3072, 4096)
        );
    }
}
