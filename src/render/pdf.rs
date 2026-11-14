//! PDF 的页面布局与后台栅格化。
//!
//! 页面位置用逻辑像素计算，栅格化丢到后台线程，翻页时界面不会被阻塞；
//! 已经渲染过的页按 `scale_key` 缓存，避免重复光栅化。

use std::ops::Range;
use std::sync::Arc;

use gpui_kit::*;

use crate::pdf::{self, PdfSource};
use crate::{
    DocumentContent, PDF_OVERSCAN, PDF_PAGE_GAP, PDF_PAGE_MARGIN, PDF_TOP_PADDING, PoemReader,
};

/// Where the pages of a PDF land in the scroll area, in logical pixels.
pub(crate) struct PdfLayout {
    pub(crate) pages: Vec<Size<f32>>,
    pub(crate) viewport: Size<f32>,
    pub(crate) content_width: f32,
    /// Raster pixels per PDF point.
    pub(crate) scale: f32,
    /// Quantized `scale`, used to tell cached pages from stale ones.
    pub(crate) scale_key: u32,
}

impl PdfLayout {
    pub(crate) fn new(source: &PdfSource, viewport: Size<f32>, zoom: f32, dpi: f32) -> Self {
        let page_width = ((viewport.width - 2.0 * PDF_PAGE_MARGIN) * zoom).max(160.0);
        let pages = source
            .page_sizes()
            .iter()
            .map(|size| {
                let ratio = (size.height / size.width).clamp(0.1, 10.0);
                Size::new(page_width, page_width * ratio)
            })
            .collect::<Vec<_>>();

        let reference_width = source
            .page_sizes()
            .first()
            .map(|size| size.width)
            .unwrap_or(1.0);
        let scale_key = pdf::scale_key(
            source.clamp_scale((page_width * dpi / reference_width).clamp(0.4, 4.0)),
        );

        Self {
            pages,
            viewport,
            content_width: (page_width + 2.0 * PDF_PAGE_MARGIN).max(viewport.width),
            scale: scale_key as f32 / 10.0,
            scale_key,
        }
    }

    fn page_top(&self, index: usize) -> f32 {
        let mut top = PDF_TOP_PADDING;
        for page in &self.pages[..index] {
            top += page.height + PDF_PAGE_GAP;
        }
        top
    }

    /// Convert a `page + fraction` anchor into a distance from the top.
    pub(crate) fn offset_for_anchor(&self, anchor: f32) -> f32 {
        if self.pages.is_empty() {
            return 0.0;
        }

        let anchor = anchor.max(0.0);
        let index = (anchor.floor() as usize).min(self.pages.len() - 1);
        let fraction = (anchor - index as f32).clamp(0.0, 1.0);
        self.page_top(index) + fraction * self.pages[index].height
    }

    /// Convert a distance from the top into a `page + fraction` anchor, so the
    /// reading position survives zoom and window-size changes.
    pub(crate) fn anchor_for_offset(&self, offset: f32) -> f32 {
        if self.pages.is_empty() {
            return 0.0;
        }

        let mut top = PDF_TOP_PADDING;
        for (index, page) in self.pages.iter().enumerate() {
            let bottom = top + page.height;
            if offset <= bottom || index + 1 == self.pages.len() {
                let fraction = ((offset - top) / page.height).clamp(0.0, 1.0);
                return index as f32 + fraction;
            }
            top = bottom + PDF_PAGE_GAP;
        }

        (self.pages.len() - 1) as f32
    }

    /// Pages that intersect the visible part of the scroll area.
    pub(crate) fn visible_range(&self, offset: f32, viewport_height: f32) -> Range<usize> {
        let count = self.pages.len();
        if count == 0 {
            return 0..0;
        }

        let bottom = offset + viewport_height.max(1.0);
        let mut top = PDF_TOP_PADDING;
        let mut first = None;
        let mut last = 0;
        for (index, page) in self.pages.iter().enumerate() {
            let page_bottom = top + page.height;
            if page_bottom >= offset && top <= bottom {
                first.get_or_insert(index);
                last = index;
            }
            top = page_bottom + PDF_PAGE_GAP;
        }

        let first = first.unwrap_or(0).saturating_sub(PDF_OVERSCAN);
        let last = (last + PDF_OVERSCAN).min(count - 1);
        first..(last + 1)
    }
}

/// Rasterize one page off the UI thread and put it in the view's cache.
pub(crate) fn spawn_pdf_render(
    source: Arc<PdfSource>,
    index: usize,
    scale: f32,
    scale_key: u32,
    window: &mut Window,
    cx: &mut Context<PoemReader>,
) {
    cx.spawn_in(window, async move |this, cx| {
        let image = cx
            .background_spawn(async move {
                // A malformed page must not take the whole application down.
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    source.render(index, scale)
                }))
                .ok()
                .flatten()
            })
            .await;

        this.update_in(cx, move |this, _window, cx| {
            if let Some(DocumentContent::Pdf(view)) =
                this.document.as_ref().map(|document| &document.content)
            {
                view.finish_render(index, scale_key, image, index);
            }
            cx.notify();
        })
        .ok();
    })
    .detach();
}
