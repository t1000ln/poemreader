//! 应用的启动与窗口初始化。
//!
//! 这里只负责“把窗口开起来”：进程级 DPI 适配、主窗口的尺寸与位置、主题模式，
//! 以及启动时要打开哪个文档。视图本身（[`PoemReader`]）不关心窗口是怎么建起来的。

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::{Theme, ThemeMode, TitleBar, scroll::ScrollbarMode};
use gpui_kit::*;

use crate::loader::{DocumentKind, LoadedContent, load_document};
use crate::settings::{ReadingBackground, Store};
use crate::{PoemReader, file_name_of};

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
    #[cfg(windows)]
    let scale = unsafe { GetDpiForSystem() as f32 / 96.0 };
    #[cfg(not(windows))]
    let scale = 1.0;
    scale
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

/// Switch the component theme so menus, the about dialog and the window
/// controls stay legible against the chosen reading background.
pub(crate) fn apply_theme_mode(background: ReadingBackground, cx: &mut App) {
    let mode = match background {
        ReadingBackground::Night => ThemeMode::Dark,
        ReadingBackground::Paper | ReadingBackground::Mint => ThemeMode::Light,
        // Default follows whatever appearance the system already selected.
        ReadingBackground::Default => return,
    };
    Theme::change(mode, None, cx);
}

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

/// 建立应用、打开主窗口并挂上退出时的保存逻辑。
pub(crate) fn run() {
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
        Theme::set_scrollbar_mode(ScrollbarMode::Always, cx);
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
