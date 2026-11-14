//! Reading Markdown into the block model the reader already draws for Word.
//!
//! Reusing [`word::Block`] keeps the Markdown view consistent with `.docx`:
//! selection, copying, zooming and printing all work without a second layout
//! engine.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ::markdown::mdast::{self, Node};
use ::markdown::{ParseOptions, to_mdast};
use gpui_kit::RenderImage;
use image::Frame;

use crate::word::{
    Align, Block, InlineImage, Paragraph, ParagraphBorder, Run, Table, TableCell, WordDocument,
};

/// Width of the text column, in points. Six and a half inches matches a Word
/// page with one-inch margins and prints without being scaled down.
const CONTENT_WIDTH: f32 = 468.0;
/// Body text size, in points.
const BODY_SIZE: f32 = 12.0;
const COMPACT_SIZE: f32 = 10.5;
const CODE_SIZE: f32 = 10.0;
/// Code is drawn as a filled card so it stays legible on both the light and
/// the dark reading backgrounds.
const CODE_BACKGROUND: u32 = 0xF2F2F2;
const CODE_FOREGROUND: u32 = 0x24292F;
/// Code blocks are outlined so the whole block reads as one unit, even where
/// the fill ends at the last character.
const CODE_BORDER: u32 = 0xC3CAD1;
/// Corner radius of that outline, in points.
const CODE_RADIUS: f32 = 6.0;
const HEADER_FILL: u32 = 0xEDEDED;
const HEADER_TEXT: u32 = 0x24292F;
const RULE_COLOR: u32 = 0x9A9A9A;
const BLOCKQUOTE_INDENT: f32 = 18.0;
const LIST_INDENT: f32 = 16.0;
const CODE_INDENT: f32 = 10.0;

#[cfg(windows)]
const CODE_FONT: &str = "Consolas";
#[cfg(target_os = "macos")]
const CODE_FONT: &str = "Menlo";
#[cfg(not(any(windows, target_os = "macos")))]
const CODE_FONT: &str = "DejaVu Sans Mono";

/// Parse Markdown into the block model used by the Word renderer.
///
/// `base` is the directory of the document, used to resolve relative image
/// paths.
pub fn load(text: &str, base: &Path) -> Result<WordDocument, String> {
    let tree = to_mdast(text, &ParseOptions::gfm())
        .map_err(|error| format!("无法解析 Markdown：{error}"))?;
    let Node::Root(root) = tree else {
        return Err(String::from("这个 Markdown 文件没有可显示的内容"));
    };

    let mut definitions = HashMap::new();
    collect_definitions(&root.children, &mut definitions);

    let mut builder = Builder {
        base,
        blocks: Vec::new(),
        paragraphs: 0,
        definitions,
        images: HashMap::new(),
    };
    builder.flow(&root.children, FlowCtx::default());

    if builder.blocks.is_empty() {
        return Err(String::from("这个 Markdown 文件没有可显示的内容"));
    }

    Ok(WordDocument {
        blocks: builder.blocks,
        content_width: CONTENT_WIDTH,
        paragraphs: builder.paragraphs,
    })
}

#[derive(Clone, Copy, Default)]
struct FlowCtx {
    indent: f32,
    /// Whether the text is in a table's header row, which is drawn on a light
    /// fill and therefore needs a dark colour of its own.
    header: bool,
    /// Tables use a slightly smaller body size so more columns fit.
    compact: bool,
}

impl FlowCtx {
    fn with_indent(self, indent: f32) -> Self {
        Self { indent, ..self }
    }

    fn body_size(self) -> f32 {
        if self.compact {
            COMPACT_SIZE
        } else {
            BODY_SIZE
        }
    }
}

/// A link or image target collected from the definitions of the document.
struct Resource {
    url: String,
}

struct Builder<'a> {
    base: &'a Path,
    blocks: Vec<Block>,
    paragraphs: usize,
    definitions: HashMap<String, Resource>,
    /// Decoded images, keyed by URL, so a repeated badge is only read once.
    images: HashMap<String, Option<InlineImage>>,
}

#[derive(Clone, Copy, Default)]
struct InlineStyle {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    code: bool,
    /// Text on a filled table header cell.
    header: bool,
}

impl Builder<'_> {
    fn flow(&mut self, nodes: &[Node], ctx: FlowCtx) {
        for node in nodes {
            self.block(node, ctx);
        }
    }

    fn block(&mut self, node: &Node, ctx: FlowCtx) {
        match node {
            Node::Paragraph(paragraph) => {
                let runs = self.runs(&paragraph.children, style_of(ctx));
                self.push_paragraph(runs, ctx, None, ParaKind::Body);
            }
            Node::Heading(heading) => {
                let runs = self.runs(&heading.children, style_of(ctx));
                self.push_paragraph(runs, ctx, None, ParaKind::Heading(heading.depth));
            }
            Node::List(list) => self.list(list, ctx),
            Node::Blockquote(quote) => {
                self.flow(
                    &quote.children,
                    ctx.with_indent(ctx.indent + BLOCKQUOTE_INDENT),
                );
            }
            Node::Code(code) => self.code_block(&code.value, ctx),
            Node::Table(table) => self.table(table, ctx),
            Node::ThematicBreak(_) => self.rule(ctx),
            Node::Html(html) => self.html_block(&html.value, ctx),
            Node::FootnoteDefinition(definition) => self.footnote(definition, ctx),
            Node::Definition(_)
            | Node::Yaml(_)
            | Node::Toml(_)
            | Node::MdxjsEsm(_)
            | Node::MdxFlowExpression(_)
            | Node::Math(_) => {}
            // Anything else that turns up here is phrasing content.
            other => {
                let runs = self.runs(std::slice::from_ref(other), style_of(ctx));
                self.push_paragraph(runs, ctx, None, ParaKind::Body);
            }
        }
    }

    fn list(&mut self, list: &mdast::List, ctx: FlowCtx) {
        let mut number = list.start.unwrap_or(1);
        let spread = list.spread;

        for child in &list.children {
            let Node::ListItem(item) = child else {
                continue;
            };

            let marker = if list.ordered {
                let marker = format!("{number}.");
                number += 1;
                marker
            } else {
                String::from("•")
            };
            let marker = match item.checked {
                Some(true) => String::from("☑"),
                Some(false) => String::from("☐"),
                None => marker,
            };

            let item_ctx = ctx.with_indent(ctx.indent + LIST_INDENT);
            let mut marker = Some(marker);
            for child in &item.children {
                if let Node::Paragraph(paragraph) = child
                    && marker.is_some()
                {
                    let runs = self.runs(&paragraph.children, style_of(ctx));
                    self.push_paragraph(runs, item_ctx, marker.take(), ParaKind::List { spread });
                    continue;
                }

                if marker.is_some() {
                    // The item starts with a code block or a nested list, so
                    // the marker gets a line of its own.
                    self.push_paragraph(
                        Vec::new(),
                        item_ctx,
                        marker.take(),
                        ParaKind::List { spread },
                    );
                }
                self.block(child, item_ctx);
            }

            if let Some(marker) = marker {
                self.push_paragraph(
                    Vec::new(),
                    item_ctx,
                    Some(marker),
                    ParaKind::List { spread },
                );
            }
        }
    }

    fn footnote(&mut self, definition: &mdast::FootnoteDefinition, ctx: FlowCtx) {
        let mut marker = Some(format!("[^{}]", definition.identifier));
        let item_ctx = ctx.with_indent(ctx.indent + LIST_INDENT);
        for child in &definition.children {
            if let Node::Paragraph(paragraph) = child
                && marker.is_some()
            {
                let runs = self.runs(&paragraph.children, style_of(ctx));
                self.push_paragraph(runs, item_ctx, marker.take(), ParaKind::Body);
                continue;
            }
            if marker.is_some() {
                self.push_paragraph(Vec::new(), item_ctx, marker.take(), ParaKind::Body);
            }
            self.block(child, item_ctx);
        }
        if let Some(marker) = marker {
            self.push_paragraph(Vec::new(), item_ctx, Some(marker), ParaKind::Body);
        }
    }

    fn code_block(&mut self, value: &str, ctx: FlowCtx) {
        let lines = value.lines().collect::<Vec<_>>();
        let lines = if lines.is_empty() { vec![""] } else { lines };
        let last = lines.len() - 1;
        let indent = ctx.indent + CODE_INDENT;

        for (index, line) in lines.into_iter().enumerate() {
            let text = line.replace('\t', "    ");
            // A blank line still needs something to paint the card with.
            let text = if text.is_empty() {
                String::from(" ")
            } else {
                text
            };
            let run = Run {
                text,
                bold: false,
                italic: false,
                underline: false,
                strike: false,
                size: None,
                color: Some(CODE_FOREGROUND),
                // The paragraph fills the whole card, so the characters need no
                // highlight of their own.
                highlight: None,
                font: Some(String::from(CODE_FONT)),
                image: None,
            };
            let kind = ParaKind::CodeLine {
                first: index == 0,
                last: index == last,
            };
            self.push_paragraph(vec![run], ctx.with_indent(indent), None, kind);
        }
    }

    fn rule(&mut self, ctx: FlowCtx) {
        let run = Run {
            text: "─".repeat(36),
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            size: None,
            color: Some(RULE_COLOR),
            highlight: None,
            font: None,
            image: None,
        };
        self.push_paragraph(vec![run], ctx, None, ParaKind::Rule);
    }

    fn html_block(&mut self, html: &str, ctx: FlowCtx) {
        let text = strip_html(html);
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let runs = vec![Run {
            text: text.to_string(),
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            size: None,
            color: None,
            highlight: None,
            font: None,
            image: None,
        }];
        self.push_paragraph(runs, ctx, None, ParaKind::Body);
    }

    fn table(&mut self, table: &mdast::Table, ctx: FlowCtx) {
        let rows = table
            .children
            .iter()
            .filter_map(|row| match row {
                Node::TableRow(row) => Some(row),
                _ => None,
            })
            .collect::<Vec<_>>();
        if rows.is_empty() {
            return;
        }

        let columns = rows
            .iter()
            .map(|row| row.children.len())
            .chain(std::iter::once(table.align.len()))
            .max()
            .unwrap_or(0);
        if columns == 0 {
            return;
        }

        let widths = column_widths(&rows, columns, (CONTENT_WIDTH - ctx.indent).max(120.0));
        let mut table_rows = Vec::with_capacity(rows.len());

        for (index, row) in rows.iter().enumerate() {
            let header = index == 0;
            let mut cells = Vec::with_capacity(columns);
            for column in 0..columns {
                let mut blocks = match row.children.get(column) {
                    Some(Node::TableCell(cell)) => {
                        let saved = std::mem::take(&mut self.blocks);
                        self.flow(
                            &cell.children,
                            FlowCtx {
                                indent: 0.0,
                                header,
                                compact: true,
                            },
                        );
                        std::mem::replace(&mut self.blocks, saved)
                    }
                    _ => Vec::new(),
                };

                let align = alignment(table.align.get(column));
                for block in &mut blocks {
                    if let Block::Paragraph(paragraph) = block {
                        paragraph.align = align;
                    }
                }
                if blocks.is_empty() {
                    blocks.push(empty_paragraph(FlowCtx {
                        indent: 0.0,
                        header,
                        compact: true,
                    }));
                }

                cells.push(TableCell {
                    span: 1,
                    fill: header.then_some(HEADER_FILL),
                    blocks,
                });
            }
            table_rows.push(cells);
        }

        self.blocks.push(Block::Table(Table {
            rows: table_rows,
            columns: widths,
            borders: true,
        }));
    }

    fn push_paragraph(
        &mut self,
        runs: Vec<Run>,
        ctx: FlowCtx,
        marker: Option<String>,
        kind: ParaKind,
    ) {
        if runs.is_empty() && marker.is_none() {
            return;
        }

        self.paragraphs += 1;
        let (size, line_spacing, space_before, space_after) = kind.metrics(ctx);
        // Only the first and last lines close the outline, so the lines of one
        // code block share a single box.
        let border = match kind {
            ParaKind::CodeLine { first, last } => ParagraphBorder {
                color: Some(CODE_BORDER),
                top: first,
                bottom: last,
                left: true,
                right: true,
                radius: CODE_RADIUS,
            },
            _ => ParagraphBorder::default(),
        };
        let fill = match kind {
            ParaKind::CodeLine { .. } => Some(CODE_BACKGROUND),
            _ => None,
        };

        self.blocks.push(Block::Paragraph(Paragraph {
            runs,
            align: kind.align(),
            indent: ctx.indent,
            first_line: 0.0,
            space_before,
            space_after,
            line_spacing,
            marker,
            size,
            color: kind.color(ctx),
            font: kind.font(),
            bold: kind.bold(ctx),
            italic: false,
            border,
            fill,
        }));
    }

    fn runs(&mut self, nodes: &[Node], style: InlineStyle) -> Vec<Run> {
        let mut runs = Vec::new();
        for node in nodes {
            self.inline(node, style, &mut runs);
        }
        runs
    }

    fn inline(&mut self, node: &Node, style: InlineStyle, runs: &mut Vec<Run>) {
        match node {
            Node::Text(text) => push_run(runs, &clean_text(&text.value), style),
            Node::Emphasis(emphasis) => {
                let style = InlineStyle {
                    italic: true,
                    ..style
                };
                self.inline_children(&emphasis.children, style, runs);
            }
            Node::Strong(strong) => {
                let style = InlineStyle {
                    bold: true,
                    ..style
                };
                self.inline_children(&strong.children, style, runs);
            }
            Node::Delete(delete) => {
                let style = InlineStyle {
                    strike: true,
                    ..style
                };
                self.inline_children(&delete.children, style, runs);
            }
            Node::InlineCode(code) => {
                let style = InlineStyle {
                    code: true,
                    ..style
                };
                push_run(runs, &clean_text(&code.value), style);
            }
            Node::Link(link) => {
                let style = InlineStyle {
                    underline: true,
                    ..style
                };
                self.inline_children(&link.children, style, runs);
                if link.children.is_empty() && !link.url.is_empty() {
                    push_run(runs, &link.url, style);
                }
            }
            Node::LinkReference(link) => {
                let style = InlineStyle {
                    underline: true,
                    ..style
                };
                self.inline_children(&link.children, style, runs);
                if let Some(resource) = self.definitions.get(&link.identifier)
                    && link.children.is_empty()
                {
                    let url = resource.url.clone();
                    push_run(runs, &url, style);
                }
            }
            Node::Image(image) => self.image(&image.url, &image.alt, style, runs),
            Node::ImageReference(image) => {
                let url = self
                    .definitions
                    .get(&image.identifier)
                    .map(|resource| resource.url.clone());
                match url {
                    Some(url) => self.image(&url, &image.alt, style, runs),
                    None => self.image_alt(&image.alt, style, runs),
                }
            }
            Node::Break(_) => push_run(runs, "\n", style),
            Node::FootnoteReference(reference) => {
                push_run(runs, &format!("[^{}]", reference.identifier), style);
            }
            Node::InlineMath(math) => push_run(runs, &math.value, style),
            Node::Html(html) => {
                let text = strip_html(&html.value);
                push_run(runs, &text, style);
            }
            Node::MdxJsxTextElement(element) => {
                self.inline_children(&element.children, style, runs);
            }
            other => {
                if let Some(children) = other.children() {
                    self.inline_children(children, style, runs);
                }
            }
        }
    }

    fn inline_children(&mut self, nodes: &[Node], style: InlineStyle, runs: &mut Vec<Run>) {
        for node in nodes {
            self.inline(node, style, runs);
        }
    }

    fn image(&mut self, url: &str, alt: &str, style: InlineStyle, runs: &mut Vec<Run>) {
        if let Some(image) = self.load_image(url) {
            runs.push(Run {
                text: String::new(),
                bold: style.bold,
                italic: style.italic,
                underline: false,
                strike: false,
                size: None,
                color: None,
                highlight: None,
                font: None,
                image: Some(image),
            });
        } else {
            self.image_alt(alt, style, runs);
        }
    }

    fn image_alt(&mut self, alt: &str, style: InlineStyle, runs: &mut Vec<Run>) {
        let alt = alt.trim();
        let text = if alt.is_empty() {
            String::from("[图片]")
        } else {
            format!("[{alt}]")
        };
        let style = InlineStyle {
            italic: true,
            ..style
        };
        push_run(runs, &text, style);
    }

    fn load_image(&mut self, url: &str) -> Option<InlineImage> {
        if let Some(cached) = self.images.get(url) {
            return cached.clone();
        }
        let loaded = self.read_image(url);
        self.images.insert(url.to_string(), loaded.clone());
        loaded
    }

    fn read_image(&self, url: &str) -> Option<InlineImage> {
        let path = local_image_path(self.base, url)?;
        let bytes = std::fs::read(path).ok()?;
        let image = image::load_from_memory(&bytes).ok()?;
        let pixels = image.to_rgba8();
        let (width, height) = pixels.dimensions();
        if width == 0 || height == 0 {
            return None;
        }

        let render = RenderImage::new(vec![Frame::new(pixels.clone())]);
        // Images are authored for screens, where 96 pixels are 72 points.
        let mut width = width as f32 * 72.0 / 96.0;
        let mut height = height as f32 * 72.0 / 96.0;
        let max_width = CONTENT_WIDTH - 2.0 * CODE_INDENT;
        if width > max_width {
            height *= max_width / width;
            width = max_width;
        }

        Some(InlineImage {
            image: Arc::new(render),
            pixels: Arc::new(pixels),
            width,
            height,
        })
    }
}

#[derive(Clone, Copy)]
enum ParaKind {
    Body,
    List { spread: bool },
    Heading(u8),
    CodeLine { first: bool, last: bool },
    Rule,
}

impl ParaKind {
    fn metrics(self, ctx: FlowCtx) -> (f32, f32, f32, f32) {
        match self {
            ParaKind::Body => (ctx.body_size(), 1.55, 2.0, 6.0),
            ParaKind::List { spread } => {
                let after = if spread { 5.0 } else { 2.0 };
                (ctx.body_size(), 1.45, 0.0, after)
            }
            ParaKind::Heading(depth) => match depth {
                1 => (24.0, 1.15, 20.0, 9.0),
                2 => (20.0, 1.18, 17.0, 7.0),
                3 => (17.0, 1.20, 14.0, 6.0),
                4 => (15.0, 1.22, 11.0, 4.0),
                5 => (13.5, 1.24, 9.0, 3.0),
                _ => (12.5, 1.26, 7.0, 3.0),
            },
            ParaKind::CodeLine { first, last } => (
                CODE_SIZE,
                1.25,
                if first { 5.0 } else { 0.0 },
                if last { 6.0 } else { 0.0 },
            ),
            ParaKind::Rule => (9.0, 1.0, 7.0, 8.0),
        }
    }

    fn align(self) -> Align {
        match self {
            ParaKind::Rule => Align::Center,
            _ => Align::Left,
        }
    }

    fn color(self, ctx: FlowCtx) -> Option<u32> {
        match self {
            ParaKind::CodeLine { .. } => Some(CODE_FOREGROUND),
            _ if ctx.header => Some(HEADER_TEXT),
            _ => None,
        }
    }

    fn font(self) -> Option<String> {
        match self {
            ParaKind::CodeLine { .. } => Some(String::from(CODE_FONT)),
            _ => None,
        }
    }

    fn bold(self, ctx: FlowCtx) -> bool {
        matches!(self, ParaKind::Heading(_)) || ctx.header
    }
}

fn style_of(ctx: FlowCtx) -> InlineStyle {
    InlineStyle {
        header: ctx.header,
        ..InlineStyle::default()
    }
}

fn push_run(runs: &mut Vec<Run>, text: &str, style: InlineStyle) {
    if text.is_empty() {
        return;
    }

    let color = style_color(style);
    let highlight = style.code.then_some(CODE_BACKGROUND);
    let font = style_font(style);

    if let Some(last) = runs.last_mut()
        && last.image.is_none()
        && last.bold == style.bold
        && last.italic == style.italic
        && last.underline == style.underline
        && last.strike == style.strike
        && last.color == color
        && last.highlight == highlight
        && last.font == font
    {
        last.text.push_str(text);
        return;
    }

    runs.push(Run {
        text: text.to_string(),
        bold: style.bold,
        italic: style.italic,
        underline: style.underline,
        strike: style.strike,
        size: None,
        color,
        highlight,
        font,
        image: None,
    });
}

fn style_font(style: InlineStyle) -> Option<String> {
    style.code.then(|| String::from(CODE_FONT))
}

fn style_color(style: InlineStyle) -> Option<u32> {
    if style.code {
        Some(CODE_FOREGROUND)
    } else if style.header {
        Some(HEADER_TEXT)
    } else {
        None
    }
}

fn empty_paragraph(ctx: FlowCtx) -> Block {
    Block::Paragraph(Paragraph {
        runs: Vec::new(),
        align: Align::Left,
        indent: 0.0,
        first_line: 0.0,
        space_before: 0.0,
        space_after: 0.0,
        line_spacing: 1.2,
        marker: None,
        size: ctx.body_size(),
        color: None,
        font: None,
        bold: false,
        italic: false,
        border: ParagraphBorder::default(),
        fill: None,
    })
}

/// Line breaks in a Markdown source are soft, so they become spaces when the
/// text is reflowed to the reader's width.
fn clean_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_was_space = false;
    for character in text.chars() {
        let character = match character {
            '\r' | '\n' | '\t' => ' ',
            character => character,
        };
        if character == ' ' {
            // Soft breaks often leave runs of spaces, which would show up as
            // gaps in the middle of a wrapped line.
            if last_was_space {
                continue;
            }
            last_was_space = true;
        } else {
            last_was_space = false;
        }
        out.push(character);
    }
    out
}

fn alignment(align: Option<&mdast::AlignKind>) -> Align {
    match align {
        Some(mdast::AlignKind::Center) => Align::Center,
        Some(mdast::AlignKind::Right) => Align::Right,
        _ => Align::Left,
    }
}

/// Collect the link and image definitions of the document so references can
/// be resolved while the blocks are built.
fn collect_definitions(nodes: &[Node], definitions: &mut HashMap<String, Resource>) {
    for node in nodes {
        match node {
            Node::Definition(definition) => {
                definitions.insert(
                    definition.identifier.clone(),
                    Resource {
                        url: definition.url.clone(),
                    },
                );
            }
            other => {
                if let Some(children) = other.children() {
                    collect_definitions(children, definitions);
                }
            }
        }
    }
}

/// Widths proportional to the longest cell of each column, kept inside the
/// text column.
fn column_widths(rows: &[&mdast::TableRow], columns: usize, available: f32) -> Vec<f32> {
    let mut weights = vec![3usize; columns];
    for row in rows {
        for (index, cell) in row.children.iter().enumerate().take(columns) {
            if let Node::TableCell(cell) = cell {
                weights[index] = weights[index].max(text_width(&cell.children));
            }
        }
    }

    let total = weights
        .iter()
        .map(|weight| (*weight).max(4) as f32)
        .sum::<f32>();
    let mut widths = weights
        .iter()
        .map(|weight| ((*weight).max(4) as f32 / total * available).max(28.0))
        .collect::<Vec<_>>();

    let total = widths.iter().sum::<f32>();
    if total > available {
        let scale = available / total;
        for width in &mut widths {
            *width *= scale;
        }
    }

    widths
}

/// Display width of a run of phrasing content: CJK characters take two cells
/// so Chinese tables get sensible column widths.
fn text_width(nodes: &[Node]) -> usize {
    nodes
        .iter()
        .map(|node| match node {
            Node::Text(text) => text.value.chars().map(char_width).sum(),
            Node::InlineCode(code) => code.value.chars().map(char_width).sum(),
            Node::Image(_) | Node::ImageReference(_) => 6,
            other => other.children().map_or(0, |children| text_width(children)),
        })
        .sum()
}

fn char_width(character: char) -> usize {
    match character as u32 {
        0x1100..=0x115F
        | 0x2E80..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x20000..=0x3FFFD => 2,
        _ => 1,
    }
}

/// Resolve a Markdown resource URL to a local file, if it points at one.
fn local_image_path(base: &Path, url: &str) -> Option<PathBuf> {
    let url = url.trim();
    if url.is_empty()
        || url.starts_with("http://")
        || url.starts_with("https://")
        || url.starts_with("data:")
        || url.starts_with("//")
    {
        return None;
    }

    let path = url.split(['#', '?']).next().unwrap_or(url);
    let path = percent_decode(path);
    let path = path
        .strip_prefix("file:///")
        .or_else(|| path.strip_prefix("file://"))
        .unwrap_or(&path);
    if path.is_empty() {
        return None;
    }

    let path = Path::new(path);
    if path.is_absolute() {
        Some(path.to_path_buf())
    } else {
        Some(base.join(path))
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let (Some(high), Some(low)) = (
                bytes.get(index + 1).and_then(|byte| hex_digit(*byte)),
                bytes.get(index + 2).and_then(|byte| hex_digit(*byte)),
            )
        {
            out.push(high * 16 + low);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Turn a fragment of inline HTML into the text it would display. Comments are
/// dropped and block tags become line breaks, so documentation that mixes
/// HTML with Markdown still reads as plain text.
fn strip_html(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;

    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let after = &rest[start..];

        if let Some(comment) = after.strip_prefix("<!--") {
            match comment.find("-->") {
                Some(end) => {
                    rest = &comment[end + 3..];
                    continue;
                }
                None => {
                    rest = "";
                    break;
                }
            }
        }

        let Some(end) = after.find('>') else {
            rest = "";
            break;
        };
        let tag = &after[1..end];
        let name = tag
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or("");
        if name.eq_ignore_ascii_case("br")
            || name.eq_ignore_ascii_case("p")
            || name.eq_ignore_ascii_case("div")
            || name.eq_ignore_ascii_case("li")
            || name.eq_ignore_ascii_case("tr")
        {
            out.push('\n');
        }
        rest = &after[end + 1..];
    }

    out.push_str(rest);
    decode_entities(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(markdown: &str) -> WordDocument {
        load(markdown, Path::new(".")).expect("markdown should parse")
    }

    fn paragraphs(document: &WordDocument) -> Vec<&Paragraph> {
        document
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::Paragraph(paragraph) => Some(paragraph),
                Block::Table(_) => None,
            })
            .collect()
    }

    #[test]
    fn headings_are_large_and_bold() {
        let document = parse("# 标题\n\n正文");
        let paragraphs = paragraphs(&document);
        assert_eq!(paragraphs[0].size, 24.0);
        assert!(paragraphs[0].bold);
        assert_eq!(paragraphs[1].size, BODY_SIZE);
    }

    #[test]
    fn inline_styles_and_code_are_kept() {
        let document = parse("正文 **加粗** *斜体* ~~删除~~ `代码`");
        let paragraph = paragraphs(&document)[0];
        assert!(
            paragraph
                .runs
                .iter()
                .any(|run| run.bold && run.text == "加粗")
        );
        assert!(
            paragraph
                .runs
                .iter()
                .any(|run| run.italic && run.text == "斜体")
        );
        assert!(
            paragraph
                .runs
                .iter()
                .any(|run| run.strike && run.text == "删除")
        );
        assert!(
            paragraph
                .runs
                .iter()
                .any(|run| run.text == "代码" && run.font.as_deref() == Some(CODE_FONT))
        );
    }

    #[test]
    fn lists_and_quotes_are_indented() {
        let document = parse("- 一\n- 二\n\n> 引用");
        let paragraphs = paragraphs(&document);
        assert_eq!(paragraphs[0].marker.as_deref(), Some("•"));
        assert_eq!(paragraphs[1].marker.as_deref(), Some("•"));
        assert!(paragraphs[2].indent >= BLOCKQUOTE_INDENT);
    }

    #[test]
    fn tables_keep_their_columns() {
        let document = parse("| 甲 | 乙 |\n| :-- | --: |\n| 1 | 2 |");
        let table = document
            .blocks
            .iter()
            .find_map(|block| match block {
                Block::Table(table) => Some(table),
                Block::Paragraph(_) => None,
            })
            .expect("table should be kept");
        assert_eq!(table.columns.len(), 2);
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[1][0].blocks.len(), 1);
    }

    #[test]
    fn sample_document_loads_its_image() {
        let document = load(include_str!("../samples/sample.md"), Path::new("samples"))
            .expect("sample markdown should parse");
        let has_image = document.blocks.iter().any(|block| match block {
            Block::Paragraph(paragraph) => paragraph.runs.iter().any(|run| run.image.is_some()),
            Block::Table(_) => false,
        });
        assert!(has_image, "the sample image should be decoded");
    }

    #[test]
    fn code_blocks_use_a_monospace_font() {
        let document = parse("```rust\nfn main() {}\n```");
        let paragraph = paragraphs(&document)[0];
        assert_eq!(paragraph.font.as_deref(), Some(CODE_FONT));
        assert_eq!(paragraph.runs[0].text, "fn main() {}");
        // The card behind the block is the paragraph's fill, so the characters
        // themselves carry no highlight.
        assert!(paragraph.runs[0].highlight.is_none());
        assert_eq!(paragraph.fill, Some(CODE_BACKGROUND));
    }

    #[test]
    fn code_blocks_are_outlined_as_one_box() {
        let document = parse("```\n一\n二\n三\n```");
        let lines = paragraphs(&document);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].border.top && !lines[0].border.bottom);
        assert!(!lines[1].border.top && !lines[1].border.bottom);
        assert!(!lines[2].border.top && lines[2].border.bottom);
        for paragraph in &lines {
            assert_eq!(paragraph.border.color, Some(CODE_BORDER));
            assert_eq!(paragraph.border.radius, CODE_RADIUS);
            assert!(paragraph.border.left && paragraph.border.right);
            assert_eq!(paragraph.fill, Some(CODE_BACKGROUND));
        }

        let single = parse("```\n一\n```");
        let border = paragraphs(&single)[0].border;
        assert!(border.top && border.bottom && border.left && border.right);

        let body = parse("正文 `代码`");
        assert_eq!(paragraphs(&body)[0].border, ParagraphBorder::default());
        assert_eq!(paragraphs(&body)[0].fill, None);
    }
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        let Some(end) = after.find(';').filter(|end| *end <= 12) else {
            out.push('&');
            rest = &after[1..];
            continue;
        };
        let entity = &after[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| {
                    entity
                        .strip_prefix('#')
                        .and_then(|decimal| decimal.parse::<u32>().ok())
                })
                .and_then(char::from_u32),
        };

        match decoded {
            Some(character) => out.push(character),
            None => {
                out.push('&');
                out.push_str(entity);
                out.push(';');
            }
        }
        rest = &after[end + 1..];
    }

    out.push_str(rest);
    out
}
