//! Page rasterization for PDF documents.
//!
//! The parsed document lives in [`PdfSource`], which is safe to share with the
//! background executor. [`PdfView`] is the UI-side cache of already rasterized
//! pages; it is only touched from the render thread.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui_kit::{RenderImage, Size};
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::{LoadPdfError, Pdf};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings, render};
use image::{Frame, RgbaImage};

/// Ceiling for a single page's raster, so an oversized page cannot exhaust memory.
const MAX_PAGE_PIXELS: f32 = 24_000_000.0;

/// How many rasterized pages are kept around at once, and how much memory they
/// are allowed to occupy (four bytes per pixel).
const MAX_CACHED_PAGES: usize = 12;
const MAX_CACHED_PIXELS: usize = 64_000_000;

/// A parsed PDF that can be rendered from any thread.
pub struct PdfSource {
    pdf: Pdf,
    /// Page sizes in PDF points, with the page rotation already applied.
    page_sizes: Vec<Size<f32>>,
}

impl PdfSource {
    pub fn load(bytes: Vec<u8>) -> Result<Self, String> {
        let pdf = Pdf::new(bytes).map_err(|error| match error {
            LoadPdfError::Decryption(_) => {
                String::from("无法打开加密的 PDF：目前不支持需要密码的文件")
            }
            LoadPdfError::Invalid => String::from("无法解析这个 PDF：文件可能已损坏"),
        })?;

        let page_sizes = pdf
            .pages()
            .iter()
            .map(|page| {
                let (width, height) = page.render_dimensions();
                Size::new(width.max(1.0), height.max(1.0))
            })
            .collect::<Vec<_>>();

        if page_sizes.is_empty() {
            return Err(String::from("这份 PDF 中没有可显示的页面"));
        }

        Ok(Self { pdf, page_sizes })
    }

    pub fn page_count(&self) -> usize {
        self.page_sizes.len()
    }

    pub fn page_sizes(&self) -> &[Size<f32>] {
        &self.page_sizes
    }

    /// Keep the largest page within [`MAX_PAGE_PIXELS`].
    pub fn clamp_scale(&self, scale: f32) -> f32 {
        let largest = self
            .page_sizes
            .iter()
            .map(|size| size.width * size.height)
            .fold(0.0_f32, f32::max);
        if largest <= 0.0 {
            return scale;
        }
        scale.min((MAX_PAGE_PIXELS / largest).sqrt())
    }

    pub fn render(&self, index: usize, scale: f32) -> Option<Arc<RenderImage>> {
        let bitmap = self.render_bitmap(index, scale)?;
        let buffer = RgbaImage::from_raw(bitmap.width, bitmap.height, bitmap.data)?;
        Some(Arc::new(RenderImage::new(vec![Frame::new(buffer)])))
    }

    /// Rasterize one page for printing, in premultiplied BGRA (rows top-down).
    pub fn render_bitmap(&self, index: usize, scale: f32) -> Option<PageBitmap> {
        let page = self.pdf.pages().get(index)?;
        let cache = RenderCache::new();
        let pixmap = render(
            page,
            &cache,
            &InterpreterSettings::default(),
            &RenderSettings::default(),
            &PixmapSettings {
                x_scale: scale,
                y_scale: scale,
                bg_color: WHITE,
            },
        );

        let width = pixmap.width() as u32;
        let height = pixmap.height() as u32;
        if width == 0 || height == 0 {
            return None;
        }

        // Hayro produces premultiplied RGBA; both GPUI and the printer expect BGRA.
        let mut data = pixmap.data_as_u8_slice().to_vec();
        for pixel in data.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }

        Some(PageBitmap {
            width,
            height,
            data,
        })
    }
}

/// A rasterized page, ready for the screen or the printer: premultiplied BGRA,
/// rows from top to bottom.
pub struct PageBitmap {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// Quantize a raster scale so that small window resizes reuse the same images.
pub fn scale_key(scale: f32) -> u32 {
    (scale * 10.0).round().max(1.0) as u32
}

/// The page raster cache of one open PDF.
pub struct PdfView {
    source: Arc<PdfSource>,
    /// Raster scale the cached pages were produced at.
    scale_key: Cell<u32>,
    cache: RefCell<HashMap<usize, CachedPage>>,
    /// `(page, scale key)` pairs currently being rasterized.
    rendering: RefCell<HashSet<(usize, u32)>>,
    /// Pages the renderer gave up on, so they are not retried every frame.
    failed: RefCell<HashSet<usize>>,
}

struct CachedPage {
    scale_key: u32,
    image: Arc<RenderImage>,
    pixels: usize,
}

impl PdfView {
    pub fn new(source: Arc<PdfSource>) -> Self {
        Self {
            source,
            scale_key: Cell::new(0),
            cache: RefCell::new(HashMap::new()),
            rendering: RefCell::new(HashSet::new()),
            failed: RefCell::new(HashSet::new()),
        }
    }

    pub fn source(&self) -> Arc<PdfSource> {
        self.source.clone()
    }

    pub fn page_count(&self) -> usize {
        self.source.page_count()
    }

    /// Switch to a new raster scale, dropping everything produced at the old one.
    pub fn set_scale_key(&self, key: u32) {
        if self.scale_key.get() == key {
            return;
        }
        self.scale_key.set(key);
        self.cache.borrow_mut().clear();
        self.rendering.borrow_mut().clear();
        self.failed.borrow_mut().clear();
    }

    pub fn image(&self, index: usize) -> Option<Arc<RenderImage>> {
        let cache = self.cache.borrow();
        cache
            .get(&index)
            .filter(|page| page.scale_key == self.scale_key.get())
            .map(|page| page.image.clone())
    }

    pub fn has_failed(&self, index: usize) -> bool {
        self.failed.borrow().contains(&index)
    }

    /// Reserve a page for rendering; false when it is already queued.
    pub fn begin_render(&self, index: usize, key: u32) -> bool {
        if self.failed.borrow().contains(&index) {
            return false;
        }
        self.rendering.borrow_mut().insert((index, key))
    }

    pub fn finish_render(
        &self,
        index: usize,
        key: u32,
        image: Option<Arc<RenderImage>>,
        focus: usize,
    ) {
        self.rendering.borrow_mut().remove(&(index, key));

        let Some(image) = image else {
            if key == self.scale_key.get() {
                self.failed.borrow_mut().insert(index);
            }
            return;
        };

        // A page that finished after a zoom change is stale; drop it.
        if key != self.scale_key.get() {
            return;
        }

        let mut cache = self.cache.borrow_mut();
        let size = image.size(0);
        let pixels = size.width.0.max(0) as usize * size.height.0.max(0) as usize;
        cache.insert(
            index,
            CachedPage {
                scale_key: key,
                image,
                pixels,
            },
        );

        // Keep the pages around the reader, dropping the most distant ones
        // first once either budget is exceeded.
        while cache.len() > MAX_CACHED_PAGES
            || cache.values().map(|page| page.pixels).sum::<usize>() > MAX_CACHED_PIXELS
        {
            let farthest = cache
                .keys()
                .copied()
                .max_by_key(|page| page.abs_diff(focus));
            match farthest {
                Some(page) if cache.len() > 1 => {
                    cache.remove(&page);
                }
                _ => break,
            }
        }
    }
}
