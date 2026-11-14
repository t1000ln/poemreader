//! Word 文档的段落与表格排版。
//!
//! 每一段被拆成若干「文本块」，每个块登记成一条可划选的行；段落里的图片
//! 单独成块，并跟随段落的对齐方式。

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::*;

use super::{LinePlace, LineText, SelectableLine};
use crate::events::Selection;
use crate::word;

// ---------------------------------------------------------------- Word layout

/// State shared by the Word block builders: the scale, the colours, and the
/// lines registered for selection while the blocks are built.
pub(crate) struct WordRender<'a> {
    pub(crate) scale: f32,
    pub(crate) foreground: Hsla,
    pub(crate) base: &'a Font,
    /// Colour painted behind the selected text.
    pub(crate) highlight: Hsla,
    pub(crate) selection: Option<Selection>,
    /// Selectable lines, in document order.
    pub(crate) lines: &'a mut Vec<SelectableLine>,
    pub(crate) next_paragraph: usize,
    pub(crate) next_row: usize,
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
pub(crate) fn word_block(
    block: &word::Block,
    render: &mut WordRender,
    row: Option<usize>,
) -> AnyElement {
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

    let mut element = div()
        .w_full()
        .pl(px(paragraph.indent * scale))
        .pt(px(paragraph.space_before * scale))
        .pb(px(paragraph.space_after * scale))
        .text_align(match paragraph.align {
            word::Align::Center => TextAlign::Center,
            word::Align::Right => TextAlign::Right,
            _ => TextAlign::Left,
        })
        .children(blocks);

    if let Some(fill) = paragraph.fill {
        // Painted behind the whole paragraph, so a code block's card reaches
        // the edges of its outline instead of stopping at each character.
        element = element.bg(rgb(fill));
    }

    if let Some(color) = paragraph.border.color {
        let border = paragraph.border;
        let radius = px(border.radius * scale);
        element = element.border_color(rgb(color));
        // Only the top and bottom lines own the rounded corners, so the lines
        // of one code block meet at right angles in between.
        if border.top {
            element = element.border_t_1().rounded_tl(radius).rounded_tr(radius);
        }
        if border.bottom {
            element = element.border_b_1().rounded_bl(radius).rounded_br(radius);
        }
        if border.left {
            element = element.border_l_1();
        }
        if border.right {
            element = element.border_r_1();
        }
    }

    element.into_any_element()
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
