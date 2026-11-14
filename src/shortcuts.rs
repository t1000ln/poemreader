//! 窗口级快捷键。
//!
//! 阅读区不是可聚焦的文本输入框，键盘事件不会被它接住，所以复制和打印用
//! `intercept_keystrokes` 在窗口层拦截。这里只负责「按键 → 行为」的映射，
//! 行为本身仍由视图实现。

use gpui_kit::*;

use crate::PoemReader;

impl PoemReader {
    /// 注册 Ctrl+C（复制所选文字）和 Ctrl+P（打印）。
    ///
    /// 返回的订阅必须由视图持有，一旦被丢弃，拦截会立刻失效。
    pub(crate) fn bind_shortcuts(cx: &mut Context<Self>) -> Subscription {
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
    }
}
