// GUI subsystem in every profile: no console window is created, in debug or release.
#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme as _, IconName, TitleBar, WindowExt as _,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    notification::Notification,
    scroll::{Scrollbar, ScrollbarMode},
    v_flex,
};
use gpui_kit::*;

mod cfb;
mod doc;
mod events;
mod excel;
mod loader;
mod markdown;
mod ooxml;
mod pdf;
mod picture;
mod print;
#[cfg(windows)]
mod print_office;
mod render;
mod settings;
mod shortcuts;
mod text;
mod window;
mod word;
mod xls;

use events::{CellSelection, Interaction, Selection};
use excel::Workbook;
use loader::{DocumentKind, LoadedContent, load_document};
use pdf::PdfView;
use picture::Picture;
use print::{PrintContent, PrintJob};
use render::excel::{ExcelRender, excel_cells};
use render::pdf::{PdfLayout, spawn_pdf_render};
use render::word::{WordRender, word_block};
use render::{LineText, SelectableLine};
use settings::{PointerMode, ReadingBackground, Store};
use text::TextView;
use word::WordDocument;

/// Text sizes offered in the 「字体大小」 menu.
const FONT_SIZES: [f32; 6] = [14., 16., 18., 20., 24., 28.];

/// Page zoom offered in the same menu while a PDF is open.
const PDF_ZOOMS: [f32; 5] = [0.75, 1.0, 1.25, 1.5, 2.0];

/// Space above the first page and around every page, in logical pixels.
const PDF_TOP_PADDING: f32 = 24.;
const PDF_PAGE_MARGIN: f32 = 26.;
const PDF_PAGE_GAP: f32 = 20.;
/// Pages kept rendered above and below the visible ones.
const PDF_OVERSCAN: usize = 1;

/// Space around a picture when it is fitted to the reading area, in logical
/// pixels.
const IMAGE_MARGIN: f32 = 26.0;
/// A picture is never blown up more than this when it is fitted to the window,
/// so a small icon does not fill the screen with a blurry square.
const IMAGE_MAX_UPSCALE: f32 = 4.0;
/// Range the wheel can zoom a picture to. It matches the range the settings
/// keep, so a zoomed picture is restored at the same size next launch.
const IMAGE_ZOOM_MIN: f32 = 0.5;
const IMAGE_ZOOM_MAX: f32 = 4.0;
/// How much one wheel line changes the zoom of a picture.
const IMAGE_ZOOM_PER_LINE: f32 = 1.05;

/// Padding and line spacing of the text body, in logical pixels. They are also
/// used when placing the selection, so both must stay in one place.
const TEXT_PADDING_X: f32 = 40.;
const TEXT_PADDING_Y: f32 = 28.;
const TEXT_MAX_WIDTH: f32 = 880.;
/// Height of a text row as a multiple of the font size.
const LINE_HEIGHT_RATIO: f32 = 1.7;

/// How long the window has to stop resizing before the text is wrapped again.
const REWRAP_DEBOUNCE: Duration = Duration::from_millis(120);

/// How long scrolling must stay idle before the position is written to disk.
const SAVE_POSITION_DEBOUNCE: Duration = Duration::from_millis(250);

/// The app icon, also embedded in the executable by `build.rs`.
const APP_ICON: &[u8] = include_bytes!("../assets/icon.svg");

fn main() {
    window::run();
}

pub(crate) fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// The body of a loaded document.
pub(crate) enum DocumentContent {
    Text(TextView),
    Pdf(PdfView),
    Word(Arc<WordDocument>),
    Excel(Arc<Workbook>),
    Markdown(Arc<WordDocument>),
    Image(Arc<Picture>),
}

/// A loaded document.
struct Document {
    path: PathBuf,
    file_name: SharedString,
    content: DocumentContent,
}

impl Document {
    fn new(
        path: PathBuf,
        content: LoadedContent,
        font: &Font,
        font_size: Pixels,
        line_height: f32,
        wrap_width: f32,
        text_system: &Arc<TextSystem>,
    ) -> Self {
        let content = match content {
            LoadedContent::Text(text) => DocumentContent::Text(TextView::new(
                Arc::new(text),
                font,
                font_size,
                line_height,
                wrap_width,
                text_system,
            )),
            LoadedContent::Pdf(source) => DocumentContent::Pdf(PdfView::new(source)),
            LoadedContent::Word(document) => DocumentContent::Word(document),
            LoadedContent::Excel(workbook) => DocumentContent::Excel(workbook),
            LoadedContent::Markdown(document) => DocumentContent::Markdown(document),
            LoadedContent::Image(picture) => DocumentContent::Image(picture),
        };

        Self {
            file_name: file_name_of(&path).into(),
            path,
            content,
        }
    }

    fn status_line(&self) -> String {
        match &self.content {
            DocumentContent::Text(text) => {
                format!("{} · 文本 · {} 行", self.file_name, text.line_count())
            }
            DocumentContent::Pdf(view) => {
                format!("{} · PDF · {} 页", self.file_name, view.page_count())
            }
            DocumentContent::Word(document) => {
                format!("{} · Word · {} 段", self.file_name, document.paragraphs)
            }
            DocumentContent::Markdown(document) => {
                format!("{} · Markdown · {} 段", self.file_name, document.paragraphs)
            }
            DocumentContent::Excel(workbook) => {
                format!(
                    "{} · Excel · {} 个工作表",
                    self.file_name,
                    workbook.sheets.len()
                )
            }
            DocumentContent::Image(picture) => {
                let frames = if picture.frame_count() > 1 {
                    format!(" · {} 帧", picture.frame_count())
                } else {
                    String::new()
                };
                format!(
                    "{} · {}{} · {} × {}",
                    self.file_name,
                    picture.format,
                    frames,
                    picture.natural_width.round() as u32,
                    picture.natural_height.round() as u32
                )
            }
        }
    }
}

impl ReadingBackground {
    const ALL: [ReadingBackground; 4] = [
        ReadingBackground::Default,
        ReadingBackground::Paper,
        ReadingBackground::Mint,
        ReadingBackground::Night,
    ];

    fn label(self) -> &'static str {
        match self {
            ReadingBackground::Default => "默认（跟随主题）",
            ReadingBackground::Paper => "米黄纸张",
            ReadingBackground::Mint => "淡绿护眼",
            ReadingBackground::Night => "深色夜间",
        }
    }

    fn background(self, cx: &App) -> Hsla {
        match self {
            ReadingBackground::Default => cx.theme().background,
            ReadingBackground::Paper => rgb(0xF6EFE0).into(),
            ReadingBackground::Mint => rgb(0xE8F3E9).into(),
            ReadingBackground::Night => rgb(0x1B1E24).into(),
        }
    }

    fn foreground(self, cx: &App) -> Hsla {
        match self {
            ReadingBackground::Default => cx.theme().foreground,
            ReadingBackground::Paper => rgb(0x3A3226).into(),
            ReadingBackground::Mint => rgb(0x24382C).into(),
            ReadingBackground::Night => rgb(0xE6E2DA).into(),
        }
    }
}

/// The scroll position to apply once the scroll area has been laid out.
#[derive(Clone, Copy)]
enum PendingScroll {
    /// A raw scroll offset, used by text documents.
    Offset(f32),
    /// A `page + fraction` anchor, used by PDF documents.
    Anchor(f32),
}

/// A position in the text body: a byte offset within one line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TextPos {
    line: usize,
    byte: usize,
}

/// 阅读器视图。
///
/// 字段对 crate 内可见：交互状态由 `events` 模块直接改写，渲染与加载留在
/// 本文件，两边共用同一份状态，避免为每个字段再写一对访问器。
pub(crate) struct PoemReader {
    document: Option<Document>,
    font_size: Pixels,
    pdf_zoom: f32,
    background: ReadingBackground,
    /// What the left button does in the reading area.
    pub(crate) pointer_mode: PointerMode,
    /// Sheet of the open Excel workbook being shown.
    excel_sheet: usize,
    /// Currently selected text, kept after the drag ends so it can be copied.
    pub(crate) selection: Option<Selection>,
    /// Lines of a Word document that can be selected, rebuilt on every render.
    document_lines: Vec<SelectableLine>,
    /// Cells of the current sheet that are selected.
    pub(crate) cell_selection: Option<CellSelection>,
    /// Bounds of the cells drawn in the last frame, with their sheet
    /// coordinates, shared with the prepaint listener of the sheet.
    cell_bounds: Rc<RefCell<Vec<(u32, u32, Bounds<Pixels>)>>>,
    /// Layout of the text rows currently rendered, for hit testing and
    /// selection. Only the rows on screen are kept.
    row_layouts: HashMap<usize, Rc<RefCell<Option<TextLayout>>>>,
    load_task: Option<Task<()>>,
    save_task: Option<Task<()>>,
    /// Pending re-wrap after the window width settled.
    rewrap_task: Option<Task<()>>,
    /// Re-wraps the text when the window is resized.
    bounds_subscription: Option<Subscription>,
    pub(crate) scroll_handle: ScrollHandle,
    /// Scroll position to apply on the next render.
    pending_scroll: Option<PendingScroll>,
    /// Set while the left button is down over the reading area.
    pub(crate) interaction: Option<Interaction>,
    /// Latest reading position, readable after the view is gone.
    latest_position: Rc<Cell<Option<f32>>>,
    pub(crate) store: Rc<RefCell<Store>>,
    _subscriptions: Vec<Subscription>,
}

impl PoemReader {
    pub(crate) fn new(
        store: Rc<RefCell<Store>>,
        latest_position: Rc<Cell<Option<f32>>>,
        initial: Option<(PathBuf, DocumentKind, LoadedContent)>,
        font: Font,
        cx: &mut Context<Self>,
    ) -> Self {
        let (font_size, pdf_zoom, background, pointer_mode) = {
            let store = store.borrow();
            let settings = store.settings();
            (
                px(settings.font_size()),
                settings.pdf_zoom(),
                settings.background,
                settings.pointer_mode,
            )
        };

        let (document, pending_scroll) = match initial {
            Some((path, kind, content)) => {
                let stored = store.borrow().position_for(&path);
                // Remember it so the next launch reopens this file even when no
                // path was given on the command line.
                store.borrow_mut().set_last_file(&path);
                store.borrow().save();

                let pending = match (kind, stored) {
                    (DocumentKind::Pdf, Some(anchor)) => Some(PendingScroll::Anchor(anchor)),
                    (DocumentKind::Pdf, None) => Some(PendingScroll::Anchor(0.0)),
                    // Still reset the scroll area, which may be left over from
                    // a previous document.
                    (_, Some(offset)) => Some(PendingScroll::Offset(offset)),
                    (_, None) => Some(PendingScroll::Offset(0.0)),
                };
                let document = Document::new(
                    path,
                    content,
                    &font,
                    font_size,
                    font_size.as_f32() * LINE_HEIGHT_RATIO,
                    text_wrap_width(TEXT_MAX_WIDTH),
                    cx.text_system(),
                );
                (Some(document), pending)
            }
            None => (None, None),
        };

        let quit_subscription = {
            let store = store.clone();
            cx.on_app_quit(move |this, _cx| {
                this.remember_position();
                let store = store.clone();
                async move {
                    store.borrow().save();
                }
            })
        };

        // Window-wide shortcuts (copy, print) live in `shortcuts.rs`; the
        // returned subscription has to be kept alive by the view.
        let shortcut_subscription = Self::bind_shortcuts(cx);

        Self {
            document,
            font_size,
            pdf_zoom,
            background,
            pointer_mode,
            excel_sheet: 0,
            selection: None,
            document_lines: Vec::new(),
            cell_selection: None,
            cell_bounds: Rc::new(RefCell::new(Vec::new())),
            row_layouts: HashMap::new(),
            load_task: None,
            save_task: None,
            rewrap_task: None,
            bounds_subscription: None,
            scroll_handle: ScrollHandle::new(),
            pending_scroll,
            interaction: None,
            latest_position,
            store,
            _subscriptions: vec![quit_subscription, shortcut_subscription],
        }
    }

    pub(crate) fn persist_settings(&self) {
        self.store.borrow().save();
    }

    /// Store the current reading position for the open document.
    fn remember_position(&self) {
        let Some(document) = self.document.as_ref() else {
            return;
        };
        let Some(position) = self.latest_position.get() else {
            return;
        };
        self.store
            .borrow_mut()
            .set_position(&document.path, position);
    }

    /// Save the position once scrolling settles, cancelling the previous timer.
    pub(crate) fn schedule_position_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(SAVE_POSITION_DEBOUNCE).await;
            this.update_in(cx, |this, _window, _cx| {
                this.remember_position();
                this.persist_settings();
            })
            .ok();
        }));
    }

    fn is_pdf(&self) -> bool {
        matches!(
            self.document.as_ref().map(|document| &document.content),
            Some(DocumentContent::Pdf(_))
        )
    }

    fn is_text(&self) -> bool {
        matches!(
            self.document.as_ref().map(|document| &document.content),
            Some(DocumentContent::Text(_))
        )
    }

    /// Whether the open document is a sheet, which is selected by cell rather
    /// than by byte offset.
    pub(crate) fn is_excel(&self) -> bool {
        matches!(
            self.document.as_ref().map(|document| &document.content),
            Some(DocumentContent::Excel(_))
        )
    }

    /// Map a pointer position to the line and byte offset under it.
    pub(crate) fn text_position_at(&self, point: Point<Pixels>) -> Option<TextPos> {
        match self.document.as_ref().map(|document| &document.content) {
            Some(DocumentContent::Text(text)) => self.text_row_at(text, point),
            Some(DocumentContent::Word(_)) | Some(DocumentContent::Markdown(_)) => {
                self.document_line_at(point)
            }
            _ => None,
        }
    }

    /// Map a pointer position to a row of a plain-text document.
    fn text_row_at(&self, text: &TextView, point: Point<Pixels>) -> Option<TextPos> {
        // Only the rows on screen were laid out, which is all the pointer can
        // reach without scrolling.
        let mut entries = self
            .row_layouts
            .iter()
            .filter_map(|(index, slot)| {
                slot.borrow()
                    .as_ref()
                    .cloned()
                    .map(|layout| (*index, layout))
            })
            .collect::<Vec<_>>();
        if entries.is_empty() {
            return None;
        }
        entries.sort_by_key(|(index, _)| *index);

        // Rows are laid out top to bottom: take the last one that starts above
        // the pointer, so gaps and blank space below the text still resolve.
        let mut chosen = entries[0].0;
        for (index, layout) in &entries {
            if point.y < layout.bounds().top() {
                break;
            }
            chosen = *index;
            if point.y <= layout.bounds().bottom() {
                break;
            }
        }

        let layout = entries
            .iter()
            .find(|(index, _)| *index == chosen)
            .map(|(_, layout)| layout.clone())?;
        let byte = layout
            .index_for_position(point)
            .unwrap_or_else(|index| index);

        let line = text.row_text(chosen);
        let mut byte = byte.min(line.len());
        while !line.is_char_boundary(byte) {
            byte -= 1;
        }

        Some(TextPos { line: chosen, byte })
    }

    /// Map a pointer position to a line of a Word document.
    fn document_line_at(&self, point: Point<Pixels>) -> Option<TextPos> {
        // The nearest line wins, measured in both directions: the cells of one
        // table row share a top edge, so the horizontal distance decides
        // between them, and clicks in the margins still resolve.
        let mut best: Option<(usize, f32)> = None;
        for (index, line) in self.document_lines.iter().enumerate() {
            let layout = line.layout.borrow();
            let Some(layout) = layout.as_ref() else {
                continue;
            };
            let bounds = layout.bounds();
            let dx = if point.x < bounds.left() {
                (bounds.left() - point.x).as_f32()
            } else if point.x > bounds.right() {
                (point.x - bounds.right()).as_f32()
            } else {
                0.0
            };
            let dy = if point.y < bounds.top() {
                (bounds.top() - point.y).as_f32()
            } else if point.y > bounds.bottom() {
                (point.y - bounds.bottom()).as_f32()
            } else {
                0.0
            };
            let distance = dx * dx + dy * dy;
            if best.is_none_or(|(_, previous)| distance < previous) {
                best = Some((index, distance));
            }
        }

        let (index, _) = best?;
        let line = self.document_lines.get(index)?;
        let layout = line.layout.borrow();
        let layout = layout.as_ref()?;
        let byte = layout
            .index_for_position(point)
            .unwrap_or_else(|index| index);
        let mut byte = byte.min(line.text.len());
        while !line.text.is_char_boundary(byte) {
            byte -= 1;
        }

        Some(TextPos { line: index, byte })
    }

    /// Map a pointer position to a cell of the open sheet.
    pub(crate) fn cell_at(&self, point: Point<Pixels>) -> Option<(u32, u32)> {
        let cells = self.cell_bounds.borrow();
        if cells.is_empty() {
            return None;
        }

        // Cells are recorded with the bounds they were last painted at, which
        // already include the scroll offset and the zoom.
        if let Some((row, column, _)) = cells.iter().find(|(_, _, bounds)| bounds.contains(&point))
        {
            return Some((*row, *column));
        }

        // Dragging past the edge of the sheet still selects the nearest cell.
        let distance = |bounds: &Bounds<Pixels>| {
            let dx = point.x - bounds.center().x;
            let dy = point.y - bounds.center().y;
            dx.as_f32() * dx.as_f32() + dy.as_f32() * dy.as_f32()
        };
        cells
            .iter()
            .min_by(|left, right| distance(&left.2).total_cmp(&distance(&right.2)))
            .map(|(row, column, _)| (*row, *column))
    }

    /// The selected text, ready for the clipboard.
    ///
    /// Rows that only continue a wrapped line are joined without a newline, so
    /// the copied text matches the file.
    fn selected_text(&self) -> Option<String> {
        if let Some(selection) = self.cell_selection {
            return self.cells_text(selection);
        }
        let selection = self.selection?;
        if selection.is_empty() {
            return None;
        }
        match self.document.as_ref().map(|document| &document.content) {
            Some(DocumentContent::Text(text)) => self.rows_text(text, selection),
            Some(DocumentContent::Word(_)) | Some(DocumentContent::Markdown(_)) => {
                self.lines_text(selection)
            }
            _ => None,
        }
    }

    /// Text of a selection in a plain-text document.
    fn rows_text(&self, text: &TextView, selection: Selection) -> Option<String> {
        let (start, end) = selection.ordered();
        let mut text_out = String::new();
        for index in start.line..=end.line {
            let line = text.row_text(index);
            if index > start.line {
                if index >= text.row_count() {
                    break;
                }
                if text.row_starts_line(index) {
                    text_out.push('\n');
                }
            }

            let from = if index == start.line {
                start.byte.min(line.len())
            } else {
                0
            };
            let to = if index == end.line {
                end.byte.min(line.len())
            } else {
                line.len()
            };
            if from < to {
                text_out.push_str(&line[from..to]);
            }
        }

        (!text_out.is_empty()).then_some(text_out)
    }

    /// Text of a selection in a Word document, taken from the lines the last
    /// render registered. The lines of one paragraph are joined without a
    /// separator, the cells of one table row with a tab, and everything else
    /// with a newline; the spaces standing in for a first-line indent are left
    /// out.
    fn lines_text(&self, selection: Selection) -> Option<String> {
        let (start, end) = selection.ordered();
        let mut text_out = String::new();
        let mut previous: Option<&SelectableLine> = None;
        for index in start.line..=end.line {
            let Some(line) = self.document_lines.get(index) else {
                break;
            };
            if let Some(previous) = previous {
                if previous.paragraph != line.paragraph {
                    if previous.row.is_some() && previous.row == line.row {
                        text_out.push('\t');
                    } else {
                        text_out.push('\n');
                    }
                }
            }
            previous = Some(line);

            let text = line.text.as_ref();
            let skip = line.skip.min(text.len());
            let from = if index == start.line {
                start.byte.clamp(skip, text.len())
            } else {
                skip
            };
            let to = if index == end.line {
                end.byte.min(text.len())
            } else {
                text.len()
            };
            if from < to && text.is_char_boundary(from) && text.is_char_boundary(to) {
                text_out.push_str(&text[from..to]);
            }
        }

        (!text_out.is_empty()).then_some(text_out)
    }

    /// Text of a cell selection, taken from the sheet model so cells that are
    /// scrolled out of sight are copied too. Cells are separated by tabs and
    /// rows by newlines, as spreadsheets are normally pasted.
    fn cells_text(&self, selection: CellSelection) -> Option<String> {
        let Some(DocumentContent::Excel(workbook)) =
            self.document.as_ref().map(|document| &document.content)
        else {
            return None;
        };
        let sheet = workbook.sheets.get(
            self.excel_sheet
                .min(workbook.sheets.len().saturating_sub(1)),
        )?;
        let ((top, left), (bottom, right)) = selection.ordered();

        let mut rows = Vec::new();
        for row in top..=bottom {
            // Trailing empty cells only add tabs, so each row ends with its
            // last cell that holds something.
            let last = (left..=right).rev().find(|column| {
                sheet
                    .cell(row, *column)
                    .is_some_and(|cell| !cell.text.is_empty())
            });
            let Some(last) = last else {
                continue;
            };
            let mut line = String::new();
            for column in left..=last {
                if column > left {
                    line.push('\t');
                }
                if let Some(cell) = sheet.cell(row, column) {
                    line.push_str(&cell.text);
                }
            }
            rows.push(line);
        }

        (!rows.is_empty()).then(|| rows.join("\n"))
    }

    /// Copy the selection to the clipboard; false when nothing was selected.
    pub(crate) fn copy_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(text) = self.selected_text() else {
            return false;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        true
    }

    /// Print the open document, showing the system print dialog first.
    pub(crate) fn print_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(job) = self.print_job() else {
            window.push_notification(Notification::info("还没有可打印的文件"), cx);
            return;
        };

        // Both the dialog and the spooling run off the UI thread: the dialog
        // waits for the user and long PDFs take a while to rasterize, and the
        // reader should stay responsive in the meantime.
        let owner = print::foreground_owner();
        cx.spawn_in(window, async move |view, cx| {
            let result = cx
                .background_spawn(async move { print::print(owner, job) })
                .await;

            view.update_in(cx, |_this, window, cx| match result {
                Ok(Some(summary)) => {
                    let notification = if summary.printed == 0 {
                        if summary.total == 0 {
                            Notification::info("文档没有可打印的内容")
                        } else {
                            Notification::info(format!(
                                "指定范围内没有可打印的页面（文档共 {} 页）",
                                summary.total
                            ))
                        }
                    } else if summary.printed < summary.total {
                        Notification::success(format!(
                            "已发送 {} 页到打印机（文档共 {} 页）",
                            summary.printed, summary.total
                        ))
                    } else {
                        Notification::success(format!("已发送 {} 页到打印机", summary.printed))
                    };
                    window.push_notification(notification, cx);
                }
                Ok(None) => {}
                Err(message) => window
                    .push_notification(Notification::error(format!("打印失败：{message}")), cx),
            })
            .ok();
        })
        .detach();
    }

    /// The open document as a printable job.
    fn print_job(&self) -> Option<PrintJob> {
        let document = self.document.as_ref()?;
        let content = match &document.content {
            DocumentContent::Text(text) => PrintContent::Text {
                text: text.source().clone(),
                font_pt: (self.font_size.as_f32() * 72.0 / 96.0).clamp(7.0, 20.0),
            },
            DocumentContent::Pdf(view) => PrintContent::Pdf(view.source()),
            DocumentContent::Word(word) | DocumentContent::Markdown(word) => {
                PrintContent::Word(word.clone())
            }
            DocumentContent::Excel(workbook) => PrintContent::Excel(workbook.clone()),
            DocumentContent::Image(picture) => PrintContent::Image {
                pixels: picture.pixels.clone(),
            },
        };

        Some(PrintJob {
            title: document.file_name.to_string(),
            content,
        })
    }

    /// Ask the platform for a file and open it.
    fn load_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("打开".into()),
        });

        // Nothing to track while the dialog is open, so the prompt runs
        // detached; the load it starts is tracked in `load_task`.
        cx.spawn_in(window, async move |view, cx| {
            let path = match receiver.await {
                Ok(Ok(Some(mut paths))) => match paths.pop() {
                    Some(path) => path,
                    None => return,
                },
                Ok(Ok(None)) => return,
                Ok(Err(error)) => {
                    let message = format!("无法打开文件选择对话框：{error}");
                    view.update_in(cx, |_, window, cx| {
                        window.push_notification(Notification::error(message), cx);
                    })
                    .ok();
                    return;
                }
                Err(_) => return,
            };

            view.update_in(cx, |this, window, cx| this.open_path(path, window, cx))
                .ok();
        })
        .detach();
    }

    /// Load the document at `path` in the background and show it.
    pub(crate) fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        // Parsing and rasterizing a PDF can take a moment, so keep it off the
        // UI thread.
        let read_path = path.clone();
        self.load_task = Some(cx.spawn_in(window, async move |view, cx| {
            let loaded = cx
                .background_spawn(async move { load_document(&read_path) })
                .await;

            match loaded {
                Ok((kind, content)) => {
                    let title = format!("PoemReader — {}", file_name_of(&path));

                    view.update_in(cx, move |this, window, cx| {
                        // Remember where we were in the previous document before
                        // switching, then resume the new one where it left off.
                        this.remember_position();
                        let stored = this.store.borrow().position_for(&path);
                        this.pending_scroll = Some(match (kind, stored) {
                            (DocumentKind::Pdf, Some(anchor)) => PendingScroll::Anchor(anchor),
                            (DocumentKind::Pdf, None) => PendingScroll::Anchor(0.0),
                            (_, Some(offset)) => PendingScroll::Offset(offset),
                            (_, None) => PendingScroll::Offset(0.0),
                        });
                        this.latest_position.set(None);
                        this.excel_sheet = 0;

                        this.store.borrow_mut().set_last_file(&path);
                        this.persist_settings();

                        let document = Document::new(
                            path.clone(),
                            content,
                            &window.text_style().font(),
                            this.font_size,
                            this.line_height(),
                            this.wrap_width_for(window),
                            cx.text_system(),
                        );

                        window.set_window_title(&title);
                        this.document = Some(document);
                        this.selection = None;
                        this.cell_selection = None;
                        this.interaction = None;
                        this.row_layouts.clear();
                        this.document_lines.clear();
                        cx.notify();
                    })
                    .ok();
                }
                Err(message) => {
                    view.update_in(cx, |_, window, cx| {
                        window.push_notification(Notification::error(message), cx);
                    })
                    .ok();
                }
            }
        }));
    }

    fn set_font_size(&mut self, size: f32, window: &Window, cx: &mut Context<Self>) {
        self.font_size = px(size);
        self.store.borrow_mut().settings_mut().font_size = size;
        self.persist_settings();
        self.rewrap_text(window, cx);
        cx.notify();
    }

    /// Height of a text row, in logical pixels.
    fn line_height(&self) -> f32 {
        self.font_size.as_f32() * LINE_HEIGHT_RATIO
    }

    /// The width the text is wrapped for, taken from the window so a resize is
    /// noticed before the reading area has been laid out again.
    fn wrap_width_for(&self, window: &Window) -> f32 {
        text_wrap_width(window.viewport_size().width.as_f32())
    }

    /// Wrap the text again once the font size or the window width changed.
    fn sync_text_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(DocumentContent::Text(text)) =
            self.document.as_ref().map(|document| &document.content)
        else {
            return;
        };

        let wrap_width = self.wrap_width_for(window);
        let font = window.text_style().font();
        if text.matches(&font, self.font_size, self.line_height(), wrap_width) {
            return;
        }
        if self.rewrap_task.is_some() {
            return;
        }

        // Wrapping a large file takes a moment, so wait for a resize to settle
        // instead of re-wrapping on every frame of the drag.
        self.rewrap_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(REWRAP_DEBOUNCE).await;
            this.update_in(cx, |this, window, cx| this.rewrap_text(window, cx))
                .ok();
        }));
    }

    /// Lay the text out for the current font size and width, keeping the first
    /// visible line where it was.
    fn rewrap_text(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.rewrap_task = None;

        let scroll = self.scroll_offset();
        let font_size = self.font_size;
        let line_height = self.line_height();
        let wrap_width = self.wrap_width_for(window);
        let font = window.text_style().font();
        let text_system = cx.text_system();

        let Some(document) = self.document.as_mut() else {
            return;
        };
        let DocumentContent::Text(text) = &mut document.content else {
            return;
        };
        if text.matches(&font, font_size, line_height, wrap_width) {
            return;
        }

        let anchor = text.source_at(scroll);
        let source = text.source().clone();
        *text = TextView::new(
            source,
            &font,
            font_size,
            line_height,
            wrap_width,
            text_system,
        );

        let offset = text.offset_for_source(anchor);
        self.scroll_handle.set_offset(point(px(0.), px(-offset)));
        self.row_layouts.clear();
        // Row indices change when the text is wrapped again.
        self.selection = None;
        cx.notify();
    }

    /// Change the zoom, keeping the reader on the same line.
    fn set_zoom(&mut self, zoom: f32, window: &Window, cx: &mut Context<Self>) {
        if (self.pdf_zoom - zoom).abs() < f32::EPSILON {
            return;
        }

        let position = self.current_position(window).unwrap_or(0.0);
        let previous = self.pdf_zoom;
        self.pdf_zoom = zoom;
        self.store.borrow_mut().settings_mut().pdf_zoom = zoom;
        self.persist_settings();

        self.pending_scroll = Some(if self.is_pdf() {
            // PDFs remember a page and a fraction of that page.
            PendingScroll::Anchor(position)
        } else {
            // Word and Excel lay out at a fixed scale, so the pixel offset
            // follows the zoom directly.
            PendingScroll::Offset(position * zoom / previous)
        });
        self.latest_position.set(None);
        cx.notify();
    }

    /// Zoom a picture with the wheel, keeping the point under the pointer in
    /// place. The picture is moved by dragging it or with the scrollbars.
    fn zoom_image_at(&mut self, event: &ScrollEvent, window: &Window, cx: &mut Context<Self>) {
        let Some(DocumentContent::Image(picture)) =
            self.document.as_ref().map(|document| &document.content)
        else {
            return;
        };

        // A touch pan scrolls the picture like any other document; only a
        // wheel or trackpad scroll zooms.
        if event.source == ScrollSource::Touch {
            return;
        }

        let line_height = window.line_height();
        let lines = match event.delta {
            ScrollDelta::Lines(delta) => delta.y,
            ScrollDelta::Pixels(delta) => delta.y.as_f32() / line_height.as_f32().max(1.0),
        };
        if lines.abs() < f32::EPSILON {
            return;
        }

        let viewport = self.scroll_viewport(window);
        // The scroll area handles the wheel before this listener runs, so undo
        // that scroll: the wheel zooms instead of moving the picture. It also
        // drops one axis of a diagonal scroll, so undo the same amount.
        let mut applied = event.delta.pixel_delta(line_height);
        if !applied.x.is_zero() && !applied.y.is_zero() {
            if applied.x.abs() > applied.y.abs() {
                applied.y = Pixels::ZERO;
            } else {
                applied.x = Pixels::ZERO;
            }
        }
        let offset = self.scroll_handle.offset() - applied;

        let (width, height) = image_layout(picture, viewport, self.pdf_zoom);
        let content_width = (width + 2.0 * IMAGE_MARGIN).max(viewport.width);
        let content_height = (height + 2.0 * IMAGE_MARGIN).max(viewport.height);
        let image_origin = point(
            px((content_width - width) / 2.0),
            px((content_height - height) / 2.0),
        );

        // The picture keeps the same spot under the pointer after it is scaled.
        // A pointer in the margin zooms around the centre instead of dragging
        // the nearest edge under itself.
        let pointer = event.position - self.scroll_handle.bounds().origin;
        let content = pointer - offset;
        let x = (content.x - image_origin.x).as_f32() / width;
        let y = (content.y - image_origin.y).as_f32() / height;
        let anchor = if (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y) {
            point(x, y)
        } else {
            point(0.5, 0.5)
        };

        let zoom =
            (self.pdf_zoom * IMAGE_ZOOM_PER_LINE.powf(lines)).clamp(IMAGE_ZOOM_MIN, IMAGE_ZOOM_MAX);
        if (zoom - self.pdf_zoom).abs() < f32::EPSILON {
            // At the limit the wheel does nothing, so put the scroll back as
            // well instead of letting the picture drift.
            self.scroll_handle.set_offset(offset);
            cx.notify();
            return;
        }

        let (new_width, new_height) = image_layout(picture, viewport, zoom);
        let new_content_width = (new_width + 2.0 * IMAGE_MARGIN).max(viewport.width);
        let new_content_height = (new_height + 2.0 * IMAGE_MARGIN).max(viewport.height);
        let new_image_origin = point(
            px((new_content_width - new_width) / 2.0),
            px((new_content_height - new_height) / 2.0),
        );
        let new_offset = point(
            pointer.x - new_image_origin.x - px(anchor.x * new_width),
            pointer.y - new_image_origin.y - px(anchor.y * new_height),
        );

        self.pdf_zoom = zoom;
        self.store.borrow_mut().settings_mut().pdf_zoom = zoom;
        self.persist_settings();
        self.scroll_handle.set_offset(new_offset);
        self.latest_position.set(None);
        cx.notify();
    }

    fn set_background(&mut self, background: ReadingBackground, cx: &mut App) {
        self.background = background;
        self.store.borrow_mut().settings_mut().background = background;
        self.persist_settings();
        window::apply_theme_mode(background, cx);
    }

    /// Distance scrolled from the top of the document, in logical pixels.
    fn scroll_offset(&self) -> f32 {
        (-self.scroll_handle.offset().y.as_f32()).max(0.0)
    }

    /// Size of the scroll area. Before the first layout only the window size
    /// is known, which is close enough for the first frame.
    fn scroll_viewport(&self, window: &Window) -> Size<f32> {
        let bounds = self.scroll_handle.bounds().size;
        let mut width = bounds.width.as_f32();
        let mut height = bounds.height.as_f32();
        if width < 1.0 || height < 1.0 {
            let window_size = window.viewport_size();
            if width < 1.0 {
                width = window_size.width.as_f32();
            }
            if height < 1.0 {
                height = (window_size.height.as_f32() - 64.0).max(320.0);
            }
        }
        Size::new(width, height)
    }

    fn pdf_layout(&self, view: &PdfView, window: &Window) -> PdfLayout {
        let source = view.source();
        PdfLayout::new(
            &source,
            self.scroll_viewport(window),
            self.pdf_zoom,
            window.scale_factor(),
        )
    }

    /// Reading position in the format stored for the current document: a scroll
    /// offset for text, a page anchor for PDFs.
    fn current_position(&self, window: &Window) -> Option<f32> {
        let document = self.document.as_ref()?;
        Some(match &document.content {
            DocumentContent::Text(_)
            | DocumentContent::Word(_)
            | DocumentContent::Excel(_)
            | DocumentContent::Markdown(_)
            | DocumentContent::Image(_) => self.scroll_handle.offset().y.as_f32(),
            DocumentContent::Pdf(view) => self
                .pdf_layout(view, window)
                .anchor_for_offset(self.scroll_offset()),
        })
    }

    fn apply_pending_scroll(&mut self, window: &Window) {
        let Some(pending) = self.pending_scroll.take() else {
            return;
        };

        let offset = match pending {
            PendingScroll::Offset(offset) => offset,
            PendingScroll::Anchor(anchor) => {
                let layout = match self.document.as_ref().map(|document| &document.content) {
                    Some(DocumentContent::Pdf(view)) => self.pdf_layout(view, window),
                    _ => return,
                };
                -layout.offset_for_anchor(anchor)
            }
        };

        self.scroll_handle.set_offset(point(px(0.), px(offset)));
    }

    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let font_size = self.font_size;
        let pdf_zoom = self.pdf_zoom;
        let background = self.background;
        let background_color = background.background(cx);
        let foreground = background.foreground(cx);
        let zoomable = !self.is_text();
        let menu_variant = ButtonCustomVariant::new(cx)
            .foreground(foreground)
            .hover(foreground.opacity(0.10))
            .active(foreground.opacity(0.16));

        let file_menu = {
            let view = view.clone();
            Button::new("menu-file")
                .custom(menu_variant)
                .compact()
                .label("文件")
                .dropdown_menu(move |menu, _window, _cx| {
                    let load = view.clone();
                    let print = view.clone();
                    menu.min_w(px(220.))
                        .item(
                            PopupMenuItem::new("加载文件…")
                                .icon(IconName::FolderOpen)
                                .on_click(move |_, window, cx| {
                                    load.update(cx, |this, cx| this.load_file(window, cx));
                                }),
                        )
                        .item(PopupMenuItem::new("打印…").on_click(move |_, window, cx| {
                            print.update(cx, |this, cx| this.print_document(window, cx));
                        }))
                        .separator()
                        .item(
                            PopupMenuItem::new("退出")
                                .icon(IconName::Close)
                                .on_click(|_, _, cx| cx.quit()),
                        )
                })
        };

        // Text has a font size; the paged and tabular formats are zoomed instead.
        let size_menu = {
            let view = view.clone();
            Button::new("menu-font")
                .custom(menu_variant)
                .compact()
                .label(if zoomable {
                    "页面缩放"
                } else {
                    "字体大小"
                })
                .dropdown_menu(move |menu, _window, _cx| {
                    if zoomable {
                        let mut menu = menu.min_w(px(180.)).label("页面缩放");
                        for zoom in PDF_ZOOMS {
                            let view = view.clone();
                            menu = menu.item(
                                PopupMenuItem::new(format!("{}%", (zoom * 100.0).round() as i32))
                                    .checked((pdf_zoom - zoom).abs() < 0.001)
                                    .on_click(move |_, window, cx| {
                                        view.update(cx, |this, cx| this.set_zoom(zoom, window, cx));
                                    }),
                            );
                        }
                        menu
                    } else {
                        let mut menu = menu.min_w(px(180.)).label("正文字号");
                        for size in FONT_SIZES {
                            let view = view.clone();
                            menu = menu.item(
                                PopupMenuItem::new(format!("{size} px"))
                                    .checked(font_size == px(size))
                                    .on_click(move |_, window, cx| {
                                        view.update(cx, |this, cx| {
                                            this.set_font_size(size, window, cx)
                                        });
                                    }),
                            );
                        }
                        menu
                    }
                })
        };

        let background_menu = {
            let view = view.clone();
            Button::new("menu-background")
                .custom(menu_variant)
                .compact()
                .label("窗口背景色")
                .dropdown_menu(move |menu, _window, _cx| {
                    let mut menu = menu.min_w(px(200.)).label("窗口背景色");
                    for choice in ReadingBackground::ALL {
                        let view = view.clone();
                        menu = menu.item(
                            PopupMenuItem::new(choice.label())
                                .checked(background == choice)
                                .on_click(move |_, _, cx| {
                                    view.update(cx, |this, cx| this.set_background(choice, cx));
                                }),
                        );
                    }
                    menu
                })
        };

        let details = match self.document.as_ref() {
            Some(document) => document.status_line(),
            None => String::from("未加载文件"),
        };

        TitleBar::new()
            .bg(background_color)
            .text_color(foreground)
            .child(
                h_flex()
                    .flex_1()
                    .min_w(px(0.))
                    .items_center()
                    .gap_1()
                    .px_2()
                    .child(
                        svg()
                            .data(APP_ICON)
                            .flex_shrink_0()
                            .w(px(18.))
                            .h(px(18.))
                            .mr_1()
                            .text_color(foreground),
                    )
                    .child(file_menu)
                    .child(size_menu)
                    .child(background_menu)
                    // The title bar owns window dragging, which consumes the mouse-up
                    // a normal `on_click` waits for, so act on press here.
                    .child(
                        Button::new("menu-about")
                            .custom(menu_variant)
                            .compact()
                            .label("关于")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|_, _, window, cx| show_about(window, cx)),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .max_w(px(360.))
                            .truncate()
                            .text_xs()
                            .opacity(0.6)
                            .child(details),
                    ),
            )
    }

    fn render_body(
        &mut self,
        window: &mut Window,
        foreground: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let kind = self.document.as_ref().map(|document| &document.content);
        match kind {
            Some(DocumentContent::Pdf(_)) => return self.render_pdf_body(window, foreground, cx),
            Some(DocumentContent::Word(_)) | Some(DocumentContent::Markdown(_)) => {
                return self.render_word_body(window, foreground, cx);
            }
            Some(DocumentContent::Excel(_)) => {
                return self.render_excel_body(window, foreground, cx);
            }
            Some(DocumentContent::Image(_)) => {
                return self.render_image_body(window, foreground, cx);
            }
            _ => {}
        }

        let font_size = self.font_size;
        let line_height = self.line_height();
        let font = window.text_style().font();
        let highlight = cx.theme().selection;
        let selection = self.selection;
        let offset = self.scroll_offset();
        let viewport = self.scroll_viewport(window);

        let Some(document) = self.document.as_mut() else {
            return empty_body(foreground);
        };
        let DocumentContent::Text(text) = &document.content else {
            return empty_body(foreground);
        };

        // Only the rows that can be seen are built, plus a screen above and
        // below so a fast scroll does not outrun the layout.
        let overscan = (viewport.height * 0.75).max(300.0);
        let row_count = text.row_count();
        let first = text.row_at(offset - overscan);
        let last = text.row_at(offset + viewport.height + overscan);
        let column_width = text.wrap_width() + 2.0 * TEXT_PADDING_X;
        let content_height = text.content_height() + 2.0 * TEXT_PADDING_Y;

        self.row_layouts.clear();
        let mut rows = Vec::with_capacity(if row_count == 0 { 0 } else { last - first + 1 });
        for index in first..(last + 1).min(row_count) {
            let slot = Rc::new(RefCell::new(None));
            self.row_layouts.insert(index, slot.clone());

            let line = text.row_text(index);
            let mut row = div()
                .id(("poem-row", index))
                .absolute()
                .top(px(TEXT_PADDING_Y + text.row_top(index)))
                .left(px(TEXT_PADDING_X))
                .right(px(TEXT_PADDING_X))
                .h(px(text.row_height(index)))
                .text_size(font_size)
                .line_height(px(line_height))
                // The text was wrapped for exactly this width, so letting the
                // element wrap again could split a row in two.
                .whitespace_nowrap();

            if !line.is_empty() {
                // The selected slice is painted with the selection colour as
                // its run background, which keeps the text layout intact.
                let styled = match selection
                    .and_then(|selection| selection.bytes_in_line(index, line.len()))
                {
                    Some(selected) => StyledText::new(SharedString::from(line)).with_runs(
                        line_runs(line.len(), &font, foreground, highlight, selected),
                    ),
                    None => StyledText::new(SharedString::from(line)),
                };

                row = row.child(LineText::new(styled, slot));
            }

            rows.push(row.into_any_element());
        }

        let content = div()
            .relative()
            .mx_auto()
            .w(px(column_width))
            .h(px(content_height))
            .flex_shrink_0()
            .children(rows);

        div()
            .id("poem-scroll-root")
            .relative()
            .flex_1()
            .min_h(px(0.))
            .w_full()
            .flex()
            .flex_col()
            .child(
                self.reading_area(
                    div()
                        .id("poem-scroll")
                        .size_full()
                        .flex()
                        .flex_col()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll_handle)
                        .on_scroll(cx.listener(|this, _, window, cx| {
                            this.schedule_position_save(window, cx)
                        }))
                        .child(content),
                    cx,
                ),
            )
            .child(
                div().absolute().inset_0().child(
                    Scrollbar::vertical(&self.scroll_handle)
                        .id("poem-scrollbar")
                        .mode(ScrollbarMode::Always)
                        .viewport_from_layout(),
                ),
            )
            .into_any_element()
    }

    /// Word documents are drawn from their own paragraph model, with the text
    /// column matching the page width declared in the file.
    fn render_word_body(
        &mut self,
        window: &mut Window,
        foreground: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let document = match self.document.as_ref().map(|document| &document.content) {
            Some(DocumentContent::Word(document)) | Some(DocumentContent::Markdown(document)) => {
                document.clone()
            }
            _ => return empty_body(foreground),
        };

        let scale = self.pdf_zoom * 4.0 / 3.0;
        let base_font = window.text_style().font();
        let viewport = self.scroll_viewport(window);
        let column_width = (document.content_width * scale).max(160.0);
        let highlight = cx.theme().selection;

        // The lines are rebuilt on every render, and the selection refers to
        // their order, which only changes with the document itself.
        self.document_lines.clear();
        let blocks = {
            let mut render = WordRender {
                scale,
                foreground,
                base: &base_font,
                highlight,
                selection: self.selection,
                lines: &mut self.document_lines,
                next_paragraph: 0,
                next_row: 0,
            };
            document
                .blocks
                .iter()
                .map(|block| word_block(block, &mut render, None))
                .collect::<Vec<_>>()
        };

        // Auto margins would centre a column that is wider than the window and
        // clip it on both sides, so wide pages start at the left edge instead.
        let mut content = div().w(px(column_width)).flex_shrink_0().py(px(24.));
        if column_width <= viewport.width + 0.5 {
            content = content.mx_auto();
        }
        let content = content.children(blocks);

        let mut root = div()
            .id("word-scroll-root")
            .relative()
            .flex_1()
            .min_h(px(0.))
            .w_full()
            .flex()
            .flex_col()
            .child(
                self.reading_area(
                    div()
                        .id("word-scroll")
                        .size_full()
                        .flex()
                        .flex_col()
                        .overflow_scroll()
                        .track_scroll(&self.scroll_handle)
                        .on_scroll(cx.listener(|this, _, window, cx| {
                            this.schedule_position_save(window, cx)
                        }))
                        .child(content),
                    cx,
                ),
            )
            .child(
                div().absolute().inset_0().child(
                    Scrollbar::vertical(&self.scroll_handle)
                        .id("word-scrollbar")
                        .mode(ScrollbarMode::Always)
                        .viewport_from_layout(),
                ),
            );

        // A zoomed-in page can be wider than the window.
        if column_width > viewport.width + 0.5 {
            root = root.child(
                div().absolute().inset_0().child(
                    Scrollbar::horizontal(&self.scroll_handle)
                        .id("word-scrollbar-x")
                        .mode(ScrollbarMode::Always)
                        .viewport_from_layout(),
                ),
            );
        }

        root.into_any_element()
    }

    /// Excel sheets are drawn as a grid; only the cells on screen are built, so
    /// a long sheet scrolls as smoothly as a short one.
    fn render_excel_body(
        &mut self,
        window: &mut Window,
        foreground: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let workbook = match self.document.as_ref().map(|document| &document.content) {
            Some(DocumentContent::Excel(workbook)) => workbook.clone(),
            _ => return empty_body(foreground),
        };
        let index = self
            .excel_sheet
            .min(workbook.sheets.len().saturating_sub(1));
        let sheet = &workbook.sheets[index];

        let zoom = self.pdf_zoom;
        let offset = self.scroll_handle.offset();
        let left = (-offset.x.as_f32()).max(0.0);
        let top = (-offset.y.as_f32()).max(0.0);
        let viewport = self.scroll_viewport(window);
        let base_font = window.text_style().font();

        let first_row = sheet.row_at(top / zoom);
        let last_row = sheet.row_at((top + viewport.height) / zoom);
        let first_column = sheet.column_at(left / zoom);
        let last_column = sheet.column_at((left + viewport.width) / zoom);

        let mut positions: Vec<Option<(u32, u32)>> = Vec::new();
        let cells = excel_cells(
            ExcelRender {
                sheet,
                styles: &workbook.styles,
                zoom,
                foreground,
                base: &base_font,
                highlight: cx.theme().selection,
                selection: self.cell_selection,
            },
            first_row,
            last_row,
            first_column,
            last_column,
            &mut positions,
        );

        // The cells record where they were painted, so a pointer can be turned
        // back into the cell under it without repeating the layout maths.
        let storage = self.cell_bounds.clone();
        let content = div()
            .relative()
            .flex_shrink_0()
            .w(px((sheet.width() * zoom).max(viewport.width)))
            .h(px((sheet.height() * zoom).max(viewport.height)))
            .on_children_prepainted(move |painted: Vec<Bounds<Pixels>>, _window, _cx| {
                let mut entries = Vec::with_capacity(painted.len());
                for (index, bounds) in painted.into_iter().enumerate() {
                    if let Some(&Some((row, column))) = positions.get(index) {
                        entries.push((row, column, bounds));
                    }
                }
                *storage.borrow_mut() = entries;
            })
            .children(cells);

        let mut root = div()
            .id("excel-scroll-root")
            .relative()
            .flex_1()
            .min_h(px(0.))
            .w_full()
            .flex()
            .flex_col()
            .child(
                self.reading_area(
                    div()
                        .id("excel-scroll")
                        .size_full()
                        .flex()
                        .flex_col()
                        .overflow_scroll()
                        .track_scroll(&self.scroll_handle)
                        .on_scroll(cx.listener(|this, _, window, cx| {
                            this.schedule_position_save(window, cx)
                        }))
                        .child(content),
                    cx,
                ),
            )
            .child(
                div().absolute().inset_0().child(
                    Scrollbar::vertical(&self.scroll_handle)
                        .id("excel-scrollbar")
                        .mode(ScrollbarMode::Always)
                        .viewport_from_layout(),
                ),
            );

        if sheet.width() * zoom > viewport.width + 0.5 {
            root = root.child(
                div().absolute().inset_0().child(
                    Scrollbar::horizontal(&self.scroll_handle)
                        .id("excel-scrollbar-x")
                        .mode(ScrollbarMode::Always)
                        .viewport_from_layout(),
                ),
            );
        }

        if workbook.sheets.len() < 2 {
            return root.into_any_element();
        }

        let view = cx.entity();
        let tabs = workbook
            .sheets
            .iter()
            .enumerate()
            .map(|(sheet_index, sheet)| {
                let view = view.clone();
                let mut tab = div()
                    .id(("sheet-tab", sheet_index))
                    .px_3()
                    .py(px(3.))
                    .rounded_md()
                    .cursor_pointer()
                    .text_sm()
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        view.update(cx, |this, cx| this.set_sheet(sheet_index, cx));
                    });
                if sheet_index == index {
                    tab = tab.bg(foreground.opacity(0.12));
                }
                tab.child(sheet.name.clone()).into_any_element()
            })
            .collect::<Vec<_>>();

        v_flex()
            .flex_1()
            .min_h(px(0.))
            .w_full()
            .child(
                h_flex()
                    .w_full()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(foreground.opacity(0.12))
                    .children(tabs),
            )
            .child(root)
            .into_any_element()
    }

    fn set_sheet(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.excel_sheet == index {
            return;
        }
        self.excel_sheet = index;
        // The cells of the previous sheet are gone, and so is the selection.
        self.cell_selection = None;
        self.scroll_handle.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }

    /// PDFs are shown as rasterized pages, each rendered on demand so a long
    /// document opens instantly and only costs memory where it is being read.
    fn render_pdf_body(
        &self,
        window: &mut Window,
        foreground: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(DocumentContent::Pdf(view)) =
            self.document.as_ref().map(|document| &document.content)
        else {
            return div().into_any_element();
        };

        let layout = self.pdf_layout(view, window);
        view.set_scale_key(layout.scale_key);

        // Rasterize every page the reader can see, plus one page of overscan.
        let visible = layout.visible_range(self.scroll_offset(), layout.viewport.height);
        let source = view.source();
        for index in visible.clone() {
            if view.image(index).is_none() && view.begin_render(index, layout.scale_key) {
                spawn_pdf_render(
                    source.clone(),
                    index,
                    layout.scale,
                    layout.scale_key,
                    window,
                    cx,
                );
            }
        }

        let page_bg: Hsla = rgb(0xFFFFFF).into();
        let page_border = foreground.opacity(0.14);
        let pages = layout
            .pages
            .iter()
            .enumerate()
            .map(|(index, page)| {
                let mut page_el = div()
                    .id(("pdf-page", index))
                    .flex_shrink_0()
                    .w(px(page.width))
                    .h(px(page.height))
                    .bg(page_bg)
                    .border_1()
                    .border_color(page_border);

                if let Some(image) = view.image(index) {
                    page_el = page_el.child(img(image).w_full().h_full());
                } else {
                    let label = if view.has_failed(index) {
                        "这一页无法显示"
                    } else {
                        "正在渲染…"
                    };
                    page_el = page_el
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(div().text_xs().text_color(rgb(0x8A8A8A)).child(label));
                }

                page_el.into_any_element()
            })
            .collect::<Vec<_>>();

        let content = v_flex()
            .w(px(layout.content_width))
            .flex_shrink_0()
            .py(px(PDF_TOP_PADDING))
            .gap(px(PDF_PAGE_GAP))
            .items_center()
            .children(pages);

        let mut root = div()
            .id("pdf-scroll-root")
            .relative()
            .flex_1()
            .min_h(px(0.))
            .w_full()
            .flex()
            .flex_col()
            .child(
                self.reading_area(
                    div()
                        .id("pdf-scroll")
                        .size_full()
                        .flex()
                        .flex_col()
                        .overflow_scroll()
                        .track_scroll(&self.scroll_handle)
                        .on_scroll(cx.listener(|this, _, window, cx| {
                            this.schedule_position_save(window, cx)
                        }))
                        .child(content),
                    cx,
                ),
            )
            .child(
                div().absolute().inset_0().child(
                    Scrollbar::vertical(&self.scroll_handle)
                        .id("pdf-scrollbar")
                        .mode(ScrollbarMode::Always)
                        .viewport_from_layout(),
                ),
            );

        // Only zoomed-in pages are wider than the window, so the horizontal
        // scrollbar is only shown when it can actually move.
        if layout.content_width > layout.viewport.width + 0.5 {
            root = root.child(
                div().absolute().inset_0().child(
                    Scrollbar::horizontal(&self.scroll_handle)
                        .id("pdf-scrollbar-x")
                        .mode(ScrollbarMode::Always)
                        .viewport_from_layout(),
                ),
            );
        }

        root.into_any_element()
    }

    /// A picture is fitted to the reading area, and zooming in makes it larger
    /// than the window so it can be scrolled in both directions.
    fn render_image_body(
        &self,
        window: &mut Window,
        foreground: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(DocumentContent::Image(picture)) =
            self.document.as_ref().map(|document| &document.content)
        else {
            return empty_body(foreground);
        };

        let viewport = self.scroll_viewport(window);
        let (width, height) = image_layout(picture, viewport, self.pdf_zoom);
        let content_width = (width + 2.0 * IMAGE_MARGIN).max(viewport.width);
        let content_height = (height + 2.0 * IMAGE_MARGIN).max(viewport.height);

        let content = div()
            .flex_shrink_0()
            .w(px(content_width))
            .h(px(content_height))
            .flex()
            .items_center()
            .justify_center()
            .child(img(picture.image()).w(px(width)).h(px(height)));

        let mut root = div()
            .id("image-scroll-root")
            .relative()
            .flex_1()
            .min_h(px(0.))
            .w_full()
            .flex()
            .flex_col()
            .child(
                self.reading_area(
                    div()
                        .id("image-scroll")
                        .size_full()
                        .flex()
                        .flex_col()
                        .overflow_scroll()
                        .track_scroll(&self.scroll_handle)
                        .on_scroll(cx.listener(|this, _, window, cx| {
                            this.schedule_position_save(window, cx)
                        }))
                        .on_scroll(cx.listener(|this, event: &ScrollEvent, window, cx| {
                            this.zoom_image_at(event, window, cx)
                        }))
                        .child(content),
                    cx,
                ),
            )
            .child(
                div().absolute().inset_0().child(
                    Scrollbar::vertical(&self.scroll_handle)
                        .id("image-scrollbar")
                        .mode(ScrollbarMode::Always)
                        .viewport_from_layout(),
                ),
            );

        // Only a picture wider than the window needs horizontal scrolling.
        if content_width > viewport.width + 0.5 {
            root = root.child(
                div().absolute().inset_0().child(
                    Scrollbar::horizontal(&self.scroll_handle)
                        .id("image-scrollbar-x")
                        .mode(ScrollbarMode::Always)
                        .viewport_from_layout(),
                ),
            );
        }

        root.into_any_element()
    }
}

/// Size of a picture on screen: fitted to the reading area, then scaled by the
/// page zoom, and never blown up more than [`IMAGE_MAX_UPSCALE`].
fn image_layout(picture: &Picture, viewport: Size<f32>, zoom: f32) -> (f32, f32) {
    let available_width = (viewport.width - 2.0 * IMAGE_MARGIN).max(80.0);
    let available_height = (viewport.height - 2.0 * IMAGE_MARGIN).max(80.0);
    let fit = (available_width / picture.width)
        .min(available_height / picture.height)
        .min(IMAGE_MAX_UPSCALE);
    let scale = (fit * zoom).max(0.01);

    (
        (picture.width * scale).max(1.0),
        (picture.height * scale).max(1.0),
    )
}

/// Width available to the text block for a given window width, in logical
/// pixels. The block is centred and no wider than [`TEXT_MAX_WIDTH`].
fn text_wrap_width(viewport_width: f32) -> f32 {
    (viewport_width.min(TEXT_MAX_WIDTH) - 2.0 * TEXT_PADDING_X).max(160.)
}

/// Shown when no document is open.
fn empty_body(foreground: Hsla) -> AnyElement {
    div()
        .flex_1()
        .min_h(px(0.))
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .child(
            v_flex()
                .items_center()
                .gap_2()
                .child(
                    svg()
                        .data(APP_ICON)
                        .w(px(72.))
                        .h(px(72.))
                        .text_color(foreground.opacity(0.25)),
                )
                .child(div().text_xl().child("尚未加载作品"))
                .child(
                    div()
                        .text_sm()
                        .opacity(0.65)
                        .child("使用菜单「文件 → 加载文件…」或把文件拖入窗口，打开文本、Markdown、PDF、Word 或 Excel 文件"),
                ),
        )
        .into_any_element()
}

/// Runs for one line, with the selected slice painted over the selection colour.
fn line_runs(
    text_len: usize,
    font: &Font,
    color: Hsla,
    highlight: Hsla,
    selected: Range<usize>,
) -> Vec<TextRun> {
    let run = |len: usize, background_color: Option<Hsla>| TextRun {
        len,
        font: font.clone(),
        color,
        background_color,
        underline: None,
        strikethrough: None,
    };

    let mut runs = Vec::with_capacity(3);
    if selected.start > 0 {
        runs.push(run(selected.start, None));
    }
    if !selected.is_empty() {
        runs.push(run(selected.len(), Some(highlight)));
    }
    if selected.end < text_len {
        runs.push(run(text_len - selected.end, None));
    }
    runs
}

impl Render for PoemReader {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // GPUI reuses the element tree while a window is resized, so the text is
        // only wrapped again when the resize is reported here.
        if self.bounds_subscription.is_none() {
            self.bounds_subscription =
                Some(cx.observe_window_bounds(window, |this, window, cx| {
                    // Text is wrapped again, and the other layouts are rebuilt so
                    // the visible rows and pages follow the new window size.
                    this.sync_text_layout(window, cx);
                    cx.notify();
                }));
        }

        self.sync_text_layout(window, cx);
        self.apply_pending_scroll(window);

        let background = self.background.background(cx);
        let foreground = self.background.foreground(cx);

        let title_bar = self.render_title_bar(cx).into_any_element();
        let body = self.render_body(window, foreground, cx);

        if let Some(position) = self.current_position(window) {
            self.latest_position.set(Some(position));
        }

        v_flex()
            .size_full()
            .bg(background)
            .text_color(foreground)
            .child(title_bar)
            .child(body)
            // Files dragged in from the system arrive as an `ExternalPaths`
            // drag, so the whole window is a drop target.
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.open_dropped_files(paths, window, cx);
            }))
    }
}

fn show_about(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, |dialog, _window, _cx| {
        dialog
            .title("关于 PoemReader")
            .child(
                v_flex()
                    .gap_2()
                    .text_sm()
                    .child("PoemReader 0.1.0")
                    .child("一个使用 GPUI Kit 构建的极简阅读器。")
                    .child("· 支持纯文本（UTF-8 / GB18030）")
                    .child("· 纯文本按需排版，可打开任意大小的文件")
                    .child("· 支持 PDF：按页完整渲染字体、表格与图片")
                    .child("· 支持 Markdown（.md）：标题、强调、列表、表格、代码块、引用与图片")
                    .child("· 支持 Word（.docx）：段落样式、字符格式、列表、表格与图片")
                    .child("· 支持 Excel（.xlsx）：单元格字体、颜色、边框、数字格式与多工作表")
                    .child("· 支持图片：JPEG、PNG、BMP、WebP、ICO、GIF（含动画）与 SVG")
                    .child("· 「文件 → 加载文件…」打开本地文件")
                    .child("· 也可以把文件从系统拖入窗口直接打开")
                    .child("· 纯文本按行渲染，Markdown / Word / Excel 按自身排版绘制，PDF 按页渲染，图片按窗口大小适配")
                    .child("· 所有文档都支持鼠标拖拽平移，图片还可用鼠标滚轮缩放")
                    .child("· 纯文本可用右键菜单切换「拖拽平移 / 划选文字」")
                    .child("· 划选后用 Ctrl+C 或右键菜单「复制所选文字」复制")
                    .child("· 「文件 → 打印…」或 Ctrl+P 打印文本、Markdown、PDF、Word、Excel 与图片")
                    .child("· 打印对话框中可选「页码」只打印指定页或页码范围")
                    .child("· 「字体大小」调整纯文本；Markdown / PDF / Word / Excel / 图片为「页面缩放」")
                    .child("· 「窗口背景色」切换阅读配色")
                    .child("· 字号、缩放、配色和阅读位置会自动保存，下次启动时恢复"),
            )
            .footer(
                Button::new("about-ok")
                    .primary()
                    .label("确定")
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            )
    });
}
