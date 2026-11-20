//! 应用的启动与窗口初始化。
//!
//! 这里只负责“把窗口开起来”：进程级 DPI 适配、主窗口的尺寸与位置、主题模式，
//! 以及启动时要打开哪个文档。视图本身（[`PoemReader`]）不关心窗口是怎么建起来的。

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::{Theme, ThemeMode, TitleBar, scroll::ScrollbarMode};
use gpui_kit::*;

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
///
/// Only the path is resolved here: reading and laying the file out happens
/// after the window is up, so a large document never delays the first frame.
fn resolve_initial_path(store: &Store) -> Option<PathBuf> {
    if let Some(path) = std::env::args_os().nth(1).map(PathBuf::from)
        && path.is_file()
    {
        return Some(path);
    }

    let path = store.settings().last_file.clone()?;
    path.is_file().then_some(path)
}

/// 建立应用、打开主窗口并挂上退出时的保存逻辑。
pub(crate) fn run() {
    enable_dpi_awareness();

    let store = Rc::new(RefCell::new(Store::load()));
    let latest_position = Rc::new(Cell::new(None::<f32>));
    let initial = resolve_initial_path(&store.borrow());
    let initial_title = match initial.as_ref() {
        Some(path) => format!("PoemReader — {}", file_name_of(path)),
        None => String::from("PoemReader"),
    };
    let initial_background = store.borrow().settings().background;

    application().with_assets(assets::Assets).run(move |cx| {
        gpui_kit::init(cx);
        // Keep the reading scrollbar visible instead of fading it out when idle.
        Theme::set_scrollbar_mode(ScrollbarMode::Always, cx);
        apply_theme_mode(initial_background, cx);

        let bounds = initial_window_bounds(cx);
        let reader = open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(560.), px(420.))),
                ..TitleBar::window_options()
            },
            cx,
            {
                let store = store.clone();
                let latest_position = latest_position.clone();
                let initial = initial.clone();
                move |window, cx| {
                    window.set_window_title(&initial_title);
                    // The file is read — and a long one wrapped — after the
                    // window is up, so it never delays the first frame.
                    cx.new(move |cx| PoemReader::new(store, latest_position, initial, cx))
                }
            },
        )
        .expect("failed to open the PoemReader window");

        // Ask the default printer for its paper now, so the first preview has
        // the page size ready instead of waiting on the driver. Some drivers
        // take seconds to answer; keeping this off the shared background
        // executor stops a slow printer from making file loads queue behind it.
        std::thread::spawn(crate::preview::warm_paper);

        // Closing the window with its close button removes it without running
        // the app-quit handlers, so persist the latest reading position here
        // as well. The callback only holds shared state and lives as long as
        // the application, hence the subscription is intentionally leaked.
        let close_store = store.clone();
        let close_position = latest_position.clone();
        let reader_id = reader.0.window_id();
        let subscription = cx.on_window_closed(move |cx, window_id| {
            // Only the reader's own closing saves the reading position. A
            // preview window is closed on every file switch; saving there would
            // write the *old* document's position under the *new* last_file,
            // and would also put a synchronous disk write in the switch path.
            if window_id != reader_id {
                return;
            }

            let mut store = close_store.borrow_mut();
            if let Some(position) = close_position.get()
                && let Some(path) = store.settings().last_file.clone()
            {
                store.set_position(&path, position);
            }
            store.save();
            drop(store);

            // The preview is a companion window: once the reader is gone it has
            // nothing left to show, and the application can shut down.
            for handle in cx.windows() {
                handle
                    .update(cx, |_, window, _| window.remove_window())
                    .ok();
            }
        });
        std::mem::forget(subscription);
    });
}
