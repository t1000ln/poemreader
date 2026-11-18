//! 窗口级快捷键。
//!
//! 阅读区不是可聚焦的文本输入框，键盘事件不会被它接住，所以复制、打印和
//! 预览用 `intercept_keystrokes` 在窗口层拦截。这里只负责「按键 → 行为」的
//! 映射，行为本身仍由视图实现。
//!
//! 拦截器是应用级的，预览窗口里的按键也会走到这里；按窗口编号区分，交给
//! 对应的那个窗口处理。

use gpui_kit::*;

use crate::PoemReader;

impl PoemReader {
    /// 注册 Ctrl+O（加载文件）、Ctrl+C（复制所选文字）、Ctrl+P（打印）、
    /// Ctrl+Shift+P（打印预览），以及预览窗口里的 Esc（关闭）、Ctrl+P（打印）
    /// 和缩放。
    ///
    /// 返回的订阅必须由视图持有，一旦被丢弃，拦截会立刻失效。
    pub(crate) fn bind_shortcuts(cx: &mut Context<Self>) -> Subscription {
        let view = cx.entity().downgrade();
        cx.intercept_keystrokes(move |event, window, cx| {
            let keystroke = &event.keystroke;
            let in_preview = view
                .update(cx, |this, _| {
                    this.preview_window_id() == Some(window.window_handle().window_id())
                })
                .unwrap_or(false);

            if in_preview {
                // The preview window only answers to closing, printing and
                // zooming; reading shortcuts belong to the reader.
                if keystroke.key == "escape" {
                    // Close the window we were given rather than through its
                    // handle: it is mid-update, so the handle would be refused.
                    window.remove_window();
                    view.update(cx, |this, _| this.forget_preview()).ok();
                    cx.stop_propagation();
                    return;
                }
                if !(keystroke.modifiers.control || keystroke.modifiers.platform) {
                    return;
                }
                match keystroke.key.as_str() {
                    "p" => {
                        view.update(cx, |this, cx| this.print_preview(window, cx)).ok();
                        cx.stop_propagation();
                    }
                    "=" | "+" => {
                        view.update(cx, |this, cx| this.zoom_preview(1.25, cx)).ok();
                        cx.stop_propagation();
                    }
                    "-" => {
                        view.update(cx, |this, cx| this.zoom_preview(1.0 / 1.25, cx)).ok();
                        cx.stop_propagation();
                    }
                    _ => {}
                }
                return;
            }

            if !(keystroke.modifiers.control || keystroke.modifiers.platform) {
                return;
            }

            match keystroke.key.as_str() {
                // 打开文件：和大多数编辑器一样，Ctrl+O 直接弹文件对话框。
                "o" => {
                    view.update(cx, |this, cx| this.load_file(window, cx)).ok();
                    cx.stop_propagation();
                }
                "c" => {
                    if view
                        .update(cx, |this, cx| this.copy_selection(cx))
                        .unwrap_or(false)
                    {
                        cx.stop_propagation();
                    }
                }
                // Shift asks for the preview, plain Ctrl+P prints straight away.
                "p" if keystroke.modifiers.shift => {
                    view.update(cx, |this, cx| this.open_preview(window, cx))
                        .ok();
                    cx.stop_propagation();
                }
                "p" => {
                    view.update(cx, |this, cx| this.print_document(window, cx))
                        .ok();
                    cx.stop_propagation();
                }
                _ => {}
            }
        })
    }
}
