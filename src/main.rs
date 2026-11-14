// GUI subsystem in every profile: no console window is created, in debug or release.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme as _, IconName, Theme, ThemeMode, TitleBar, WindowExt as _,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    h_flex,
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem},
    notification::Notification,
    scroll::{Scrollbar, ScrollbarMode},
    v_flex,
};
use gpui_kit::*;

mod cfb;
mod doc;
mod excel;
mod ooxml;
mod pdf;
mod print;
#[cfg(windows)]
mod print_office;
mod settings;
mod text;
mod word;
mod xls;

use excel::Workbook;
use ooxml::Package;
use pdf::{PdfSource, PdfView};
use print::{PrintContent, PrintJob};
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

/// Padding and line spacing of the text body, in logical pixels. They are also
/// used when placing the selection, so both must stay in one place.
const TEXT_PADDING_X: f32 = 40.;
const TEXT_PADDING_Y: f32 = 28.;
const TEXT_MAX_WIDTH: f32 = 880.;
/// Height of a text row as a multiple of the font size.
const LINE_HEIGHT_RATIO: f32 = 1.7;

/// PDFs and Office documents are decoded and held in memory as a whole, so
/// they keep a size cap. Text files are wrapped into rows while being read and
/// have no size limit.
const MAX_DOCUMENT_BYTES: u64 = 128 * 1024 * 1024;

/// How long the window has to stop resizing before the text is wrapped again.
const REWRAP_DEBOUNCE: Duration = Duration::from_millis(120);

/// How long scrolling must stay idle before the position is written to disk.
const SAVE_POSITION_DEBOUNCE: Duration = Duration::from_millis(250);

/// The app icon, also embedded in the executable by `build.rs`.
const APP_ICON: &[u8] = include_bytes!("../assets/icon.svg");

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    fn SetProcessDpiAwarenessContext(value: isize) -> i32;
    fn GetDpiForSystem() -> u32;
}

/// The window manifest embedded by GPUI already selects PerMonitorV2; this is
/// a fallback for builds where that manifest is missing, and it fails
/// harmlessly when the awareness was already set.
fn enable_dpi_awareness() {
    #[cfg(windows)]
    unsafe {
        // DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2
        let _ = SetProcessDpiAwarenessContext(-4);
    }
}

fn system_scale_factor() -> f32 {
    unsafe { GetDpiForSystem() as f32 / 96.0 }
    #[cfg(not(windows))]
    {
        1.0
    }
}

/// Center a logical-size window on the primary display.
///
/// `Bounds::centered` centres against the display size GPUI reports, which on
/// Windows is the physical size while window bounds are logical. Dividing by
/// the real scale factor keeps the window inside the visible work area on
/// high-DPI displays.
fn initial_window_bounds(cx: &App) -> Bounds<Pixels> {
    let window_size = size(px(1120.), px(800.));
    let Some(display) = cx.primary_display() else {
        return Bounds::new(point(px(80.), px(80.)), window_size);
    };

    let scale = system_scale_factor();
    let visible = display.visible_bounds();
    let logical_width = visible.size.width.as_f32() / scale;
    let logical_height = visible.size.height.as_f32() / scale;
    let origin = point(
        px(((logical_width - window_size.width.as_f32()) / 2.0).max(0.0)),
        px(((logical_height - window_size.height.as_f32()) / 2.0).max(0.0)),
    );

    Bounds::new(origin, window_size)
}

fn main() {
    enable_dpi_awareness();

    let store = Rc::new(RefCell::new(Store::load()));
    let latest_position = Rc::new(Cell::new(None::<f32>));
    let initial = resolve_initial_document(&store.borrow());
    let initial_title = match initial.as_ref() {
        Some((path, _, _)) => format!("PoemReader — {}", file_name_of(path)),
        None => String::from("PoemReader"),
    };
    let initial_background = store.borrow().settings().background;

    application().with_assets(assets::Assets).run(move |cx| {
        gpui_kit::init(cx);
        // Keep the reading scrollbar visible instead of fading it out when idle.
        component::Theme::set_scrollbar_mode(ScrollbarMode::Always, cx);
        apply_theme_mode(initial_background, cx);

        let bounds = initial_window_bounds(cx);
        let _ = open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(560.), px(420.))),
                ..TitleBar::window_options()
            },
            cx,
            {
                let store = store.clone();
                let latest_position = latest_position.clone();
                move |window, cx| {
                    window.set_window_title(&initial_title);
                    let font = window.text_style().font();
                    cx.new(move |cx| PoemReader::new(store, latest_position, initial, font, cx))
                }
            },
        )
        .expect("failed to open the PoemReader window");

        // Closing the window with its close button removes it without running
        // the app-quit handlers, so persist the latest reading position here
        // as well. The callback only holds shared state and lives as long as
        // the application, hence the subscription is intentionally leaked.
        let close_store = store.clone();
        let close_position = latest_position.clone();
        let subscription = cx.on_window_closed(move |_cx, _window_id| {
            let mut store = close_store.borrow_mut();
            if let Some(position) = close_position.get()
                && let Some(path) = store.settings().last_file.clone()
            {
                store.set_position(&path, position);
            }
            store.save();
        });
        std::mem::forget(subscription);
    });
}

/// Prefer the file passed on the command line, then the file read last session.
/// Prefer the file passed on the command line, then the file read last session.
fn resolve_initial_document(store: &Store) -> Option<(PathBuf, DocumentKind, LoadedContent)> {
    if let Some(path) = std::env::args_os().nth(1).map(PathBuf::from)
        && let Ok((kind, content)) = load_document(&path)
    {
        return Some((path, kind, content));
    }

    let path = store.settings().last_file.clone()?;
    load_document(&path)
        .ok()
        .map(|(kind, content)| (path, kind, content))
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DocumentKind {
    Text,
    Pdf,
    Word,
    Excel,
}

/// What a background load produced. Held only long enough to build a
/// [`Document`] on the render thread.
enum LoadedContent {
    Text(String),
    Pdf(Arc<PdfSource>),
    Word(Arc<WordDocument>),
    Excel(Arc<Workbook>),
}

/// The body of a loaded document.
enum DocumentContent {
    Text(TextView),
    Pdf(PdfView),
    Word(Arc<WordDocument>),
    Excel(Arc<Workbook>),
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
            DocumentContent::Excel(workbook) => {
                format!(
                    "{} · Excel · {} 个工作表",
                    self.file_name,
                    workbook.sheets.len()
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

/// Switch the component theme so menus, the about dialog and the window
/// controls stay legible against the chosen reading background.
fn apply_theme_mode(background: ReadingBackground, cx: &mut App) {
    let mode = match background {
        ReadingBackground::Night => ThemeMode::Dark,
        ReadingBackground::Paper | ReadingBackground::Mint => ThemeMode::Light,
        // Default follows whatever appearance the system already selected.
        ReadingBackground::Default => return,
    };
    Theme::change(mode, None, cx);
}

/// Where the pages of a PDF land in the scroll area, in logical pixels.
struct PdfLayout {
    pages: Vec<Size<f32>>,
    viewport: Size<f32>,
    content_width: f32,
    /// Raster pixels per PDF point.
    scale: f32,
    /// Quantized `scale`, used to tell cached pages from stale ones.
    scale_key: u32,
}

impl PdfLayout {
    fn new(source: &PdfSource, viewport: Size<f32>, zoom: f32, dpi: f32) -> Self {
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
    fn offset_for_anchor(&self, anchor: f32) -> f32 {
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
    fn anchor_for_offset(&self, offset: f32) -> f32 {
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
    fn visible_range(&self, offset: f32, viewport_height: f32) -> Range<usize> {
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

/// The scroll position to apply once the scroll area has been laid out.
#[derive(Clone, Copy)]
enum PendingScroll {
    /// A raw scroll offset, used by text documents.
    Offset(f32),
    /// A `page + fraction` anchor, used by PDF documents.
    Anchor(f32),
}

/// What the left button is currently doing in the reading area.
#[derive(Clone, Copy)]
enum Interaction {
    /// Dragging the content around.
    Pan {
        /// Where the pointer was when the drag started.
        origin: Point<Pixels>,
        /// The scroll offset at that moment.
        offset: Point<Pixels>,
    },
    /// Dragging across text to select it.
    Select,
    /// Dragging across the cells of a sheet to select them.
    SelectCells,
}

/// A position in the text body: a byte offset within one line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct TextPos {
    line: usize,
    byte: usize,
}

/// The text the reader has selected, from `anchor` to a movable `head`.
#[derive(Clone, Copy)]
struct Selection {
    anchor: TextPos,
    head: TextPos,
}

impl Selection {
    fn ordered(&self) -> (TextPos, TextPos) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// The byte range of this selection within `line`.
    fn bytes_in_line(&self, line: usize, len: usize) -> Option<Range<usize>> {
        let (start, end) = self.ordered();
        if line < start.line || line > end.line {
            return None;
        }

        let from = if line == start.line {
            start.byte.min(len)
        } else {
            0
        };
        let to = if line == end.line {
            end.byte.min(len)
        } else {
            len
        };

        (from < to).then_some(from..to)
    }
}

/// A line of a Word document that can be selected, with the layout captured
/// after rendering.
struct SelectableLine {
    /// The text as drawn, including the list marker and any first-line indent.
    text: SharedString,
    /// Bytes at the start of the line that belong to the layout but not to the
    /// paragraph, such as the spaces standing in for a first-line indent.
    skip: usize,
    /// Paragraph the line belongs to; the lines of one paragraph are copied
    /// without a separator between them.
    paragraph: usize,
    /// Table row the line belongs to, so the cells of one row are copied with
    /// tabs between them.
    row: Option<usize>,
    layout: Rc<RefCell<Option<TextLayout>>>,
}

/// Where a text block sits in the document, which decides how its text is
/// joined with the neighbouring blocks when copied.
#[derive(Clone, Copy)]
struct LinePlace {
    paragraph: usize,
    row: Option<usize>,
    /// Leading bytes that belong to the layout but not to the paragraph.
    skip: usize,
}

/// The cells of a sheet the reader has selected.
#[derive(Clone, Copy, PartialEq, Eq)]
struct CellSelection {
    anchor: (u32, u32),
    head: (u32, u32),
}

impl CellSelection {
    fn ordered(&self) -> ((u32, u32), (u32, u32)) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    fn contains(&self, row: u32, column: u32) -> bool {
        let ((top, left), (bottom, right)) = self.ordered();
        (top..=bottom).contains(&row) && (left..=right).contains(&column)
    }
}

/// A rendered line of text that publishes its layout after prepaint, so the
/// view can turn pointer positions back into byte offsets.
struct LineText {
    styled: StyledText,
    layout: Rc<RefCell<Option<TextLayout>>>,
}

impl LineText {
    fn new(styled: StyledText, layout: Rc<RefCell<Option<TextLayout>>>) -> Self {
        Self { styled, layout }
    }
}

impl IntoElement for LineText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for LineText {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        self.styled.request_layout(id, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.styled
            .prepaint(id, inspector_id, bounds, request_layout, window, cx);
        *self.layout.borrow_mut() = Some(self.styled.layout().clone());
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.styled.paint(
            id,
            inspector_id,
            bounds,
            request_layout,
            prepaint,
            window,
            cx,
        );
    }
}

struct PoemReader {
    document: Option<Document>,
    font_size: Pixels,
    pdf_zoom: f32,
    background: ReadingBackground,
    /// What the left button does in the reading area.
    pointer_mode: PointerMode,
    /// Sheet of the open Excel workbook being shown.
    excel_sheet: usize,
    /// Currently selected text, kept after the drag ends so it can be copied.
    selection: Option<Selection>,
    /// Lines of a Word document that can be selected, rebuilt on every render.
    document_lines: Vec<SelectableLine>,
    /// Cells of the current sheet that are selected.
    cell_selection: Option<CellSelection>,
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
    scroll_handle: ScrollHandle,
    /// Scroll position to apply on the next render.
    pending_scroll: Option<PendingScroll>,
    /// Set while the left button is down over the reading area.
    interaction: Option<Interaction>,
    /// Latest reading position, readable after the view is gone.
    latest_position: Rc<Cell<Option<f32>>>,
    store: Rc<RefCell<Store>>,
    _subscriptions: Vec<Subscription>,
}

impl PoemReader {
    fn new(
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

        // Window-wide shortcuts: copy the selection and print. The reading area
        // is not a focusable text input, so an interceptor is the simplest way
        // to notice them.
        let shortcut_subscription = {
            let view = cx.entity().downgrade();
            cx.intercept_keystrokes(move |event, window, cx| {
                let keystroke = &event.keystroke;
                if !(keystroke.modifiers.control || keystroke.modifiers.platform) {
                    return;
                }

                match keystroke.key.as_str() {
                    "c" => {
                        if view
                            .update(cx, |this, cx| this.copy_selection(cx))
                            .unwrap_or(false)
                        {
                            cx.stop_propagation();
                        }
                    }
                    "p" => {
                        view.update(cx, |this, cx| this.print_document(window, cx))
                            .ok();
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            })
        };

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

    fn persist_settings(&self) {
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
    fn schedule_position_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(SAVE_POSITION_DEBOUNCE).await;
            this.update_in(cx, |this, _window, _cx| {
                this.remember_position();
                this.persist_settings();
            })
            .ok();
        }));
    }

    /// Wire up the reading area: the left button either pans the content or
    /// selects text, depending on the pointer mode, and the right button opens
    /// the menu that switches between the two.
    fn reading_area(&self, element: Stateful<Div>, cx: &mut Context<Self>) -> impl IntoElement {
        let selecting = self.uses_text_selection();
        let panning = matches!(self.interaction, Some(Interaction::Pan { .. }));
        let cursor = if selecting {
            CursorStyle::IBeam
        } else if panning {
            CursorStyle::ClosedHand
        } else {
            CursorStyle::OpenHand
        };

        let element = element
            .cursor(cursor)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                    this.begin_interaction(event, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                this.update_interaction(event, cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseUpEvent, window, cx| {
                    this.end_interaction(window, cx);
                }),
            );

        let view = cx.entity();
        let selectable = self.supports_selection();
        let pointer_mode = self.pointer_mode;
        let has_selection = self.has_selection();
        element.context_menu(move |menu, _window, _cx| {
            build_reading_menu(menu, view.clone(), selectable, pointer_mode, has_selection)
        })
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

    fn is_excel(&self) -> bool {
        matches!(
            self.document.as_ref().map(|document| &document.content),
            Some(DocumentContent::Excel(_))
        )
    }

    /// Formats that hold text the reader can drag over; PDF pages are images.
    fn supports_selection(&self) -> bool {
        matches!(
            self.document.as_ref().map(|document| &document.content),
            Some(DocumentContent::Text(_))
                | Some(DocumentContent::Word(_))
                | Some(DocumentContent::Excel(_))
        )
    }

    fn uses_text_selection(&self) -> bool {
        self.supports_selection() && self.pointer_mode == PointerMode::Select
    }

    fn has_selection(&self) -> bool {
        self.cell_selection.is_some()
            || self
                .selection
                .is_some_and(|selection| !selection.is_empty())
    }

    fn begin_interaction(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        if self.uses_text_selection() {
            if self.is_excel() {
                if let Some(cell) = self.cell_at(event.position) {
                    self.selection = None;
                    self.cell_selection = Some(CellSelection {
                        anchor: cell,
                        head: cell,
                    });
                    self.interaction = Some(Interaction::SelectCells);
                    cx.notify();
                    return;
                }
            } else if let Some(position) = self.text_position_at(event.position) {
                self.cell_selection = None;
                self.selection = Some(Selection {
                    anchor: position,
                    head: position,
                });
                self.interaction = Some(Interaction::Select);
                cx.notify();
                return;
            }
        }

        // A press anywhere in pan mode drops the previous selection.
        self.selection = None;
        self.cell_selection = None;
        self.interaction = Some(Interaction::Pan {
            origin: event.position,
            offset: self.scroll_handle.offset(),
        });
        cx.notify();
    }

    fn update_interaction(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if event.pressed_button != Some(MouseButton::Left) {
            // The release happened outside the window, or the drag was cancelled.
            self.interaction = None;
            return;
        }

        match self.interaction {
            Some(Interaction::Pan { origin, offset }) => {
                // The content follows the pointer instead of the pointer moving
                // over the content.
                self.scroll_handle
                    .set_offset(offset + (event.position - origin));
                cx.notify();
            }
            Some(Interaction::Select) => {
                let Some(position) = self.text_position_at(event.position) else {
                    return;
                };
                let Some(selection) = self.selection.as_mut() else {
                    return;
                };
                if selection.head != position {
                    selection.head = position;
                    cx.notify();
                }
            }
            Some(Interaction::SelectCells) => {
                let Some(cell) = self.cell_at(event.position) else {
                    return;
                };
                let Some(selection) = self.cell_selection.as_mut() else {
                    return;
                };
                if selection.head != cell {
                    selection.head = cell;
                    cx.notify();
                }
            }
            None => {}
        }
    }

    fn end_interaction(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.interaction.take() {
            Some(Interaction::Pan { .. }) => self.schedule_position_save(window, cx),
            Some(Interaction::Select) | Some(Interaction::SelectCells) => cx.notify(),
            None => {}
        }
    }

    fn set_pointer_mode(&mut self, mode: PointerMode, cx: &mut Context<Self>) {
        if self.pointer_mode == mode {
            return;
        }
        self.pointer_mode = mode;
        self.store.borrow_mut().settings_mut().pointer_mode = mode;
        self.persist_settings();
        self.interaction = None;
        cx.notify();
    }

    /// Map a pointer position to the line and byte offset under it.
    fn text_position_at(&self, point: Point<Pixels>) -> Option<TextPos> {
        match self.document.as_ref().map(|document| &document.content) {
            Some(DocumentContent::Text(text)) => self.text_row_at(text, point),
            Some(DocumentContent::Word(_)) => self.document_line_at(point),
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
    fn cell_at(&self, point: Point<Pixels>) -> Option<(u32, u32)> {
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
            Some(DocumentContent::Word(_)) => self.lines_text(selection),
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
    fn copy_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(text) = self.selected_text() else {
            return false;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        true
    }

    /// Print the open document, showing the system print dialog first.
    fn print_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
            DocumentContent::Word(word) => PrintContent::Word(word.clone()),
            DocumentContent::Excel(workbook) => PrintContent::Excel(workbook.clone()),
        };

        Some(PrintJob {
            title: document.file_name.to_string(),
            content,
        })
    }

    /// Ask the platform for a file and load it in the background.
    fn load_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("打开".into()),
        });

        self.load_task = Some(cx.spawn_in(window, async move |view, cx| {
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

            // Parsing and rasterizing a PDF can take a moment, so keep it off
            // the UI thread.
            let read_path = path.clone();
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

    fn set_background(&mut self, background: ReadingBackground, cx: &mut App) {
        self.background = background;
        self.store.borrow_mut().settings_mut().background = background;
        self.persist_settings();
        apply_theme_mode(background, cx);
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
            DocumentContent::Text(_) | DocumentContent::Word(_) | DocumentContent::Excel(_) => {
                self.scroll_handle.offset().y.as_f32()
            }
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
            Some(DocumentContent::Word(_)) => return self.render_word_body(window, foreground, cx),
            Some(DocumentContent::Excel(_)) => {
                return self.render_excel_body(window, foreground, cx);
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
            Some(DocumentContent::Word(document)) => document.clone(),
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
                        .child("使用菜单「文件 → 加载文件…」选择一个文本、PDF、Word 或 Excel 文件"),
                ),
        )
        .into_any_element()
}

// ---------------------------------------------------------------- Word layout

/// State shared by the Word block builders: the scale, the colours, and the
/// lines registered for selection while the blocks are built.
struct WordRender<'a> {
    scale: f32,
    foreground: Hsla,
    base: &'a Font,
    /// Colour painted behind the selected text.
    highlight: Hsla,
    selection: Option<Selection>,
    /// Selectable lines, in document order.
    lines: &'a mut Vec<SelectableLine>,
    next_paragraph: usize,
    next_row: usize,
}

impl WordRender<'_> {
    fn paragraph_id(&mut self) -> usize {
        self.next_paragraph += 1;
        self.next_paragraph
    }

    fn row_id(&mut self) -> usize {
        self.next_row += 1;
        self.next_row
    }

    /// Builds one text block and registers it as a selectable line.
    fn text_block(
        &mut self,
        text: String,
        mut runs: Vec<TextRun>,
        place: LinePlace,
        size: f32,
        line_height: f32,
        color: Hsla,
    ) -> AnyElement {
        let index = self.lines.len();
        if let Some(selected) = self
            .selection
            .and_then(|selection| selection.bytes_in_line(index, text.len()))
        {
            paint_selection(&mut runs, selected, self.highlight);
        }

        let slot = Rc::new(RefCell::new(None));
        let shared = SharedString::from(text);
        let styled = if runs.is_empty() {
            StyledText::new(shared.clone())
        } else {
            StyledText::new(shared.clone()).with_runs(runs)
        };
        self.lines.push(SelectableLine {
            text: shared,
            skip: place.skip,
            paragraph: place.paragraph,
            row: place.row,
            layout: slot.clone(),
        });

        div()
            .w_full()
            .text_size(px(size))
            .line_height(px(line_height))
            .text_color(color)
            .child(LineText::new(styled, slot))
            .into_any_element()
    }
}

/// Paints the selected byte range of a text block with the selection colour by
/// splitting the runs that overlap it.
fn paint_selection(runs: &mut Vec<TextRun>, selected: Range<usize>, highlight: Hsla) {
    if selected.is_empty() {
        return;
    }

    let mut painted = Vec::with_capacity(runs.len() + 2);
    let mut offset = 0usize;
    for run in runs.drain(..) {
        let end = offset + run.len;
        let from = selected.start.max(offset);
        let to = selected.end.min(end);
        if from >= to {
            painted.push(run);
        } else {
            if from > offset {
                let mut before = run.clone();
                before.len = from - offset;
                painted.push(before);
            }
            let mut middle = run.clone();
            middle.len = to - from;
            middle.background_color = Some(highlight);
            painted.push(middle);
            if to < end {
                let mut after = run.clone();
                after.len = end - to;
                painted.push(after);
            }
        }
        offset = end;
    }
    *runs = painted;
}

/// One block of a Word document, scaled from points to logical pixels.
fn word_block(block: &word::Block, render: &mut WordRender, row: Option<usize>) -> AnyElement {
    match block {
        word::Block::Paragraph(paragraph) => word_paragraph(paragraph, render, row),
        word::Block::Table(table) => word_table(table, render),
    }
}

fn word_paragraph(
    paragraph: &word::Paragraph,
    render: &mut WordRender,
    row: Option<usize>,
) -> AnyElement {
    let scale = render.scale;
    let foreground = render.foreground;
    let base = render.base;
    let size = (paragraph.draw_size() * scale).max(4.0);
    let line_height = size * 1.3 * paragraph.line_spacing.max(0.8);
    let color = paragraph
        .color
        .map(|color| rgb(color).into())
        .unwrap_or(foreground);
    let paragraph_id = render.paragraph_id();

    let mut face = match &paragraph.font {
        Some(name) => font(name.clone()),
        None => base.clone(),
    };
    if paragraph.bold {
        face = face.bold();
    }
    if paragraph.italic {
        face = face.italic();
    }

    let mut text = String::new();
    let mut runs: Vec<TextRun> = Vec::new();
    let mut blocks: Vec<AnyElement> = Vec::new();
    // The first-line indent only covers the first block of the paragraph.
    let mut skip = 0usize;

    // Chinese documents indent the first line rather than the whole paragraph.
    if paragraph.first_line > 0.0 && paragraph.size > 0.0 {
        let spaces = ((paragraph.first_line / paragraph.size).round() as usize).clamp(1, 8);
        let indent = "\u{3000}".repeat(spaces);
        skip = indent.len();
        push_text_run(
            &mut text, &mut runs, &indent, &face, color, None, false, false,
        );
    }
    if let Some(marker) = paragraph.marker.as_deref() {
        push_text_run(
            &mut text,
            &mut runs,
            &format!("{marker}\u{00a0}"),
            &face,
            color,
            None,
            false,
            false,
        );
    }

    for run in &paragraph.runs {
        if let Some(image) = &run.image {
            if !text.is_empty() {
                blocks.push(render.text_block(
                    std::mem::take(&mut text),
                    std::mem::take(&mut runs),
                    LinePlace {
                        paragraph: paragraph_id,
                        row,
                        skip: std::mem::take(&mut skip),
                    },
                    size,
                    line_height,
                    color,
                ));
            }
            let image_element = img(image.image.clone())
                .w(px((image.width * scale).max(1.0)))
                .h(px((image.height * scale).max(1.0)))
                .into_any_element();
            // Images have no text alignment of their own, so the paragraph's
            // alignment is applied by centring the image in the column.
            blocks.push(match paragraph.align {
                word::Align::Center => div()
                    .w_full()
                    .flex()
                    .justify_center()
                    .child(image_element)
                    .into_any_element(),
                word::Align::Right => div()
                    .w_full()
                    .flex()
                    .justify_end()
                    .child(image_element)
                    .into_any_element(),
                _ => image_element,
            });
            continue;
        }

        let mut run_face = match &run.font {
            Some(name) => font(name.clone()),
            None => match &paragraph.font {
                Some(name) => font(name.clone()),
                None => base.clone(),
            },
        };
        if run.bold || paragraph.bold {
            run_face = run_face.bold();
        }
        if run.italic || paragraph.italic {
            run_face = run_face.italic();
        }
        let run_color = run
            .color
            .or(paragraph.color)
            .map(|color| rgb(color).into())
            .unwrap_or(foreground);
        push_text_run(
            &mut text,
            &mut runs,
            &run.text,
            &run_face,
            run_color,
            run.highlight.map(|color| rgb(color).into()),
            run.underline,
            run.strike,
        );
    }

    if !text.is_empty() {
        blocks.push(render.text_block(
            text,
            runs,
            LinePlace {
                paragraph: paragraph_id,
                row,
                skip,
            },
            size,
            line_height,
            color,
        ));
    }
    if blocks.is_empty() {
        blocks.push(div().h(px(line_height)).into_any_element());
    }

    div()
        .w_full()
        .pl(px(paragraph.indent * scale))
        .pt(px(paragraph.space_before * scale))
        .pb(px(paragraph.space_after * scale))
        .text_align(match paragraph.align {
            word::Align::Center => TextAlign::Center,
            word::Align::Right => TextAlign::Right,
            _ => TextAlign::Left,
        })
        .children(blocks)
        .into_any_element()
}

fn push_text_run(
    text: &mut String,
    runs: &mut Vec<TextRun>,
    content: &str,
    face: &Font,
    color: Hsla,
    background: Option<Hsla>,
    underline: bool,
    strike: bool,
) {
    if content.is_empty() {
        return;
    }
    runs.push(TextRun {
        len: content.len(),
        font: face.clone(),
        color,
        background_color: background,
        underline: underline.then_some(UnderlineStyle {
            thickness: px(1.),
            color: None,
            wavy: false,
        }),
        strikethrough: strike.then_some(StrikethroughStyle {
            thickness: px(1.),
            color: None,
        }),
    });
    text.push_str(content);
}

fn word_table(table: &word::Table, render: &mut WordRender) -> AnyElement {
    let scale = render.scale;
    let border = render.foreground.opacity(0.35);
    let mut rows = Vec::new();
    for row in &table.rows {
        let mut cells = Vec::new();
        let mut column = 0usize;
        let row_id = render.row_id();
        for cell in row {
            let span = cell.span.max(1);
            let width = (column..column + span)
                .map(|index| table.columns.get(index).copied().unwrap_or(60.0) * scale)
                .sum::<f32>()
                .max(24.0);
            column += span;

            let mut element = div().flex_shrink_0().w(px(width)).px(px(5.)).py(px(3.));
            if let Some(fill) = cell.fill {
                element = element.bg(rgb(fill));
            }
            if table.borders {
                element = element.border_1().border_color(border);
            }
            let mut blocks = Vec::with_capacity(cell.blocks.len());
            for block in &cell.blocks {
                blocks.push(word_block(block, render, Some(row_id)));
            }
            element = element.child(v_flex().w_full().children(blocks));
            cells.push(element.into_any_element());
        }
        rows.push(
            h_flex()
                .w_full()
                .items_stretch()
                .children(cells)
                .into_any_element(),
        );
    }

    v_flex().w_full().children(rows).into_any_element()
}

// --------------------------------------------------------------- Excel layout

/// State shared by the Excel builders: the sheet, its styles and the current
/// selection.
struct ExcelRender<'a> {
    sheet: &'a excel::Sheet,
    styles: &'a [excel::CellStyle],
    zoom: f32,
    foreground: Hsla,
    base: &'a Font,
    /// Colour painted behind the selected cells.
    highlight: Hsla,
    selection: Option<CellSelection>,
}

/// The gridlines and cell elements that are visible in a sheet.
fn excel_cells(
    render: ExcelRender,
    first_row: usize,
    last_row: usize,
    first_column: usize,
    last_column: usize,
    // Sheet coordinates of each element, `None` for the gridlines.
    positions: &mut Vec<Option<(u32, u32)>>,
) -> Vec<AnyElement> {
    let sheet = render.sheet;
    let zoom = render.zoom;
    let foreground = render.foreground;
    let mut elements = Vec::new();
    let grid = foreground.opacity(0.15);
    let rows = sheet.row_count();
    let columns = sheet.column_count();

    // Gridlines for the visible part of the sheet, painted under the cells.
    for row in first_row..=last_row.min(rows.saturating_sub(1)) {
        let top = sheet.row_tops[row] * zoom;
        elements.push(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(px(top))
                .h(px(1.))
                .bg(grid)
                .into_any_element(),
        );
        positions.push(None);
    }
    for column in first_column..=last_column.min(columns.saturating_sub(1)) {
        let left = sheet.column_lefts[column] * zoom;
        elements.push(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(left))
                .w(px(1.))
                .bg(grid)
                .into_any_element(),
        );
        positions.push(None);
    }

    let start = sheet
        .rows
        .partition_point(|row| (row.index as usize) < first_row);
    for row in &sheet.rows[start..] {
        let row_index = row.index as usize;
        if row_index > last_row {
            break;
        }
        let cells_start = row
            .cells
            .partition_point(|cell| (cell.column as usize) < first_column);
        for cell in &row.cells[cells_start..] {
            let column = cell.column as usize;
            if column > last_column || column >= columns {
                break;
            }
            if sheet.is_covered(row.index, cell.column) {
                continue;
            }

            let (row_span, column_span) = sheet
                .merge_at(row.index, cell.column)
                .map(|merge| (merge.rows as usize, merge.columns as usize))
                .unwrap_or((1, 1));
            let bottom = (row_index + row_span).min(rows);
            let right = (column + column_span).min(columns);

            let left = sheet.column_lefts[column] * zoom;
            let top = sheet.row_tops[row_index] * zoom;
            let width = (sheet.column_lefts[right] - sheet.column_lefts[column]) * zoom;
            let height = (sheet.row_tops[bottom] - sheet.row_tops[row_index]) * zoom;

            elements.push(excel_cell(
                cell,
                &render,
                render
                    .selection
                    .is_some_and(|selection| selection.contains(row.index, cell.column)),
                left,
                top,
                width,
                height,
            ));
            positions.push(Some((row.index, cell.column)));
        }
    }

    elements
}

fn excel_cell(
    cell: &excel::Cell,
    render: &ExcelRender,
    selected: bool,
    left: f32,
    top: f32,
    width: f32,
    height: f32,
) -> AnyElement {
    let zoom = render.zoom;
    let foreground = render.foreground;
    let base = render.base;
    let style = render.styles.get(cell.style);

    let mut element = div()
        .absolute()
        .left(px(left))
        .top(px(top))
        .w(px(width))
        .h(px(height))
        .overflow_hidden();

    // The selection covers the cell fill, so the selected cells stand out even
    // when they are shaded.
    if selected {
        element = element.bg(render.highlight);
    } else if let Some(fill) = style.and_then(|style| style.fill) {
        element = element.bg(rgb(fill));
    }

    if let Some(style) = style {
        let border_color = [
            style.border_top,
            style.border_right,
            style.border_bottom,
            style.border_left,
        ]
        .into_iter()
        .flatten()
        .next();
        if let Some(color) = border_color {
            element = element.border_color(rgb(color));
            if style.border_top.is_some() {
                element = element.border_t_1();
            }
            if style.border_bottom.is_some() {
                element = element.border_b_1();
            }
            if style.border_left.is_some() {
                element = element.border_l_1();
            }
            if style.border_right.is_some() {
                element = element.border_r_1();
            }
        }
    }

    let size = (style.map(|style| style.size).unwrap_or(15.0) * zoom).max(4.0);
    let color = style
        .and_then(|style| style.color)
        .map(|color| rgb(color).into())
        .unwrap_or(foreground);
    let align = style
        .map(|style| style.horizontal(cell.numeric))
        .unwrap_or_default();
    let valign = style.map(|style| style.valign).unwrap_or_default();
    let wrap = style.is_some_and(|style| style.wrap);
    let bold = style.is_some_and(|style| style.bold);
    let italic = style.is_some_and(|style| style.italic);
    let underline = style.is_some_and(|style| style.underline);

    let mut text = div()
        .size_full()
        .flex()
        .flex_col()
        .px(px(3.))
        .py(px(1.))
        .text_size(px(size))
        .line_height(px(size * 1.25))
        .text_color(color)
        .text_align(match align {
            excel::HorizontalAlign::Center => TextAlign::Center,
            excel::HorizontalAlign::Right => TextAlign::Right,
            excel::HorizontalAlign::Left => TextAlign::Left,
        })
        .child(cell.text.clone());

    text = match valign {
        excel::VerticalAlign::Top => text.justify_start(),
        excel::VerticalAlign::Center => text.justify_center(),
        excel::VerticalAlign::Bottom => text.justify_end(),
    };
    if wrap {
        text = text.whitespace_normal();
    } else {
        text = text.whitespace_nowrap();
    }
    if bold || italic || underline {
        if bold {
            text = text.font_weight(FontWeight::BOLD);
        }
        if italic {
            text = text.italic();
        }
        if underline {
            text = text.underline();
        }
        text = text.font_family(base.family.clone());
    }

    element.child(text).into_any_element()
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

/// The right-click menu of the reading area: switches between panning and
/// selecting, and copies the selection.
fn build_reading_menu(
    menu: PopupMenu,
    view: Entity<PoemReader>,
    selectable: bool,
    pointer_mode: PointerMode,
    has_selection: bool,
) -> PopupMenu {
    let menu = menu.min_w(px(240.));

    if !selectable {
        // PDF pages are images and Word / Excel documents are laid out from
        // their own model, so only panning makes sense here.
        return menu
            .item(
                PopupMenuItem::new("拖拽平移（此文档不支持划选）")
                    .checked(true)
                    .disabled(true),
            )
            .separator()
            .item(PopupMenuItem::new("复制所选文字（Ctrl+C）").disabled(true));
    }

    let pan = view.clone();
    let select = view.clone();
    let copy = view.clone();

    menu.item(
        PopupMenuItem::new("拖拽平移")
            .checked(pointer_mode == PointerMode::Pan)
            .on_click(move |_, _window, cx| {
                pan.update(cx, |this, cx| this.set_pointer_mode(PointerMode::Pan, cx));
            }),
    )
    .item(
        PopupMenuItem::new("划选文字")
            .checked(pointer_mode == PointerMode::Select)
            .on_click(move |_, _window, cx| {
                select.update(cx, |this, cx| {
                    this.set_pointer_mode(PointerMode::Select, cx)
                });
            }),
    )
    .separator()
    .item(
        PopupMenuItem::new("复制所选文字（Ctrl+C）")
            .disabled(!has_selection)
            .on_click(move |_, _window, cx| {
                copy.update(cx, |this, cx| {
                    this.copy_selection(cx);
                });
            }),
    )
}

/// Rasterize one page off the UI thread and put it in the view's cache.
fn spawn_pdf_render(
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
                    .child("· 支持 Word（.docx）：段落样式、字符格式、列表、表格与图片")
                    .child("· 支持 Excel（.xlsx）：单元格字体、颜色、边框、数字格式与多工作表")
                    .child("· 「文件 → 加载文件…」打开本地文件")
                    .child("· 纯文本按行渲染，PDF 按页渲染，Word / Excel 按自身排版绘制")
                    .child("· 所有文档都支持滚轮与鼠标拖拽平移")
                    .child("· 纯文本可用右键菜单切换「拖拽平移 / 划选文字」")
                    .child("· 划选后用 Ctrl+C 或右键菜单「复制所选文字」复制")
                    .child("· 「文件 → 打印…」或 Ctrl+P 打印文本、PDF、Word 与 Excel")
                    .child("· 打印对话框中可选「页码」只打印指定页或页码范围")
                    .child("· 「字体大小」调整正文；PDF / Word / Excel 为「页面缩放」")
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

/// Load a text, PDF, Word or Excel document from disk.
fn load_document(path: &Path) -> Result<(DocumentKind, LoadedContent), String> {
    let bytes = read_file_bytes(path)?;
    if is_pdf(path, &bytes) {
        return PdfSource::load(bytes)
            .map(|source| (DocumentKind::Pdf, LoadedContent::Pdf(Arc::new(source))));
    }
    if bytes.starts_with(b"PK\x03\x04") {
        return load_office(bytes);
    }
    if is_compound_file(&bytes) {
        return load_compound_office(path, bytes);
    }

    Ok((DocumentKind::Text, LoadedContent::Text(decode_text(bytes))))
}

/// Word and Excel documents are ZIP packages; which one it is depends on the
/// parts inside.
fn load_office(bytes: Vec<u8>) -> Result<(DocumentKind, LoadedContent), String> {
    let package = Package::open(&bytes)?;
    if package.has("word/document.xml") {
        let document = word::load(&package)?;
        Ok((DocumentKind::Word, LoadedContent::Word(Arc::new(document))))
    } else if package.has("xl/workbook.xml") {
        let workbook = excel::load(&package)?;
        Ok((
            DocumentKind::Excel,
            LoadedContent::Excel(Arc::new(workbook)),
        ))
    } else {
        Err(String::from("这是一个压缩包，但不是 Word 或 Excel 文档"))
    }
}

fn read_file_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = std::fs::metadata(path).map_err(|error| format!("无法读取文件信息：{error}"))?;
    if (is_pdf_path(path) || is_office_path(path)) && metadata.len() > MAX_DOCUMENT_BYTES {
        return Err(format!(
            "文件过大（{:.1} MB），最大支持 {} MB",
            metadata.len() as f64 / (1024.0 * 1024.0),
            MAX_DOCUMENT_BYTES / (1024 * 1024)
        ));
    }

    std::fs::read(path).map_err(|error| format!("读取文件失败：{error}"))
}

fn is_pdf_path(path: &Path) -> bool {
    has_extension(path, &["pdf"])
}

fn is_office_path(path: &Path) -> bool {
    has_extension(
        path,
        &["docx", "xlsx", "docm", "xlsm", "dotx", "xltx", "doc", "xls"],
    )
}

/// Legacy Word and Excel documents share the OLE2 container; which one it is
/// depends on the streams inside or on the file extension.
fn load_compound_office(
    path: &Path,
    bytes: Vec<u8>,
) -> Result<(DocumentKind, LoadedContent), String> {
    if has_extension(path, &["xls"]) {
        return xls::load(bytes).map(|workbook| {
            (
                DocumentKind::Excel,
                LoadedContent::Excel(Arc::new(workbook)),
            )
        });
    }
    if has_extension(path, &["doc"]) {
        return doc::load(bytes)
            .map(|document| (DocumentKind::Word, LoadedContent::Word(Arc::new(document))));
    }

    // The extension is unknown, so pick the reader from the streams inside.
    let container = cfb::CompoundFile::open(bytes)?;
    if container.has("Workbook") || container.has("Book") {
        return Err(String::from("这是一个 Excel 文档，请使用 .xls 扩展名"));
    }
    if container.has("WordDocument") {
        return Err(String::from("这是一个 Word 文档，请使用 .doc 扩展名"));
    }
    Err(String::from("这是一个复合文档，但不是 Word 或 Excel 文件"))
}

fn is_compound_file(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1])
}

fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension().is_some_and(|extension| {
        extensions
            .iter()
            .any(|candidate| extension.eq_ignore_ascii_case(candidate))
    })
}

fn is_pdf(path: &Path, bytes: &[u8]) -> bool {
    is_pdf_path(path) || bytes.starts_with(b"%PDF-")
}

/// Decode a text file, accepting UTF-8 (with or without BOM) and falling back
/// to GB18030 so that Chinese text saved by older Windows editors works.
fn decode_text(mut bytes: Vec<u8>) -> String {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        bytes.drain(..3);
    }

    // Taking the buffer over keeps a large UTF-8 file from being copied once more.
    match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => {
            let bytes = error.into_bytes();
            let (decoded, _, had_errors) = encoding_rs::GB18030.decode(&bytes);
            if had_errors {
                String::from_utf8_lossy(&bytes).into_owned()
            } else {
                decoded.into_owned()
            }
        }
    }
}
