//! Printing the open document through the system print dialog.
//!
//! Text is drawn with GDI text output and PDF pages are rasterized at the
//! printer's resolution, so the spooler receives device-quality pages instead
//! of a screenshot of the window.
//!
//! The same layout also feeds the print preview: [`render_pages`] paints a job
//! onto off-screen sheets with the very same code, so the pages the preview
//! shows are the pages the printer would produce. Only the page boundaries
//! differ — `StartPage`/`EndPage` on a printer, a white sheet and a pixel
//! read-back on a bitmap — which is what [`Device`] hides.

use std::sync::Arc;

use image::RgbaImage;

use crate::excel::Workbook;
use crate::pdf::PdfSource;
use crate::word::WordDocument;

/// What to print.
#[derive(Clone)]
pub enum PrintContent {
    /// The whole text of a document, printed at the given point size.
    Text { text: Arc<String>, font_pt: f32 },
    /// A PDF, rasterized at the printer's resolution.
    Pdf(Arc<PdfSource>),
    /// A Word document, laid out into pages with the printer's text metrics.
    Word(Arc<WordDocument>),
    /// A workbook, split into pages along its column and row boundaries.
    Excel(Arc<Workbook>),
    /// A single picture, fitted to the printable area of one page.
    Image { pixels: Arc<RgbaImage> },
}

/// A print request: the name shown in the spooler and the content to print.
#[derive(Clone)]
pub struct PrintJob {
    pub title: String,
    pub content: PrintContent,
    /// How content wider than the paper is treated.
    pub fit: PrintFit,
    /// Which way round the sheet goes.
    pub orientation: Orientation,
    /// How many document pages share one sheet.
    pub pages_per_sheet: PagesPerSheet,
    /// How a picture is printed: size, anchor and poster paging.
    pub image: ImagePrint,
}

/// Which way round the sheet is printed.
///
/// The choice belongs to the job rather than to the printer: the layout is
/// measured against the printable area, so a wider page really does fit more
/// on a line, and the preview has to be laid out the same way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Orientation {
    #[default]
    Portrait,
    Landscape,
}

impl Orientation {
    /// The name shown on the print controls.
    pub fn label(self) -> &'static str {
        match self {
            Orientation::Portrait => "纵向",
            Orientation::Landscape => "横向",
        }
    }

    /// The other way round, for a control that toggles between the two.
    pub fn toggled(self) -> Self {
        match self {
            Orientation::Portrait => Orientation::Landscape,
            Orientation::Landscape => Orientation::Portrait,
        }
    }
}

/// What happens to content that does not fit the printable width.
///
/// Word and Excel layouts are driven by the paper they are drawn on, so a
/// document authored for a wider sheet would otherwise be cut at the right
/// edge of the page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PrintFit {
    /// Shrink the layout until every column, table and picture fits the paper.
    #[default]
    FitToPaper,
    /// Keep the document at its own size and let the page edge cut what spills
    /// over — useful to check what a clipped print looks like.
    ActualSize,
}

impl PrintFit {
    /// The name shown on the print controls.
    pub fn label(self) -> &'static str {
        match self {
            PrintFit::FitToPaper => "适应纸张",
            PrintFit::ActualSize => "原始大小",
        }
    }

    /// The other mode, for a control that toggles between the two.
    pub fn toggled(self) -> Self {
        match self {
            PrintFit::FitToPaper => PrintFit::ActualSize,
            PrintFit::ActualSize => PrintFit::FitToPaper,
        }
    }
}

/// How much of the paper a picture takes up.
///
/// A picture has no layout to reflow — the only thing a zoom can change is the
/// picture itself. The percentage is of the size that fits the paper, so 100%
/// fills the sheet the way an unscaled print always has: larger is centred and
/// cut at the printable edge, smaller sits in the middle of a white page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageScale(u32);

impl Default for ImageScale {
    fn default() -> Self {
        Self(100)
    }
}

impl ImageScale {
    /// The steps the print controls offer, smallest first.
    pub const ALL: [Self; 8] = [
        Self(25),
        Self(50),
        Self(75),
        Self(100),
        Self(150),
        Self(200),
        Self(300),
        Self(400),
    ];

    /// The factor applied to the size that fits the paper.
    pub fn factor(self) -> f32 {
        self.0 as f32 / 100.0
    }

    /// The name shown in the menus.
    pub fn label(self) -> String {
        if self.0 == 100 {
            String::from("100%（适应纸张）")
        } else {
            format!("{}%", self.0)
        }
    }

    /// The shorter name shown on the preview toolbar, where a bare percentage
    /// would read like the zoom of the preview itself.
    pub fn short_label(self) -> String {
        format!("图片 {}%", self.0)
    }
}

/// Which part of an enlarged picture lands on the paper.
///
/// Both components are 0–1: 0 is the left or top edge, 1 the right or bottom,
/// 0.5 the middle. A fraction rather than a distance is kept so the anchor
/// stays on the same place of the picture when the paper or the scale changes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageAnchor {
    pub x: f32,
    pub y: f32,
}

impl Default for ImageAnchor {
    fn default() -> Self {
        Self { x: 0.5, y: 0.5 }
    }
}

impl ImageAnchor {
    /// Kept inside the picture, whatever a drag or an old setting asks for.
    pub fn clamped(self) -> Self {
        Self {
            x: if self.x.is_finite() { self.x.clamp(0.0, 1.0) } else { 0.5 },
            y: if self.y.is_finite() { self.y.clamp(0.0, 1.0) } else { 0.5 },
        }
    }
}

/// How a picture is printed: how large, from where, and on how many pages.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ImagePrint {
    /// How large the picture is drawn on the paper.
    pub scale: ImageScale,
    /// Which part of an enlarged picture the paper shows.
    pub anchor: ImageAnchor,
    /// Split a picture too large for one page across several, to be put back
    /// together by hand — the poster print of a copy shop.
    pub poster: bool,
}

/// Where a picture lands on a page, and how much of it spills over.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ImageLayout {
    /// Size the picture is drawn at, fit to the paper and then scaled.
    pub(crate) drawn: (f32, f32),
    /// How far it can be moved on each axis before an edge leaves the paper.
    pub(crate) pan: (f32, f32),
    /// Pages it covers, as columns and rows.
    pub(crate) tiles: (usize, usize),
}

/// Lay a picture out on a printable area: the drawing size, the room it can be
/// moved in, and how many pages a poster print needs.
pub(crate) fn image_layout(
    image: (u32, u32),
    printable: (f32, f32),
    print: ImagePrint,
) -> ImageLayout {
    let source = (image.0.max(1) as f32, image.1.max(1) as f32);
    let printable = (printable.0.max(1.0), printable.1.max(1.0));
    let fit = (printable.0 / source.0).min(printable.1 / source.1);
    let factor = fit * print.scale.factor();
    let drawn = (source.0 * factor, source.1 * factor);

    let columns = if print.poster {
        (drawn.0 / printable.0).ceil().max(1.0) as usize
    } else {
        1
    };
    let rows = if print.poster {
        (drawn.1 / printable.1).ceil().max(1.0) as usize
    } else {
        1
    };

    ImageLayout {
        drawn,
        pan: (
            (drawn.0 - printable.0).max(0.0),
            (drawn.1 - printable.1).max(0.0),
        ),
        tiles: (columns, rows),
    }
}

/// Where the top left corner of the picture lands inside the printable area.
///
/// `area` is `(left, top, width, height)` of the block the picture is drawn on,
/// in the units the layout was measured in. A picture smaller than the area is
/// centred in it; a larger one is moved out of the way by the anchor, 0.5 being
/// the middle. The print path and the preview both come through here, so the
/// picture the reader drags across the page is at the place it will be printed.
pub(crate) fn image_origin(
    area: (f32, f32, f32, f32),
    layout: ImageLayout,
    anchor: ImageAnchor,
) -> (f32, f32) {
    let (left, top, width, height) = area;
    let anchor = anchor.clamped();
    let slack = |area: f32, drawn: f32| (area - drawn).max(0.0) * 0.5;
    (
        left + slack(width, layout.drawn.0) - layout.pan.0 * anchor.x,
        top + slack(height, layout.drawn.1) - layout.pan.1 * anchor.y,
    )
}


/// How many document pages are shrunk onto one sheet of paper.
///
/// The pages keep the layout they have on their own sheet and are scaled down,
/// so a letter still breaks its lines where the letter does; only the paper
/// they land on carries more of them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PagesPerSheet {
    #[default]
    One,
    Two,
    Four,
    Six,
    Eight,
}

impl PagesPerSheet {
    /// The choices the print controls offer, in order.
    pub const ALL: [Self; 5] = [
        PagesPerSheet::One,
        PagesPerSheet::Two,
        PagesPerSheet::Four,
        PagesPerSheet::Six,
        PagesPerSheet::Eight,
    ];

    /// How many pages one sheet holds.
    pub fn count(self) -> usize {
        match self {
            PagesPerSheet::One => 1,
            PagesPerSheet::Two => 2,
            PagesPerSheet::Four => 4,
            PagesPerSheet::Six => 6,
            PagesPerSheet::Eight => 8,
        }
    }

    /// The caption shown on the print controls.
    pub fn label(self) -> &'static str {
        match self {
            PagesPerSheet::One => "1 合 1",
            PagesPerSheet::Two => "2 合 1",
            PagesPerSheet::Four => "4 合 1",
            PagesPerSheet::Six => "6 合 1",
            PagesPerSheet::Eight => "8 合 1",
        }
    }

    /// Columns and rows the pages sit in.
    ///
    /// The grid follows the paper so every page stays upright: a tall sheet
    /// takes two pages side by side, a wide one stacks them one above the
    /// other.
    pub fn grid(self, landscape: bool) -> (usize, usize) {
        match (self, landscape) {
            (PagesPerSheet::One, _) => (1, 1),
            (PagesPerSheet::Two, false) => (2, 1),
            (PagesPerSheet::Two, true) => (1, 2),
            (PagesPerSheet::Four, _) => (2, 2),
            (PagesPerSheet::Six, false) => (3, 2),
            (PagesPerSheet::Six, true) => (2, 3),
            (PagesPerSheet::Eight, false) => (4, 2),
            (PagesPerSheet::Eight, true) => (2, 4),
        }
    }
}

/// Where one page of a group lands on the sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Slot {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Space left between the pages of a group, as a fraction of a slot.
const SLOT_GAP: f32 = 0.03;

/// Position and size of the `slot`-th page of `grid` on a sheet.
///
/// `printable` is the printable area of the sheet and `page` the size of one
/// whole page; the page keeps its own shape inside the slot, so a slot of a
/// different shape leaves white space rather than stretching it.
pub(crate) fn slot_rect(
    printable: (i32, i32),
    grid: (usize, usize),
    page: (i32, i32),
    slot: usize,
) -> Slot {
    let columns = grid.0.max(1) as i32;
    let rows = grid.1.max(1) as i32;
    let cell_width = (printable.0 / columns).max(1);
    let cell_height = (printable.1 / rows).max(1);
    let gap = (cell_width.min(cell_height) as f32 * SLOT_GAP).round() as i32;

    let room = ((cell_width - gap * 2).max(1), (cell_height - gap * 2).max(1));
    let scale = (room.0 as f32 / page.0.max(1) as f32)
        .min(room.1 as f32 / page.1.max(1) as f32);
    let width = ((page.0.max(1) as f32 * scale).round() as i32).clamp(1, room.0);
    let height = ((page.1.max(1) as f32 * scale).round() as i32).clamp(1, room.1);

    let column = slot as i32 % columns;
    let row = slot as i32 / columns;
    Slot {
        x: column * cell_width + (cell_width - width) / 2,
        y: row * cell_height + (cell_height - height) / 2,
        width,
        height,
    }
}

/// Where a page lands on the paper it is painted onto, in preview pixels.
///
/// The printable area is what the printer reports as `HORZRES`/`VERTRES`; the
/// sheet also covers the hard margins the mechanism cannot print on, so a
/// preview shows the same white border a real page has.
#[derive(Clone, Copy, Debug)]
pub struct SheetGeometry {
    pub printable_width: i32,
    pub printable_height: i32,
    /// Size of the whole sheet, hard margins included.
    pub width: u32,
    pub height: u32,
    /// Where the printable area starts on the sheet.
    pub origin_x: i32,
    pub origin_y: i32,
}

impl SheetGeometry {
    /// The printable area fills the sheet: used when no printer answers.
    pub fn without_margins(printable_width: i32, printable_height: i32) -> Self {
        Self {
            printable_width,
            printable_height,
            width: printable_width.max(1) as u32,
            height: printable_height.max(1) as u32,
            origin_x: 0,
            origin_y: 0,
        }
    }

    /// Width over height of the whole sheet.
    pub fn ratio(&self) -> f32 {
        if self.height == 0 {
            return 1.0;
        }
        self.width as f32 / self.height as f32
    }

    /// Whether the sheet is wider than it is tall.
    pub fn is_landscape(&self) -> bool {
        self.width > self.height
    }

    /// The same sheet turned a quarter turn: width and height trade places.
    ///
    /// The hard margins turn with it — the bottom margin of a portrait sheet
    /// becomes the left margin of a landscape one — which is what a driver
    /// reports after it has been told to print sideways.
    pub fn turned(&self) -> Self {
        Self {
            printable_width: self.printable_height,
            printable_height: self.printable_width,
            width: self.height,
            height: self.width,
            origin_x: (self.height as i32 - self.origin_y - self.printable_height).max(0),
            origin_y: self.origin_x.max(0),
        }
    }
}

/// One painted page: its index in the layout and its pixels.
///
/// The pixels are premultiplied BGRA with the rows from the top down, which is
/// the byte order both a GPUI texture and `StretchDIBits` read; the preview can
/// hand them over as they are.
pub type PaintedPage = (usize, RgbaImage);

/// What a layout hands back: the page count (`None` when the layout stopped
/// early) and the pages that were asked for.
pub type RenderedPages = (Option<usize>, Vec<PaintedPage>);

/// The pages the user selected in the print dialog, numbered from one.
///
/// Several ranges can be selected at once (for example `1,3-5`), so the pages
/// are kept as a sorted list of inclusive spans.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PageRange {
    All,
    Pages(Vec<(u32, u32)>),
}

impl PageRange {
    /// `index` is the zero-based page index of the layout.
    pub fn includes(&self, index: usize) -> bool {
        match self {
            PageRange::All => true,
            PageRange::Pages(ranges) => {
                let page = index.saturating_add(1) as u32;
                ranges
                    .iter()
                    .any(|(first, last)| page >= *first && page <= *last)
            }
        }
    }
}

/// What a print job produced: how many pages were sent to the printer and how
/// many pages the document has in total.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrintSummary {
    pub printed: usize,
    pub total: usize,
}

/// The result of laying a job out.
///
/// `complete` is false when the layout was allowed to stop as soon as the
/// wanted pages were ready, which is what keeps a preview of a long document
/// responsive; the page count is then only the number of pages seen so far.
pub(crate) struct LayoutOutcome {
    pub(crate) summary: PrintSummary,
    pub(crate) complete: bool,
}

impl LayoutOutcome {
    /// A layout that ran to the end of the document.
    pub(crate) fn whole(summary: PrintSummary) -> Self {
        Self {
            summary,
            complete: true,
        }
    }
}

/// The window the print dialog should belong to, so it opens above the reader
/// and makes it modal.
///
/// Captured on the UI thread; the dialog itself is shown on the printing
/// thread, which keeps the reader responsive while it is open.
#[derive(Clone, Copy)]
pub struct PrintOwner {
    #[cfg(windows)]
    hwnd: windows::Win32::Foundation::HWND,
}

#[cfg(windows)]
unsafe impl Send for PrintOwner {}

/// Lay `job` out onto off-screen sheets and return the finished pages.
///
/// Only the pages picked by `range` are painted and returned. The rest of the
/// document still has to be measured to know where the pages break, unless
/// `stop_after` says the layout may stop once that page index has been reached:
/// the total is then `None`, because the pages behind it were never looked at.
///
/// Stopping early is what keeps the preview instant on a long file — laying out
/// a megabyte of text costs a few hundred milliseconds, and the reader only
/// ever needs the pages in front of them first.
pub fn render_pages(
    job: &PrintJob,
    sheet: SheetGeometry,
    dpi: f32,
    range: &PageRange,
    stop_after: Option<usize>,
) -> Result<RenderedPages, String> {
    #[cfg(windows)]
    {
        imp::render_pages(job, sheet, dpi, range, stop_after)
    }
    #[cfg(not(windows))]
    {
        let _ = (job, sheet, dpi, range, stop_after);
        Err(String::from("当前平台暂不支持打印预览"))
    }
}

#[cfg(windows)]
pub(crate) use surface::Device;

#[cfg(windows)]
mod surface {
    use std::ffi::c_void;
    use std::mem::size_of;

    use image::RgbaImage;
    use windows::Win32::Graphics::Gdi::{
        BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
        DeleteDC, DeleteObject, GdiFlush, GetDC, HALFTONE, HBITMAP, HDC, HGDIOBJ, ReleaseDC,
        SRCCOPY, SelectObject, SetBrushOrgEx, SetStretchBltMode, SetViewportOrgEx, StretchDIBits,
    };
    use windows::Win32::Storage::Xps::{EndPage, StartPage};

    use super::SheetGeometry;

    /// Where a page is painted.
    ///
    /// Drawing code only ever needs the context, so the two devices differ in
    /// one place only: what a page boundary means.
    pub(crate) struct Device {
        hdc: HDC,
        sheet: Option<Sheet>,
        /// Pages are painted on a sheet of their own first and shrunk onto the
        /// real one, several at a time.
        shrink: Option<Shrink>,
    }

    /// An off-screen sheet that pages are painted onto and read back from.
    struct Sheet {
        dc: HDC,
        bitmap: HBITMAP,
        /// Top-down BGRA, `width * height * 4` bytes.
        pixels: *mut u8,
        width: usize,
        height: usize,
        origin: (i32, i32),
        /// Pages painted so far, as `(page index, top-down BGRA)`.
        pages: Vec<(usize, RgbaImage)>,
    }

    /// Several document pages on one sheet of paper.
    ///
    /// Every page is laid out on a sheet of its own, exactly as it would be
    /// printed alone, and then drawn into a slot of the real page: the line
    /// breaks survive the reduction, which is the whole point of printing two
    /// pages per sheet.
    struct Shrink {
        /// The sheet the layout paints a whole page onto.
        source: Sheet,
        /// Columns and rows of the grid the pages are arranged in.
        grid: (usize, usize),
        /// Printable area of the sheet that receives them.
        printable: (i32, i32),
        /// Slot the next finished page goes into.
        slot: usize,
        /// Sheets finished so far, which is the index of the one being filled.
        page: usize,
        /// A sheet has been started on the target and not yet finished.
        open: bool,
    }

    impl Sheet {
        fn new(width: i32, height: i32, origin: (i32, i32)) -> Result<Self, String> {
            let width = width.max(1);
            let height = height.max(1);
            let screen = unsafe { GetDC(None) };
            let dc = unsafe { CreateCompatibleDC(Some(screen)) };
            let _ = unsafe { ReleaseDC(None, screen) };
            if dc.0.is_null() {
                return Err(String::from("无法创建预览用的绘图设备"));
            }

            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    // A negative height asks for rows from the top down, which
                    // is the order the preview reads them back in.
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: 0, // BI_RGB
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut pixels: *mut c_void = std::ptr::null_mut();
            let bitmap = match unsafe {
                CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut pixels, None, 0)
            } {
                Ok(bitmap) => bitmap,
                Err(_) => {
                    let _ = unsafe { DeleteDC(dc) };
                    return Err(String::from("无法分配预览页面"));
                }
            };
            if pixels.is_null() {
                let _ = unsafe { DeleteDC(dc) };
                let _ = unsafe { DeleteObject(HGDIOBJ(bitmap.0)) };
                return Err(String::from("无法分配预览页面"));
            }
            unsafe { SelectObject(dc, HGDIOBJ(bitmap.0)) };

            Ok(Self {
                dc,
                bitmap,
                pixels: pixels as *mut u8,
                width: width as usize,
                height: height as usize,
                origin,
                pages: Vec::new(),
            })
        }

        /// Start from a blank sheet of paper, then move the origin to the
        /// printable area so the drawing code keeps working in printer
        /// coordinates.
        fn blank(&self) {
            let bytes = self.width * self.height * 4;
            unsafe { std::ptr::write_bytes(self.pixels, 0xFF, bytes) };
            let _ = unsafe { SetViewportOrgEx(self.dc, self.origin.0, self.origin.1, None) };
        }

        /// The page as an image, in the byte order a texture wants.
        fn image(&self, index: usize) -> Option<(usize, RgbaImage)> {
            // GDI batches its output; the pixels are only complete once the
            // queue has been flushed.
            let _ = unsafe { GdiFlush() };
            let bytes = self.width * self.height * 4;
            let pixels = unsafe { std::slice::from_raw_parts(self.pixels, bytes) };

            // GDI paints into the sheet in the device order, which for a
            // 32-bit section is BGRA — the same order a texture wants, so the
            // bytes are kept as they are. The alpha byte is left alone by GDI,
            // so it is forced opaque.
            let mut data = Vec::with_capacity(bytes);
            for pixel in pixels.as_chunks::<4>().0 {
                data.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
            }
            let image = RgbaImage::from_raw(self.width as u32, self.height as u32, data)?;
            Some((index, image))
        }
    }

    impl Drop for Sheet {
        fn drop(&mut self) {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
                let _ = DeleteDC(self.dc);
            }
        }
    }

    impl Device {
        /// Pages go to the printer the context belongs to.
        pub(crate) fn printer(hdc: HDC) -> Self {
            Self {
                hdc,
                sheet: None,
                shrink: None,
            }
        }

        /// Pages are painted onto a sheet this size and kept in memory.
        pub(crate) fn sheet(geometry: SheetGeometry) -> Result<Self, String> {
            let sheet = Sheet::new(
                geometry.width as i32,
                geometry.height as i32,
                (geometry.origin_x, geometry.origin_y),
            )?;
            Ok(Self {
                hdc: sheet.dc,
                sheet: Some(sheet),
                shrink: None,
            })
        }

        /// Shrink the pages onto the sheet `grid.0` by `grid.1` at a time.
        ///
        /// `printable` is the printable area of the sheet that receives them.
        /// Off-screen the pages are painted on a sheet the size of the paper,
        /// hard margins included, so a preview shows the same white border
        /// around every shrunken page that a real one has.
        pub(crate) fn shrink_onto(
            mut self,
            grid: (usize, usize),
            printable: (i32, i32),
        ) -> Result<Self, String> {
            let (width, height, origin) = match &self.sheet {
                Some(sheet) => (sheet.width as i32, sheet.height as i32, sheet.origin),
                None => (printable.0, printable.1, (0, 0)),
            };
            let source = Sheet::new(width, height, origin)?;
            self.shrink = Some(Shrink {
                source,
                grid,
                printable,
                slot: 0,
                page: 0,
                open: false,
            });
            Ok(self)
        }

        /// The context to draw into.
        pub(crate) fn hdc(&self) -> HDC {
            if let Some(shrink) = &self.shrink {
                return shrink.source.dc;
            }
            self.sheet.as_ref().map(|sheet| sheet.dc).unwrap_or(self.hdc)
        }

        /// Begin page `index`.
        pub(crate) fn start_page(&mut self, index: usize) -> Result<(), String> {
            let _ = index;
            if let Some(shrink) = self.shrink.as_ref() {
                shrink.source.blank();
                return Ok(());
            }
            self.begin_sheet()
        }

        /// Finish page `index`, keeping it if this is a sheet.
        pub(crate) fn end_page(&mut self, index: usize) -> Result<(), String> {
            if self.shrink.is_some() {
                return self.compose();
            }
            self.finish_sheet(index)
        }

        /// Close the sheet that is still being filled, if any.
        ///
        /// A document whose last group is not full would otherwise lose the
        /// pages already drawn on it.
        pub(crate) fn finish(&mut self) -> Result<(), String> {
            let Some(shrink) = self.shrink.as_ref() else {
                return Ok(());
            };
            if !shrink.open {
                return Ok(());
            }
            let index = shrink.page;
            self.finish_sheet(index)?;
            if let Some(shrink) = self.shrink.as_mut() {
                shrink.open = false;
                shrink.slot = 0;
                shrink.page += 1;
            }
            Ok(())
        }

        /// The pages painted so far.
        pub(crate) fn into_pages(mut self) -> Vec<(usize, RgbaImage)> {
            match self.sheet.take() {
                Some(mut sheet) => std::mem::take(&mut sheet.pages),
                None => Vec::new(),
            }
        }

        /// Start a page on the target — a printer page or a blank sheet.
        fn begin_sheet(&mut self) -> Result<(), String> {
            match &self.sheet {
                None => {
                    if unsafe { StartPage(self.hdc) } <= 0 {
                        return Err(String::from("无法开始打印页面"));
                    }
                    Ok(())
                }
                Some(sheet) => {
                    sheet.blank();
                    Ok(())
                }
            }
        }

        /// Finish the page that was started on the target.
        fn finish_sheet(&mut self, index: usize) -> Result<(), String> {
            match &mut self.sheet {
                None => {
                    if unsafe { EndPage(self.hdc) } <= 0 {
                        return Err(String::from("写入打印页面失败"));
                    }
                    Ok(())
                }
                Some(sheet) => {
                    if let Some(page) = sheet.image(index) {
                        sheet.pages.push(page);
                    }
                    Ok(())
                }
            }
        }

        /// Draw the page that was just finished into the sheet being filled,
        /// and hand that sheet over once the grid is full.
        fn compose(&mut self) -> Result<(), String> {
            let Some(mut shrink) = self.shrink.take() else {
                return Ok(());
            };
            let result = (|| -> Result<(), String> {
                if !shrink.open {
                    self.begin_sheet()?;
                    shrink.open = true;
                }

                let page = (shrink.source.width as i32, shrink.source.height as i32);
                let slot = super::slot_rect(shrink.printable, shrink.grid, page, shrink.slot);
                let target = self.sheet.as_ref().map(|sheet| sheet.dc).unwrap_or(self.hdc);
                draw_shrunk(target, &shrink.source, slot);

                shrink.slot += 1;
                if shrink.slot >= shrink.grid.0.max(1) * shrink.grid.1.max(1) {
                    let index = shrink.page;
                    self.finish_sheet(index)?;
                    shrink.slot = 0;
                    shrink.page += 1;
                    shrink.open = false;
                }
                Ok(())
            })();
            self.shrink = Some(shrink);
            result
        }
    }

    /// Draw a whole page into `slot` of the sheet being composed.
    fn draw_shrunk(hdc: HDC, source: &Sheet, slot: super::Slot) {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: source.width as i32,
                biHeight: -(source.height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0, // BI_RGB
                ..Default::default()
            },
            ..Default::default()
        };

        // Shrinking text and rules with the default mode drops pixels and
        // turns them into speckle; halftone blends instead.
        unsafe {
            SetStretchBltMode(hdc, HALFTONE);
            let _ = SetBrushOrgEx(hdc, 0, 0, None);
            let _ = StretchDIBits(
                hdc,
                slot.x,
                slot.y,
                slot.width,
                slot.height,
                0,
                0,
                source.width as i32,
                source.height as i32,
                Some(source.pixels as *const c_void),
                &info,
                DIB_RGB_COLORS,
                SRCCOPY,
            );
        }
    }
}

/// The window that is currently in front, used as the print dialog's owner.
pub fn foreground_owner() -> PrintOwner {
    #[cfg(windows)]
    {
        PrintOwner {
            hwnd: unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() },
        }
    }
    #[cfg(not(windows))]
    {
        PrintOwner {}
    }
}

/// Show the print dialog for `owner` and print `job` to the chosen printer.
///
/// Blocks until the dialog is dismissed and the job is spooled, so it must run
/// off the UI thread. Returns how many pages were printed and how many the
/// document has, or `None` when the dialog was cancelled.
pub fn print(owner: PrintOwner, job: PrintJob) -> Result<Option<PrintSummary>, String> {
    #[cfg(windows)]
    {
        imp::print(owner, job)
    }
    #[cfg(not(windows))]
    {
        let _ = (owner, job);
        Err(String::from("当前平台暂不支持打印"))
    }
}

#[cfg(windows)]
mod imp {
    use super::{
        Device, LayoutOutcome, Orientation, PageRange, PrintContent, PrintJob, PrintOwner,
        PrintSummary, RenderedPages, SheetGeometry,
    };

    use std::ffi::c_void;
    use std::mem::size_of;

    use windows::Win32::Foundation::{
        COLORREF, GlobalFree, HGLOBAL, SIZE,
    };
    use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
    use windows::Win32::Graphics::Gdi::{
        BITMAPINFO, BITMAPINFOHEADER, CLIP_DEFAULT_PRECIS, CreateFontW, DEFAULT_CHARSET,
        DIB_RGB_COLORS, DEVMODEW, DMORIENT_LANDSCAPE, DMORIENT_PORTRAIT, DM_ORIENTATION, DeleteDC,
        DeleteObject, GetDeviceCaps, GetTextExtentExPointW, GetTextExtentPoint32W, HDC, HFONT,
        HGDIOBJ, HORZRES, LOGPIXELSX, LOGPIXELSY, OUT_DEFAULT_PRECIS, PROOF_QUALITY, ResetDCW,
        SRCCOPY, SelectObject, SetBkMode, SetTextColor, StretchDIBits, TRANSPARENT, TextOutW,
        VERTRES,
    };
    use windows::Win32::Storage::Xps::{AbortDoc, DOCINFOW, EndDoc, StartDocW};
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
    use windows::Win32::UI::Controls::Dialogs::{
        PD_NOSELECTION, PD_PAGENUMS, PD_RETURNDC, PD_USEDEVMODECOPIESANDCOLLATE, PRINTDLGEX_FLAGS,
        PRINTDLGEXW, PRINTPAGERANGE, PrintDlgExW,
    };
    use windows::core::{PCWSTR, w};

    /// Margin around the printed area, in inches.
    const MARGIN_INCHES: f32 = 0.5;

    /// Upper page bound offered when the page count is only known after layout.
    const UNKNOWN_PAGE_LIMIT: u32 = 9999;

    /// How many separate page ranges the dialog can return.
    const MAX_PAGE_RANGES: usize = 16;

    pub(super) fn print(owner: PrintOwner, job: PrintJob) -> Result<Option<PrintSummary>, String> {
        let known_pages = match &job.content {
            PrintContent::Pdf(source) => Some(source.page_count()),
            // A poster print is split by the paper it lands on, which is only
            // known once the dialog has handed the context over, so the page
            // range is left open there.
            PrintContent::Image { .. } => (!job.image.poster).then_some(1),
            _ => None,
        };
        let Some((hdc, range)) = open_dialog(owner, known_pages, job.orientation)? else {
            return Ok(None);
        };
        // The dialog hands the context over to us.
        let _dc = DcGuard(hdc);

        let dpi_x = unsafe { GetDeviceCaps(Some(hdc), LOGPIXELSX) }.max(96);
        let dpi_y = unsafe { GetDeviceCaps(Some(hdc), LOGPIXELSY) }.max(96);
        let width = unsafe { GetDeviceCaps(Some(hdc), HORZRES) };
        let height = unsafe { GetDeviceCaps(Some(hdc), VERTRES) };
        if width <= 0 || height <= 0 {
            return Err(String::from("打印机没有报告可打印区域"));
        }

        let mut doc_name: Vec<u16> = job.title.encode_utf16().collect();
        doc_name.push(0);
        let info = DOCINFOW {
            cbSize: size_of::<DOCINFOW>() as i32,
            lpszDocName: PCWSTR::from_raw(doc_name.as_ptr()),
            lpszOutput: PCWSTR::null(),
            lpszDatatype: PCWSTR::null(),
            fwType: 0,
        };
        if unsafe { StartDocW(hdc, &info) } <= 0 {
            return Err(String::from("无法开始打印作业"));
        }

        // Pages are opened on the printer the dialog handed over. When several
        // pages share a sheet the device lays every page out on a sheet of its
        // own first and shrinks it onto this one.
        let count = job.pages_per_sheet.count();
        let grid = job.pages_per_sheet.grid(width > height);
        let mut device = Device::printer(hdc);
        if count > 1 {
            device = device.shrink_onto(grid, (width, height))?;
        }

        let result = match &job.content {
            PrintContent::Text { text, font_pt } => print_text(
                &mut device,
                text,
                *font_pt,
                width,
                height,
                dpi_x,
                dpi_y,
                &range,
                None,
            ),
            PrintContent::Pdf(source) => {
                print_pdf(&mut device, source, width, height, dpi_x, dpi_y, &range)
            }
            PrintContent::Word(document) => crate::print_office::print_word(
                &mut device,
                document,
                width,
                height,
                dpi_x,
                dpi_y,
                &range,
                None,
                job.fit,
            ),
            PrintContent::Excel(workbook) => crate::print_office::print_excel(
                &mut device,
                workbook,
                width,
                height,
                dpi_x,
                dpi_y,
                &range,
                None,
                job.fit,
            ),
            PrintContent::Image { pixels } => crate::print_office::print_image(
                &mut device,
                pixels,
                width,
                height,
                dpi_x,
                dpi_y,
                &range,
                job.image,
            ),
        };

        match result {
            Ok(outcome) => {
                // A last sheet that is not full still holds pages.
                device.finish()?;
                if unsafe { EndDoc(hdc) } <= 0 {
                    return Err(String::from("打印作业提交失败"));
                }
                Ok(Some(PrintSummary {
                    printed: outcome.summary.printed.div_ceil(count),
                    total: outcome.summary.total.div_ceil(count),
                }))
            }
            Err(message) => {
                unsafe { AbortDoc(hdc) };
                Err(message)
            }
        }
    }

    /// Lay the job out onto off-screen sheets, painting only `range`.
    ///
    /// The layout is the printing layout, so the page count and the position of
    /// every line are the ones the printer would produce; the pages outside
    /// `range` are still laid out, only without drawing.
    pub(super) fn render_pages(
        job: &PrintJob,
        sheet: SheetGeometry,
        dpi: f32,
        range: &PageRange,
        stop_after: Option<usize>,
    ) -> Result<RenderedPages, String> {
        let count = job.pages_per_sheet.count();
        let mut device = Device::sheet(sheet)?;
        if count > 1 {
            let grid = job.pages_per_sheet.grid(sheet.is_landscape());
            device = device.shrink_onto(grid, (sheet.printable_width, sheet.printable_height))?;
        }
        // The preview asks for sheets, so a request for one of them covers the
        // pages of its group: a half-painted group would put its pages in the
        // wrong slots.
        let (range, stop_after) = if count > 1 {
            (
                pages_of_sheets(range, count),
                stop_after.map(|page| (page + 1) * count - 1),
            )
        } else {
            (range.clone(), stop_after)
        };
        let range = &range;

        let dpi = dpi.round().max(48.0) as i32;
        let width = sheet.printable_width;
        let height = sheet.printable_height;

        let outcome = match &job.content {
            PrintContent::Text { text, font_pt } => print_text(
                &mut device,
                text,
                *font_pt,
                width,
                height,
                dpi,
                dpi,
                range,
                stop_after,
            ),
            PrintContent::Pdf(source) => {
                print_pdf(&mut device, source, width, height, dpi, dpi, range)
            }
            PrintContent::Word(document) => crate::print_office::print_word(
                &mut device,
                document,
                width,
                height,
                dpi,
                dpi,
                range,
                stop_after,
                job.fit,
            ),
            PrintContent::Excel(workbook) => crate::print_office::print_excel(
                &mut device,
                workbook,
                width,
                height,
                dpi,
                dpi,
                range,
                stop_after,
                job.fit,
            ),
            PrintContent::Image { pixels } => crate::print_office::print_image(
                &mut device,
                pixels,
                width,
                height,
                dpi,
                dpi,
                range,
                job.image,
            ),
        }?;

        // A last sheet that is not full still holds pages.
        device.finish()?;
        let total = outcome
            .complete
            .then(|| outcome.summary.total.div_ceil(count));
        Ok((total, device.into_pages()))
    }

    /// The document pages that fill the sheets a preview asked for.
    ///
    /// The preview counts sheets, the layout counts pages: sheet `n` is pages
    /// `n * count - count + 1` to `n * count`, and all of them have to be
    /// painted or the group would be composed from the wrong pages.
    pub(super) fn pages_of_sheets(range: &PageRange, count: usize) -> PageRange {
        let count = count.max(1) as u32;
        match range {
            PageRange::All => PageRange::All,
            PageRange::Pages(ranges) => PageRange::Pages(
                ranges
                    .iter()
                    .map(|(first, last)| {
                        let start = first.saturating_sub(1) * count + 1;
                        let end = last.saturating_mul(count).max(start);
                        (start, end)
                    })
                    .collect(),
            ),
        }
    }

    /// Show the system print dialog. Returns the device context and the page
    /// range, or `None` when the user cancels.
    ///
    /// The extended dialog is used because it lets the user enter several
    /// ranges at once (`1,3-5`), which the classic one cannot express.
    fn open_dialog(
        owner: PrintOwner,
        known_pages: Option<usize>,
        orientation: Orientation,
    ) -> Result<Option<(HDC, PageRange)>, String> {
        // The dialog can only offer a page range when it has an upper bound.
        // PDFs know their page count up front; the other formats paginate
        // while printing, so they offer a generous limit and the surplus
        // pages simply produce nothing.
        let last_page = known_pages
            .map(|pages| pages.clamp(1, UNKNOWN_PAGE_LIMIT as usize) as u32)
            .unwrap_or(UNKNOWN_PAGE_LIMIT);

        let mut page_ranges = [PRINTPAGERANGE::default(); MAX_PAGE_RANGES];
        let mut dialog = PRINTDLGEXW {
            lStructSize: size_of::<PRINTDLGEXW>() as u32,
            hwndOwner: owner.hwnd,
            Flags: PRINTDLGEX_FLAGS(
                PD_RETURNDC.0 | PD_NOSELECTION.0 | PD_USEDEVMODECOPIESANDCOLLATE.0,
            ),
            nPageRanges: 0,
            nMaxPageRanges: MAX_PAGE_RANGES as u32,
            lpPageRanges: page_ranges.as_mut_ptr(),
            nMinPage: 1,
            nMaxPage: last_page,
            nCopies: 1,
            // Zero is not a valid starting page; the system expects
            // `START_PAGE_GENERAL` and rejects the dialog with E_INVALIDARG.
            nStartPage: u32::MAX,
            ..Default::default()
        };

        // `PrintDlgExW` uses COM internally and requires the calling thread to
        // have initialized it.
        let _com = ComGuard::init();
        if let Err(error) = unsafe { PrintDlgExW(&mut dialog) } {
            return Err(format!("打印对话框出错（0x{:08X}）", error.code().0));
        }

        let hdc = dialog.hDC;
        if hdc.0.is_null() {
            // Cancelled by the user.
            free_dialog_handles(&dialog);
            return Ok(None);
        }
        // Turned before anything is measured on it: the layout reads
        // `HORZRES`/`VERTRES` from the context, and a landscape sheet is wider.
        let hdc = match set_orientation(hdc, dialog.hDevMode, orientation) {
            Ok(hdc) => hdc,
            Err(error) => {
                free_dialog_handles(&dialog);
                return Err(error);
            }
        };
        free_dialog_handles(&dialog);

        let range = if dialog.Flags.0 & PD_PAGENUMS.0 != 0 && dialog.nPageRanges > 0 {
            let count = (dialog.nPageRanges as usize).min(MAX_PAGE_RANGES);
            let ranges = unsafe { std::slice::from_raw_parts(dialog.lpPageRanges, count) };
            let ranges: Vec<(u32, u32)> = ranges
                .iter()
                .map(|range| {
                    (
                        range.nFromPage.max(1),
                        range.nToPage.max(range.nFromPage.max(1)),
                    )
                })
                .collect();
            match merge_ranges(ranges) {
                Some(ranges) => PageRange::Pages(ranges),
                None => PageRange::All,
            }
        } else {
            PageRange::All
        };

        Ok(Some((hdc, range)))
    }

    /// Puts the ranges in order and joins adjacent or overlapping ones.
    fn merge_ranges(mut ranges: Vec<(u32, u32)>) -> Option<Vec<(u32, u32)>> {
        ranges.retain(|(first, last)| last >= first);
        if ranges.is_empty() {
            return None;
        }
        ranges.sort_unstable();
        let mut merged: Vec<(u32, u32)> = Vec::with_capacity(ranges.len());
        for (first, last) in ranges {
            match merged.last_mut() {
                Some(previous) if first <= previous.1.saturating_add(1) => {
                    previous.1 = previous.1.max(last);
                }
                _ => merged.push((first, last)),
            }
        }
        Some(merged)
    }

    /// Releases the buffers the dialog allocated for us.
    fn free_dialog_handles(dialog: &PRINTDLGEXW) {
        for handle in [dialog.hDevMode, dialog.hDevNames] {
            if !handle.0.is_null() {
                let _ = unsafe { GlobalFree(Some(HGLOBAL(handle.0))) };
            }
        }
    }

    /// Turn the sheet the way the job asks for.
    ///
    /// The driver's own `DEVMODE` — the one the dialog filled in, paper choice
    /// and all — is edited in place and handed back through `ResetDC`, because
    /// the orientation of the paper is the driver's business: it decides how
    /// large the printable area is and how the sheet leaves the mechanism.
    ///
    /// Returns the context to print on, which may be the reset one.
    pub(super) fn set_orientation(
        hdc: HDC,
        devmode: HGLOBAL,
        orientation: Orientation,
    ) -> Result<HDC, String> {
        if devmode.0.is_null() {
            // A driver that hands back no `DEVMODE` keeps its own setting.
            return Ok(hdc);
        }
        let pointer = unsafe { GlobalLock(devmode) } as *mut DEVMODEW;
        if pointer.is_null() {
            return Ok(hdc);
        }

        let value = match orientation {
            Orientation::Portrait => DMORIENT_PORTRAIT,
            Orientation::Landscape => DMORIENT_LANDSCAPE,
        };
        unsafe {
            let devmode = &mut *pointer;
            devmode.dmFields |= DM_ORIENTATION;
            devmode.Anonymous1.Anonymous1.dmOrientation = value as i16;
        }
        let reset = unsafe { ResetDCW(hdc, pointer) };
        let _ = unsafe { GlobalUnlock(devmode) };

        if reset.0.is_null() {
            return Err(String::from("打印机不接受这个纸张方向"));
        }
        Ok(reset)
    }

    /// Initializes COM for the dialog and balances `CoUninitialize` on drop.
    struct ComGuard(bool);

    impl ComGuard {
        fn init() -> Self {
            let result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
            // A failure means COM was already initialized with another model
            // (or not at all); the dialog still works and nothing must be
            // unbalanced.
            Self(result.is_ok())
        }
    }

    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }

    /// Text is wrapped to the printable width with GDI's own text metrics and
    /// paginated as it goes, so a long file never has to fit in memory as a
    /// second copy. Pages outside `range` are laid out but not printed.
    ///
    /// `stop_after` lets the layout quit once that page index has been closed;
    /// the outcome then reports an incomplete page count, since the rest of the
    /// text was never measured.
    #[allow(clippy::too_many_arguments)]
    fn print_text(
        device: &mut Device,
        text: &str,
        font_pt: f32,
        width: i32,
        height: i32,
        dpi_x: i32,
        dpi_y: i32,
        range: &PageRange,
        stop_after: Option<usize>,
    ) -> Result<LayoutOutcome, String> {
        let hdc = device.hdc();
        let margin_x = (dpi_x as f32 * MARGIN_INCHES) as i32;
        let margin_y = (dpi_y as f32 * MARGIN_INCHES) as i32;
        let left = margin_x;
        let top = margin_y;
        let right = width - margin_x;
        let bottom = height - margin_y;
        let max_width = (right - left).max(1);

        let font_height = -(((font_pt.max(6.0) * dpi_y as f32 / 72.0).round()) as i32).max(1);
        let font = unsafe {
            CreateFontW(
                font_height,
                0,
                0,
                0,
                400, // FW_NORMAL
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                PROOF_QUALITY,
                0,
                w!("Microsoft YaHei"),
            )
        };
        if font.0.is_null() {
            return Err(String::from("无法创建打印字体"));
        }

        let previous = unsafe { SelectObject(hdc, HGDIOBJ(font.0)) };
        let _font = FontGuard {
            hdc,
            previous,
            font,
        };
        unsafe {
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, COLORREF(0));
        }

        // A representative line height for CJK and Latin text.
        let sample: [u16; 3] = [0x6c49, b'A' as u16, b'g' as u16];
        let mut extent = SIZE::default();
        let _ = unsafe { GetTextExtentPoint32W(hdc, &sample, &mut extent) };
        let line_height = ((extent.cy.max(1) as f32) * 1.45).round() as i32;
        let line_height = line_height.max(1);

        // Wrap every source line to the printable width and run the pages as the
        // rows are produced.
        let mut pager = Pager {
            device,
            hdc,
            left,
            top,
            bottom,
            line_height,
            y: top,
            open: false,
            drawing: false,
            index: 0,
            pages: 0,
            range,
            stop_after,
            stopped: false,
        };

        'lines: for line in text.split('\n') {
            let line = line.strip_suffix('\r').unwrap_or(line);
            // Tabs are expanded so a tab does not measure as one blank column.
            let utf16: Vec<u16> = line.replace('\t', "    ").encode_utf16().collect();
            if utf16.is_empty() {
                if !pager.row(&[])? {
                    break 'lines;
                }
                continue;
            }

            let mut start = 0;
            while start < utf16.len() {
                let rest = &utf16[start..];
                let mut fit = 0;
                let mut size = SIZE::default();
                let _ = unsafe {
                    GetTextExtentExPointW(
                        hdc,
                        PCWSTR::from_raw(rest.as_ptr()),
                        rest.len() as i32,
                        max_width,
                        Some(&mut fit),
                        None,
                        &mut size,
                    )
                };

                // Always consume at least one character so a single glyph wider
                // than the page cannot loop forever.
                let count = if fit <= 0 {
                    1
                } else {
                    (fit as usize).min(rest.len())
                };
                if !pager.row(&rest[..count])? {
                    break 'lines;
                }
                start += count;
            }
        }

        let outcome = pager.finish()?;
        if outcome.summary.total == 0 && range.includes(0) {
            blank_page(device, 0)?;
            return Ok(LayoutOutcome::whole(PrintSummary {
                printed: 1,
                total: 1,
            }));
        }
        Ok(outcome)
    }

    /// Writes rows down the printable area and starts a new page when it fills.
    struct Pager<'a> {
        device: &'a mut Device,
        hdc: HDC,
        left: i32,
        top: i32,
        bottom: i32,
        line_height: i32,
        y: i32,
        /// A page is being laid out; it is only sent to the printer when it is
        /// selected by `range`.
        open: bool,
        drawing: bool,
        /// Pages laid out so far.
        index: usize,
        /// Pages sent to the printer.
        pages: usize,
        range: &'a PageRange,
        /// Highest page that may be opened; the layout quits beyond it.
        stop_after: Option<usize>,
        stopped: bool,
    }

    impl Pager<'_> {
        /// Write one row. False means the layout asked to stop.
        fn row(&mut self, chars: &[u16]) -> Result<bool, String> {
            let next = self.y + self.line_height;
            if !self.open || next > self.bottom {
                self.close()?;
                if !self.begin()? {
                    return Ok(false);
                }
                self.y = self.top;
            }

            if self.drawing && !chars.is_empty() {
                let _ = unsafe { TextOutW(self.hdc, self.left, self.y, chars) };
            }
            self.y += self.line_height;
            Ok(true)
        }

        /// Lay out the next page, opening it only when it is selected.
        fn begin(&mut self) -> Result<bool, String> {
            if let Some(stop) = self.stop_after
                && self.index > stop
            {
                self.stopped = true;
                return Ok(false);
            }
            let index = self.index;
            self.open = true;
            self.drawing = self.range.includes(index);
            self.index += 1;
            if self.drawing {
                self.device.start_page(index)?;
                self.pages += 1;
            }
            Ok(true)
        }

        fn close(&mut self) -> Result<(), String> {
            if !self.open {
                return Ok(());
            }
            self.open = false;
            if self.drawing {
                self.drawing = false;
                self.device.end_page(self.index - 1)?;
            }
            Ok(())
        }

        fn finish(mut self) -> Result<LayoutOutcome, String> {
            self.close()?;
            Ok(LayoutOutcome {
                summary: PrintSummary {
                    printed: self.pages,
                    total: self.index,
                },
                complete: !self.stopped,
            })
        }
    }

    /// PDF pages are rendered at the printer's own resolution and stretched onto
    /// the printable area, so text and images stay sharp. Pages outside `range`
    /// are skipped without being rendered.
    fn print_pdf(
        device: &mut Device,
        source: &crate::pdf::PdfSource,
        width: i32,
        height: i32,
        dpi_x: i32,
        dpi_y: i32,
        range: &PageRange,
    ) -> Result<LayoutOutcome, String> {
        let hdc = device.hdc();
        let margin_x = (dpi_x as f32 * MARGIN_INCHES) as i32;
        let margin_y = (dpi_y as f32 * MARGIN_INCHES) as i32;
        let available_width = (width - 2 * margin_x).max(1);
        let available_height = (height - 2 * margin_y).max(1);

        let total = source.page_count();
        let mut pages = 0;
        for index in 0..total {
            if !range.includes(index) {
                continue;
            }
            let size = source.page_sizes()[index];
            // Size on paper if the page were printed at its natural size.
            let natural_width = size.width / 72.0 * dpi_x as f32;
            let natural_height = size.height / 72.0 * dpi_y as f32;
            if natural_width <= 0.0 || natural_height <= 0.0 {
                continue;
            }

            let factor = (available_width as f32 / natural_width)
                .min(available_height as f32 / natural_height);
            let target_width = ((natural_width * factor).round() as i32).max(1);
            let target_height = ((natural_height * factor).round() as i32).max(1);

            let scale = source.clamp_scale((target_width as f32 / size.width).clamp(0.2, 8.0));
            let Some(bitmap) = source.render_bitmap(index, scale) else {
                return Err(format!("第 {} 页无法渲染", index + 1));
            };

            let x = margin_x + ((available_width - target_width) / 2).max(0);
            let y = margin_y + ((available_height - target_height) / 2).max(0);

            device.start_page(index)?;

            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: bitmap.width as i32,
                    // A negative height asks for rows from the top down.
                    biHeight: -(bitmap.height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: 0, // BI_RGB
                    ..Default::default()
                },
                ..Default::default()
            };
            let written = unsafe {
                StretchDIBits(
                    hdc,
                    x,
                    y,
                    target_width,
                    target_height,
                    0,
                    0,
                    bitmap.width as i32,
                    bitmap.height as i32,
                    Some(bitmap.data.as_ptr() as *const c_void),
                    &info,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                )
            };
            if written == 0 || written as u32 == u32::MAX {
                return Err(format!("第 {} 页无法发送到打印机", index + 1));
            }

            device.end_page(index)?;
            pages += 1;
        }

        if total == 0 && range.includes(0) {
            pages = blank_page(device, 0)?;
            return Ok(LayoutOutcome::whole(PrintSummary {
                printed: pages,
                total: pages,
            }));
        }
        Ok(LayoutOutcome::whole(PrintSummary {
            printed: pages,
            total,
        }))
    }

    /// Emit one empty page so a document with no content still produces output.
    fn blank_page(device: &mut Device, index: usize) -> Result<usize, String> {
        device.start_page(index)?;
        device.end_page(index)?;
        Ok(1)
    }

    /// Frees the printer context however the job ends.
    struct DcGuard(HDC);

    impl Drop for DcGuard {
        fn drop(&mut self) {
            let _ = unsafe { DeleteDC(self.0) };
        }
    }

    /// Restores the font that was selected before printing.
    struct FontGuard {
        hdc: HDC,
        previous: HGDIOBJ,
        font: HFONT,
    }

    impl Drop for FontGuard {
        fn drop(&mut self) {
            unsafe {
                SelectObject(self.hdc, self.previous);
                let _ = DeleteObject(HGDIOBJ(self.font.0));
            }
        }
    }
}

/// A page read back from the sheet carries the bytes in the order the screen
/// reads them: blue, green, red, alpha. Swapping red and blue here is invisible
/// in the odd-looking check below but turns every warm colour of a document
/// cold on screen.
#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use windows::Win32::Foundation::{COLORREF, RECT};
    use windows::Win32::Graphics::Gdi::{CreateSolidBrush, DeleteObject, FillRect, HGDIOBJ};

    #[test]
    fn sheets_are_read_back_in_texture_order() {
        let geometry = SheetGeometry::without_margins(32, 32);
        let mut device = Device::sheet(geometry).expect("sheet");
        let hdc = device.hdc();
        device.start_page(0).expect("start");

        // Paint the whole sheet red: `COLORREF` is `0x00BBGGRR`.
        let brush = unsafe { CreateSolidBrush(COLORREF(0x0000_00FF)) };
        let area = RECT {
            left: 0,
            top: 0,
            right: 32,
            bottom: 32,
        };
        unsafe {
            FillRect(hdc, &area, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));
        }

        device.end_page(0).expect("end");
        let pages = device.into_pages();
        let (index, image) = pages.first().expect("the painted page is kept");
        assert_eq!(*index, 0);

        // Blue first, red last: red is still red once the texture reads it.
        let pixel = image.get_pixel(16, 16).0;
        assert_eq!(pixel, [0, 0, 255, 255]);
    }

    /// Turning the sheet is not just swapping the sides: the hard margins turn
    /// with it, so the bottom margin of a portrait page becomes the left margin
    /// of a landscape one and the printable area stays where the mechanism can
    /// reach it.
    #[test]
    fn turning_a_sheet_rotates_its_hard_margins() {
        let portrait = SheetGeometry {
            printable_width: 1800,
            printable_height: 2600,
            width: 2000,
            height: 2800,
            origin_x: 100,
            origin_y: 50,
        };

        let landscape = portrait.turned();
        assert!(!portrait.is_landscape());
        assert!(landscape.is_landscape());
        assert_eq!((landscape.width, landscape.height), (2800, 2000));
        assert_eq!(
            (landscape.printable_width, landscape.printable_height),
            (2600, 1800)
        );
        // 下边距 2800 - 50 - 2600 = 150 成了左边距，左边距 100 成了上边距。
        assert_eq!((landscape.origin_x, landscape.origin_y), (150, 100));
    }

    /// The orientation really reaches the driver: after `ResetDC` the printer
    /// reports the printable area of a turned sheet. Needs a printer, so it is
    /// opt-in: `cargo test the_driver_hands_back_a_turned_sheet -- --ignored`.
    #[test]
    #[ignore]
    fn the_driver_hands_back_a_turned_sheet() {
        use windows::Win32::Foundation::GlobalFree;
        use windows::Win32::Graphics::Gdi::{
            CreateDCW, DEVMODEW, DeleteDC, DM_OUT_BUFFER, GetDeviceCaps, HORZRES, VERTRES,
        };
        use windows::Win32::Graphics::Printing::{DocumentPropertiesW, GetDefaultPrinterW, PRINTER_HANDLE};
        use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
        use windows::core::{PCWSTR, PWSTR, w};

        let mut needed = 0u32;
        let _ = unsafe { GetDefaultPrinterW(None, &mut needed) };
        if needed == 0 {
            panic!("this machine has no default printer");
        }
        let mut buffer = vec![0u16; needed as usize];
        let _ = unsafe { GetDefaultPrinterW(Some(PWSTR(buffer.as_mut_ptr())), &mut needed) }.ok();
        let name = String::from_utf16_lossy(&buffer[..needed as usize - 1]);
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();

        let hdc = unsafe { CreateDCW(w!("WINSPOOL"), PCWSTR(wide.as_ptr()), PCWSTR::null(), None) };
        assert!(!hdc.0.is_null(), "the printer refused a context");
        let portrait_width = unsafe { GetDeviceCaps(Some(hdc), HORZRES) };
        let portrait_height = unsafe { GetDeviceCaps(Some(hdc), VERTRES) };

        // The driver's own DEVMODE, which the test then turns like the dialog
        // path does.
        let size = unsafe {
            DocumentPropertiesW(
                None,
                PRINTER_HANDLE::default(),
                PCWSTR(wide.as_ptr()),
                None,
                None,
                0,
            )
        };
        assert!(size >= size_of::<DEVMODEW>() as i32);
        let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, size as usize) }.expect("alloc");
        let pointer = unsafe { GlobalLock(handle) };
        assert!(!pointer.is_null());
        unsafe {
            DocumentPropertiesW(
                None,
                PRINTER_HANDLE::default(),
                PCWSTR(wide.as_ptr()),
                Some(pointer as *mut DEVMODEW),
                None,
                DM_OUT_BUFFER.0,
            )
        };
        let _ = unsafe { GlobalUnlock(handle) };

        let turned = super::imp::set_orientation(hdc, handle, Orientation::Landscape)
            .expect("the driver accepted the turned sheet");
        let width = unsafe { GetDeviceCaps(Some(turned), HORZRES) };
        let height = unsafe { GetDeviceCaps(Some(turned), VERTRES) };
        assert!(
            width > height,
            "{width} × {height}：ResetDC 之后纸还是竖着的"
        );
        assert_ne!(
            (width, height),
            (portrait_width, portrait_height),
            "打印区域没有变化，方向多半没有生效"
        );

        unsafe {
            let _ = DeleteDC(hdc);
            let _ = GlobalFree(Some(handle));
        }
    }

    /// Four pages on one sheet, named after what they check.
    fn text_job(lines: usize) -> PrintJob {
        let mut text = String::new();
        for line in 0..lines {
            text.push_str(&format!("第 {line} 行，用来铺满一页\n"));
        }
        PrintJob {
            title: String::from("nup"),
            content: PrintContent::Text {
                text: std::sync::Arc::new(text),
                font_pt: 10.0,
            },
            fit: PrintFit::FitToPaper,
            orientation: Orientation::Portrait,
            pages_per_sheet: PagesPerSheet::One,
            image: ImagePrint::default(),
        }
    }

    /// Whether any pixel in the rectangle is not white paper.
    fn has_ink(image: &RgbaImage, x: u32, y: u32, width: u32, height: u32) -> bool {
        (y..(y + height).min(image.height()))
            .any(|row| (x..(x + width).min(image.width())).any(|column| {
                let pixel = image.get_pixel(column, row).0;
                pixel[0] < 0xF0 || pixel[1] < 0xF0 || pixel[2] < 0xF0
            }))
    }

    #[test]
    fn a_sheet_is_divided_into_the_slots_of_its_grid() {
        let printable = (800, 1000);
        let page = (800, 1000);

        let first = slot_rect(printable, (2, 2), page, 0);
        let second = slot_rect(printable, (2, 2), page, 1);
        let third = slot_rect(printable, (2, 2), page, 2);
        let fourth = slot_rect(printable, (2, 2), page, 3);

        assert!(first.x < 40 && first.y < 40, "the first page sits top left");
        assert_eq!(second.y, first.y, "the second is beside it");
        assert!(second.x >= first.x + first.width, "and does not overlap it");
        assert!(third.y >= first.y + first.height, "the third starts a new row");
        assert_eq!(third.x, first.x);
        assert_eq!(fourth.x, second.x);
        assert_eq!(fourth.y, third.y);
        assert!(first.width <= 400 && first.height <= 500, "it fits its cell");
        assert!(
            fourth.x + fourth.width <= printable.0 && fourth.y + fourth.height <= printable.1,
            "the last page stays on the sheet"
        );
    }

    #[test]
    fn a_page_keeps_its_shape_inside_a_slot() {
        // A tall page in a wide slot: it shrinks to the height and leaves the
        // sides white instead of being stretched.
        let slot = slot_rect((1000, 500), (1, 1), (500, 1000), 0);
        assert!(slot.height <= 500);
        let shape = slot.width as f32 / slot.height as f32;
        assert!((shape - 0.5).abs() < 0.02, "{shape}: 页面被拉变形了");
    }

    #[test]
    fn a_grid_follows_the_shape_of_the_sheet() {
        assert_eq!(PagesPerSheet::Two.grid(false), (2, 1), "two pages side by side");
        assert_eq!(PagesPerSheet::Two.grid(true), (1, 2), "stacked on a wide sheet");
        assert_eq!(PagesPerSheet::Four.grid(false), (2, 2));
        assert_eq!(PagesPerSheet::Six.grid(false), (3, 2));
        assert_eq!(PagesPerSheet::Eight.grid(false), (4, 2));
    }

    #[test]
    fn a_preview_request_covers_the_pages_of_its_sheets() {
        // Sheet 2 of a four up job is pages 5 to 8, and they all have to be
        // painted or the group would be filled with the wrong pages.
        let range = imp::pages_of_sheets(&PageRange::Pages(vec![(2, 2)]), 4);
        assert_eq!(range, PageRange::Pages(vec![(5, 8)]));
        let range = imp::pages_of_sheets(&PageRange::Pages(vec![(1, 3), (7, 7)]), 2);
        assert_eq!(range, PageRange::Pages(vec![(1, 6), (13, 14)]));
        assert_eq!(imp::pages_of_sheets(&PageRange::All, 4), PageRange::All);
    }

    #[test]
    fn four_pages_share_one_sheet() {
        let sheet = SheetGeometry::without_margins(600, 800);
        let mut job = text_job(160);
        let (total, _) = render_pages(&job, sheet, 96.0, &PageRange::All, None).expect("layout");
        let pages = total.expect("the whole document was laid out");
        assert!(pages >= 4, "the sample has to fill more than one sheet");

        job.pages_per_sheet = PagesPerSheet::Four;
        let (total, sheets) =
            render_pages(&job, sheet, 96.0, &PageRange::All, None).expect("layout");
        assert_eq!(total, Some(pages.div_ceil(4)), "four pages per sheet");
        assert_eq!(sheets.len(), pages.div_ceil(4));

        // Every quadrant of the first sheet carries the text of one page.
        let (_, first) = &sheets[0];
        let (width, height) = (first.width() / 2, first.height() / 2);
        for (column, row) in [(0, 0), (width, 0), (0, height), (width, height)] {
            assert!(
                has_ink(first, column, row, width, height),
                "第 {} 个格子是空的",
                column / width + 1 + 2 * (row / height)
            );
        }
    }

    #[test]
    fn a_half_full_sheet_is_still_printed() {
        let sheet = SheetGeometry::without_margins(600, 800);
        let mut job = text_job(160);
        job.pages_per_sheet = PagesPerSheet::Four;

        // Ask for the first sheet only: the pages of the last one are never
        // laid out, and the sheet that was started still has to be finished.
        let (_, sheets) =
            render_pages(&job, sheet, 96.0, &PageRange::Pages(vec![(1, 1)]), Some(0))
                .expect("layout");
        assert_eq!(sheets.len(), 1, "one sheet was asked for and one came back");
        assert!(has_ink(&sheets[0].1, 0, 0, 100, 100), "and it is painted");
    }
}
