//! 打印预览窗口。
//!
//! 预览在单独的窗口里打开，主窗口的阅读位置与选择都不受影响。预览把打印排版
//! 原样画到离屏的纸上：纸张尺寸取自默认打印机，绘制走的是 `print` 与
//! `print_office` 里同一套代码，所以预览显示的页数和每一行落在哪里，就是打印
//! 出来的样子。
//!
//! 等待都放在后台：纸张尺寸启动时先问一次，打开预览立刻就能排版；排版只排到
//! 当前屏的最后一页就先显示，剩下的页数另外补。位图按需生成：只有看得见的页
//! 才真的画，且只保留最近的几页。一张 A4 在 144 dpi 下约 8 MB，十几页就占满
//! 预算，所以缓存按页数和高低像素双重限制。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::component::{
    Icon, IconName, Selectable as _, TitleBar, WindowExt as _,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    notification::Notification,
    scroll::{Scrollbar, ScrollbarMode},
    v_flex,
};
use gpui_kit::*;
use image::Frame;

use crate::print::{
    self, ImageAnchor, ImagePrint, ImageScale, Orientation, PageRange, PagesPerSheet,
    PrintContent, PrintFit, PrintJob, SheetGeometry,
};
use crate::settings::ReadingBackground;
use crate::{APP_ICON, PDF_PAGE_GAP, PDF_PAGE_MARGIN, PDF_TOP_PADDING};

/// 页号按钮的小图标：一个字母「n」，对应页脚印的「n / m」。
const PAGE_NUMBER_ICON: &[u8] = include_bytes!("../assets/icons/page-number.svg");

/// 预览位图的分辨率。位图只用来显示，144 dpi 缩到屏幕宽度仍然清晰。
pub(crate) const PREVIEW_DPI: f32 = 144.0;

/// 打印「当前视图」时希望达到的分辨率，以及上限。
///
/// 放得越大，能印的字就越少，需要从同一块纸里取的像素也就越少；把目标定在
/// 300 dpi、最多 600 dpi，既能印出放大的细节，又不会为了裁一小块而画一整页
/// 巨图。
const VISIBLE_PRINT_DPI: f32 = 300.0;
const VISIBLE_PRINT_DPI_MAX: f32 = 600.0;

/// 画一页放大版位图的内存上限，超过就降低分辨率。
const MAX_VISIBLE_BYTES: f32 = 96.0 * 1024.0 * 1024.0;

/// 预览时最多保留几页位图，以及总的像素预算。
const MAX_CACHED_PAGES: usize = 8;
const MAX_CACHED_PIXELS: usize = 28_000_000;

/// 预览可以缩放的区间。
const ZOOM_MIN: f32 = 0.5;
const ZOOM_MAX: f32 = 2.5;
const ZOOM_STEP: f32 = 1.25;

/// 数页数的那趟在自己的线程里跑，界面隔这么久来看一次数完没有。
const COUNT_POLL: Duration = Duration::from_millis(20);

/// 指针在按钮上停多久才弹提示。
///
/// 工具栏上一排按钮，扫过去就弹一片气泡反而看不清；停得略久一点，提示才像
/// 是「我想知道这个按钮干什么」的答复。比 gpui 默认的 500ms 慢一档。
const TOOLTIP_DELAY: Duration = Duration::from_millis(700);

/// 排版依据的纸张与打印机。
#[derive(Clone)]
pub(crate) struct PageInfo {
    pub(crate) sheet: SheetGeometry,
    /// 例如「210 × 297 毫米」，显示在工具栏上。
    pub(crate) paper: String,
    /// 版面所依据的打印机；没有打印机时为 `None`。
    pub(crate) printer: Option<String>,
}

/// 上次问到的纸张尺寸。
///
/// 默认打印机在一次运行期内几乎不会变，而向驱动要一次纸张要几十毫秒到几秒
/// 不等，所以启动时先问一次，之后打开预览就不必再等。
static PAPER: Mutex<Option<PageInfo>> = Mutex::new(None);

fn cached_paper() -> Option<PageInfo> {
    PAPER.lock().ok().and_then(|cached| cached.clone())
}

fn remember_paper(info: &PageInfo) {
    // A missing printer is not worth remembering: one may be added later in the
    // same session, and the question is cheap when nothing answers it.
    if info.printer.is_none() {
        return;
    }
    if let Ok(mut cached) = PAPER.lock() {
        *cached = Some(info.clone());
    }
}

/// 提前问一次纸张尺寸，省掉第一次打开预览的等待。
pub(crate) fn warm_paper() {
    if cached_paper().is_none() {
        remember_paper(&PageInfo::current());
    }
}

impl PageInfo {
    /// Ask the default printer for its paper size.
    ///
    /// The printer's own context is used so the preview is laid out on the
    /// paper the document would actually be printed on, hard margins included.
    /// Everything is then scaled to [`PREVIEW_DPI`], which is what the bitmaps
    /// are painted at.
    fn current() -> Self {
        #[cfg(windows)]
        if let Some(info) = printer_paper() {
            return info;
        }
        fallback()
    }

    /// The same paper, laid out the way `orientation` asks for.
    ///
    /// The driver reports the sheet the way it is set up by default, which is
    /// usually portrait; printing sideways really does hand the driver a turned
    /// sheet, so the preview turns the geometry the same way. Which way the
    /// geometry already lies decides what has to be done, so asking twice for
    /// the same orientation changes nothing.
    pub(crate) fn oriented(&self, orientation: Orientation) -> Self {
        if self.sheet.is_landscape() == (orientation == Orientation::Landscape) {
            return self.clone();
        }
        let sheet = self.sheet.turned();
        Self {
            sheet,
            paper: paper_label(&sheet),
            printer: self.printer.clone(),
        }
    }
}

/// 「297 × 210 毫米」，按纸面尺寸写，横过来之后数字也跟着换。
fn paper_label(sheet: &SheetGeometry) -> String {
    let width = sheet.width as f32 / PREVIEW_DPI * 25.4;
    let height = sheet.height as f32 / PREVIEW_DPI * 25.4;
    format!("{width:.0} × {height:.0} 毫米")
}

/// A4 with the printable area filling the sheet, for machines without a
/// printer: the layout still paginates, only the paper is a guess.
fn fallback() -> PageInfo {
    let width = (8.268 * PREVIEW_DPI).round() as i32;
    let height = (11.693 * PREVIEW_DPI).round() as i32;
    PageInfo {
        sheet: SheetGeometry::without_margins(width, height),
        paper: String::from("A4 · 210 × 297 毫米"),
        printer: None,
    }
}

#[cfg(windows)]
fn printer_paper() -> Option<PageInfo> {
    use windows::Win32::Graphics::Gdi::{
        CreateDCW, DeleteDC, GetDeviceCaps, HORZRES, LOGPIXELSX, LOGPIXELSY, PHYSICALHEIGHT,
        PHYSICALOFFSETX, PHYSICALOFFSETY, PHYSICALWIDTH, VERTRES,
    };
    use windows::Win32::Graphics::Printing::GetDefaultPrinterW;
    use windows::core::{PCWSTR, PWSTR, w};

    let mut needed = 0u32;
    let _ = unsafe { GetDefaultPrinterW(None, &mut needed) };
    if needed == 0 {
        return None;
    }

    let mut buffer = vec![0u16; needed as usize];
    if unsafe { GetDefaultPrinterW(Some(PWSTR(buffer.as_mut_ptr())), &mut needed) }.0 == 0 {
        return None;
    }
    let name = String::from_utf16_lossy(&buffer[..(needed as usize).saturating_sub(1)]);

    let dc = unsafe { CreateDCW(w!("WINSPOOL"), PCWSTR(buffer.as_ptr()), PCWSTR::null(), None) };
    if dc.0.is_null() {
        return None;
    }

    let dpi_x = unsafe { GetDeviceCaps(Some(dc), LOGPIXELSX) }.max(48);
    let dpi_y = unsafe { GetDeviceCaps(Some(dc), LOGPIXELSY) }.max(48);
    let printable_width = unsafe { GetDeviceCaps(Some(dc), HORZRES) };
    let printable_height = unsafe { GetDeviceCaps(Some(dc), VERTRES) };
    let physical_width = unsafe { GetDeviceCaps(Some(dc), PHYSICALWIDTH) };
    let physical_height = unsafe { GetDeviceCaps(Some(dc), PHYSICALHEIGHT) };
    let offset_x = unsafe { GetDeviceCaps(Some(dc), PHYSICALOFFSETX) };
    let offset_y = unsafe { GetDeviceCaps(Some(dc), PHYSICALOFFSETY) };
    let _ = unsafe { DeleteDC(dc) };

    if printable_width <= 0 || printable_height <= 0 {
        return None;
    }

    let scale_x = PREVIEW_DPI / dpi_x as f32;
    let scale_y = PREVIEW_DPI / dpi_y as f32;
    let printable_width = (printable_width as f32 * scale_x).round().max(1.0) as i32;
    let printable_height = (printable_height as f32 * scale_y).round().max(1.0) as i32;

    // Not every driver reports the sheet; without it the printable area is the
    // sheet, which is what printing does anyway.
    let sheet = if physical_width > 0 && physical_height > 0 {
        SheetGeometry {
            printable_width,
            printable_height,
            width: (physical_width as f32 * scale_x).round().max(1.0) as u32,
            height: (physical_height as f32 * scale_y).round().max(1.0) as u32,
            origin_x: (offset_x as f32 * scale_x).round() as i32,
            origin_y: (offset_y as f32 * scale_y).round() as i32,
        }
    } else {
        SheetGeometry::without_margins(printable_width, printable_height)
    };

    let paper = paper_label(&sheet);
    Some(PageInfo {
        sheet,
        paper,
        printer: Some(name),
    })
}

/// 一次排版交回的东西：总页数（提前收尾时为 `None`），加上画好的页。
type FinishedPages = (Option<usize>, Vec<(usize, Arc<RenderImage>)>);

/// 页面在滚动区域里的位置，单位是逻辑像素。
struct Layout {
    page: Size<f32>,
    content_width: f32,
    /// 画出来或占位的页数。
    count: usize,
}

impl Layout {
    /// 一页纸在屏幕上占多大。
    fn page_size(info: &PageInfo, viewport: Size<f32>, zoom: f32) -> Size<f32> {
        let width = ((viewport.width - 2.0 * PDF_PAGE_MARGIN) * zoom).max(200.0);
        let height = (width / info.sheet.ratio()).max(120.0);
        Size::new(width, height)
    }

    /// 总页数还不知道时，先按一屏再加两页占位，够滚动区域撑开。
    fn estimate(info: &PageInfo, viewport: Size<f32>, zoom: f32) -> usize {
        let page = Self::page_size(info, viewport, zoom);
        let step = page.height + PDF_PAGE_GAP;
        ((viewport.height / step).ceil() as usize).max(1) + 2
    }

    fn new(info: &PageInfo, viewport: Size<f32>, zoom: f32, count: usize) -> Self {
        let page = Self::page_size(info, viewport, zoom);
        Self {
            page,
            content_width: (page.width + 2.0 * PDF_PAGE_MARGIN).max(viewport.width),
            count: count.max(1),
        }
    }

    fn step(&self) -> f32 {
        self.page.height + PDF_PAGE_GAP
    }

    /// 一页纸左上角在内容里的位置。
    fn page_top(&self, index: usize) -> f32 {
        PDF_TOP_PADDING + index as f32 * self.step()
    }

    fn content_height(&self) -> f32 {
        2.0 * PDF_TOP_PADDING + self.count as f32 * self.step() - PDF_PAGE_GAP
    }

    /// 滚动位置落在第几页。
    fn page_at(&self, offset: f32) -> usize {
        let distance = (offset - PDF_TOP_PADDING).max(0.0);
        ((distance / self.step()).floor() as usize).min(self.count.saturating_sub(1))
    }

    /// 看得见的页，外加一页余量，翻页时不会看到空白。
    fn visible(&self, offset: f32, viewport_height: f32) -> std::ops::Range<usize> {
        let first = self.page_at(offset).saturating_sub(1);
        let last = self.page_at(offset + viewport_height).min(self.count - 1);
        first..(last + 1)
    }
}

/// 预览窗口里看得见的那一块纸。
///
/// 位置按页面的相对坐标记（0 是左/上，1 是右/下），这样同一块区域既能在显示
/// 的位图上找到，也能在放大重排后的位图上找到。
#[derive(Clone, Copy, Debug, PartialEq)]
struct Region {
    page: usize,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

/// 从一整页的位图里裁出 `region`，用作「打印当前视图」的一页。
fn crop_region(image: &image::RgbaImage, region: &Region) -> Option<image::RgbaImage> {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return None;
    }
    let left = (region.left.clamp(0.0, 1.0) * width as f32).floor() as u32;
    let top = (region.top.clamp(0.0, 1.0) * height as f32).floor() as u32;
    let right = ((region.right.clamp(0.0, 1.0) * width as f32).ceil() as u32).min(width);
    let bottom = ((region.bottom.clamp(0.0, 1.0) * height as f32).ceil() as u32).min(height);
    if right <= left + 1 || bottom <= top + 1 {
        return None;
    }
    Some(image::imageops::crop_imm(image, left, top, right - left, bottom - top).to_image())
}

/// 把 `pages` 拼成页码区间，相邻的页合成一段，排版时少走几趟。
fn ranges_of(pages: &[usize]) -> PageRange {
    let mut sorted = pages.to_vec();
    sorted.sort_unstable();
    sorted.dedup();

    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for index in sorted {
        // `PageRange` counts pages from one, the preview from zero.
        let page = index.min(u32::MAX as usize - 1) as u32 + 1;
        match ranges.last_mut() {
            Some(last) if page <= last.1.saturating_add(1) => last.1 = last.1.max(page),
            _ => ranges.push((page, page)),
        }
    }

    if ranges.is_empty() {
        PageRange::Pages(Vec::new())
    } else {
        PageRange::Pages(ranges)
    }
}

/// 预览编号，用来区分同一份文档的多次排版。
fn next_generation() -> u64 {
    thread_local! {
        static COUNTER: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }
    COUNTER.with(|counter| {
        let next = counter.get().wrapping_add(1);
        counter.set(next);
        next
    })
}

/// 预览窗口的大小与位置：盖在主窗口中间，比主窗口略小。
fn preview_bounds(owner: &Window) -> Bounds<Pixels> {
    let owner = owner.bounds();
    let width = (owner.size.width.as_f32() * 0.9).clamp(520.0, 1200.0);
    let height = (owner.size.height.as_f32() * 0.92).clamp(420.0, 1000.0);
    let origin = point(
        owner.origin.x + px((owner.size.width.as_f32() - width) * 0.5),
        owner.origin.y + px((owner.size.height.as_f32() - height) * 0.5),
    );
    Bounds::new(origin, size(px(width), px(height)))
}

/// 打开预览窗口。
///
/// `owner` 只用来决定新窗口摆在哪；预览读的是 `job` 里的快照，打开后即使主窗口
/// 换了文档也不受影响。窗口走 `gpui-kit` 的入口，根视图才会包上通知层与对话框层。
pub(crate) fn open(
    job: PrintJob,
    picture: Option<Arc<RenderImage>>,
    file_name: String,
    background: ReadingBackground,
    reader: WeakEntity<crate::PoemReader>,
    owner: &Window,
    cx: &mut App,
) -> Result<(AnyWindowHandle, Entity<PreviewWindow>), String> {
    let bounds = preview_bounds(owner);
    let title = format!("打印预览 — {file_name}");
    let (window, view) = gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(480.), px(360.))),
            ..TitleBar::window_options()
        },
        cx,
        move |window, cx| {
            window.set_window_title(&title);
            cx.new(move |_cx| PreviewWindow::new(job, picture, file_name, background, reader))
        },
    )
    .map_err(|error| error.to_string())?;

    window
        .update(cx, |_, window, _| window.activate_window())
        .ok();
    Ok((window, view))
}

/// 打印预览窗口的视图：一列排好的纸，加上缩放与打印。
pub(crate) struct PreviewWindow {
    job: PrintJob,
    /// 图片文件的那张纹理，和阅读窗口里用的是同一份。拖动画面时手上摆的就是
    /// 它：图片按新位置直接重画，不必等整页位图重排一遍。别的文档没有。
    picture: Option<Arc<RenderImage>>,
    file_name: String,
    background: ReadingBackground,
    /// 主窗口；预览里换了纸张方向要告诉它，免得关掉之后直接打印又变回纵向。
    reader: WeakEntity<crate::PoemReader>,
    /// 驱动报告的纸张，尚未按 `job.orientation` 转向。
    base: PageInfo,
    /// 排版实际使用的纸张：`base` 按当前方向摆好之后的样子。
    info: PageInfo,
    /// 纸张与可打印区域是从后台问默认打印机拿的；结果回来之前不排版，
    /// 否则会先按猜的 A4 排一遍，等真实纸张到了再全部重来。
    paper_known: bool,
    /// 已经向驱动问过一次，不重复问。
    paper_requested: bool,
    /// 本次预览的编号，用来丢掉迟到的排版结果。
    generation: u64,
    cache: HashMap<usize, Arc<RenderImage>>,
    /// 正在后台排版的页。
    rendering: HashSet<usize>,
    /// 正在后台数总页数。
    counting: bool,
    /// 已经数过一次，失败也不反复重试。
    counted: bool,
    /// 总页数，第一次完整排版后才知道。
    total: Option<usize>,
    /// 文档页总数。缩印时一张纸放好几页，版面按张数排（total），而页脚
    /// 「n / m」的 m 要用文档页数，与直接打印同一套页号，两者分开记。
    doc_total: Option<usize>,
    /// 排版失败，界面不再重试。
    failed: bool,
    zoom: f32,
    scroll_handle: ScrollHandle,
    /// 屏幕上这一页的尺寸，用来把指针的位移换成纸张上的像素。
    page_size: Size<f32>,
    /// 正在拖动画面设定基准点：按下时的指针位置与当时的基准点。
    dragging: Option<(Point<Pixels>, ImageAnchor)>,
    /// 画面挪过位置，屏幕上的页已经过时，等重画。
    image_dirty: bool,
    /// 画面挪动之后要重画、但屏幕上还留着旧图的页。
    stale: HashSet<usize>,
    /// 已经画出来的页没有页脚：画的时候文档总页数还不知道。等页数数完，
    /// 把它们标成待重画，补上「n / m」。
    footers_pending: bool,
}

impl PreviewWindow {
    fn new(
        job: PrintJob,
        picture: Option<Arc<RenderImage>>,
        file_name: String,
        background: ReadingBackground,
        reader: WeakEntity<crate::PoemReader>,
    ) -> Self {
        let paper = cached_paper();
        let base = paper.clone().unwrap_or_else(fallback);
        let info = base.oriented(job.orientation);
        Self {
            job,
            picture,
            file_name,
            background,
            reader,
            base,
            info,
            paper_known: paper.is_some(),
            paper_requested: paper.is_some(),
            generation: next_generation(),
            cache: HashMap::new(),
            rendering: HashSet::new(),
            counting: false,
            counted: false,
            total: None,
            doc_total: None,
            failed: false,
            zoom: 1.0,
            scroll_handle: ScrollHandle::new(),
            page_size: Size::new(1.0, 1.0),
            dragging: None,
            image_dirty: false,
            stale: HashSet::new(),
            footers_pending: false,
        }
    }

    /// 只有按纸张宽度排版的格式才谈得上「适应纸张」。
    fn fit_applies(&self) -> bool {
        matches!(
            self.job.content,
            PrintContent::Word(_) | PrintContent::Excel(_)
        )
    }

    /// 只有图片能缩放画面本身。
    fn image_applies(&self) -> bool {
        matches!(self.job.content, PrintContent::Image { .. })
    }

    /// 打印正在预览的这份文档。
    pub(crate) fn print_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        crate::run_print_job(self.job.clone(), window, cx);
    }

    /// 在「适应纸张」和「原始大小」之间切换。
    ///
    /// 两种模式的页宽不同，页里的每一行也就落在别处，所以已经画好的页要全部
    /// 丢掉重排——预览仍然显示的就是按新设置打印出来的样子。
    ///
    /// 换过的适应方式回传给主窗口：直接打印走的是主窗口记下的那一份，这里不
    /// 报过去的话，关掉预览再打印就又回到旧设置了。
    pub(crate) fn toggle_fit(&mut self, cx: &mut Context<Self>) {
        self.job.fit = self.job.fit.toggled();
        self.reader
            .update(cx, |reader, cx| reader.remember_print_fit(self.job.fit, cx))
            .ok();
        self.relayout(cx);
    }

    /// 页脚写不写「n / m」页号：开着的时候每一页底部都印上。
    pub(crate) fn toggle_page_number(&mut self, cx: &mut Context<Self>) {
        let on = !self.job.page_number;
        self.job.page_number = on;
        self.reader
            .update(cx, |reader, cx| reader.remember_page_number(on, cx))
            .ok();
        self.relayout(cx);
    }

    /// 图片怎么印：印多大、从画面哪一块开始、要不要拼成多页。
    ///
    /// 缩放与拼接会改变页数，所以整份预览要重排；挪动基准点只改变页上的画面，
    /// 走 [`Self::repaint_image`]。
    pub(crate) fn set_image_print(&mut self, print: ImagePrint, cx: &mut Context<Self>) {
        self.set_image_print_from(print, true, cx);
    }

    /// `report` 说明这次是预览自己发起的，要回写主窗口；主窗口发起时它自己
    /// 的实体正被借用着，再碰 `reader` 会撞上 GPUI 的重复借用而崩溃。
    pub(crate) fn set_image_print_from(
        &mut self,
        print: ImagePrint,
        report: bool,
        cx: &mut Context<Self>,
    ) {
        if self.job.image == print {
            return;
        }
        self.job.image = print;
        if report {
            self.reader
                .update(cx, |reader, cx| reader.remember_image_print(print, cx))
                .ok();
        }
        self.relayout(cx);
    }

    /// 图片印多大，其余设置保持不变。
    pub(crate) fn set_image_scale(&mut self, scale: ImageScale, cx: &mut Context<Self>) {
        let print = ImagePrint {
            scale,
            ..self.job.image
        };
        self.set_image_print(print, cx);
    }

    /// 放大后一页装不下时，是否分到多页再拼起来。
    pub(crate) fn toggle_image_poster(&mut self, cx: &mut Context<Self>) {
        let print = ImagePrint {
            poster: !self.job.image.poster,
            ..self.job.image
        };
        self.set_image_print(print, cx);
    }

    /// 画面在纸上挪了位置：页还是那些页，页上的图不一样了，重画即可。
    ///
    /// 拖动的每一帧不走这里——重排整页位图要几十毫秒，跟不上指针；那几帧摆的
    /// 是 [`Self::image_overlay`]，松开左键才来这里把位图画准。
    fn repaint_image(&mut self, cx: &mut Context<Self>) {
        self.image_dirty = true;
        self.generation = next_generation();
        self.rendering.clear();
        cx.notify();
    }

    /// 画面在纸上能挪动的余量，按预览的分辨率算；不能挪时为 `None`。
    fn image_pan(&self) -> Option<(f32, f32)> {
        self.image_page().map(|(_, layout)| layout.pan)
    }

    /// 页上真正能落墨的那一块，按纸张坐标算：可打印区域再去掉半英寸白边。
    ///
    /// 打印时图片就裁在这里面（见 [`crate::print_office::print_image`]）；拖动时
    /// 页上摆的那一层必须用同一块，松开左键换成位图才不会跳一下。
    fn image_area(&self) -> (f32, f32, f32, f32) {
        let sheet = self.info.sheet;
        let margin = PREVIEW_DPI * crate::print_office::MARGIN_INCHES;
        (
            sheet.origin_x as f32 + margin,
            sheet.origin_y as f32 + margin,
            (sheet.printable_width as f32 - 2.0 * margin).max(1.0),
            (sheet.printable_height as f32 - 2.0 * margin).max(1.0),
        )
    }

    /// 图片这一页的摆法：能落墨的那一块，和画面在里面的位置与大小。
    ///
    /// 只有画面比纸大时才谈得上挪动，所以别的排法都回 `None`；拼接分页的每一
    /// 页各是一块不同的画面，也不适用。
    fn image_page(&self) -> Option<((f32, f32, f32, f32), print::ImageLayout)> {
        let PrintContent::Image { pixels } = &self.job.content else {
            return None;
        };
        if self.job.image.poster || self.job.pages_per_sheet != PagesPerSheet::One {
            return None;
        }
        let area = self.image_area();
        let layout = print::image_layout(pixels.dimensions(), (area.2, area.3), self.job.image);
        (layout.pan.0 > 0.5 || layout.pan.1 > 0.5).then_some((area, layout))
    }

    /// 屏幕上的页比纸面放大了多少，用来在纸张坐标与页上坐标之间换算。
    fn page_scale(&self) -> (f32, f32) {
        let sheet = self.info.sheet;
        (
            self.page_size.width / sheet.width.max(1) as f32,
            self.page_size.height / sheet.height.max(1) as f32,
        )
    }

    /// 按下左键：从这一处开始拖动画面的位置。
    fn begin_image_drag(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        if self.image_pan().is_none() {
            return;
        }
        self.dragging = Some((event.position, self.job.image.anchor));
        cx.notify();
    }

    /// 拖动：把指针走过的距离换成画面在纸上的位移。
    ///
    /// 指针往右走，画面跟着往右走，也就是纸面取的是画面更靠左的那一块。
    /// 这里只挪基准点，界面据此重画一层图片，所以画面跟着指针走；整页位图等到
    /// 松开左键再画，那是打印时的那一份，画一次就够。
    fn drag_image(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some((start, anchor)) = self.dragging else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.dragging = None;
            return;
        }
        let Some((pan_x, pan_y)) = self.image_pan() else {
            return;
        };

        // 屏幕上的页面被缩放显示过，先把指针的位移换回纸张上的像素。
        let (scale_x, scale_y) = self.page_scale();
        let (to_sheet_x, to_sheet_y) = (1.0 / scale_x.max(1e-4), 1.0 / scale_y.max(1e-4));

        let mut next = anchor;
        if pan_x > 0.5 {
            let dx = (event.position.x - start.x).as_f32() * to_sheet_x;
            next.x = (anchor.x - dx / pan_x).clamp(0.0, 1.0);
        }
        if pan_y > 0.5 {
            let dy = (event.position.y - start.y).as_f32() * to_sheet_y;
            next.y = (anchor.y - dy / pan_y).clamp(0.0, 1.0);
        }

        if next == self.job.image.anchor {
            return;
        }
        self.job.image.anchor = next;
        if self.image_overlay().is_some() {
            // 页上摆的那一层跟着指针走，这是拖动的全部开销。
            cx.notify();
        } else {
            // 没有纹理可摆（理论上的兜底）：退回按新位置重画整页位图。
            self.repaint_image(cx);
        }
    }

    /// 松开左键：位置定下来，页上的位图按新的基准点画准，也告诉主窗口，关掉
    /// 预览后直接打印还是这一块。
    fn end_image_drag(&mut self, cx: &mut Context<Self>) {
        if self.dragging.take().is_none() {
            return;
        }
        let print = self.job.image;
        self.reader
            .update(cx, |reader, cx| reader.remember_image_print(print, cx))
            .ok();
        self.repaint_image(cx);
    }

    /// 拖动画面时页上多摆的一层：图片按当前的基准点直接画出来。
    ///
    /// 页上那张位图是按老的基准点画的，画面一挪就跟不上了；这一层盖在它上面，
    /// 指针走到哪画面跟到哪，等新位图画好（[`Self::repaint_image`]）再让位。
    /// 图比纸大的时候纸面取的就是图中一块，所以这一层裁在能落墨的那块里。
    fn image_overlay(&self) -> Option<AnyElement> {
        if self.dragging.is_none() && self.stale.is_empty() {
            return None;
        }
        let picture = self.picture.clone()?;
        let (area, layout) = self.image_page()?;
        let (left, top, area_width, area_height) = area;
        let (x, y) = print::image_origin(area, layout, self.job.image.anchor);
        let (scale_x, scale_y) = self.page_scale();

        Some(
            div()
                .absolute()
                .left(px(left * scale_x))
                .top(px(top * scale_y))
                .w(px(area_width * scale_x))
                .h(px(area_height * scale_y))
                .overflow_hidden()
                .bg(rgb(0xFFFFFF))
                .child(
                    img(picture)
                        .absolute()
                        .left(px((x - left) * scale_x))
                        .top(px((y - top) * scale_y))
                        .w(px(layout.drawn.0 * scale_x))
                        .h(px(layout.drawn.1 * scale_y)),
                )
                .into_any_element(),
        )
    }

    /// 把纸横过来，或者再竖回去。
    pub(crate) fn toggle_orientation(&mut self, cx: &mut Context<Self>) {
        self.set_orientation(self.job.orientation.toggled(), cx);
    }

    /// 每张纸印几页：2 合 1、4 合 1……
    ///
    /// 页还是原来那些页，只是缩小了铺在同一张纸上，所以整份文档要重排一遍。
    pub(crate) fn set_pages_per_sheet(&mut self, pages: PagesPerSheet, cx: &mut Context<Self>) {
        self.set_pages_per_sheet_from(pages, true, cx);
    }

    /// `report` 说明这次是预览自己发起的，要回写主窗口；主窗口发起时它自己
    /// 的实体正被借用着，再碰 `reader` 会撞上 GPUI 的重复借用而崩溃。
    pub(crate) fn set_pages_per_sheet_from(
        &mut self,
        pages: PagesPerSheet,
        report: bool,
        cx: &mut Context<Self>,
    ) {
        if self.job.pages_per_sheet == pages {
            return;
        }
        self.job.pages_per_sheet = pages;
        if report {
            self.reader
                .update(cx, |reader, cx| {
                    reader.remember_pages_per_sheet(pages, cx)
                })
                .ok();
        }
        self.relayout(cx);
    }

    /// 换纸张方向。
    ///
    /// 横过来的纸更宽也更矮，每行能放下的字和每页的行数都变了，所以预览要整页
    /// 重排；主窗口也记住这个方向，关掉预览之后直接打印还是同一张横纸。
    ///
    /// `report` 说明这次换向是预览自己发起的，要回写主窗口；主窗口发起时它
    /// 自己的实体正被借用着，再碰 `reader` 会撞上 GPUI 的重复借用而崩溃。
    pub(crate) fn set_orientation(
        &mut self,
        orientation: Orientation,
        cx: &mut Context<Self>,
    ) {
        self.set_orientation_from(orientation, true, cx);
    }

    pub(crate) fn set_orientation_from(
        &mut self,
        orientation: Orientation,
        report: bool,
        cx: &mut Context<Self>,
    ) {
        if self.job.orientation == orientation {
            return;
        }
        self.job.orientation = orientation;
        self.info = self.base.oriented(orientation);
        if report {
            self.reader
                .update(cx, |reader, cx| {
                    reader.remember_print_orientation(orientation, cx)
                })
                .ok();
        }
        self.relayout(cx);
    }

    /// 页面的排版依据变了：已经画好的页全部丢掉重排。
    fn relayout(&mut self, cx: &mut Context<Self>) {
        self.generation = next_generation();
        self.cache.clear();
        self.rendering.clear();
        self.stale.clear();
        self.image_dirty = false;
        self.footers_pending = false;
        self.counting = false;
        self.counted = false;
        self.total = None;
        self.doc_total = None;
        self.failed = false;
        cx.notify();
    }

    /// 打印预览里看得见的这一块。
    ///
    /// 放大的时候读者想印的是那一段的细节，而不是整页缩小；所以这里按当前的
    /// 缩放把这一页重排一遍，只裁下可见的那一块，再铺满一张纸打印。
    pub(crate) fn print_visible(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let viewport = self.scroll_viewport(window);
        let Some(region) = self.visible_region(viewport) else {
            window.push_notification(Notification::info("预览里没有可见的页面"), cx);
            return;
        };

        let sheet = self.info.sheet;
        // 打印视图印的是单独一块，不参与多页合一。
        let job = PrintJob {
            pages_per_sheet: PagesPerSheet::One,
            ..self.job.clone()
        };
        let dpi = self.visible_dpi(&region);

        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let factor = dpi / PREVIEW_DPI;
                    let scaled = SheetGeometry {
                        printable_width: (sheet.printable_width as f32 * factor).round() as i32,
                        printable_height: (sheet.printable_height as f32 * factor).round() as i32,
                        width: (sheet.width as f32 * factor).round().max(1.0) as u32,
                        height: (sheet.height as f32 * factor).round().max(1.0) as u32,
                        origin_x: (sheet.origin_x as f32 * factor).round() as i32,
                        origin_y: (sheet.origin_y as f32 * factor).round() as i32,
                    };
                    let range = PageRange::Pages(vec![(
                        region.page.min(u32::MAX as usize - 1) as u32 + 1,
                        region.page.min(u32::MAX as usize - 1) as u32 + 1,
                    )]);
                    let (_, pages) =
                        print::render_pages(&job, scaled, dpi, &range, Some(region.page), None)?;
                    let (_, page) = pages
                        .into_iter()
                        .next()
                        .ok_or_else(|| String::from("这一页没有画出来"))?;
                    crop_region(&page, &region)
                        .ok_or_else(|| String::from("可见区域太小，无法不打印"))
                })
                .await;

            match result {
                Ok(pixels) => {
                    this.update_in(cx, move |this, window, cx| {
                        let job = PrintJob {
                            title: format!("{} · 当前视图", this.job.title),
                            content: PrintContent::Image {
                                pixels: Arc::new(pixels),
                            },
                            fit: PrintFit::FitToPaper,
                            orientation: this.job.orientation,
                            pages_per_sheet: PagesPerSheet::One,
                            // 这一张是裁下来的视图，铺满纸才是它的用途。
                            image: ImagePrint::default(),
                            // 裁下来的不是整页，页号没有意义。
                            page_number: false,
                        };
                        crate::run_print_job(job, window, cx);
                    })
                    .ok();
                }
                Err(message) => {
                    this.update_in(cx, move |_, window, cx| {
                        window.push_notification(Notification::error(message), cx);
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    /// 可见区域落在第几页的哪一块。
    fn visible_region(&self, viewport: Size<f32>) -> Option<Region> {
        let count = self.total.unwrap_or_else(|| {
            Layout::estimate(&self.info, viewport, self.zoom)
        });
        let layout = Layout::new(&self.info, viewport, self.zoom, count);
        let offset = self.scroll_offset();
        let x_offset = (-self.scroll_handle.offset().x.as_f32()).max(0.0);

        // The page under the middle of the window is the one being looked at,
        // which keeps a straddling view from picking the sliver at the top.
        let page = layout.page_at(offset + viewport.height * 0.5);
        let top = layout.page_top(page);
        let page_left = ((layout.content_width - layout.page.width) * 0.5).max(0.0);

        let y0 = (offset - top).max(0.0);
        let y1 = (offset + viewport.height - top).min(layout.page.height);
        let x0 = (x_offset - page_left).max(0.0);
        let x1 = (x_offset + viewport.width - page_left).min(layout.page.width);
        if y1 <= y0 + 1.0 || x1 <= x0 + 1.0 {
            return None;
        }

        Some(Region {
            page,
            left: x0 / layout.page.width,
            right: x1 / layout.page.width,
            top: y0 / layout.page.height,
            bottom: y1 / layout.page.height,
        })
    }

    /// 让裁下来的那一块铺满一张纸时仍然清晰的分辨率。
    fn visible_dpi(&self, region: &Region) -> f32 {
        let sheet = self.info.sheet;
        let page_inches = sheet.width as f32 / PREVIEW_DPI;
        let crop_inches = ((region.right - region.left) * page_inches).max(0.05);
        let paper_inches = sheet.printable_width as f32 / PREVIEW_DPI;

        let mut dpi = (paper_inches * VISIBLE_PRINT_DPI / crop_inches)
            .clamp(PREVIEW_DPI, VISIBLE_PRINT_DPI_MAX);
        // A big sheet at a high resolution would need hundreds of megabytes for
        // one page; back off until the bitmap is affordable.
        let bytes = |dpi: f32| {
            (sheet.width as f32 * dpi / PREVIEW_DPI)
                * (sheet.height as f32 * dpi / PREVIEW_DPI)
                * 4.0
        };
        while dpi > PREVIEW_DPI && bytes(dpi) > MAX_VISIBLE_BYTES {
            dpi *= 0.8;
        }
        dpi
    }

    /// 缩小或放大，`factor` 小于 1 是缩小。
    pub(crate) fn zoom_by(&mut self, factor: f32, cx: &mut Context<Self>) {
        self.set_zoom(self.zoom * factor, cx);
    }

    fn set_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        let zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        if (zoom - self.zoom).abs() < 0.001 {
            return;
        }
        // The pages scale with the zoom, so the same relative position keeps the
        // reader on the same line.
        let offset = self.scroll_offset() * zoom / self.zoom;
        self.zoom = zoom;
        self.scroll_handle.set_offset(point(px(0.), px(-offset)));
        cx.notify();
    }

    fn scroll_offset(&self) -> f32 {
        (-self.scroll_handle.offset().y.as_f32()).max(0.0)
    }

    /// Size of the page list. Before the first layout only the window size is
    /// known, which is close enough for the first frame.
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

    /// 把一页的结果放进缓存，并按预算丢掉离读者最远的那些页。
    fn store(&mut self, index: usize, image: Arc<RenderImage>, focus: usize) {
        self.cache.insert(index, image);

        let pixels = |image: &Arc<RenderImage>| {
            let size = image.size(0);
            size.width.0.max(0) as usize * size.height.0.max(0) as usize
        };
        while self.cache.len() > MAX_CACHED_PAGES
            || self.cache.values().map(pixels).sum::<usize>() > MAX_CACHED_PIXELS
        {
            let farthest = self
                .cache
                .keys()
                .copied()
                .max_by_key(|page| page.abs_diff(focus));
            match farthest {
                Some(page) if self.cache.len() > 1 => {
                    self.cache.remove(&page);
                }
                _ => break,
            }
        }
    }

    /// 向默认打印机问纸张尺寸。打开驱动可能要几秒，所以放在后台，结果回来
    /// 之后再排版；在此之前界面只显示占位页。
    fn request_paper(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let generation = self.generation;
        cx.spawn_in(window, async move |this, cx| {
            let info = cx
                .background_spawn(async move {
                    // A driver that refuses to answer must not leave the preview
                    // waiting for ever: the fallback paper still paginates.
                    std::panic::catch_unwind(PageInfo::current).unwrap_or_else(|_| fallback())
                })
                .await;
            this.update_in(cx, move |this, _window, cx| {
                this.finish_paper(generation, info);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Rasterize the requested pages off the UI thread.
    ///
    /// `stop_after` is the last page the layout has to reach: everything behind
    /// it stays unmeasured, which is what makes the first page of a long
    /// document appear at once. Whatever is skipped is counted afterwards by
    /// [`Self::request_count`].
    fn request_pages(
        &mut self,
        pages: Vec<usize>,
        stop_after: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let job = self.job.clone();
        let sheet = self.info.sheet;
        let generation = self.generation;
        let requested = pages.clone();
        // 页脚的 m 是文档页总数：直接打印也是这个语义，缩印时 n 才不会
        // 超过 m。计数这一趟是空的，无所谓。
        let page_total = self.doc_total;
        // 这一趟刚开始时总页数知不知道：决定交回来的页上有没有页脚。
        let footers = page_total.is_some();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let range = ranges_of(&pages);
                    // A document that cannot be laid out must not take the whole
                    // application down.
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        print::render_pages(&job, sheet, PREVIEW_DPI, &range, stop_after, page_total)
                    }))
                    .ok()
                    .and_then(|result| result.ok())
                    .map(|(total, pages)| {
                        let pages = pages
                            .into_iter()
                            .map(|(index, image)| {
                                (index, Arc::new(RenderImage::new(vec![Frame::new(image)])))
                            })
                            .collect::<Vec<_>>();
                        (total, pages)
                    })
                })
                .await;

            this.update_in(cx, move |this, _window, cx| {
                this.finish_render(generation, &requested, footers, result);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 只数页数、不画页：全文分页一趟。
    ///
    /// 放在自己的线程里跑。后台执行器一次只跑一趟活，一本几十万字的小说
    /// 要数上几十秒，占着它的话读者滚到哪一页都得干等。
    fn request_count(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let job = self.job.clone();
        let sheet = self.info.sheet;
        let generation = self.generation;
        let page_total = self.doc_total;
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // 空的范围：只排版，不画页。
            let range = PageRange::Pages(Vec::new());
            let counted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                print::render_pages(&job, sheet, PREVIEW_DPI, &range, None, page_total)
            }))
            .ok()
            .and_then(|result| result.ok())
            .and_then(|(total, _)| total);
            let _ = sender.send(counted);
        });

        cx.spawn_in(window, async move |this, cx| {
            let counted = loop {
                match receiver.try_recv() {
                    Ok(counted) => break counted,
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        cx.background_executor().timer(COUNT_POLL).await;
                    }
                    // 数页数的那趟没了（比如线程崩了）：当作数不出来。
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => break None,
                }
            };
            this.update_in(cx, move |this, _window, cx| {
                this.finish_count(generation, counted);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 收到全文页数。页脚要等它：数完才知道「共几页」。
    fn finish_count(&mut self, generation: u64, counted: Option<usize>) {
        if self.generation != generation {
            return;
        }
        self.counting = false;
        self.counted = true;
        if let Some(total) = counted {
            // render_pages 返回文档页数；预览按张排版，缩印时换算成张数。
            self.doc_total = Some(total);
            self.total = Some(total.div_ceil(self.job.pages_per_sheet.count()));
        }
    }

    /// 收到默认打印机的纸张尺寸。预览在此之前只显示占位页。
    fn finish_paper(&mut self, generation: u64, info: PageInfo) {
        if self.generation != generation || self.paper_known {
            return;
        }
        remember_paper(&info);
        self.base = info;
        self.info = self.base.oriented(self.job.orientation);
        self.paper_known = true;
    }

    /// 收到后台排版的结果；迟到或属于别份文档的结果直接丢掉。
    fn finish_render(
        &mut self,
        generation: u64,
        requested: &[usize],
        footers: bool,
        result: Option<FinishedPages>,
    ) {
        if self.generation != generation {
            return;
        }
        let focus = requested.first().copied().unwrap_or(0);

        for page in requested {
            self.rendering.remove(page);
            self.stale.remove(page);
        }

        match result {
            Some((total, pages)) => {
                // 这一趟刚开始时总页数还不知道，页脚就印不上。数完再把这几
                // 页重画一遍补上；不能为了页脚让第一页干等着。
                let footerless = self.job.page_number && !footers && !pages.is_empty();
                if let Some(total) = total {
                    // render_pages 返回文档页数；预览按张排版，缩印时换算成张数。
                    self.doc_total = Some(total);
                    self.total = Some(total.div_ceil(self.job.pages_per_sheet.count()));
                    // 页数是完整的，就不必再单独数一遍。
                    self.counted = true;
                }
                for (index, image) in pages {
                    self.store(index, image, focus);
                }
                if footerless {
                    self.footers_pending = true;
                }
            }
            None => self.failed = true,
        }
    }

    fn render_title_bar(&self, foreground: Hsla, cx: &mut Context<Self>) -> AnyElement {
        TitleBar::new()
            .bg(self.background.background(cx))
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
                            .w(px(16.))
                            .h(px(16.))
                            .mr_1()
                            .text_color(foreground),
                    )
                    .child(div().text_sm().child("打印预览"))
                    .child(div().flex_1())
                    .child(
                        div()
                            .max_w(px(420.))
                            .truncate()
                            .text_xs()
                            .opacity(0.6)
                            .child(self.file_name.clone()),
                    ),
            )
            .into_any_element()
    }

    /// 页面列表：只画看得见的页，内容高度由版面算出来。
    fn render_pages(
        &mut self,
        window: &mut Window,
        foreground: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let viewport = self.scroll_viewport(window);
        let zoom = self.zoom;
        let offset = self.scroll_offset();

        // Until the document has been measured to its end the page count is a
        // guess: enough pages are shown to fill the screen and a little more.
        let count = self
            .total
            .unwrap_or_else(|| Layout::estimate(&self.info, viewport, zoom));
        let layout = Layout::new(&self.info, viewport, zoom, count);
        let current = layout.page_at(offset);
        let visible = layout.visible(offset, viewport.height);
        // Dragging the picture needs to know how large the page is on screen.
        self.page_size = layout.page;

        let failed = self.failed;
        let ready = self.paper_known;

        // 画面挪过位置：屏幕上的页已经过时，但旧图先留在原处，
        // 等新的一页画好再换上去，拖动时才不会整页闪成占位文字。
        if self.image_dirty && ready && !failed {
            self.image_dirty = false;
            self.stale.extend(visible.clone());
        }

        // 总页数数出来了：之前画好的页上没有页脚。把它们标成待重画，新图画
        // 好再换上去，中间不会闪回占位文字。
        if self.footers_pending && self.total.is_some() && ready && !failed {
            self.footers_pending = false;
            let painted = self.cache.keys().copied().collect::<Vec<_>>();
            self.stale.extend(painted);
        }

        // Ask for the pages on screen that are not painted yet. One layout pass
        // draws them all, and it may stop right after the last one.
        let missing = visible
            .clone()
            .filter(|index| {
                (!self.cache.contains_key(index) || self.stale.contains(index))
                    && !self.rendering.contains(index)
            })
            .collect::<Vec<_>>();
        // The document still has to be measured to its end.
        let count_rest =
            ready && !failed && self.total.is_none() && !self.counting && !self.counted;

        // 先画看得见的页，页数另外数：一本几十万字的小说要数上很久，把第一
        // 页押在它后面就是一片空白。页号开着时，数完再把这批页重画一遍补上
        // 页脚（见 `footers_pending`）。
        if ready && !failed && !missing.is_empty() {
            let stop_after = missing.last().copied();
            for index in &missing {
                self.rendering.insert(*index);
            }
            self.request_pages(missing, stop_after, window, cx);
        }
        if count_rest {
            self.counting = true;
            self.request_count(window, cx);
        }

        // Only the pages in view are built: a long document would otherwise
        // rebuild a placeholder element for every page on every frame.
        let page_left = ((layout.content_width - layout.page.width) * 0.5).max(0.0);
        let pages = visible
            .map(|index| {
                let mut page = div()
                    .id(("preview-page", index))
                    .absolute()
                    .top(px(layout.page_top(index)))
                    .left(px(page_left))
                    .flex_shrink_0()
                    .w(px(layout.page.width))
                    .h(px(layout.page.height))
                    .bg(rgb(0xFFFFFF))
                    .border_1()
                    .border_color(foreground.opacity(0.16));

                // 放大到纸面装不下时，可以在页上拖动画面，设定纸上印的是哪一块。
                if index == 0 && self.image_pan().is_some() {
                    let cursor = if self.dragging.is_some() {
                        CursorStyle::ClosedHand
                    } else {
                        CursorStyle::OpenHand
                    };
                    page = page
                        .cursor(cursor)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                this.begin_image_drag(event, cx)
                            }),
                        )
                        .on_mouse_move(cx.listener(
                            |this, event: &MouseMoveEvent, _, cx| this.drag_image(event, cx),
                        ))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.end_image_drag(cx)),
                        );
                }

                match self.cache.get(&index) {
                    Some(image) => {
                        page = page.child(img(image.clone()).w_full().h_full());
                    }
                    None => {
                        let label = if failed {
                            "这一页无法预览"
                        } else if !ready {
                            "正在识别纸张…"
                        } else {
                            "正在排版…"
                        };
                        page = page
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(div().text_xs().text_color(rgb(0x8A8A8A)).child(label));
                    }
                }

                // 画面挪过位置时，位图还停在老地方；这一层按新的位置盖上去。
                if index == 0 {
                    page = page.children(self.image_overlay());
                }

                page.into_any_element()
            })
            .collect::<Vec<_>>();

        let content = div()
            .relative()
            .w(px(layout.content_width))
            .flex_shrink_0()
            .h(px(layout.content_height()))
            .children(pages);

        let toolbar = self.render_toolbar(current, foreground, cx);

        let mut root = v_flex()
            .flex_1()
            .min_h(px(0.))
            .w_full()
            .child(toolbar)
            .child(
                div()
                    .id("preview-scroll-root")
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .w_full()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id("preview-scroll")
                            .size_full()
                            .flex()
                            .flex_col()
                            .overflow_scroll()
                            .track_scroll(&self.scroll_handle)
                            .child(content),
                    ),
            )
            .child(div().absolute().inset_0().child(
                Scrollbar::vertical(&self.scroll_handle)
                    .id("preview-scrollbar")
                    .mode(ScrollbarMode::Always)
                    .viewport_from_layout(),
            ));

        // A zoomed-in page can be wider than the window.
        if layout.content_width > viewport.width + 0.5 {
            root = root.child(div().absolute().inset_0().child(
                Scrollbar::horizontal(&self.scroll_handle)
                    .id("preview-scrollbar-x")
                    .mode(ScrollbarMode::Always)
                    .viewport_from_layout(),
            ));
        }

        root.into_any_element()
    }

    fn render_toolbar(
        &self,
        current: usize,
        foreground: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // 按钮底下垫一层淡填充，边界才看得出来：一排纯文字并排放着，看上去
        // 像一句话，不像能点的东西。悬停、按下依次加深一档。
        //
        // variant 的 color 会被框架先跟透明色混掉八成（见 mix_oklab 的用法），
        // 所以这里写的数值是目标透明度的五倍：0.35 → 实际 0.07。
        let chip = ButtonCustomVariant::new(cx)
            .color(foreground.opacity(0.35))
            .foreground(foreground)
            .hover(foreground.opacity(0.16))
            .active(foreground.opacity(0.26));

        // 「打印」是这一页上唯一的正事，用反色的实心块把它和别的按钮分开。
        // 实心底色走实例样式（bg 不会被混淡），文字用纸面的颜色反衬。
        let solid = ButtonCustomVariant::new(cx)
            .foreground(self.background.background(cx))
            .hover(foreground)
            .active(foreground.opacity(0.78));

        // 分组之间的一道细线：缩放、版面、打印三件事不混在一起。
        let divider = || {
            div()
                .w(px(1.))
                .h(px(16.))
                .mx_1()
                .flex_shrink_0()
                .bg(foreground.opacity(0.12))
        };

        // 普通按钮：淡底 + 一圈细边框，圆角由样式统一给。
        let chip_button = |id: &'static str| {
            Button::new(id)
                .custom(chip)
                .compact()
                .border_1()
                .border_color(foreground.opacity(0.14))
        };

        let close = chip_button("preview-close")
            
            .icon(IconName::Close)
            .tooltip("关闭预览，不打印")
            .tooltip_show_delay(TOOLTIP_DELAY)
            .on_click(cx.listener(|_, _, window, _cx| window.remove_window()));

        let out = chip_button("preview-zoom-out")
            
            .icon(IconName::Minus)
            .tooltip("把预览缩小一档（只影响屏幕上的显示）")
            .tooltip_show_delay(TOOLTIP_DELAY)
            .on_click(cx.listener(|this, _, _, cx| this.zoom_by(1.0 / ZOOM_STEP, cx)));

        let into = chip_button("preview-zoom-in")
            
            .icon(IconName::Plus)
            .tooltip("把预览放大一档（只影响屏幕上的显示）")
            .tooltip_show_delay(TOOLTIP_DELAY)
            .on_click(cx.listener(|this, _, _, cx| this.zoom_by(ZOOM_STEP, cx)));

        let print = Button::new("preview-print")
            .custom(solid)
            .bg(foreground.opacity(0.90))
            .compact()
            .label("打印…")
            .tooltip("按现在的设置打印整份文档")
            .tooltip_show_delay(TOOLTIP_DELAY)
            .on_click(cx.listener(|this, _, window, cx| this.print_document(window, cx)));

        // 「打印视图」印的是当前看得见的那一块，放大之后正好把细节印大。
        let print_visible = chip_button("preview-print-visible")
            
            .icon(IconName::Eye)
            .label("打印视图…")
            .tooltip("只打印预览里看得见的那一块：放大之后印细节正合适")
            .tooltip_show_delay(TOOLTIP_DELAY)
            .on_click(cx.listener(|this, _, window, cx| this.print_visible(window, cx)));

        let fit = self.fit_applies().then(|| {
            let other = self.job.fit.toggled();
            chip_button("preview-fit")
                
                .icon(IconName::Maximize)
                .label(self.job.fit.label())
                .tooltip(format!(
                    "点一下换成「{}」：{}",
                    other.label(),
                    match other {
                        PrintFit::FitToPaper => "内容都收进纸宽里，一行也不裁",
                        PrintFit::ActualSize => "按原尺寸印，超出纸边的部分会被裁掉",
                    }
                ))
                .tooltip_show_delay(TOOLTIP_DELAY)
                .on_click(cx.listener(|this, _, _, cx| this.toggle_fit(cx)))
        });

        // 图片印多大：缩放的是画面本身，纸张不变，只有图变大变小（放大到一页
        // 装不下时还会多出页来）。
        let image_scale = self.image_applies().then(|| {
            let view = cx.entity();
            let current = self.job.image.scale;
            chip_button("preview-image-scale")
                
                .icon(IconName::ResizeCorner)
                .label(current.short_label())
                .tooltip("图片印多大：100% 铺满可印区域，放大后画面会被裁掉或分成多页")
                .tooltip_show_delay(TOOLTIP_DELAY)
                .dropdown_menu(move |menu, _window, _cx| {
                    let mut menu = menu.min_w(px(180.)).label("图片打印缩放");
                    for scale in ImageScale::ALL {
                        let view = view.clone();
                        menu = menu.item(
                            PopupMenuItem::new(scale.label())
                                .checked(scale == current)
                                .on_click(move |_, _, cx| {
                                    view.update(cx, |this, cx| this.set_image_scale(scale, cx));
                                }),
                        );
                    }
                    menu
                })
        });

        // 放大到一页装不下时，可以选择分到多页上，拼起来就是整张图。
        let poster = self.image_applies().then(|| {
            let on = self.job.image.poster;
            chip_button("preview-poster")
                
                .icon(IconName::GalleryVerticalEnd)
                .label("拼接分页")
                .selected(on)
                .tooltip(if on {
                    "已开启：图放大到一页装不下时，分到多页上按顺序拼起来"
                } else {
                    "放大后一页装不下时，把画面分到多页上，铺开拼成整张图"
                })
                .tooltip_show_delay(TOOLTIP_DELAY)
                .on_click(cx.listener(|this, _, _, cx| this.toggle_image_poster(cx)))
        });

        // 按钮上写的是当前方向，点一下换另一种：预览里看到的就是纸横竖的样子。
        let orientation = chip_button("preview-orientation")
            
            .icon(IconName::RotateCw)
            .label(self.job.orientation.label())
            .tooltip(format!(
                "点一下换成「{}」纸张：预览里的纸会跟着转",
                self.job.orientation.toggled().label()
            ))
            .tooltip_show_delay(TOOLTIP_DELAY)
            .on_click(cx.listener(|this, _, _, cx| this.toggle_orientation(cx)));

        // 页脚写不写页号：开着的时候每一页底部都印「n / m」。
        let page_number = chip_button("preview-page-number")
            .icon(Icon::default().data(PAGE_NUMBER_ICON))
            .label("页号")
            .selected(self.job.page_number)
            .tooltip(if self.job.page_number {
                "已开启：每一页底部印「页号 / 总页数」，再点一下关掉"
            } else {
                "打开后在每一页的底部印「页号 / 总页数」"
            })
            .tooltip_show_delay(TOOLTIP_DELAY)
            .on_click(cx.listener(|this, _, _, cx| this.toggle_page_number(cx)));

        // 一张纸印几页，直接从菜单里选：按钮上写的就是当前的组合方式。
        let pages_per_sheet = {
            let view = cx.entity();
            let current = self.job.pages_per_sheet;
            chip_button("preview-pages")

                .icon(IconName::LayoutDashboard)
                .label(current.label())
                .tooltip("一张纸印几页：把多页缩小排在同一张纸上（缩印）")
                .tooltip_show_delay(TOOLTIP_DELAY)
                .dropdown_menu(move |menu, _window, _cx| {
                    let mut menu = menu.min_w(px(140.)).label("每张纸打印页数");
                    for pages in PagesPerSheet::ALL {
                        let view = view.clone();
                        menu = menu.item(
                            PopupMenuItem::new(pages.label())
                                .checked(pages == current)
                                .on_click(move |_, _, cx| {
                                    view.update(cx, |this, cx| {
                                        this.set_pages_per_sheet(pages, cx)
                                    });
                                }),
                        );
                    }
                    menu
                })
        };

        let mut page_label = match self.total {
            Some(total) => format!("第 {} / {} 页", current + 1, total),
            // The page in front of the reader is already drawn; only the pages
            // behind it are still being measured.
            None => format!("第 {} 页 · 正在统计页数…", current + 1),
        };
        if self.image_pan().is_some() {
            page_label.push_str(" · 拖动图片可调整位置");
        }

        let details = if !self.paper_known {
            String::from("正在识别纸张…")
        } else {
            match &self.info.printer {
                Some(printer) => format!("{} · {}", self.info.paper, printer),
                None => format!("{} · 未找到打印机", self.info.paper),
            }
        };

        h_flex()
            .w_full()
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(foreground.opacity(0.12))
            .child(close)
            .child(divider())
            .child(div().text_sm().child(page_label))
            .child(div().flex_1())
            .child(
                div()
                    .min_w(px(0.))
                    .max_w(px(280.))
                    .truncate()
                    .text_xs()
                    .opacity(0.6)
                    .child(details),
            )
            .child(divider())
            .child(out)
            .child(
                div()
                    .min_w(px(48.))
                    .text_center()
                    .text_sm()
                    .opacity(0.8)
                    .child(format!("{}%", (self.zoom * 100.0).round() as i32)),
            )
            .child(into)
            .child(divider())
            .children(fit)
            .children(image_scale)
            .children(poster)
            .child(orientation)
            .child(pages_per_sheet)
            .child(page_number)
            .child(divider())
            .child(print_visible)
            .child(print)
            .into_any_element()
    }
}

impl Render for PreviewWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The paper size decides every page break, so it is asked for as soon
        // as the window exists; the window is on screen meanwhile.
        if !self.paper_known && !self.paper_requested {
            self.paper_requested = true;
            self.request_paper(window, cx);
        }

        let background = self.background.background(cx);
        let foreground = self.background.foreground(cx);
        let title_bar = self.render_title_bar(foreground, cx);
        let body = self.render_pages(window, foreground, cx);

        v_flex()
            .size_full()
            .bg(background)
            .text_color(foreground)
            .child(title_bar)
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui_kit::Size;

    use super::{Layout, PDF_TOP_PADDING, PREVIEW_DPI, PageInfo, Region, crop_region, fallback};
    use crate::loader::{LoadedContent, load_document};
    use crate::print::{self, PageRange, PrintJob};

    fn job_of(path: &str) -> PrintJob {
        let (_, content) = load_document(std::path::Path::new(path)).expect("load");
        let content = match content {
            LoadedContent::Text(text) => print::PrintContent::Text {
                text: Arc::new(text),
                font_pt: 10.0,
            },
            LoadedContent::Word(document) | LoadedContent::Markdown(document) => {
                print::PrintContent::Word(document)
            }
            LoadedContent::Excel(workbook) => print::PrintContent::Excel(workbook),
            LoadedContent::Pdf(source) => print::PrintContent::Pdf(source),
            LoadedContent::Image(picture) => print::PrintContent::Image {
                pixels: picture.pixels.clone(),
            },
        };
        PrintJob {
            title: String::from("preview"),
            content,
            fit: print::PrintFit::FitToPaper,
            orientation: print::Orientation::Portrait,
            pages_per_sheet: print::PagesPerSheet::One,
            image: print::ImagePrint::default(),
            page_number: false,
        }
    }

    /// A page that only carries white paper means the drawing never happened,
    /// which is the regression this check guards against.
    fn has_ink(image: &image::RgbaImage) -> bool {
        image
            .as_raw()
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[..3].iter().any(|channel| 255 - channel > 64))
    }

    /// The pages are drawn at absolute offsets, so the offsets and the height
    /// of the scrolling content have to agree or the last page drifts away.
    #[test]
    fn pages_sit_inside_the_scrolled_content() {
        let info = fallback();
        let viewport = Size::new(900.0, 700.0);
        let layout = Layout::new(&info, viewport, 1.0, 5);

        assert!(layout.page.height > 0.0);
        assert_eq!(layout.page_top(0), PDF_TOP_PADDING);
        let bottom = layout.page_top(4) + layout.page.height + PDF_TOP_PADDING;
        assert!((bottom - layout.content_height()).abs() < 0.01);

        let visible = layout.visible(0.0, viewport.height);
        assert_eq!(visible.start, 0);
        assert!(visible.end <= 3, "more pages were drawn than fit on screen");

        // Scrolled to the second page, the range follows it.
        let visible = layout.visible(layout.step(), viewport.height);
        assert_eq!(visible.start, 0, "the page above stays for a smooth scroll");
        assert!(visible.end >= 2);
    }

    /// `ranges_of` turns zero-based page indices into the one-based ranges the
    /// print layout understands; an off-by-one here paints nothing at all.
    #[test]
    fn page_indices_are_converted_to_one_based_ranges() {
        let ranges = super::ranges_of(&[0]);
        assert_eq!(ranges, PageRange::Pages(vec![(1, 1)]));

        let ranges = super::ranges_of(&[0, 2]);
        assert_eq!(ranges, PageRange::Pages(vec![(1, 1), (3, 3)]));

        let ranges = super::ranges_of(&[0, 1, 2]);
        assert_eq!(ranges, PageRange::Pages(vec![(1, 3)]));
    }

    /// The visible block is cut by relative position, so the same fractions
    /// work on the page as displayed and on the same page re-rendered at print
    /// resolution.
    #[test]
    fn a_region_is_cut_out_of_a_page() {
        let page = image::RgbaImage::from_pixel(100, 200, image::Rgba([10, 20, 30, 255]));
        let region = Region {
            page: 0,
            left: 0.25,
            top: 0.5,
            right: 0.75,
            bottom: 1.0,
        };

        let crop = crop_region(&page, &region).expect("the region is inside the page");
        assert_eq!(crop.dimensions(), (50, 100));
        assert_eq!(crop.get_pixel(0, 0).0, [10, 20, 30, 255]);

        // Nothing visible means nothing to print.
        let empty = Region {
            page: 0,
            left: 0.5,
            top: 0.5,
            right: 0.5,
            bottom: 0.5,
        };
        assert!(crop_region(&page, &empty).is_none());
    }

    /// A preview only needs the pages in front of the reader: laying the rest
    /// of a long file out would stall the first page behind the whole document.
    #[test]
    fn a_layout_can_stop_after_the_pages_asked_for() {
        let info = PageInfo::current();
        let mut text = String::new();
        for _ in 0..6000 {
            text.push_str("床前明月光，疑是地上霜。举头望明月，低头思故乡。\n");
        }
        let job = PrintJob {
            title: String::from("long"),
            content: print::PrintContent::Text {
                text: Arc::new(text),
                font_pt: 10.0,
            },
            fit: print::PrintFit::FitToPaper,
            orientation: print::Orientation::Portrait,
            pages_per_sheet: print::PagesPerSheet::One,
            image: print::ImagePrint::default(),
            page_number: false,
        };

        let range = PageRange::Pages(vec![(1, 1)]);
        let (total, pages) =
            print::render_pages(&job, info.sheet, PREVIEW_DPI, &range, Some(0), None).expect("render");

        assert!(total.is_none(), "a stopped layout cannot know the page count");
        assert_eq!(pages.len(), 1, "only the page asked for is painted");
        assert!(has_ink(&pages[0].1), "the painted page is blank");
    }

    /// Every format goes through the same sheets, so a change to the layout
    /// shows up in all of them or in none.
    #[test]
    fn preview_pages_match_the_print_layout() {
        let info = PageInfo::current();
        let ranges = PageRange::Pages(vec![(1, 2)]);

        for name in [
            "sample.txt",
            "sample.md",
            "sample.docx",
            "sample.xlsx",
            "sample.pdf",
            "sample-image.png",
        ] {
            let job = job_of(&format!("samples/{name}"));
            let (total, pages) =
                print::render_pages(&job, info.sheet, PREVIEW_DPI, &ranges, None, None).expect("render");

            assert!(
                total.is_some_and(|total| total >= 1),
                "{name}: no pages laid out"
            );
            assert!(
                pages.iter().any(|(_, image)| has_ink(image)),
                "{name}: the painted pages are blank"
            );
        }
    }


    /// 纵向与横向说的是同一张纸，只是摆法不同：尺寸互换，纸面标注跟着换，
    /// 而要两遍同样的方向不会来回翻。
    #[test]
    fn paper_turns_between_the_two_orientations() {
        let portrait = fallback();
        let landscape = portrait.oriented(print::Orientation::Landscape);

        assert!(!portrait.sheet.is_landscape());
        assert!(landscape.sheet.is_landscape());
        assert_eq!(landscape.sheet.width, portrait.sheet.height);
        assert_eq!(landscape.sheet.height, portrait.sheet.width);
        assert_eq!(
            landscape.sheet.printable_height,
            portrait.sheet.printable_width
        );
        assert_ne!(landscape.paper, portrait.paper, "标注要跟着纸一起换");

        let same = landscape.oriented(print::Orientation::Landscape);
        assert_eq!(same.sheet.width, landscape.sheet.width);
        assert_eq!(same.paper, landscape.paper);

        let back = landscape.oriented(print::Orientation::Portrait);
        assert_eq!(back.sheet.width, portrait.sheet.width);
        assert_eq!(back.sheet.printable_width, portrait.sheet.printable_width);
    }

    /// 横纸确实更宽：一张宽表在竖纸上要分两页，横过来一页装得下。
    ///
    /// 这是「方向」真正走到排版里的证据——不只是画个横框。
    #[test]
    fn a_landscape_sheet_fits_more_columns_on_a_page() {
        let info = PageInfo::current();
        let job = job_of("samples/sample.xlsx");
        let range = PageRange::Pages(vec![(1, 1)]);

        let (portrait, _) =
            print::render_pages(&job, info.sheet, PREVIEW_DPI, &range, None, None).expect("portrait");
        let landscape_sheet = info
            .oriented(print::Orientation::Landscape)
            .sheet;
        let (landscape, _) = print::render_pages(&job, landscape_sheet, PREVIEW_DPI, &range, None, None)
            .expect("landscape");

        let portrait = portrait.expect("the whole table was measured");
        let landscape = landscape.expect("the whole table was measured");
        assert!(
            landscape < portrait,
            "横纸 {landscape} 页、竖纸 {portrait} 页：方向没有进到排版里"
        );
    }

    /// 屏幕上的纸也跟着转：同样的宽度下，横纸明显更矮，比例与纸面一致。
    #[test]
    fn a_landscape_page_is_shorter_on_screen() {
        let portrait = fallback();
        let landscape = portrait.oriented(print::Orientation::Landscape);
        let viewport = Size::new(900.0, 700.0);

        let tall = Layout::page_size(&portrait, viewport, 1.0);
        let wide = Layout::page_size(&landscape, viewport, 1.0);
        assert_eq!(tall.width, wide.width);
        assert!(
            wide.height < tall.height * 0.85,
            "横纸 {} 高、竖纸 {} 高",
            wide.height,
            tall.height
        );
        assert!((wide.width / wide.height - landscape.sheet.ratio()).abs() < 0.01);
    }

}
