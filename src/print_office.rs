//! Printing Word and Excel documents with GDI.
//!
//! The reader draws these formats with GPUI, which cannot be used from the
//! printing thread, so the page layout is built again here from the same
//! models: Word paragraphs are wrapped with the printer's own text metrics and
//! Excel sheets are split into pages along their row and column boundaries.

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::Arc;

use image::RgbaImage;
use windows::Win32::Foundation::{COLORREF, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    BITMAPINFO, BITMAPINFOHEADER, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush,
    DEFAULT_CHARSET, DEFAULT_GUI_FONT, DEFAULT_QUALITY, DIB_RGB_COLORS, DRAW_TEXT_FORMAT,
    DT_BOTTOM, DT_CENTER, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_TOP, DT_VCENTER,
    DT_WORDBREAK, DeleteObject, DrawTextW, FillRect, FrameRect, GetStockObject,
    GetTextExtentExPointW, GetTextExtentPoint32W, HALFTONE, HBRUSH, HDC, HFONT, HGDIOBJ,
    IntersectClipRect, OUT_DEFAULT_PRECIS, RestoreDC, SRCCOPY, SaveDC, SelectObject, SetBkMode,
    SetBrushOrgEx, SetStretchBltMode, SetTextColor, StretchDIBits, TRANSPARENT, TextOutW,
};
use windows::core::PCWSTR;

use crate::excel::{self, Workbook};
use crate::print::{Device, ImagePrint, LayoutOutcome, PageRange, PrintFit, PrintSummary};
use crate::word;

thread_local! {
    /// Drawing is switched off while pages outside the selected range are laid
    /// out, so text metrics stay the same but nothing reaches the printer.
    static DRAWING: Cell<bool> = const { Cell::new(true) };
}

fn drawing() -> bool {
    DRAWING.with(Cell::get)
}

fn set_drawing(enabled: bool) {
    DRAWING.with(|drawing| drawing.set(enabled));
}

/// Restores normal drawing however a print job ends.
struct DrawingGuard;

impl Drop for DrawingGuard {
    fn drop(&mut self) {
        set_drawing(true);
    }
}

/// Margin around the printed content, in inches.
pub(crate) const MARGIN_INCHES: f32 = 0.5;
/// Space between a table cell's border and its text, in points.
const CELL_PADDING_PT: f32 = 4.0;
const BOLD_WEIGHT: i32 = 700;
const NORMAL_WEIGHT: i32 = 400;
/// Colour of table borders.
const LINE_COLOR: u32 = 0x808080;

// ------------------------------------------------------------------ documents

/// Print a Word document at the page width declared in the file.
///
/// `stop_after` is the last page a preview needs; the rest of the document is
/// then left unmeasured, which is what makes a long document open at once.
#[allow(clippy::too_many_arguments)]
pub fn print_word(
    device: &mut Device,
    document: &word::WordDocument,
    width: i32,
    height: i32,
    dpi_x: i32,
    dpi_y: i32,
    range: &PageRange,
    stop_after: Option<usize>,
    fit_mode: PrintFit,
) -> Result<LayoutOutcome, String> {
    let _drawing = DrawingGuard;
    let hdc = device.hdc();
    let margin_x = dpi_x as f32 * MARGIN_INCHES;
    let margin_y = dpi_y as f32 * MARGIN_INCHES;
    let printable_width = width as f32 - 2.0 * margin_x;
    let printable_height = height as f32 - 2.0 * margin_y;
    if printable_width < 72.0 || printable_height < 72.0 {
        return Err(String::from("打印机没有足够的可打印区域"));
    }

    // Points to device pixels, scaled down when the page is wider than paper.
    let natural = dpi_x as f32 / 72.0;
    let fit = match fit_mode {
        PrintFit::FitToPaper => (printable_width / (document.content_width * natural)).min(1.0),
        PrintFit::ActualSize => 1.0,
    };
    let scale = natural * fit;
    let column_width = document.content_width * scale;
    let left = margin_x + (printable_width - column_width) * 0.5;

    let mut fonts = Fonts::new(hdc, dpi_y as f32);
    let mut brushes = Brushes::default();
    let mut pages = Pages::new(
        device,
        margin_y,
        margin_y + printable_height,
        range.clone(),
        stop_after,
    );
    unsafe {
        SetBkMode(hdc, TRANSPARENT);
    }

    let mut y = margin_y;
    for block in &document.blocks {
        let more = match block {
            word::Block::Paragraph(paragraph) => {
                let layout = layout_paragraph(hdc, &mut fonts, paragraph, column_width, scale);
                draw_layout(
                    hdc,
                    &mut brushes,
                    &layout,
                    left,
                    &mut y,
                    column_width,
                    Some(&mut pages),
                )?
            }
            word::Block::Table(table) => draw_table(
                hdc,
                &mut fonts,
                &mut brushes,
                &mut pages,
                table,
                left,
                &mut y,
                column_width,
                scale,
            )?,
        };
        if !more {
            break;
        }
    }

    let outcome = pages.finish()?;
    if outcome.summary.total == 0 && range.includes(0) {
        blank_page(device, 0)?;
        return Ok(LayoutOutcome::whole(PrintSummary {
            printed: 1,
            total: 1,
        }));
    }
    Ok(outcome)
}

/// Print every worksheet, paginating along column and row boundaries.
#[allow(clippy::too_many_arguments)]
pub fn print_excel(
    device: &mut Device,
    workbook: &Workbook,
    width: i32,
    height: i32,
    dpi_x: i32,
    dpi_y: i32,
    range: &PageRange,
    stop_after: Option<usize>,
    fit_mode: PrintFit,
) -> Result<LayoutOutcome, String> {
    let _drawing = DrawingGuard;
    let hdc = device.hdc();
    let margin_x = dpi_x as f32 * MARGIN_INCHES;
    let margin_y = dpi_y as f32 * MARGIN_INCHES;
    let printable_width = width as f32 - 2.0 * margin_x;
    let printable_height = height as f32 - 2.0 * margin_y;
    if printable_width < 72.0 || printable_height < 72.0 {
        return Err(String::from("打印机没有足够的可打印区域"));
    }

    // Cell geometry is in 96 dpi pixels; scale it to the printer's resolution.
    let mut scale = dpi_x as f32 / 96.0;
    if fit_mode == PrintFit::FitToPaper {
        // A single column wider than the paper is cut at the right edge,
        // whatever the page breaks do, so the sheet is shrunk until its widest
        // column fits. Narrow columns still spill onto the next page by column,
        // which loses nothing.
        let widest = workbook
            .sheets
            .iter()
            .flat_map(|sheet| sheet.column_lefts.windows(2))
            .map(|pair| pair[1] - pair[0])
            .fold(0.0_f32, f32::max);
        if widest > 0.0 {
            let room = printable_width / scale;
            if widest > room {
                // Never shrink past a quarter: below that a page of text stops
                // being readable, and column pagination loses nothing anyway.
                scale *= (room / widest).max(0.25);
            }
        }
    }
    let page_width = printable_width / scale;
    let page_height = printable_height / scale;

    let mut fonts = Fonts::new(hdc, dpi_y as f32);
    let mut brushes = Brushes::default();
    let mut pages = Pages::new(
        device,
        margin_y,
        margin_y + printable_height,
        range.clone(),
        stop_after,
    );
    unsafe {
        SetBkMode(hdc, TRANSPARENT);
    }

    'sheets: for sheet in &workbook.sheets {
        let columns = page_breaks(&sheet.column_lefts, page_width);
        let rows = page_breaks(&sheet.row_tops, page_height);
        for (column_start, column_end) in &columns {
            for (row_start, row_end) in &rows {
                if !pages.new_page()? {
                    break 'sheets;
                }
                draw_sheet_page(
                    hdc,
                    &mut fonts,
                    &mut brushes,
                    sheet,
                    &workbook.styles,
                    margin_x,
                    margin_y,
                    scale,
                    (*row_start, *row_end),
                    (*column_start, *column_end),
                );
            }
        }
    }

    let outcome = pages.finish()?;
    if outcome.summary.total == 0 && range.includes(0) {
        blank_page(device, 0)?;
        return Ok(LayoutOutcome::whole(PrintSummary {
            printed: 1,
            total: 1,
        }));
    }
    Ok(outcome)
}

/// Print a picture: fitted to the printable area, then enlarged or reduced by
/// the print scale, moved by its anchor, and split across pages when it is set
/// to be put back together as a poster.
///
/// The scale is of the fitted size: 100% is the picture as it always printed,
/// more shows a part of the picture at that size, less leaves a white border
/// around it.
#[allow(clippy::too_many_arguments)]
pub fn print_image(
    device: &mut Device,
    pixels: &RgbaImage,
    width: i32,
    height: i32,
    dpi_x: i32,
    dpi_y: i32,
    range: &PageRange,
    print: ImagePrint,
) -> Result<LayoutOutcome, String> {
    let _drawing = DrawingGuard;
    let hdc = device.hdc();

    let margin_x = dpi_x as f32 * MARGIN_INCHES;
    let margin_y = dpi_y as f32 * MARGIN_INCHES;
    let printable_width = width as f32 - 2.0 * margin_x;
    let printable_height = height as f32 - 2.0 * margin_y;
    if printable_width < 72.0 || printable_height < 72.0 {
        return Err(String::from("打印机没有足够的可打印区域"));
    }

    if pixels.width() == 0 || pixels.height() == 0 {
        blank_page(device, 0)?;
        return Ok(LayoutOutcome::whole(PrintSummary {
            printed: 1,
            total: 1,
        }));
    }

    let layout = crate::print::image_layout(
        pixels.dimensions(),
        (printable_width, printable_height),
        print,
    );
    let total = layout.tiles.0 * layout.tiles.1;
    // A poster hands the same picture to every page; converting it out of RGBA
    // once keeps that from being paid per page.
    let Some(dib) = Dib::new(pixels) else {
        return Err(String::from("无法读取这张图片"));
    };
    let anchor = print.anchor.clamped();

    let mut printed = 0;
    for index in 0..total {
        if !range.includes(index) {
            continue;
        }
        let column = index % layout.tiles.0;
        let row = index / layout.tiles.0;
        let (x, y) = if print.poster {
            // Every page is the same picture, moved one page up and to the
            // left: page 1 holds the top left corner of the picture.
            (
                margin_x - column as f32 * printable_width,
                margin_y - row as f32 * printable_height,
            )
        } else {
            // 画面比纸小就居中留白；比纸大就把溢出的部分按基准点挪开，基准点
            // 在 0.5 时刚好是居中。
            crate::print::image_origin(
                (margin_x, margin_y, printable_width, printable_height),
                layout,
                anchor,
            )
        };

        device.start_page(index)?;
        // An enlarged picture reaches past the printable area, where the
        // printer cannot lay down ink; the page edge is where it stops, which
        // is what the preview shows as well.
        let clip = Clip::new(
            hdc,
            margin_x,
            margin_y,
            margin_x + printable_width,
            margin_y + printable_height,
        );
        dib.blit(hdc, x, y, layout.drawn.0, layout.drawn.1);
        drop(clip);
        device.end_page(index)?;
        printed += 1;
    }

    Ok(LayoutOutcome::whole(PrintSummary { printed, total }))
}

fn blank_page(device: &mut Device, index: usize) -> Result<(), String> {
    device.start_page(index)?;
    device.end_page(index)
}

// --------------------------------------------------------------------- pages

/// Opens pages as content fills them.
///
/// Every page is laid out, but only the pages selected by `range` are opened on
/// the device; the others advance the layout with drawing switched off.
struct Pages<'a> {
    device: &'a mut Device,
    top: f32,
    bottom: f32,
    open: bool,
    drawing: bool,
    /// Pages laid out so far.
    index: usize,
    /// Pages painted.
    printed: usize,
    range: PageRange,
    /// Highest page that may be opened; the layout quits beyond it.
    stop_after: Option<usize>,
    stopped: bool,
}

impl<'a> Pages<'a> {
    fn new(
        device: &'a mut Device,
        top: f32,
        bottom: f32,
        range: PageRange,
        stop_after: Option<usize>,
    ) -> Self {
        Self {
            device,
            top,
            bottom,
            open: false,
            drawing: false,
            index: 0,
            printed: 0,
            range,
            stop_after,
            stopped: false,
        }
    }

    /// Open the next page. False means the layout asked to stop.
    fn new_page(&mut self) -> Result<bool, String> {
        if let Some(stop) = self.stop_after
            && self.index > stop
        {
            self.stopped = true;
            return Ok(false);
        }
        self.close()?;
        let index = self.index;
        self.open = true;
        self.drawing = self.range.includes(index);
        self.index += 1;
        if self.drawing {
            self.device.start_page(index)?;
            self.printed += 1;
        }
        set_drawing(self.drawing);
        Ok(true)
    }

    fn close(&mut self) -> Result<(), String> {
        if !self.open {
            return Ok(());
        }
        self.open = false;
        if self.drawing {
            self.drawing = false;
            set_drawing(true);
            self.device.end_page(self.index - 1)?;
        }
        Ok(())
    }

    /// Make sure `height` fits below `y`, starting a page when needed.
    ///
    /// A block taller than the page keeps its own page instead of being pushed
    /// to a fresh one for ever. False means the layout asked to stop.
    fn ensure(&mut self, y: &mut f32, height: f32) -> Result<bool, String> {
        if !self.open {
            if !self.new_page()? {
                return Ok(false);
            }
            *y = self.top;
            return Ok(true);
        }
        if *y > self.top + 0.5 && *y + height > self.bottom && height < self.height() {
            if !self.new_page()? {
                return Ok(false);
            }
            *y = self.top;
        }
        Ok(true)
    }

    fn height(&self) -> f32 {
        self.bottom - self.top
    }

    fn finish(&mut self) -> Result<LayoutOutcome, String> {
        self.close()?;
        Ok(LayoutOutcome {
            summary: PrintSummary {
                printed: self.printed,
                total: self.index,
            },
            complete: !self.stopped,
        })
    }
}

// ------------------------------------------------------- fonts and brushes

#[derive(PartialEq, Eq, Hash)]
struct FontKey {
    family: String,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    half_points: u32,
}

/// Creates each family and size once per print job.
struct Fonts {
    hdc: HDC,
    dpi: f32,
    fonts: HashMap<FontKey, HFONT>,
}

impl Fonts {
    fn new(hdc: HDC, dpi: f32) -> Self {
        Self {
            hdc,
            dpi,
            fonts: HashMap::new(),
        }
    }

    fn get(
        &mut self,
        family: &str,
        bold: bool,
        italic: bool,
        underline: bool,
        strike: bool,
        size_pt: f32,
    ) -> HFONT {
        let family = if family.trim().is_empty() {
            String::from("Microsoft YaHei")
        } else {
            family.trim().to_string()
        };
        let key = FontKey {
            family: family.clone(),
            bold,
            italic,
            underline,
            strike,
            half_points: (size_pt * 2.0).round().max(2.0) as u32,
        };
        if let Some(font) = self.fonts.get(&key) {
            return *font;
        }

        let mut face: Vec<u16> = family.encode_utf16().collect();
        face.push(0);
        let height = -(((size_pt * self.dpi / 72.0).round()) as i32).max(1);
        let font = unsafe {
            CreateFontW(
                height,
                0,
                0,
                0,
                if bold { BOLD_WEIGHT } else { NORMAL_WEIGHT },
                u32::from(italic),
                u32::from(underline),
                u32::from(strike),
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                DEFAULT_QUALITY,
                0, // FF_DONTCARE
                PCWSTR(face.as_ptr()),
            )
        };
        self.fonts.insert(key, font);
        font
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        // A font cannot be deleted while it is selected into the device context.
        unsafe {
            let _ = SelectObject(self.hdc, GetStockObject(DEFAULT_GUI_FONT));
            for font in self.fonts.values() {
                let _ = DeleteObject(HGDIOBJ(font.0));
            }
        }
    }
}

#[derive(Default)]
struct Brushes {
    brushes: HashMap<u32, HBRUSH>,
}

impl Brushes {
    fn get(&mut self, rgb: u32) -> HBRUSH {
        if let Some(brush) = self.brushes.get(&rgb) {
            return *brush;
        }
        let brush = unsafe { CreateSolidBrush(color_ref(rgb)) };
        self.brushes.insert(rgb, brush);
        brush
    }
}

impl Drop for Brushes {
    fn drop(&mut self) {
        for brush in self.brushes.values() {
            let _ = unsafe { DeleteObject(HGDIOBJ(brush.0)) };
        }
    }
}

fn color_ref(rgb: u32) -> COLORREF {
    let red = (rgb >> 16) & 0xFF;
    let green = (rgb >> 8) & 0xFF;
    let blue = rgb & 0xFF;
    COLORREF(red | (green << 8) | (blue << 16))
}

fn select(hdc: HDC, font: HFONT) {
    let _ = unsafe { SelectObject(hdc, HGDIOBJ(font.0)) };
}

// ------------------------------------------------------------------ measuring

fn text_width(hdc: HDC, text: &[u16]) -> f32 {
    let mut size = SIZE::default();
    let _ = unsafe { GetTextExtentPoint32W(hdc, text, &mut size) };
    size.cx as f32
}

/// How many UTF-16 units of `text` fit into `max_width`, breaking at spaces
/// when possible and never inside a surrogate pair.
fn fit_prefix(hdc: HDC, text: &[u16], max_width: f32) -> usize {
    if text.is_empty() {
        return 0;
    }
    let mut fit = 0i32;
    let mut size = SIZE::default();
    let _ = unsafe {
        GetTextExtentExPointW(
            hdc,
            PCWSTR::from_raw(text.as_ptr()),
            text.len() as i32,
            max_width.max(0.0) as i32,
            Some(&mut fit),
            None,
            &mut size,
        )
    };

    let mut count = fit.max(0) as usize;
    if count >= text.len() {
        return text.len();
    }
    if count == 0 {
        return 0;
    }

    // A trailing high surrogate must stay with its low half.
    if text[count] >= 0xDC00 && text[count] <= 0xDFFF {
        count -= 1;
    }
    // Prefer the last space so Latin words are not split in the middle.
    if let Some(space) = text[..count]
        .iter()
        .rposition(|unit| *unit == u32::from(b' ') as u16)
        && space > 0
    {
        count = space + 1;
    }
    count.max(1)
}

// ---------------------------------------------------------------- Word layout

enum Item {
    Line {
        line: Line,
        indent: f32,
    },
    Image {
        pixels: Arc<RgbaImage>,
        width: f32,
        height: f32,
    },
}

#[derive(Default)]
struct Line {
    segments: Vec<(Vec<u16>, HFONT, COLORREF)>,
    width: f32,
}

struct ParagraphLayout {
    items: Vec<Item>,
    line_height: f32,
    /// Left indent of the whole paragraph, in device units.
    indent: f32,
    space_before: f32,
    space_after: f32,
    align: word::Align,
    border: word::ParagraphBorder,
    /// Background fill behind the paragraph, in RGB.
    fill: Option<u32>,
}

impl ParagraphLayout {
    fn height(&self) -> f32 {
        self.space_before
            + self.space_after
            + self
                .items
                .iter()
                .map(|item| match item {
                    Item::Line { .. } => self.line_height,
                    Item::Image { height, .. } => *height,
                })
                .sum::<f32>()
    }
}

fn layout_paragraph(
    hdc: HDC,
    fonts: &mut Fonts,
    paragraph: &word::Paragraph,
    width: f32,
    scale: f32,
) -> ParagraphLayout {
    let line_height =
        (paragraph.draw_size() * 1.3 * paragraph.line_spacing.max(0.8) * scale).max(1.0);
    let indent = paragraph.indent.max(0.0) * scale;
    let first_indent = paragraph.first_line.max(0.0) * scale;
    let rest_width = (width - indent).max(24.0);
    let first_width = (rest_width - first_indent).max(24.0);

    let mut flow = Flow {
        hdc,
        fonts,
        items: Vec::new(),
        line: Line::default(),
        rest_width,
        limit: first_width,
        content_width: width.max(24.0),
        first_line: true,
        first_indent,
    };

    let base_family = paragraph.font.clone().unwrap_or_default();
    if let Some(marker) = paragraph.marker.as_deref() {
        flow.push_text(
            &format!("{marker}\u{00a0}"),
            &base_family,
            paragraph.bold,
            paragraph.italic,
            false,
            false,
            paragraph.size,
            run_color(None, paragraph),
        );
    }

    for run in &paragraph.runs {
        if let Some(image) = &run.image {
            flow.push_image(image, scale);
            continue;
        }
        flow.push_text(
            &run.text,
            run.font.as_deref().unwrap_or(&base_family),
            run.bold || paragraph.bold,
            run.italic || paragraph.italic,
            run.underline,
            run.strike,
            run.size.unwrap_or(paragraph.size),
            run_color(run.color, paragraph),
        );
    }
    flow.finish();
    let mut items = flow.items;

    if items.is_empty() {
        items.push(Item::Line {
            line: Line::default(),
            indent: first_indent,
        });
    }

    ParagraphLayout {
        items,
        line_height,
        indent,
        space_before: paragraph.space_before * scale,
        space_after: paragraph.space_after * scale,
        align: paragraph.align,
        border: paragraph.border,
        fill: paragraph.fill,
    }
}

fn run_color(color: Option<u32>, paragraph: &word::Paragraph) -> COLORREF {
    color
        .or(paragraph.color)
        .map(color_ref)
        .unwrap_or(COLORREF(0))
}

/// Collects wrapped lines and images for one paragraph.
struct Flow<'a> {
    hdc: HDC,
    fonts: &'a mut Fonts,
    items: Vec<Item>,
    line: Line,
    rest_width: f32,
    limit: f32,
    /// Width of the whole text column, which a picture may never exceed.
    content_width: f32,
    first_line: bool,
    first_indent: f32,
}

impl Flow<'_> {
    fn finish(&mut self) {
        self.flush();
    }

    fn flush(&mut self) {
        if self.line.segments.is_empty() {
            return;
        }
        let indent = if self.first_line {
            self.first_indent
        } else {
            0.0
        };
        self.items.push(Item::Line {
            line: std::mem::take(&mut self.line),
            indent,
        });
        if self.first_line {
            self.first_line = false;
            self.limit = self.rest_width;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push_text(
        &mut self,
        text: &str,
        family: &str,
        bold: bool,
        italic: bool,
        underline: bool,
        strike: bool,
        size_pt: f32,
        color: COLORREF,
    ) {
        if text.is_empty() {
            return;
        }
        let font = self
            .fonts
            .get(family, bold, italic, underline, strike, size_pt);
        select(self.hdc, font);

        for (index, part) in text.split('\n').enumerate() {
            if index > 0 {
                self.flush();
            }
            let utf16: Vec<u16> = part.encode_utf16().collect();
            let mut start = 0;
            while start < utf16.len() {
                let remaining = self.limit - self.line.width;
                let mut fit = fit_prefix(self.hdc, &utf16[start..], remaining);
                if fit == 0 {
                    if self.line.segments.is_empty() {
                        // A single glyph wider than the line still gets placed.
                        fit = 1;
                    } else {
                        self.flush();
                        continue;
                    }
                }

                let chunk = &utf16[start..start + fit];
                let width = text_width(self.hdc, chunk);
                self.line.segments.push((chunk.to_vec(), font, color));
                self.line.width += width;
                start += fit;
                if start < utf16.len() {
                    self.flush();
                }
            }
        }
    }

    fn push_image(&mut self, image: &word::InlineImage, scale: f32) {
        self.flush();
        if self.first_line {
            self.first_line = false;
            self.limit = self.rest_width;
        }
        let mut width = (image.width * scale).max(1.0);
        let mut height = (image.height * scale).max(1.0);
        // A picture wider than the column used to run past the right margin and
        // be cut off at the edge of the paper, which is what made a stray wide
        // picture look broken in the preview. Shrinking it to the column keeps
        // the whole picture on the page, in proportion.
        if width > self.content_width {
            height *= self.content_width / width;
            width = self.content_width;
        }
        self.items.push(Item::Image {
            pixels: image.pixels.clone(),
            width,
            height,
        });
    }
}

/// Draw one laid-out paragraph. False means the page layout asked to stop, so
/// the caller must not draw anything below it.
fn draw_layout(
    hdc: HDC,
    brushes: &mut Brushes,
    layout: &ParagraphLayout,
    x: f32,
    y: &mut f32,
    width: f32,
    mut pages: Option<&mut Pages>,
) -> Result<bool, String> {
    *y += layout.space_before;

    let last_item = layout.items.len().saturating_sub(1);
    for (index, item) in layout.items.iter().enumerate() {
        let content_width = (width - layout.indent).max(0.0);
        match item {
            Item::Line { line, indent } => {
                if let Some(pages) = pages.as_deref_mut()
                    && !pages.ensure(y, layout.line_height)?
                {
                    return Ok(false);
                }
                let offset = alignment_offset(layout.align, content_width, line.width);
                let mut cursor = x + layout.indent + indent + offset;
                let line_top = *y;
                if let Some(fill) = layout.fill {
                    fill_rect(hdc, brushes, x, line_top, width, layout.line_height, fill);
                }
                for (text, font, color) in &line.segments {
                    if text.is_empty() {
                        continue;
                    }
                    select(hdc, *font);
                    if drawing() {
                        unsafe {
                            SetTextColor(hdc, *color);
                            let _ = TextOutW(hdc, cursor.round() as i32, y.round() as i32, text);
                        }
                    }
                    cursor += text_width(hdc, text);
                }
                *y += layout.line_height;
                draw_paragraph_border(
                    hdc,
                    brushes,
                    layout,
                    index == 0,
                    index == last_item,
                    x,
                    line_top,
                    width,
                    layout.line_height,
                );
            }
            Item::Image {
                pixels,
                width: image_width,
                height: image_height,
            } => {
                if let Some(pages) = pages.as_deref_mut()
                    && !pages.ensure(y, *image_height)?
                {
                    return Ok(false);
                }
                let offset = alignment_offset(layout.align, content_width, *image_width);
                draw_rgba(
                    hdc,
                    pixels,
                    x + layout.indent + offset,
                    *y,
                    *image_width,
                    *image_height,
                );
                *y += *image_height;
            }
        }
    }

    *y += layout.space_after;
    Ok(true)
}

fn alignment_offset(align: word::Align, width: f32, content: f32) -> f32 {
    match align {
        word::Align::Center => ((width - content) * 0.5).max(0.0),
        word::Align::Right => (width - content).max(0.0),
        _ => 0.0,
    }
}

/// Draws the paragraph's outline for one line of a scrollable or printed page.
/// Consecutive lines only draw the sides, so the block shares one box.
#[allow(clippy::too_many_arguments)]
fn draw_paragraph_border(
    hdc: HDC,
    brushes: &mut Brushes,
    layout: &ParagraphLayout,
    first: bool,
    last: bool,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) {
    let border = layout.border;
    let Some(color) = border.color else {
        return;
    };
    let thickness = 1.0;
    if border.left {
        fill_rect(hdc, brushes, x, y, thickness, height, color);
    }
    if border.right {
        fill_rect(
            hdc,
            brushes,
            x + width - thickness,
            y,
            thickness,
            height,
            color,
        );
    }
    if border.top && first {
        fill_rect(hdc, brushes, x, y, width, thickness, color);
    }
    if border.bottom && last {
        fill_rect(
            hdc,
            brushes,
            x,
            y + height - thickness,
            width,
            thickness,
            color,
        );
    }
}

// -------------------------------------------------------------- Word tables

#[allow(clippy::too_many_arguments)]
fn draw_table(
    hdc: HDC,
    fonts: &mut Fonts,
    brushes: &mut Brushes,
    pages: &mut Pages,
    table: &word::Table,
    x: f32,
    y: &mut f32,
    available: f32,
    scale: f32,
) -> Result<bool, String> {
    let widths = column_widths(table, available, scale);
    if widths.is_empty() {
        return Ok(true);
    }
    let padding = (CELL_PADDING_PT * scale).max(2.0);

    for row in &table.rows {
        // Lay each cell out once so the row can be measured before it is drawn.
        let mut cells = Vec::new();
        let mut column = 0usize;
        for cell in row {
            if column >= widths.len() {
                break;
            }
            let span = cell.span.max(1).min(widths.len() - column);
            let width: f32 = widths[column..column + span].iter().sum();
            let mut paragraphs = Vec::new();
            let mut sources = Vec::new();
            collect_paragraphs(&cell.blocks, &mut sources, 0);
            for paragraph in sources {
                paragraphs.push(layout_paragraph(
                    hdc,
                    fonts,
                    paragraph,
                    (width - 2.0 * padding).max(24.0),
                    scale,
                ));
            }
            let height = paragraphs
                .iter()
                .map(|paragraph| paragraph.height())
                .sum::<f32>()
                + 2.0 * padding;
            cells.push(CellLayout {
                paragraphs,
                width,
                height,
                fill: cell.fill,
            });
            column += span;
        }
        if cells.is_empty() {
            continue;
        }

        let row_height = cells
            .iter()
            .map(|cell| cell.height)
            .fold(0.0_f32, f32::max)
            .max(2.0 * padding);
        if !pages.ensure(y, row_height)? {
            return Ok(false);
        }

        let mut cell_x = x;
        for cell in &cells {
            if let Some(fill) = cell.fill {
                fill_rect(hdc, brushes, cell_x, *y, cell.width, row_height, fill);
            }

            let saved = unsafe { SaveDC(hdc) };
            let _ = unsafe {
                IntersectClipRect(
                    hdc,
                    cell_x.round() as i32,
                    y.round() as i32,
                    (cell_x + cell.width).round() as i32,
                    (*y + row_height).round() as i32,
                )
            };
            let mut cell_y = *y + padding;
            for paragraph in &cell.paragraphs {
                // Cells are drawn whole; only the table row itself paginates.
                let _ = draw_layout(
                    hdc,
                    brushes,
                    paragraph,
                    cell_x + padding,
                    &mut cell_y,
                    (cell.width - 2.0 * padding).max(24.0),
                    None,
                )?;
            }
            let _ = unsafe { RestoreDC(hdc, saved) };

            if table.borders {
                frame_rect(hdc, brushes, cell_x, *y, cell.width, row_height, LINE_COLOR);
            }
            cell_x += cell.width;
        }

        *y += row_height;
    }

    Ok(true)
}

struct CellLayout {
    paragraphs: Vec<ParagraphLayout>,
    width: f32,
    height: f32,
    fill: Option<u32>,
}

fn column_widths(table: &word::Table, available: f32, scale: f32) -> Vec<f32> {
    let mut widths = if table.columns.is_empty() {
        let columns = table
            .rows
            .iter()
            .map(|row| row.iter().map(|cell| cell.span.max(1)).sum::<usize>())
            .max()
            .unwrap_or(1)
            .max(1);
        vec![available / columns as f32; columns]
    } else {
        table
            .columns
            .iter()
            .map(|width| (width * scale).max(8.0))
            .collect()
    };

    let total: f32 = widths.iter().sum();
    if total > available && total > 0.0 {
        let factor = available / total;
        for width in &mut widths {
            *width *= factor;
        }
    }
    widths
}

/// Nested tables are flattened to their paragraphs, which keeps the text in
/// order without a second layout engine.
fn collect_paragraphs<'a>(
    blocks: &'a [word::Block],
    out: &mut Vec<&'a word::Paragraph>,
    depth: usize,
) {
    for block in blocks {
        match block {
            word::Block::Paragraph(paragraph) => out.push(paragraph),
            word::Block::Table(table) if depth < 4 => {
                for row in &table.rows {
                    for cell in row {
                        collect_paragraphs(&cell.blocks, out, depth + 1);
                    }
                }
            }
            _ => {}
        }
    }
}

// --------------------------------------------------------------- Excel pages

/// Index ranges whose extent fits into `page_size`.
fn page_breaks(edges: &[f32], page_size: f32) -> Vec<(usize, usize)> {
    let count = edges.len().saturating_sub(1);
    if count == 0 {
        return vec![(0, 0)];
    }

    let mut ranges = Vec::new();
    let mut start = 0;
    for index in 0..count {
        let span = edges[index + 1] - edges[start];
        if span > page_size && index > start {
            ranges.push((start, index));
            start = index;
        }
    }
    ranges.push((start, count));
    ranges
}

#[allow(clippy::too_many_arguments)]
fn draw_sheet_page(
    hdc: HDC,
    fonts: &mut Fonts,
    brushes: &mut Brushes,
    sheet: &excel::Sheet,
    styles: &[excel::CellStyle],
    origin_x: f32,
    origin_y: f32,
    scale: f32,
    rows: (usize, usize),
    columns: (usize, usize),
) {
    let (row_start, row_end) = rows;
    let (column_start, column_end) = columns;
    if !drawing() || row_start >= row_end || column_start >= column_end {
        return;
    }

    let x_offset = sheet.column_lefts[column_start];
    let y_offset = sheet.row_tops[row_start];
    let row_count = sheet.row_count();
    let column_count = sheet.column_count();
    let padding = (3.0 * scale).max(1.0);

    draw_grid(
        hdc,
        brushes,
        sheet,
        origin_x,
        origin_y,
        scale,
        x_offset,
        y_offset,
        rows,
        columns,
    );

    let first = sheet
        .rows
        .partition_point(|row| (row.index as usize) < row_start);
    for row in &sheet.rows[first..] {
        let row_index = row.index as usize;
        if row_index >= row_end || row_index >= row_count {
            break;
        }
        let cells_start = row
            .cells
            .partition_point(|cell| (cell.column as usize) < column_start);
        for cell in &row.cells[cells_start..] {
            let column = cell.column as usize;
            if column >= column_end {
                break;
            }
            if sheet.is_covered(row.index, cell.column) {
                continue;
            }

            let (row_span, column_span) = sheet
                .merge_at(row.index, cell.column)
                .map(|merge| (merge.rows as usize, merge.columns as usize))
                .unwrap_or((1, 1));
            let bottom = (row_index + row_span).min(row_count);
            let right = (column + column_span).min(column_count);

            let left = origin_x + (sheet.column_lefts[column] - x_offset) * scale;
            let top = origin_y + (sheet.row_tops[row_index] - y_offset) * scale;
            let width = (sheet.column_lefts[right] - sheet.column_lefts[column]) * scale;
            let height = (sheet.row_tops[bottom] - sheet.row_tops[row_index]) * scale;
            if width <= 0.5 || height <= 0.5 {
                continue;
            }

            let style = styles.get(cell.style);
            if let Some(fill) = style.and_then(|style| style.fill) {
                fill_rect(hdc, brushes, left, top, width, height, fill);
            }

            let saved = unsafe { SaveDC(hdc) };
            let _ = unsafe {
                IntersectClipRect(
                    hdc,
                    left.round() as i32,
                    top.round() as i32,
                    (left + width).round() as i32,
                    (top + height).round() as i32,
                )
            };

            draw_cell_text(hdc, fonts, cell, style, left, top, width, height, padding);

            if let Some(style) = style {
                let thickness = (scale * 0.75).max(1.0);
                for (side, color) in [
                    (Side::Top, style.border_top),
                    (Side::Bottom, style.border_bottom),
                    (Side::Left, style.border_left),
                    (Side::Right, style.border_right),
                ] {
                    let Some(color) = color else { continue };
                    match side {
                        Side::Top => fill_rect(hdc, brushes, left, top, width, thickness, color),
                        Side::Bottom => fill_rect(
                            hdc,
                            brushes,
                            left,
                            top + height - thickness,
                            width,
                            thickness,
                            color,
                        ),
                        Side::Left => fill_rect(hdc, brushes, left, top, thickness, height, color),
                        Side::Right => fill_rect(
                            hdc,
                            brushes,
                            left + width - thickness,
                            top,
                            thickness,
                            height,
                            color,
                        ),
                    }
                }
            }

            let _ = unsafe { RestoreDC(hdc, saved) };
        }
    }
}

enum Side {
    Top,
    Bottom,
    Left,
    Right,
}

/// The pale grid Excel shows behind cells that carry no border of their own.
///
/// Without it a sheet whose cells are only shaded — or whose empty cells hold
/// nothing but a border — printed as a few floating lines with the rest of the
/// table missing, while the same sheet is ruled edge to edge on screen.
const GRID_COLOR: u32 = 0xD9D9D9;

/// Rule one page of a sheet, before the cells are painted on top of it.
#[allow(clippy::too_many_arguments)]
fn draw_grid(
    hdc: HDC,
    brushes: &mut Brushes,
    sheet: &excel::Sheet,
    origin_x: f32,
    origin_y: f32,
    scale: f32,
    x_offset: f32,
    y_offset: f32,
    rows: (usize, usize),
    columns: (usize, usize),
) {
    let (row_start, row_end) = rows;
    let (column_start, column_end) = columns;
    let row_end = row_end.min(sheet.row_count());
    let column_end = column_end.min(sheet.column_count());
    if row_start >= row_end || column_start >= column_end {
        return;
    }

    let thickness = (scale * 0.75).max(1.0);
    let left = origin_x;
    let top = origin_y;
    let right = origin_x + (sheet.column_lefts[column_end] - x_offset) * scale;
    let bottom = origin_y + (sheet.row_tops[row_end] - y_offset) * scale;

    // The closing edges are drawn as well, so a table whose last row or column
    // has no cells of its own still ends in a line.
    for row in row_start..=row_end {
        let y = origin_y + (sheet.row_tops[row] - y_offset) * scale;
        fill_rect(hdc, brushes, left, y, right - left, thickness, GRID_COLOR);
    }
    for column in column_start..=column_end {
        let x = origin_x + (sheet.column_lefts[column] - x_offset) * scale;
        fill_rect(hdc, brushes, x, top, thickness, bottom - top, GRID_COLOR);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_cell_text(
    hdc: HDC,
    fonts: &mut Fonts,
    cell: &excel::Cell,
    style: Option<&excel::CellStyle>,
    left: f32,
    top: f32,
    width: f32,
    height: f32,
    padding: f32,
) {
    if !drawing() || cell.text.is_empty() {
        return;
    }
    let size_pt = style.map(|style| style.size).unwrap_or(15.0) * 72.0 / 96.0;
    let font = fonts.get(
        "Microsoft YaHei",
        style.is_some_and(|style| style.bold),
        style.is_some_and(|style| style.italic),
        style.is_some_and(|style| style.underline),
        false,
        size_pt,
    );
    select(hdc, font);
    unsafe {
        SetTextColor(
            hdc,
            style
                .and_then(|style| style.color)
                .map(color_ref)
                .unwrap_or(COLORREF(0)),
        );
    }

    let wrap = style.is_some_and(|style| style.wrap);
    let align = style
        .map(|style| style.horizontal(cell.numeric))
        .unwrap_or_default();
    let valign = style.map(|style| style.valign).unwrap_or_default();

    let mut flags = DT_NOPREFIX.0;
    flags |= match align {
        excel::HorizontalAlign::Center => DT_CENTER.0,
        excel::HorizontalAlign::Right => DT_RIGHT.0,
        excel::HorizontalAlign::Left => DT_LEFT.0,
    };
    if wrap {
        flags |= DT_WORDBREAK.0 | DT_TOP.0;
    } else {
        flags |= DT_SINGLELINE.0
            | match valign {
                excel::VerticalAlign::Top => DT_TOP.0,
                excel::VerticalAlign::Center => DT_VCENTER.0,
                excel::VerticalAlign::Bottom => DT_BOTTOM.0,
            };
    }

    let mut rect = RECT {
        left: (left + padding).round() as i32,
        top: (top + padding * 0.5).round() as i32,
        right: (left + width - padding).round() as i32,
        bottom: (top + height - padding * 0.5).round() as i32,
    };
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return;
    }

    let mut text: Vec<u16> = cell.text.encode_utf16().collect();
    let _ = unsafe { DrawTextW(hdc, &mut text, &mut rect, DRAW_TEXT_FORMAT(flags)) };
}

// ----------------------------------------------------------------- primitives

fn fill_rect(hdc: HDC, brushes: &mut Brushes, x: f32, y: f32, width: f32, height: f32, rgb: u32) {
    if !drawing() || width < 0.5 || height < 0.5 {
        return;
    }
    let rect = RECT {
        left: x.round() as i32,
        top: y.round() as i32,
        right: (x + width).round() as i32,
        bottom: (y + height).round() as i32,
    };
    let brush = brushes.get(rgb);
    let _ = unsafe { FillRect(hdc, &rect, brush) };
}

fn frame_rect(hdc: HDC, brushes: &mut Brushes, x: f32, y: f32, width: f32, height: f32, rgb: u32) {
    if !drawing() {
        return;
    }
    let rect = RECT {
        left: x.round() as i32,
        top: y.round() as i32,
        right: (x + width).round() as i32,
        bottom: (y + height).round() as i32,
    };
    let brush = brushes.get(rgb);
    let _ = unsafe { FrameRect(hdc, &rect, brush) };
}

/// Keeps drawing inside a rectangle for as long as it lives.
///
/// The device context is shared by a whole page, so the cut-out is dropped
/// again before anything else is painted on it.
struct Clip {
    hdc: HDC,
    saved: i32,
}

impl Clip {
    fn new(hdc: HDC, left: f32, top: f32, right: f32, bottom: f32) -> Self {
        let saved = unsafe { SaveDC(hdc) };
        // Without a saved state there is nothing to restore the cut-out with,
        // so the clip is skipped rather than left on the context for good.
        if saved != 0 {
            unsafe {
                IntersectClipRect(
                    hdc,
                    left.round() as i32,
                    top.round() as i32,
                    right.round() as i32,
                    bottom.round() as i32,
                );
            }
        }
        Self { hdc, saved }
    }
}

impl Drop for Clip {
    fn drop(&mut self) {
        if self.saved != 0 {
            let _ = unsafe { RestoreDC(self.hdc, self.saved) };
        }
    }
}

fn draw_rgba(hdc: HDC, image: &RgbaImage, x: f32, y: f32, width: f32, height: f32) {
    if !drawing() {
        return;
    }
    if let Some(dib) = Dib::new(image) {
        dib.blit(hdc, x, y, width, height);
    }
}

/// A picture converted once into the bitmap `StretchDIBits` wants.
///
/// The conversion is the expensive part of drawing a picture, and a poster
/// print hands the same picture to every page, so it is done once and blitted
/// as often as needed.
struct Dib {
    data: Vec<u8>,
    info: BITMAPINFO,
    width: i32,
    height: i32,
}

impl Dib {
    fn new(image: &RgbaImage) -> Option<Self> {
        let (width, height) = image.dimensions();
        if width == 0 || height == 0 {
            return None;
        }

        // `StretchDIBits` reads a 32-bit `BI_RGB` bitmap as BGRA and ignores
        // the alpha byte, so a transparent pixel would be drawn as the colour
        // hiding behind it — usually black. Compositing over white here lets
        // the paper show through, which is what the reader sees, and gives the
        // bytes the order the call expects in the same pass.
        let mut data = Vec::with_capacity(image.as_raw().len());
        for pixel in image.as_raw().as_chunks::<4>().0 {
            let alpha = pixel[3] as u32;
            let over_white = |channel: u8| {
                ((channel as u32 * alpha + 255 * (255 - alpha) + 127) / 255) as u8
            };
            data.extend_from_slice(&[
                over_white(pixel[2]),
                over_white(pixel[1]),
                over_white(pixel[0]),
                255,
            ]);
        }

        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0, // BI_RGB
                ..Default::default()
            },
            ..Default::default()
        };

        Some(Self {
            data,
            info,
            width: width as i32,
            height: height as i32,
        })
    }

    fn blit(&self, hdc: HDC, x: f32, y: f32, width: f32, height: f32) {
        if !drawing() || width < 1.0 || height < 1.0 {
            return;
        }

        // The default stretch mode drops pixels instead of blending them, which
        // turns a small picture drawn larger into sharp blocks and a large one
        // drawn smaller into moiré. Halftone blends, and needs the brush origin
        // reset to stay aligned with the paper.
        unsafe {
            SetStretchBltMode(hdc, HALFTONE);
            let _ = SetBrushOrgEx(hdc, 0, 0, None);
        }

        let _ = unsafe {
            StretchDIBits(
                hdc,
                x.round() as i32,
                y.round() as i32,
                width.round() as i32,
                height.round() as i32,
                0,
                0,
                self.width,
                self.height,
                Some(self.data.as_ptr() as *const c_void),
                &self.info,
                DIB_RGB_COLORS,
                SRCCOPY,
            )
        };
    }
}

/// A picture is drawn through `StretchDIBits`, which knows nothing about
/// alpha: a transparent pixel used to reach the paper as black, and a small
/// picture scaled up used to be dropped into blocks. Both are checked here on
/// the same off-screen sheet the preview paints onto.
#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::print::{ImageScale, SheetGeometry};

    /// Paint `pixels` over the whole of a small sheet and return the sheet.
    fn paint(pixels: &RgbaImage) -> RgbaImage {
        let geometry = SheetGeometry::without_margins(40, 40);
        let mut device = Device::sheet(geometry).expect("sheet");
        let hdc = device.hdc();
        device.start_page(0).expect("start");
        draw_rgba(hdc, pixels, 0.0, 0.0, 40.0, 40.0);
        device.end_page(0).expect("end");
        let pages = device.into_pages();
        pages.first().expect("one page").1.clone()
    }

    #[test]
    fn a_transparent_picture_keeps_the_paper_white() {
        let clear = RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 0, 0]));
        let sheet = paint(&clear);

        // Blue first: the sheet is read back in texture order.
        assert_eq!(sheet.get_pixel(20, 20).0, [255, 255, 255, 255]);
    }

    #[test]
    fn a_picture_keeps_its_colours_when_scaled() {
        let red = RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
        let sheet = paint(&red);

        let pixel = sheet.get_pixel(20, 20).0;
        assert_eq!(pixel, [0, 0, 255, 255], "a red picture prints red");
    }

    /// Two columns and two rows of twenty pixels, with `cells` placed on it.
    fn sheet(cells: Vec<(u32, u32, usize, &str)>) -> excel::Sheet {
        let mut rows: Vec<excel::SheetRow> = Vec::new();
        let mut cells = cells;
        cells.sort_by_key(|(row, column, _, _)| (*row, *column));
        for (row, column, style, text) in cells {
            let cell = excel::Cell {
                column,
                text: text.to_string(),
                style,
                numeric: false,
            };
            match rows.last_mut() {
                Some(entry) if entry.index == row => entry.cells.push(cell),
                _ => rows.push(excel::SheetRow {
                    index: row,
                    cells: vec![cell],
                }),
            }
        }
        excel::Sheet {
            name: String::from("Sheet1"),
            column_lefts: vec![0.0, 20.0, 40.0],
            row_tops: vec![0.0, 20.0, 40.0],
            rows,
            merges: Vec::new(),
        }
    }

    /// Paint the whole sheet as one page, at one device pixel per sheet pixel.
    fn sheet_page(sheet: &excel::Sheet, styles: &[excel::CellStyle]) -> RgbaImage {
        let geometry = SheetGeometry::without_margins(40, 40);
        let mut device = Device::sheet(geometry).expect("sheet");
        let hdc = device.hdc();
        device.start_page(0).expect("start");
        let mut fonts = Fonts::new(hdc, 96.0);
        let mut brushes = Brushes::default();
        draw_sheet_page(
            hdc,
            &mut fonts,
            &mut brushes,
            sheet,
            styles,
            0.0,
            0.0,
            1.0,
            (0, 2),
            (0, 2),
        );
        device.end_page(0).expect("end");
        let pages = device.into_pages();
        pages.first().expect("one page").1.clone()
    }

    #[test]
    fn a_shaded_cell_without_text_prints_its_fill() {
        let styles = vec![
            excel::CellStyle::default(),
            excel::CellStyle {
                fill: Some(0xFF0000),
                ..Default::default()
            },
        ];
        let sheet = sheet(vec![(0, 0, 1, ""), (1, 1, 0, "值")]);
        let page = sheet_page(&sheet, &styles);

        // Texture order: blue first, so red reads back as [0, 0, 255, 255].
        assert_eq!(page.get_pixel(5, 5).0, [0, 0, 255, 255], "the fill is painted");
        assert_eq!(page.get_pixel(30, 5).0, [255, 255, 255, 255], "its neighbour is not");
    }

    #[test]
    fn a_sheet_without_borders_still_prints_its_grid() {
        let styles = vec![excel::CellStyle::default()];
        let sheet = sheet(vec![(0, 0, 0, "值")]);
        let page = sheet_page(&sheet, &styles);

        let grid = [0xD9u8, 0xD9, 0xD9, 255];
        assert_eq!(page.get_pixel(30, 30).0, [255, 255, 255, 255], "cells stay white");
        assert_eq!(page.get_pixel(30, 20).0, grid, "the row line runs to the right edge");
        assert_eq!(page.get_pixel(20, 30).0, grid, "the column line runs down the page");
        assert_eq!(page.get_pixel(30, 0).0, grid, "the top edge is ruled");
    }

    /// Print a picture on a 300 × 300 sheet and read the pages back. At 96 dpi
    /// the margins are 48 px, so the printable area is the 204 px square in the
    /// middle.
    fn print_pictures(pixels: &RgbaImage, print: ImagePrint) -> Vec<RgbaImage> {
        let geometry = SheetGeometry::without_margins(300, 300);
        let mut device = Device::sheet(geometry).expect("sheet");
        print_image(&mut device, pixels, 300, 300, 96, 96, &PageRange::All, print).expect("print");
        device
            .into_pages()
            .into_iter()
            .map(|(_, page)| page)
            .collect()
    }

    /// The first page of a picture printed at `scale`.
    fn print_picture(pixels: &RgbaImage, scale: ImageScale) -> RgbaImage {
        print_pictures(
            pixels,
            ImagePrint {
                scale,
                ..Default::default()
            },
        )
        .into_iter()
        .next()
        .expect("one page")
    }

    fn red_square() -> RgbaImage {
        RgbaImage::from_pixel(10, 10, image::Rgba([255, 0, 0, 255]))
    }

    /// Red on the left half, blue on the right, to see which part is printed.
    fn two_halves() -> RgbaImage {
        RgbaImage::from_fn(20, 20, |x, _| {
            if x < 10 {
                image::Rgba([255, 0, 0, 255])
            } else {
                image::Rgba([0, 0, 255, 255])
            }
        })
    }

    // Texture order: blue first, so red reads back as [0, 0, 255, 255] and
    // blue as [255, 0, 0, 255].
    const RED: [u8; 4] = [0, 0, 255, 255];
    const BLUE: [u8; 4] = [255, 0, 0, 255];
    const PAPER: [u8; 4] = [255, 255, 255, 255];

    #[test]
    fn a_picture_at_a_hundred_percent_fills_the_printable_area() {
        let page = print_picture(&red_square(), ImageScale::default());

        assert_eq!(page.get_pixel(150, 150).0, RED, "the middle is painted");
        assert_eq!(page.get_pixel(50, 150).0, RED, "so is the printable edge");
        assert_eq!(page.get_pixel(20, 150).0, PAPER, "the margin is not");
    }

    #[test]
    fn a_smaller_picture_sits_in_the_middle_of_a_white_page() {
        let page = print_picture(&red_square(), ImageScale::ALL[1]);

        assert_eq!(page.get_pixel(150, 150).0, RED, "the middle is painted");
        assert_eq!(page.get_pixel(60, 150).0, PAPER, "half size leaves a border");
    }

    #[test]
    fn an_enlarged_picture_stops_at_the_printable_edge() {
        let page = print_picture(&red_square(), ImageScale::ALL[6]);

        assert_eq!(page.get_pixel(150, 150).0, RED, "the middle is painted");
        assert_eq!(
            page.get_pixel(20, 150).0,
            PAPER,
            "an oversized picture is cut at the margin instead of bleeding into it"
        );
    }

    #[test]
    fn a_scaled_picture_is_still_one_page() {
        for scale in ImageScale::ALL {
            let pages = print_pictures(
                &red_square(),
                ImagePrint {
                    scale,
                    ..Default::default()
                },
            );
            assert_eq!(pages.len(), 1, "{scale:?} 不该多出页来");
        }
    }

    #[test]
    fn the_anchor_picks_which_part_of_an_enlarged_picture_is_printed() {
        // Twice the paper: the printable area shows exactly half the picture,
        // and the anchor says which half.
        let at = |x: f32| {
            print_picture_anchored(ImagePrint {
                scale: ImageScale::ALL[5],
                anchor: crate::print::ImageAnchor { x, y: 0.5 },
                poster: false,
            })
        };

        assert_eq!(at(0.0).get_pixel(150, 150).0, RED, "the left edge shows the left half");
        assert_eq!(at(1.0).get_pixel(150, 150).0, BLUE, "the right edge shows the right half");
        // 居中时接缝正好落在纸的中间，往左一点还是红的左半，往右一点是右半。
        let centered = at(0.5);
        assert_eq!(centered.get_pixel(100, 150).0, RED, "左半边");
        assert_eq!(centered.get_pixel(200, 150).0, BLUE, "右半边");
    }

    fn print_picture_anchored(print: ImagePrint) -> RgbaImage {
        print_pictures(&two_halves(), print)
            .into_iter()
            .next()
            .expect("one page")
    }

    #[test]
    fn the_anchor_does_not_move_a_picture_that_already_fits() {
        // At full size there is nothing outside the paper to move in, so the
        // picture stays centred whatever the anchor says.
        let print = ImagePrint {
            anchor: crate::print::ImageAnchor { x: 0.0, y: 0.0 },
            ..Default::default()
        };
        let page = print_pictures(&two_halves(), print)
            .into_iter()
            .next()
            .expect("one page");
        // 铺满整页：左半在左，右半在右，接缝在正中。
        assert_eq!(page.get_pixel(80, 150).0, RED, "左半边");
        assert_eq!(page.get_pixel(220, 150).0, BLUE, "右半边");
    }

    #[test]
    fn a_poster_print_splits_the_picture_across_pages() {
        // Three times the paper: a 3 by 3 grid of pages, each painted right to
        // its edge, and the whole picture is covered.
        let pages = print_pictures(
            &red_square(),
            ImagePrint {
                scale: ImageScale::ALL[6],
                poster: true,
                ..Default::default()
            },
        );

        assert_eq!(pages.len(), 9, "300% is three pages across and three down");
        for (index, page) in pages.iter().enumerate() {
            assert_eq!(page.get_pixel(150, 150).0, RED, "第 {index} 页是空的");
            assert_eq!(page.get_pixel(50, 50).0, RED, "第 {index} 页的角没有铺满");
        }
    }

    #[test]
    fn a_poster_print_can_be_printed_one_page_at_a_time() {
        let geometry = SheetGeometry::without_margins(300, 300);
        let mut device = Device::sheet(geometry).expect("sheet");
        let print = ImagePrint {
            scale: ImageScale::ALL[6],
            poster: true,
            ..Default::default()
        };
        let outcome = print_image(
            &mut device,
            &red_square(),
            300,
            300,
            96,
            96,
            &PageRange::Pages(vec![(2, 2)]),
            print,
        )
        .expect("print");

        assert_eq!(outcome.summary.total, 9, "the job still knows how many pages it has");
        assert_eq!(outcome.summary.printed, 1, "but only the page that was asked for is painted");
        assert_eq!(device.into_pages().len(), 1);
    }
}
