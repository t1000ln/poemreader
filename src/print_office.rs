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
    GetTextExtentExPointW, GetTextExtentPoint32W, HBRUSH, HDC, HFONT, HGDIOBJ, IntersectClipRect,
    OUT_DEFAULT_PRECIS, RestoreDC, SRCCOPY, SaveDC, SelectObject, SetBkMode, SetTextColor,
    StretchDIBits, TRANSPARENT, TextOutW,
};
use windows::Win32::Storage::Xps::{EndPage, StartPage};
use windows::core::PCWSTR;

use crate::excel::{self, Workbook};
use crate::print::{PageRange, PrintSummary};
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
const MARGIN_INCHES: f32 = 0.5;
/// Space between a table cell's border and its text, in points.
const CELL_PADDING_PT: f32 = 4.0;
const BOLD_WEIGHT: i32 = 700;
const NORMAL_WEIGHT: i32 = 400;
/// Colour of table borders.
const LINE_COLOR: u32 = 0x808080;

// ------------------------------------------------------------------ documents

/// Print a Word document at the page width declared in the file.
pub fn print_word(
    hdc: HDC,
    document: &word::WordDocument,
    width: i32,
    height: i32,
    dpi_x: i32,
    dpi_y: i32,
    range: &PageRange,
) -> Result<PrintSummary, String> {
    let _drawing = DrawingGuard;
    let margin_x = dpi_x as f32 * MARGIN_INCHES;
    let margin_y = dpi_y as f32 * MARGIN_INCHES;
    let printable_width = width as f32 - 2.0 * margin_x;
    let printable_height = height as f32 - 2.0 * margin_y;
    if printable_width < 72.0 || printable_height < 72.0 {
        return Err(String::from("打印机没有足够的可打印区域"));
    }

    // Points to device pixels, scaled down when the page is wider than paper.
    let natural = dpi_x as f32 / 72.0;
    let fit = (printable_width / (document.content_width * natural)).min(1.0);
    let scale = natural * fit;
    let column_width = document.content_width * scale;
    let left = margin_x + (printable_width - column_width) * 0.5;

    let mut fonts = Fonts::new(hdc, dpi_y as f32);
    let mut brushes = Brushes::default();
    let mut pages = Pages::new(hdc, margin_y, margin_y + printable_height, range.clone());
    unsafe {
        SetBkMode(hdc, TRANSPARENT);
    }

    let mut y = margin_y;
    for block in &document.blocks {
        match block {
            word::Block::Paragraph(paragraph) => {
                let layout = layout_paragraph(hdc, &mut fonts, paragraph, column_width, scale);
                draw_layout(hdc, &layout, left, &mut y, column_width, Some(&mut pages))?;
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
        }
    }

    let mut summary = pages.finish()?;
    if summary.total == 0 && range.includes(0) {
        blank_page(hdc)?;
        summary = PrintSummary {
            printed: 1,
            total: 1,
        };
    }
    Ok(summary)
}

/// Print every worksheet, paginating along column and row boundaries.
pub fn print_excel(
    hdc: HDC,
    workbook: &Workbook,
    width: i32,
    height: i32,
    dpi_x: i32,
    dpi_y: i32,
    range: &PageRange,
) -> Result<PrintSummary, String> {
    let _drawing = DrawingGuard;
    let margin_x = dpi_x as f32 * MARGIN_INCHES;
    let margin_y = dpi_y as f32 * MARGIN_INCHES;
    let printable_width = width as f32 - 2.0 * margin_x;
    let printable_height = height as f32 - 2.0 * margin_y;
    if printable_width < 72.0 || printable_height < 72.0 {
        return Err(String::from("打印机没有足够的可打印区域"));
    }

    // Cell geometry is in 96 dpi pixels; scale it to the printer's resolution.
    let scale = dpi_x as f32 / 96.0;
    let page_width = printable_width / scale;
    let page_height = printable_height / scale;

    let mut fonts = Fonts::new(hdc, dpi_y as f32);
    let mut brushes = Brushes::default();
    let mut pages = Pages::new(hdc, margin_y, margin_y + printable_height, range.clone());
    unsafe {
        SetBkMode(hdc, TRANSPARENT);
    }

    for sheet in &workbook.sheets {
        let columns = page_breaks(&sheet.column_lefts, page_width);
        let rows = page_breaks(&sheet.row_tops, page_height);
        for (column_start, column_end) in &columns {
            for (row_start, row_end) in &rows {
                pages.new_page()?;
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

    let mut summary = pages.finish()?;
    if summary.total == 0 && range.includes(0) {
        blank_page(hdc)?;
        summary = PrintSummary {
            printed: 1,
            total: 1,
        };
    }
    Ok(summary)
}

fn blank_page(hdc: HDC) -> Result<(), String> {
    start_page(hdc)?;
    end_page(hdc)
}

fn start_page(hdc: HDC) -> Result<(), String> {
    if unsafe { StartPage(hdc) } <= 0 {
        return Err(String::from("无法开始打印页面"));
    }
    Ok(())
}

fn end_page(hdc: HDC) -> Result<(), String> {
    if unsafe { EndPage(hdc) } <= 0 {
        return Err(String::from("写入打印页面失败"));
    }
    Ok(())
}

// --------------------------------------------------------------------- pages

/// Opens pages as content fills them.
///
/// Every page is laid out, but only the pages selected by `range` are opened on
/// the printer; the others advance the layout with drawing switched off.
struct Pages {
    hdc: HDC,
    top: f32,
    bottom: f32,
    open: bool,
    drawing: bool,
    /// Pages laid out so far.
    index: usize,
    /// Pages sent to the printer.
    printed: usize,
    range: PageRange,
}

impl Pages {
    fn new(hdc: HDC, top: f32, bottom: f32, range: PageRange) -> Self {
        Self {
            hdc,
            top,
            bottom,
            open: false,
            drawing: false,
            index: 0,
            printed: 0,
            range,
        }
    }

    fn new_page(&mut self) -> Result<(), String> {
        self.close()?;
        self.open = true;
        self.drawing = self.range.includes(self.index);
        self.index += 1;
        if self.drawing {
            start_page(self.hdc)?;
            self.printed += 1;
        }
        set_drawing(self.drawing);
        Ok(())
    }

    fn close(&mut self) -> Result<(), String> {
        if !self.open {
            return Ok(());
        }
        self.open = false;
        if self.drawing {
            self.drawing = false;
            set_drawing(true);
            end_page(self.hdc)?;
        }
        Ok(())
    }

    /// Make sure `height` fits below `y`, starting a page when needed.
    ///
    /// A block taller than the page keeps its own page instead of being pushed
    /// to a fresh one for ever.
    fn ensure(&mut self, y: &mut f32, height: f32) -> Result<(), String> {
        if !self.open {
            self.new_page()?;
            *y = self.top;
            return Ok(());
        }
        if *y > self.top + 0.5 && *y + height > self.bottom && height < self.height() {
            self.new_page()?;
            *y = self.top;
        }
        Ok(())
    }

    fn height(&self) -> f32 {
        self.bottom - self.top
    }

    fn finish(&mut self) -> Result<PrintSummary, String> {
        self.close()?;
        Ok(PrintSummary {
            printed: self.printed,
            total: self.index,
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
        self.items.push(Item::Image {
            pixels: image.pixels.clone(),
            width: (image.width * scale).max(1.0),
            height: (image.height * scale).max(1.0),
        });
    }
}

fn draw_layout(
    hdc: HDC,
    layout: &ParagraphLayout,
    x: f32,
    y: &mut f32,
    width: f32,
    mut pages: Option<&mut Pages>,
) -> Result<(), String> {
    *y += layout.space_before;

    for item in &layout.items {
        let content_width = (width - layout.indent).max(0.0);
        match item {
            Item::Line { line, indent } => {
                if let Some(pages) = pages.as_deref_mut() {
                    pages.ensure(y, layout.line_height)?;
                }
                let offset = alignment_offset(layout.align, content_width, line.width);
                let mut cursor = x + layout.indent + indent + offset;
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
            }
            Item::Image {
                pixels,
                width: image_width,
                height: image_height,
            } => {
                if let Some(pages) = pages.as_deref_mut() {
                    pages.ensure(y, *image_height)?;
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
    Ok(())
}

fn alignment_offset(align: word::Align, width: f32, content: f32) -> f32 {
    match align {
        word::Align::Center => ((width - content) * 0.5).max(0.0),
        word::Align::Right => (width - content).max(0.0),
        _ => 0.0,
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
) -> Result<(), String> {
    let widths = column_widths(table, available, scale);
    if widths.is_empty() {
        return Ok(());
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
        pages.ensure(y, row_height)?;

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
                draw_layout(
                    hdc,
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

    Ok(())
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
    if !drawing() {
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

fn draw_rgba(hdc: HDC, image: &RgbaImage, x: f32, y: f32, width: f32, height: f32) {
    let (source_width, source_height) = image.dimensions();
    if !drawing() || source_width == 0 || source_height == 0 || width < 1.0 || height < 1.0 {
        return;
    }

    // `StretchDIBits` expects BGRA with rows from the top down.
    let mut data = image.as_raw().clone();
    for pixel in data.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }

    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: source_width as i32,
            biHeight: -(source_height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0, // BI_RGB
            ..Default::default()
        },
        ..Default::default()
    };

    let _ = unsafe {
        StretchDIBits(
            hdc,
            x.round() as i32,
            y.round() as i32,
            width.round() as i32,
            height.round() as i32,
            0,
            0,
            source_width as i32,
            source_height as i32,
            Some(data.as_ptr() as *const c_void),
            &info,
            DIB_RGB_COLORS,
            SRCCOPY,
        )
    };
}
