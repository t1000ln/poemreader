//! 渲染层：共用件 + 三个格式分支。
//!
//! `SelectableLine` / `LinePlace` / `LineText` 是「能划选的一行」的模型——
//! Word 分支排版时逐行登记，视图在命中测试与绘制时读回来。
//! 各格式的排版细节在同级子模块里：`word`、`excel`、`pdf`。

pub(crate) mod excel;
pub(crate) mod pdf;
pub(crate) mod word;

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::*;

/// A line of a Word document that can be selected, with the layout captured
/// after rendering.
pub(crate) struct SelectableLine {
    /// The text as drawn, including the list marker and any first-line indent.
    pub(crate) text: SharedString,
    /// Bytes at the start of the line that belong to the layout but not to the
    /// paragraph, such as the spaces standing in for a first-line indent.
    pub(crate) skip: usize,
    /// Paragraph the line belongs to; the lines of one paragraph are copied
    /// without a separator between them.
    pub(crate) paragraph: usize,
    /// Table row the line belongs to, so the cells of one row are copied with
    /// tabs between them.
    pub(crate) row: Option<usize>,
    pub(crate) layout: Rc<RefCell<Option<TextLayout>>>,
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

/// A rendered line of text that publishes its layout after prepaint, so the
/// view can turn pointer positions back into byte offsets.
pub(crate) struct LineText {
    pub(crate) styled: StyledText,
    pub(crate) layout: Rc<RefCell<Option<TextLayout>>>,
}

impl LineText {
    pub(crate) fn new(styled: StyledText, layout: Rc<RefCell<Option<TextLayout>>>) -> Self {
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
