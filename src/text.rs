//! Laying a text document out into rows.
//!
//! The reader must stay responsive for very large files, so the source text is
//! wrapped once per font size and wrap width and the view only draws the rows
//! that are on screen. Row tops stay here, so hit testing, scrolling and the
//! scrollbar never depend on which rows happen to be rendered.
//!
//! Wrapping is the only step that is proportional to the file size, so the
//! character widths are measured once in batch (see [`crate::wrap`]) and large
//! files are then split at line boundaries and wrapped on several threads.
//! Chunk boundaries always fall right after a `\n`, which makes the per-chunk
//! work independent: rows from different chunks never share a source line, so
//! concatenating the per-chunk results in chunk order reproduces exactly what
//! a single pass over the whole file would have produced.

use std::sync::Arc;

use gpui_kit::*;

use crate::wrap::{wrap_line, CharWidths};

/// Space between two rows, in logical pixels.
pub const LINE_GAP: f32 = 4.;
/// Blank source lines are shorter than text rows, which reads as a paragraph break.
const BLANK_LINE_RATIO: f32 = 0.6;
/// Never wrap narrower than this, so a very narrow window still lays out sensibly.
const MIN_WRAP_WIDTH: f32 = 120.;
/// Files smaller than this are wrapped on one thread; splitting a small file
/// across threads costs more in thread setup than the wrapping itself saves.
const PARALLEL_WRAP_THRESHOLD: usize = 256 * 1024;

/// One visual row: a slice of the source text and where it sits vertically.
struct Row {
    start: usize,
    end: usize,
    /// The row continues a wrapped source line instead of starting one.
    continuation: bool,
}

/// One worker's share of the wrapping work. Offsets are relative to the
/// chunk's base; merging translates them back to absolute source offsets and
/// accumulates the heights.
struct ChunkRows {
    base: usize,
    rows: Vec<(usize, usize, bool)>,
    line_count: usize,
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

        // Chunks are cut at line boundaries; every worker reads the same
        // immutable width cache.
        let widths = CharWidths::measure(&source, font, font_size, text_system);
        let chunks = split_points(&source, parallel_workers(source.len()));
        let finished: Vec<ChunkRows> = if chunks.len() == 2 {
            vec![wrap_chunk(
                &source,
                chunks[0],
                chunks[1],
                &widths,
                wrap_width,
            )]
        } else {
            std::thread::scope(|scope| {
                let workers: Vec<_> = chunks
                    .windows(2)
                    .map(|pair| {
                        let (start, end) = (pair[0], pair[1]);
                        let source = &source;
                        let widths = &widths;
                        scope.spawn(move || {
                            wrap_chunk(source, start, end, widths, wrap_width)
                        })
                    })
                    .collect();
                workers
                    .into_iter()
                    .map(|worker| worker.join().unwrap())
                    .collect()
            })
        };

        let mut rows = Vec::new();
        let mut tops = Vec::with_capacity(256);
        tops.push(0.);
        let mut line_count = 0;
        for chunk in finished {
            line_count += chunk.line_count;
            for (start, end, continuation) in chunk.rows {
                push_row(
                    &mut rows,
                    &mut tops,
                    chunk.base,
                    start,
                    end,
                    continuation,
                    line_height,
                    blank_height,
                );
            }
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

/// How many workers to use for a file of `size` bytes. Small files stay on one
/// thread; large ones use every core, capped so no thread gets a sliver.
fn parallel_workers(size: usize) -> usize {
    if size < PARALLEL_WRAP_THRESHOLD {
        return 1;
    }
    std::thread::available_parallelism()
        .map_or(1, |count| count.get())
        .min(8)
}

/// Cut the text into `workers` chunks at `\n` boundaries. The result always
/// starts at 0 and ends at `text.len()`, with each interior point just after a
/// newline, so every chunk starts on a fresh source line.
fn split_points(text: &str, workers: usize) -> Vec<usize> {
    let mut points = vec![0];
    if workers <= 1 || text.len() < workers * 2 {
        points.push(text.len());
        return points;
    }

    let target = text.len() / workers;
    let mut search = target;
    while points.len() < workers {
        // The target may land inside a multi-byte character; slide forward to
        // the next char boundary before searching.
        while search < text.len() && !text.is_char_boundary(search) {
            search += 1;
        }
        match text[search..].find('\n') {
            Some(index) => {
                let point = search + index + 1;
                points.push(point);
                search = point + target;
            }
            None => break,
        }
    }
    points.push(text.len());
    points
}

/// Wrap `text[base..end]` into rows. `end` falls right after a `\n` (or at the
/// end of the text), so every line the slice contains is complete and no state
/// carries over to neighbouring chunks.
fn wrap_chunk(
    text: &str,
    base: usize,
    end: usize,
    widths: &CharWidths,
    wrap_width: f32,
) -> ChunkRows {
    let slice = &text[base..end];
    let mut rows = Vec::new();
    let mut line_count = 0;
    let mut offset = 0;

    while offset < slice.len() {
        let line_break = slice[offset..]
            .find('\n')
            .map_or(slice.len(), |index| offset + index);
        // Windows files end their lines with "\r\n"; the carriage return is
        // not part of the text.
        let line_end = if line_break > offset && slice.as_bytes()[line_break - 1] == b'\r' {
            line_break - 1
        } else {
            line_break
        };
        line_count += 1;

        let line = &slice[offset..line_end];
        let mut row_start = 0;
        let mut first = true;

        if !line.is_empty() {
            for boundary in wrap_line(line, wrap_width, widths) {
                if boundary <= row_start {
                    continue;
                }
                rows.push((offset + row_start, offset + boundary, !first));
                row_start = boundary;
                first = false;
            }
        }

        rows.push((offset + row_start, offset + line.len(), !first));
        offset = line_break + 1;
    }

    ChunkRows {
        base,
        rows,
        line_count,
    }
}

fn blank_height(font_size: Pixels) -> f32 {
    (font_size.as_f32() * BLANK_LINE_RATIO).max(1.)
}

/// Record one row against the absolute source text.
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
