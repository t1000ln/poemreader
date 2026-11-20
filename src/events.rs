//! 阅读区的事件绑定。
//!
//! 左边按键在「拖拽平移」和「划选文字」之间切换，松开时收尾；右键弹出菜单
//! 切换这两种模式，或者复制所选内容；文件从系统拖进来也在这一层接住。
//!
//! 这里同时放着这些事件要用的状态类型（[`Interaction`]、[`Selection`]、
//! [`CellSelection`]）：它们只被本模块改写，渲染时只读。

use std::ops::Range;

use gpui_kit::component::{
    WindowExt as _,
    menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem},
    notification::Notification,
};
use gpui_kit::*;

use crate::settings::PointerMode;
use crate::{DocumentContent, PoemReader, TextPos};

/// What the left button is currently doing in the reading area.
#[derive(Clone, Copy)]
pub(crate) enum Interaction {
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

/// The text the reader has selected, from `anchor` to a movable `head`.
#[derive(Clone, Copy)]
pub(crate) struct Selection {
    anchor: TextPos,
    head: TextPos,
}

impl Selection {
    pub(crate) fn ordered(&self) -> (TextPos, TextPos) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// The byte range of this selection within `line`.
    pub(crate) fn bytes_in_line(&self, line: usize, len: usize) -> Option<Range<usize>> {
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

/// The cells of a sheet the reader has selected.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct CellSelection {
    anchor: (u32, u32),
    head: (u32, u32),
}

impl CellSelection {
    pub(crate) fn ordered(&self) -> ((u32, u32), (u32, u32)) {
        // Compare the two axes separately: tuple ordering would only swap
        // when both row and column go backwards, so a drag up-and-left (or
        // down-and-right) left `left > right` and selected nothing.
        let (anchor_row, anchor_column) = self.anchor;
        let (head_row, head_column) = self.head;
        (
            (anchor_row.min(head_row), anchor_column.min(head_column)),
            (anchor_row.max(head_row), anchor_column.max(head_column)),
        )
    }

    pub(crate) fn contains(&self, row: u32, column: u32) -> bool {
        let ((top, left), (bottom, right)) = self.ordered();
        (top..=bottom).contains(&row) && (left..=right).contains(&column)
    }
}

impl PoemReader {
    /// Wire up the reading area: the left button either pans the content or
    /// selects text, depending on the pointer mode, and the right button opens
    /// the menu that switches between the two.
    pub(crate) fn reading_area(
        &self,
        element: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
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

    /// Formats that hold text the reader can drag over; PDF pages and picture
    /// documents cannot be selected.
    fn supports_selection(&self) -> bool {
        matches!(
            self.document.as_ref().map(|document| &document.content),
            Some(DocumentContent::Text(_))
                | Some(DocumentContent::Word(_))
                | Some(DocumentContent::Excel(_))
                | Some(DocumentContent::Markdown(_))
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
            // The cursor follows `interaction`; without a repaint it would
            // stay a closed hand until something else changed on screen.
            cx.notify();
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

    /// Open the first file dropped onto the window from outside the
    /// application. Folders cannot be opened, so they are skipped.
    pub(crate) fn open_dropped_files(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match paths.paths().iter().find(|path| path.is_file()).cloned() {
            Some(path) => self.open_path(path, window, cx),
            None => window.push_notification(
                Notification::error("无法打开拖入的内容：请拖入一个文件"),
                cx,
            ),
        }
    }
}

fn build_reading_menu(
    menu: PopupMenu,
    view: Entity<PoemReader>,
    selectable: bool,
    pointer_mode: PointerMode,
    has_selection: bool,
) -> PopupMenu {
    let menu = menu.min_w(px(240.));

    if !selectable {
        // PDF pages and pictures are images, and Word / Excel documents are
        // laid out from their own model, so only panning makes sense here.
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

#[cfg(test)]
mod tests {
    use super::CellSelection;

    fn selection(anchor: (u32, u32), head: (u32, u32)) -> CellSelection {
        CellSelection { anchor, head }
    }

    /// Dragging up-and-left used to leave `left > right` because the two
    /// points were compared as tuples instead of axis by axis.
    #[test]
    fn a_selection_dragged_up_and_left_still_covers_its_cells() {
        let selection = selection((0, 5), (3, 2));
        assert_eq!(selection.ordered(), ((0, 2), (3, 5)));
        assert!(selection.contains(1, 3));
        assert!(selection.contains(0, 5), "the anchor corner is inside");
        assert!(!selection.contains(4, 3), "outside the row range");
        assert!(!selection.contains(1, 6), "outside the column range");
    }

    #[test]
    fn a_selection_dragged_down_and_right_is_unchanged() {
        let selection = selection((1, 1), (4, 6));
        assert_eq!(selection.ordered(), ((1, 1), (4, 6)));
        assert!(selection.contains(4, 6));
        assert!(!selection.contains(0, 0));
    }
}
