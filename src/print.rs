//! Printing the open document through the system print dialog.
//!
//! Text is drawn with GDI text output and PDF pages are rasterized at the
//! printer's resolution, so the spooler receives device-quality pages instead
//! of a screenshot of the window.

use std::sync::Arc;

use image::RgbaImage;

use crate::excel::Workbook;
use crate::pdf::PdfSource;
use crate::word::WordDocument;

/// What to print.
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
pub struct PrintJob {
    pub title: String,
    pub content: PrintContent,
}

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
    use super::{PageRange, PrintContent, PrintJob, PrintOwner, PrintSummary};

    use std::ffi::c_void;
    use std::mem::size_of;

    use windows::Win32::Foundation::{COLORREF, GlobalFree, HGLOBAL, SIZE};
    use windows::Win32::Graphics::Gdi::{
        BITMAPINFO, BITMAPINFOHEADER, CLIP_DEFAULT_PRECIS, CreateFontW, DEFAULT_CHARSET,
        DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDeviceCaps, GetTextExtentExPointW,
        GetTextExtentPoint32W, HDC, HFONT, HGDIOBJ, HORZRES, LOGPIXELSX, LOGPIXELSY,
        OUT_DEFAULT_PRECIS, PROOF_QUALITY, SRCCOPY, SelectObject, SetBkMode, SetTextColor,
        StretchDIBits, TRANSPARENT, TextOutW, VERTRES,
    };
    use windows::Win32::Storage::Xps::{AbortDoc, DOCINFOW, EndDoc, EndPage, StartDocW, StartPage};
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
            PrintContent::Image { .. } => Some(1),
            _ => None,
        };
        let Some((hdc, range)) = open_dialog(owner, known_pages)? else {
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

        let result = match &job.content {
            PrintContent::Text { text, font_pt } => {
                print_text(hdc, text, *font_pt, width, height, dpi_x, dpi_y, &range)
            }
            PrintContent::Pdf(source) => {
                print_pdf(hdc, source, width, height, dpi_x, dpi_y, &range)
            }
            PrintContent::Word(document) => {
                crate::print_office::print_word(hdc, document, width, height, dpi_x, dpi_y, &range)
            }
            PrintContent::Excel(workbook) => {
                crate::print_office::print_excel(hdc, workbook, width, height, dpi_x, dpi_y, &range)
            }
            PrintContent::Image { pixels } => {
                crate::print_office::print_image(hdc, pixels, width, height, dpi_x, dpi_y, &range)
            }
        };

        match result {
            Ok(summary) => {
                if unsafe { EndDoc(hdc) } <= 0 {
                    return Err(String::from("打印作业提交失败"));
                }
                Ok(Some(summary))
            }
            Err(message) => {
                unsafe { AbortDoc(hdc) };
                Err(message)
            }
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
        free_dialog_handles(&dialog);
        if hdc.0.is_null() {
            // Cancelled by the user.
            return Ok(None);
        }

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
    #[allow(clippy::too_many_arguments)]
    fn print_text(
        hdc: HDC,
        text: &str,
        font_pt: f32,
        width: i32,
        height: i32,
        dpi_x: i32,
        dpi_y: i32,
        range: &PageRange,
    ) -> Result<PrintSummary, String> {
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
        };

        for line in text.split('\n') {
            let line = line.strip_suffix('\r').unwrap_or(line);
            let utf16: Vec<u16> = line.replace('\t', "    ").encode_utf16().collect();
            if utf16.is_empty() {
                pager.row(&[])?;
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
                pager.row(&rest[..count])?;
                start += count;
            }
        }

        let mut summary = pager.finish()?;
        if summary.total == 0 && range.includes(0) {
            blank_page(hdc)?;
            summary = PrintSummary {
                printed: 1,
                total: 1,
            };
        }
        Ok(summary)
    }

    /// Writes rows down the printable area and starts a new page when it fills.
    struct Pager<'a> {
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
    }

    impl Pager<'_> {
        fn row(&mut self, chars: &[u16]) -> Result<(), String> {
            let next = self.y + self.line_height;
            if !self.open || next > self.bottom {
                self.close()?;
                self.begin()?;
                self.y = self.top;
            }

            if self.drawing && !chars.is_empty() {
                let _ = unsafe { TextOutW(self.hdc, self.left, self.y, chars) };
            }
            self.y += self.line_height;
            Ok(())
        }

        /// Lay out the next page, opening it on the printer only when selected.
        fn begin(&mut self) -> Result<(), String> {
            self.open = true;
            self.drawing = self.range.includes(self.index);
            self.index += 1;
            if self.drawing {
                if unsafe { StartPage(self.hdc) } <= 0 {
                    return Err(String::from("无法开始打印页面"));
                }
                self.pages += 1;
            }
            Ok(())
        }

        fn close(&mut self) -> Result<(), String> {
            if !self.open {
                return Ok(());
            }
            self.open = false;
            if self.drawing {
                self.drawing = false;
                if unsafe { EndPage(self.hdc) } <= 0 {
                    return Err(String::from("写入打印页面失败"));
                }
            }
            Ok(())
        }

        fn finish(mut self) -> Result<PrintSummary, String> {
            self.close()?;
            Ok(PrintSummary {
                printed: self.pages,
                total: self.index,
            })
        }
    }

    /// PDF pages are rendered at the printer's own resolution and stretched onto
    /// the printable area, so text and images stay sharp. Pages outside `range`
    /// are skipped without being rendered.
    fn print_pdf(
        hdc: HDC,
        source: &crate::pdf::PdfSource,
        width: i32,
        height: i32,
        dpi_x: i32,
        dpi_y: i32,
        range: &PageRange,
    ) -> Result<PrintSummary, String> {
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

            if unsafe { StartPage(hdc) } <= 0 {
                return Err(String::from("无法开始打印页面"));
            }

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

            if unsafe { EndPage(hdc) } <= 0 {
                return Err(String::from("写入打印页面失败"));
            }
            pages += 1;
        }

        if total == 0 && range.includes(0) {
            pages = blank_page(hdc)?;
            return Ok(PrintSummary {
                printed: pages,
                total: pages,
            });
        }
        Ok(PrintSummary {
            printed: pages,
            total,
        })
    }

    /// Emit one empty page so a document with no content still produces output.
    fn blank_page(hdc: HDC) -> Result<usize, String> {
        if unsafe { StartPage(hdc) } <= 0 {
            return Err(String::from("无法开始打印页面"));
        }
        if unsafe { EndPage(hdc) } <= 0 {
            return Err(String::from("写入打印页面失败"));
        }
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
