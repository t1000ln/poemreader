//! Laying a text document out into rows.
//!
//! The reader must stay responsive for very large files, so the source text is
//! wrapped once per font size and wrap width and the view only draws the rows
//! that are on screen. Row tops stay here, so hit testing, scrolling and the
//! scrollbar never depend on which rows happen to be rendered.

use std::sync::Arc;

use gpui_kit::*;

/// Space between two rows, in logical pixels.
pub const LINE_GAP: f32 = 4.;
/// Blank source lines are shorter than text rows, which reads as a paragraph break.
const BLANK_LINE_RATIO: f32 = 0.6;
/// Never wrap narrower than this, so a very narrow window still lays out sensibly.
const MIN_WRAP_WIDTH: f32 = 120.;

/// One visual row: a slice of the source text and where it sits vertically.
struct Row {
    start: usize,
    end: usize,
    /// The row continues a wrapped source line instead of starting one.
    continuation: bool,
}

/// A text document split into visual rows for one font and wrap width.
pub struct TextView {
    source: Arc<String>,
    rows: Vec<Row>,
    /// Top of every row in text coordinates, relative to the first row. The
    /// last entry is the height of the whole block.
    tops: Vec<f32>,
    /// Lines in the source text, which is what the status bar counts.
    line_count: usize,
    font: Font,
    font_size: Pixels,
    line_height: f32,
    wrap_width: f32,
}

impl TextView {
    pub fn new(
        source: Arc<String>,
        font: &Font,
        font_size: Pixels,
        line_height: f32,
        wrap_width: f32,
        text_system: &Arc<TextSystem>,
    ) -> Self {
        let wrap_width = wrap_width.max(MIN_WRAP_WIDTH);
        let line_height = line_height.max(1.);
        let blank_height = blank_height(font_size);

        // The wrapper caches character widths per font and size, so wrapping a
        // large file is a single pass over its text.
        let mut wrapper = text_system.line_wrapper(font.clone(), font_size);
        let text = source.as_str();
        let mut rows = Vec::new();
        let mut tops = Vec::with_capacity(256);
        tops.push(0.);
        let mut line_count = 0;
        let mut offset = 0;

        while offset < text.len() {
            let end = text[offset..]
                .find('\n')
                .map_or(text.len(), |index| offset + index);
            // Windows files end their lines with "\r\n"; the carriage return is
            // not part of the text.
            let line_end = if end > offset && text.as_bytes()[end - 1] == b'\r' {
                end - 1
            } else {
                end
            };
            line_count += 1;

            let line = &text[offset..line_end];
            let mut row_start = 0;
            let mut first = true;

            if !line.is_empty() {
                let boundaries: Vec<usize> = wrapper
                    .wrap_line(
                        &[LineFragment::text(line)],
                        px(wrap_width),
                        IndentAdjustment::NoIndent,
                    )
                    .map(|boundary| boundary.ix.min(line.len()))
                    .collect();

                for boundary in boundaries {
                    if boundary <= row_start {
                        continue;
                    }
                    push_row(
                        &mut rows,
                        &mut tops,
                        offset,
                        row_start,
                        boundary,
                        !first,
                        line_height,
                        blank_height,
                    );
                    row_start = boundary;
                    first = false;
                }
            }

            push_row(
                &mut rows,
                &mut tops,
                offset,
                row_start,
                line.len(),
                !first,
                line_height,
                blank_height,
            );
            offset = end + 1;
        }

        Self {
            source,
            rows,
            tops,
            line_count,
            font: font.clone(),
            font_size,
            line_height,
            wrap_width,
        }
    }

    /// Number of visual rows.
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// Number of lines in the source text.
    pub fn line_count(&self) -> usize {
        self.line_count
    }

    pub fn source(&self) -> &Arc<String> {
        &self.source
    }

    pub fn wrap_width(&self) -> f32 {
        self.wrap_width
    }

    /// Height of the whole text block.
    pub fn content_height(&self) -> f32 {
        self.tops.last().copied().unwrap_or(0.)
    }

    /// True when the rows were laid out for this font, size and width.
    pub fn matches(
        &self,
        font: &Font,
        font_size: Pixels,
        line_height: f32,
        wrap_width: f32,
    ) -> bool {
        self.font_size == font_size
            && self.font == *font
            && (self.line_height - line_height).abs() < 0.01
            && (self.wrap_width - wrap_width).abs() < 0.01
    }

    pub fn row_text(&self, index: usize) -> &str {
        match self.rows.get(index) {
            Some(row) => &self.source[row.start..row.end],
            None => "",
        }
    }

    /// True when the row starts a line of the source text rather than continuing
    /// one that was wrapped.
    pub fn row_starts_line(&self, index: usize) -> bool {
        self.rows.get(index).is_some_and(|row| !row.continuation)
    }

    pub fn row_top(&self, index: usize) -> f32 {
        self.tops.get(index).copied().unwrap_or(0.)
    }

    pub fn row_height(&self, index: usize) -> f32 {
        match self.rows.get(index) {
            Some(row) if row.start == row.end => blank_height(self.font_size),
            _ => self.line_height,
        }
    }

    /// The row covering `offset`, measured down from the start of the block.
    pub fn row_at(&self, offset: f32) -> usize {
        if self.rows.is_empty() || offset <= 0. {
            return 0;
        }

        // `tops` is sorted and its last entry is the block height, so the row
        // under the offset is the last one that starts at or above it.
        let index = self.tops.partition_point(|top| *top <= offset);
        index
            .saturating_sub(1)
            .min(self.rows.len().saturating_sub(1))
    }

    /// Distance from the start of the block to the row holding `source_offset`.
    pub fn offset_for_source(&self, source_offset: usize) -> f32 {
        if self.rows.is_empty() {
            return 0.;
        }
        let index = self.rows.partition_point(|row| row.start <= source_offset);
        self.row_top(index.saturating_sub(1))
    }

    /// Source byte offset at the top of the row covering `offset`.
    pub fn source_at(&self, offset: f32) -> usize {
        self.rows
            .get(self.row_at(offset))
            .map(|row| row.start)
            .unwrap_or(0)
    }
}

fn blank_height(font_size: Pixels) -> f32 {
    (font_size.as_f32() * BLANK_LINE_RATIO).max(1.)
}

fn push_row(
    rows: &mut Vec<Row>,
    tops: &mut Vec<f32>,
    base: usize,
    start: usize,
    end: usize,
    continuation: bool,
    line_height: f32,
    blank_height: f32,
) {
    rows.push(Row {
        start: base + start,
        end: base + end,
        continuation,
    });

    let height = if start == end {
        blank_height
    } else {
        line_height
    };
    let top = tops.last().copied().unwrap_or(0.);
    tops.push(top + height + LINE_GAP);
}
