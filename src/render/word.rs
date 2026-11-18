//! Word 文档的段落与表格排版。
//!
//! 每一段被拆成若干「文本块」，每个块登记成一条可划选的行；段落里的图片
//! 单独成块，并跟随段落的对齐方式。

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::*;

use super::{LinePlace, LineText, SelectableLine};
use crate::events::Selection;
use crate::word;

// ---------------------------------------------------------------- Word layout

/// How a text block takes its width inside its line.
enum BlockWidth {
    /// Spans the whole column: a plain paragraph's text block.
    Column,
    /// Grows to share its line with inline pictures, wrapping in the space
    /// that is left.
    Grow,
    /// Keeps the width of its content: indent prefixes that must not push the
    /// pictures around.
    Natural,
}

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

    /// Builds one text block and registers it as a selectable line. The width
    /// decides how the block behaves inside its line: a column block spans the
    /// whole column, a growing block shares its line with inline pictures and
    /// wraps in the width that is left, and a natural block keeps the width of
    /// its content (indent prefixes that must not push things around).
    fn text_block(
        &mut self,
        text: String,
        mut runs: Vec<TextRun>,
        place: LinePlace,
        size: f32,
        line_height: f32,
        color: Hsla,
        width: BlockWidth,
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

        let block = div()
            .text_size(px(size))
            .line_height(px(line_height))
            .text_color(color);
        let block = match width {
            BlockWidth::Column => block.w_full(),
            BlockWidth::Grow => block.flex_grow(1.0),
            BlockWidth::Natural => block,
        };
        block
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
    let size_pt = paragraph.draw_size();
    let mut size = (size_pt * scale).max(4.0);
    let mut line_height = size * 1.3 * paragraph.line_spacing.max(0.8);
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
    // Pictures and a flushed indent prefix that share the paragraph's first
    // line with the text that follows them.
    let mut inline: Vec<AnyElement> = Vec::new();
    // The first-line indent only covers the first block of the paragraph.
    let mut skip = 0usize;
    // How many of the blocks at the tail of `blocks` are pure whitespace: the
    // split below files padding runs there, and a picture that follows them
    // pulls them onto its line — Word indents an inline picture with the
    // spaces the paragraph opens with.
    let mut whitespace_blocks = 0usize;

    // A paragraph that opens with a picture takes its first-line indent as an
    // exact left margin on the assembled line: drawing the indent as
    // ideographic spaces would measure the two characters in the paragraph's
    // text size, which is not the size the indent is defined in.
    let image_first = paragraph
        .runs
        .iter()
        .find(|run| run.image.is_some() || !run.text.trim().is_empty())
        .is_some_and(|run| run.image.is_some());
    let indent_px = if image_first {
        paragraph.first_line.max(0.0) * scale
    } else {
        0.0
    };

    // Chinese documents indent the first line rather than the whole paragraph.
    if paragraph.first_line > 0.0 && size_pt > 0.0 && !image_first {
        let spaces = ((paragraph.first_line / size_pt).round() as usize).clamp(1, 8);
        let indent = "\u{3000}".repeat(spaces);
        skip = indent.len();
        push_text_run(
            &mut text, &mut runs, &indent, &face, color, None, false, false,
        );
    }

    // A padded line is pushed into place by leading spaces of another size —
    // the title of this chapter is centred by 17 spaces of 26pt. Only one size
    // is drawn, so those spaces are re-measured at their own size and stand in
    // for as ideographic spaces of the drawn size.
    let (padding, drawn_padding, padding_chars) = leading_padding(paragraph, size_pt);
    let mut strip = 0usize;
    if padding_chars > 0 && (padding - drawn_padding).abs() > 0.5 {
        let halves = (padding / (size_pt * 0.5)).round().max(1.0) as usize;
        let mut pad = "\u{3000}".repeat(halves / 2);
        if halves % 2 == 1 {
            pad.push(' ');
        }
        skip += pad.len();
        strip = padding_chars;
        push_text_run(&mut text, &mut runs, &pad, &face, color, None, false, false);
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
            let image_element = img(image.image.clone())
                .w(px((image.width * scale).max(1.0)))
                .h(px((image.height * scale).max(1.0)))
                .flex_shrink_0()
                .into_any_element();
            // Whitespace met so far — pending text or padding blocks the split
            // below already filed away — rides on the picture's line: Word
            // draws an inline picture on the text line, and the spaces the
            // paragraph opens with indent that line rather than sitting above
            // it.
            let pending_whitespace = (!text.is_empty() && text.trim().is_empty())
                || whitespace_blocks > 0;
            if pending_whitespace {
                if whitespace_blocks > 0 {
                    let tail = blocks.len() - whitespace_blocks;
                    inline.extend(blocks.drain(tail..));
                    whitespace_blocks = 0;
                }
                if !text.is_empty() {
                    inline.push(render.text_block(
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
                        BlockWidth::Natural,
                    ));
                }
                inline.push(image_element);
                continue;
            }
            if text.trim().is_empty() && blocks.is_empty() {
                // Pictures that come before any real text wait here for the
                // text block they share.
                inline.push(image_element);
                continue;
            }
            // A picture in the middle of the text keeps its own block.
            if !inline.is_empty() || !text.is_empty() {
                blocks.push(flush_inline(
                    render,
                    &mut inline,
                    &mut text,
                    &mut runs,
                    &mut skip,
                    paragraph_id,
                    row,
                    size,
                    line_height,
                    color,
                    paragraph.align,
                    false,
                    indent_px,
                ));
            }
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
            whitespace_blocks = 0;
            continue;
        }

        // The block drawn so far is pure whitespace in another size — documents
        // pad lines with spaces that carry their own font size, and drawing
        // them in the text size pushes everything after them sideways. Give
        // the padding its own block in its own size.
        let run_size = (run.size.unwrap_or(paragraph.size) * scale).max(4.0);
        if (run_size - size).abs() > 0.5 && text.trim().is_empty() {
            if !text.is_empty() {
                // Natural width: a whitespace block carries nothing visible, so
                // the width only matters when a picture pulls the block onto
                // its inline line, where spanning the column would push
                // everything after it off the page.
                let block = render.text_block(
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
                    BlockWidth::Natural,
                );
                if inline.is_empty() {
                    blocks.push(block);
                    whitespace_blocks += 1;
                } else {
                    inline.push(block);
                }
            }
            size = run_size;
            line_height = size * 1.3 * paragraph.line_spacing.max(0.8);
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
        // Drop the padding characters the synthetic indent stands for.
        let mut content = run.text.as_str();
        if strip > 0 {
            let mut taken = 0usize;
            let mut bytes = 0usize;
            for ch in content.chars() {
                if taken == strip || !ch.is_whitespace() {
                    break;
                }
                taken += 1;
                bytes += ch.len_utf8();
            }
            strip -= taken;
            content = &content[bytes..];
        }
        push_text_run(
            &mut text,
            &mut runs,
            content,
            &run_face,
            run_color,
            run.highlight.map(|color| rgb(color).into()),
            run.underline,
            run.strike,
        );
    }

    if !text.is_empty() {
        // Word lets the spaces at the end of a paragraph hang past the margin
        // instead of wrapping on them, and documents pad lines with runs of
        // highlighted spaces. gpui never offers a wrap candidate inside
        // whitespace, so once that padding overflows the line it would wrap in
        // the middle of the real text — drop it from the end of the paragraph.
        trim_trailing_whitespace(&mut text, &mut runs);
        if inline.is_empty() {
            blocks.push(render.text_block(
                text,
                runs,
                LinePlace {
                    paragraph: paragraph_id,
                    row,
                    skip: std::mem::take(&mut skip),
                },
                size,
                line_height,
                color,
                BlockWidth::Column,
            ));
        } else {
            inline.push(render.text_block(
                text,
                runs,
                LinePlace {
                    paragraph: paragraph_id,
                    row,
                    skip: std::mem::take(&mut skip),
                },
                size,
                line_height,
                color,
                BlockWidth::Grow,
            ));
            let line = h_flex().w_full().items_end();
            let line = if indent_px > 0.0 {
                line.ml(px(indent_px))
            } else {
                line
            };
            let line = match paragraph.align {
                word::Align::Center => line.justify_center(),
                word::Align::Right => line.justify_end(),
                _ => line,
            };
            blocks.push(line.children(inline).into_any_element());
        }
    } else {
        // A paragraph that ends in pictures without any text after them draws
        // each picture as its own block, centred like the old standalone ones.
        for element in inline {
            blocks.push(match paragraph.align {
                word::Align::Center => div()
                    .w_full()
                    .flex()
                    .justify_center()
                    .child(element)
                    .into_any_element(),
                word::Align::Right => div()
                    .w_full()
                    .flex()
                    .justify_end()
                    .child(element)
                    .into_any_element(),
                _ => element,
            });
        }
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

pub(crate) fn push_text_run(
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

/// Measures the whitespace a paragraph starts with, both at the size each run
/// carries and at the size the paragraph is drawn with. Word places a padded
/// title with spaces of a larger size, so the two differ and the padding has to
/// be rebuilt in the drawn size to keep the line where Word puts it.
///
/// Returns the width at the runs' own sizes, the width at `size`, and how many
/// characters make up the padding. All three are zero when the paragraph starts
/// with real text or is nothing but whitespace.
pub(crate) fn leading_padding(paragraph: &word::Paragraph, size: f32) -> (f32, f32, usize) {
    let mut width = 0.0;
    let mut drawn = 0.0;
    let mut count = 0usize;
    let mut real_text = false;
    for run in &paragraph.runs {
        if run.image.is_some() {
            real_text = true;
            break;
        }
        let run_size = run.size.unwrap_or(paragraph.size);
        for ch in run.text.chars() {
            let advance = match ch {
                ' ' => 0.5,
                '\u{3000}' => 1.0,
                _ => {
                    real_text = true;
                    break;
                }
            };
            width += advance * run_size;
            drawn += advance * size;
            count += 1;
        }
        if real_text {
            break;
        }
    }
    if real_text && count > 0 {
        (width, drawn, count)
    } else {
        (0.0, 0.0, 0)
    }
}

/// Builds the line that carries the paragraph's leading pictures and the text
/// that follows them on the same baseline. Called when a picture meets real
/// text in the middle of a paragraph, and once more at its end.
#[allow(clippy::too_many_arguments)]
fn flush_inline(
    render: &mut WordRender,
    inline: &mut Vec<AnyElement>,
    text: &mut String,
    runs: &mut Vec<TextRun>,
    skip: &mut usize,
    paragraph_id: usize,
    row: Option<usize>,
    size: f32,
    line_height: f32,
    color: Hsla,
    align: word::Align,
    trim: bool,
    indent_px: f32,
) -> AnyElement {
    if !text.is_empty() {
        if trim {
            trim_trailing_whitespace(text, runs);
        }
        inline.push(render.text_block(
            std::mem::take(text),
            std::mem::take(runs),
            LinePlace {
                paragraph: paragraph_id,
                row,
                skip: std::mem::take(skip),
            },
            size,
            line_height,
            color,
            BlockWidth::Grow,
        ));
    }
    let line = h_flex().w_full().items_end();
    let line = if indent_px > 0.0 {
        line.ml(px(indent_px))
    } else {
        line
    };
    let line = match align {
        word::Align::Center => line.justify_center(),
        word::Align::Right => line.justify_end(),
        _ => line,
    };
    line.children(std::mem::take(inline)).into_any_element()
}

/// Drops the whitespace at the end of a paragraph's text, trimming the byte
/// lengths of the runs that carry it so the text and its runs stay in sync.
pub(crate) fn trim_trailing_whitespace(text: &mut String, runs: &mut Vec<TextRun>) {
    // A hard line break at the end of the paragraph still counts, so only the
    // spaces in front of it are hung.
    let kept = text
        .trim_end_matches(|ch: char| ch != '\n' && ch != '\r' && ch.is_whitespace())
        .len();
    if kept == text.len() {
        return;
    }
    let mut dropped = text.len() - kept;
    while dropped > 0 {
        match runs.last_mut() {
            Some(last) if last.len > dropped => {
                last.len -= dropped;
                dropped = 0;
            }
            Some(last) => {
                dropped -= last.len;
                runs.pop();
            }
            None => break,
        }
    }
    text.truncate(kept);
}

fn word_table(table: &word::Table, render: &mut WordRender) -> AnyElement {
    let scale = render.scale;
    let border = render.foreground.opacity(0.35);
    // Rough heights of every row, in points, so the contents of a merged
    // cell can be centred across all the rows it spans.
    let row_heights = estimated_row_heights(table, scale);
    let mut rows = Vec::new();
    // Rows each column still has to cover, so the last continuation cell of a
    // merge knows to close the merged area with a bottom line.
    let mut remaining: HashMap<usize, usize> = HashMap::new();
    for (index, row) in table.rows.iter().enumerate() {
        let mut cells = Vec::new();
        let mut column = 0usize;
        let row_id = render.row_id();
        for cell in row {
            let span = cell.span.max(1);
            let width = (column..column + span)
                .map(|index| table.columns.get(index).copied().unwrap_or(60.0) * scale)
                .sum::<f32>()
                .max(24.0);
            // Points of room the text has inside the cell's side padding.
            let width_pt = (width - 14.0) / scale;
            let start = column;
            column += span;

            // A continuation cell holds no content; it only carries the side
            // borders so the merged area above stays one cell.
            if cell.covered {
                let closing = remaining.get(&start) == Some(&1);
                if let Some(count) = remaining.get_mut(&start) {
                    *count -= 1;
                    if *count == 0 {
                        remaining.remove(&start);
                    }
                }
                let mut element = div().flex_shrink_0().w(px(width));
                if table.borders {
                    element = element.border_l_1().border_r_1().border_color(border);
                    element = if closing {
                        element.border_b_1()
                    } else {
                        element
                    }
                    .border_color(border);
                }
                cells.push(element.into_any_element());
                continue;
            }
            remaining.remove(&start);
            if cell.row_span > 1 {
                remaining.insert(start, cell.row_span - 1);
            }

            let mut element = div().flex_shrink_0().w(px(width)).px(px(7.));
            if let Some(fill) = cell.fill {
                element = element.bg(rgb(fill));
            }
            if table.borders {
                // A cell that merges downwards draws no bottom line, so the
                // covered rows below connect to it seamlessly.
                element = if cell.row_span > 1 {
                    element
                        .border_t_1()
                        .border_l_1()
                        .border_r_1()
                        .border_color(border)
                } else {
                    element.border_1().border_color(border)
                };
            }
            // Non-merged cells stretch with their row, so the vertical
            // alignment is a plain spacer inside the flow. A merged cell
            // spans several rows whose boxes carry no content; its contents
            // are centred across all of them with a spacer computed from the
            // estimated row heights.
            let spacer = if cell.row_span > 1 {
                merged_spacer(cell, &row_heights[index..index + cell.row_span], width_pt)
            } else {
                match cell.v_align {
                    word::VAlign::Top => 0.0,
                    word::VAlign::Center => {
                        ((row_heights[index] - estimated_blocks_height(&cell.blocks, width_pt))
                            / 2.0)
                            .max(0.0)
                    }
                    word::VAlign::Bottom => {
                        (row_heights[index] - estimated_blocks_height(&cell.blocks, width_pt))
                            .max(0.0)
                    }
                }
            };
            if cell.row_span > 1 && spacer > 0.5 {
                // A spacer in the flow would stretch the first spanned row to
                // half the merged area, so the centred content sits absolutely
                // on top of the covered rows instead.
                let mut content = v_flex().w_full();
                for block in &cell.blocks {
                    content = content.child(word_block(block, render, Some(row_id)));
                }
                element = element
                    .relative()
                    .child(
                        div()
                            .absolute()
                            .top(px(spacer * scale))
                            .left(px(7.))
                            .right(px(7.))
                            .child(content),
                    );
            } else {
                let mut blocks = Vec::with_capacity(cell.blocks.len());
                if spacer > 0.5 {
                    blocks.push(div().h(px(spacer * scale)).into_any_element());
                }
                for block in &cell.blocks {
                    blocks.push(word_block(block, render, Some(row_id)));
                }
                element = element.child(v_flex().w_full().children(blocks));
            }
            cells.push(element.into_any_element());
        }
        // The stored minimum height keeps short rows as tall as the file
        // asks; the row still grows when a cell needs more room.
        let row_element = h_flex().w_full().items_stretch().children(cells);
        let min_height = table.row_heights.get(index).copied().unwrap_or(0.0);
        let row_element = if min_height > 0.0 {
            row_element.min_h(px(min_height * scale))
        } else {
            row_element
        };
        rows.push(row_element.into_any_element());
    }

    // Word lets a table reach into the margin, so the whole sheet is shifted
    // by the table's own indent instead of starting at the text column.
    let sheet = v_flex().children(rows);
    if table.indent.abs() > 0.05 {
        sheet.ml(px(table.indent * scale))
    } else {
        sheet.w_full()
    }
    .into_any_element()
}

/// Rough height of every table row in points: the tallest cell's contents,
/// or the stored minimum height, whichever is larger.
pub(crate) fn estimated_row_heights(table: &word::Table, scale: f32) -> Vec<f32> {
    // Cells carry 7 px of side padding, which is only 7/scale points: at a
    // zoomed-in scale the padding must not eat into the text column twice.
    let padding = 14.0 / scale.max(0.1);
    table
        .rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let mut column = 0usize;
            let mut height = 0.0_f32;
            for cell in row {
                let span = cell.span.max(1);
                let width: f32 = (column..column + span)
                    .map(|at| table.columns.get(at).copied().unwrap_or(60.0))
                    .sum::<f32>();
                column += span;
                if cell.covered {
                    continue;
                }
                // Word puts no space above or below the text, so only the
                // borders add to the estimated content height.
                let content =
                    estimated_blocks_height(&cell.blocks, (width - padding).max(20.0)) + 1.0;
                height = height.max(content);
            }
            height.max(table.row_heights.get(index).copied().unwrap_or(0.0))
        })
        .collect()
}

/// Rough height of the blocks of one cell, in points.
pub(crate) fn estimated_blocks_height(blocks: &[word::Block], width: f32) -> f32 {
    blocks
        .iter()
        .map(|block| match block {
            word::Block::Paragraph(paragraph) => estimated_paragraph_height(paragraph, width),
            word::Block::Table(_) => 0.0,
        })
        .sum()
}

/// Rough height of one paragraph in points: the text is one em wide per CJK
/// glyph and half an em per other character; lines wrap at the cell width.
pub(crate) fn estimated_paragraph_height(paragraph: &word::Paragraph, width: f32) -> f32 {
    let size = paragraph.draw_size();
    let indent = ((paragraph.first_line.max(0.0) / size).round().clamp(0.0, 8.0)) * size;
    // Trailing whitespace is hung by Word, never wrapped on, so it takes no
    // part in the line count.
    let mut text = String::new();
    let mut text_width = indent;
    for run in &paragraph.runs {
        if let Some(image) = &run.image {
            text_width += image.width;
            continue;
        }
        text.push_str(&run.text);
    }
    for ch in text.trim_end().chars() {
        text_width += if is_wide_char(ch) { size } else { size * 0.5 };
    }
    // Leading padding is drawn as ideographic spaces of the drawn size, which
    // is wider than the spaces themselves when they carry a larger size.
    let (padding, drawn_padding, _) = leading_padding(paragraph, size);
    if padding > drawn_padding {
        text_width += padding - drawn_padding;
    }
    let lines = (text_width / width).ceil().max(1.0);
    let line_height = size * 1.3 * paragraph.line_spacing;
    lines * line_height + paragraph.space_before + paragraph.space_after
}

fn is_wide_char(ch: char) -> bool {
    let c = ch as u32;
    (0x1100..=0x115F).contains(&c)
        || (0x2E80..=0xA4CF).contains(&c)
        || (0xAC00..=0xD7A3).contains(&c)
        || (0xF900..=0xFAFF).contains(&c)
        || (0xFE30..=0xFE4F).contains(&c)
        || (0xFF00..=0xFF60).contains(&c)
        || (0xFFE0..=0xFFE6).contains(&c)
        || (0x20000..=0x3FFFD).contains(&c)
}

/// The vertical offset, in points, of a merged cell's contents inside the
/// rows it spans, so the contents sit where the cell's vertical alignment
/// wants them even though the row boxes below carry no content.
pub(crate) fn merged_spacer(cell: &word::TableCell, spanned_heights: &[f32], width_pt: f32) -> f32 {
    let merged: f32 = spanned_heights.iter().sum();
    let content = estimated_blocks_height(&cell.blocks, width_pt);
    let free = (merged - content).max(0.0);
    match cell.v_align {
        word::VAlign::Top => 0.0,
        word::VAlign::Center => free / 2.0,
        word::VAlign::Bottom => free,
    }
}
