//! Reading `.docx` documents into the block model the reader draws.
//!
//! WordprocessingML describes documents as a tree, so the part is parsed into a
//! generic element tree first and then walked: that keeps the many containers
//! Word wraps content in (`w:sdt`, `w:ins`, `w:hyperlink`, …) from turning the
//! reader into a state machine.

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::RenderImage;
use image::{Frame, RgbaImage};

use crate::ooxml::{Node, Package, parse_hex_color, parse_tree, root_of};

const TWIPS_PER_POINT: f32 = 20.0;
const EMU_PER_POINT: f32 = 12_700.0;
/// A4 page with one-inch margins, used when the document does not say.
const DEFAULT_CONTENT_WIDTH: f32 = 9026.0 / TWIPS_PER_POINT;
/// Word's default body size for the languages it writes for.
const DEFAULT_FONT_SIZE: f32 = 11.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

/// A parsed Word document.
pub struct WordDocument {
    pub blocks: Vec<Block>,
    /// Width of the text column, in points, taken from the section properties.
    pub content_width: f32,
    /// Width of the page itself, in points, when the file declares one.
    pub page_width: Option<f32>,
    /// Left margin of the page, in points.
    pub margin_left: f32,
    /// Right margin of the page, in points.
    pub margin_right: f32,
    /// Top margin of the page, in points.
    pub margin_top: f32,
    /// Bottom margin of the page, in points.
    pub margin_bottom: f32,
    /// Paragraphs in the body and in table cells, for the status bar.
    pub paragraphs: usize,
}

pub enum Block {
    Paragraph(Paragraph),
    Table(Table),
}
#[derive(Clone, Default)]
pub struct Paragraph {
    pub runs: Vec<Run>,
    pub align: Align,
    /// Left indent of the paragraph, in points.
    pub indent: f32,
    /// First-line indent, in points; negative for hanging indents.
    pub first_line: f32,
    pub space_before: f32,
    pub space_after: f32,
    /// Line spacing as a multiple of the font size.
    pub line_spacing: f32,
    /// List marker ("1." / "•") shown in front of the text.
    pub marker: Option<String>,
    /// Body size of the paragraph, in points.
    pub size: f32,
    pub color: Option<u32>,
    pub font: Option<String>,
    pub bold: bool,
    pub italic: bool,
    /// Border drawn around the paragraph, used by code blocks to stand out
    /// from the body text.
    pub border: ParagraphBorder,
    /// Background fill painted behind the whole paragraph, used by code blocks
    /// so their box is filled instead of every character.
    pub fill: Option<u32>,
}

/// Border around a paragraph, given as the sides it covers so that a block of
/// consecutive paragraphs can share one outline.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ParagraphBorder {
    /// Border colour, RGB; no border is drawn when it is missing.
    pub color: Option<u32>,
    pub top: bool,
    pub bottom: bool,
    pub left: bool,
    pub right: bool,
    /// Corner radius in points. The top corners round when `top` is drawn and
    /// the bottom corners when `bottom` is drawn, so the lines of one block
    /// meet at right angles in the middle.
    pub radius: f32,
}

impl Paragraph {
    /// The size a renderer that can only apply one size per paragraph should
    /// draw with: the size all runs agree on, or the largest one when they
    /// differ. Word sizes the line box from its tallest run.
    pub fn draw_size(&self) -> f32 {
        let mut sizes = self
            .runs
            .iter()
            .filter(|run| run.image.is_none())
            .map(|run| run.size.unwrap_or(self.size));
        let Some(first) = sizes.next() else {
            return self.size;
        };
        if sizes.clone().all(|size| (size - first).abs() < 0.05) {
            first
        } else {
            // One size per paragraph: use the size of the first real text.
            // Documents pad lines with runs of spaces that carry another size,
            // and drawing everything at the largest size would wrap wrongly.
            self.runs
                .iter()
                .find(|run| run.image.is_none() && !run.text.trim().is_empty())
                .and_then(|run| run.size)
                .unwrap_or(self.size)
        }
    }
}

#[derive(Clone)]
pub struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// Size set on the run itself, in points; the paragraph size applies when
    /// it is missing.
    pub size: Option<f32>,
    pub color: Option<u32>,
    pub highlight: Option<u32>,
    pub font: Option<String>,
    pub image: Option<InlineImage>,
}

#[derive(Clone)]
pub struct InlineImage {
    /// The image as the reader draws it.
    pub image: Arc<RenderImage>,
    /// The decoded pixels, which the printer draws with GDI.
    pub pixels: Arc<RgbaImage>,
    pub width: f32,
    pub height: f32,
}

pub struct Table {
    pub rows: Vec<Vec<TableCell>>,
    /// Column widths in points, from `w:tblGrid`.
    pub columns: Vec<f32>,
    /// Minimum row heights in points, from `w:trHeight` or `sprmPHt`; zero
    /// means the row is only as tall as its contents.
    pub row_heights: Vec<f32>,
    pub borders: bool,
    /// Horizontal offset of the table from the text margin, in points;
    /// negative pulls the table into the left margin.
    pub indent: f32,
}

/// Vertical alignment of the contents inside a table cell.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum VAlign {
    #[default]
    Top,
    Center,
    Bottom,
}

pub struct TableCell {
    /// Columns the cell occupies, from `w:gridSpan` or a vertical merge's
    /// restart flags.
    pub span: usize,
    /// True when this cell starts a vertical merge. `row_span` is resolved
    /// from this flag and the covered cells below by [`Table::resolve_row_spans`].
    pub merge_start: bool,
    /// True when this cell continues a vertical merge started above; it holds
    /// no content and only extends the merged area's borders.
    pub covered: bool,
    /// Rows the cell covers, one more than the covered cells below it.
    pub row_span: usize,
    /// Where the contents sit inside the cell, from `w:vAlign` or the TCGRF
    /// vertical-alignment bits.
    pub v_align: VAlign,
    pub fill: Option<u32>,
    pub blocks: Vec<Block>,
}

impl Table {
    /// Turns vertical-merge marks into row spans: the cell that starts a merge
    /// gets the number of rows its covered continuations reach, so renderers
    /// can draw the merged area as one cell.
    pub(crate) fn resolve_row_spans(&mut self) {
        // The column each cell starts at, per row, so covered cells can be
        // matched with the cell above them.
        let starts: Vec<Vec<usize>> = self
            .rows
            .iter()
            .map(|row| {
                let mut column = 0usize;
                row.iter()
                    .map(|cell| {
                        let start = column;
                        column += cell.span.max(1);
                        start
                    })
                    .collect()
            })
            .collect();

        // A continuation is only real when a merge is open in its column;
        // otherwise the file is malformed and the cell stays a plain cell.
        let mut open: HashMap<usize, ()> = HashMap::new();
        for row_index in 0..self.rows.len() {
            for cell_index in 0..self.rows[row_index].len() {
                let start = starts[row_index][cell_index];
                let (merge_start, covered) = {
                    let cell = &self.rows[row_index][cell_index];
                    (cell.merge_start, cell.covered)
                };
                if merge_start {
                    open.insert(start, ());
                } else if covered {
                    if !open.contains_key(&start) {
                        self.rows[row_index][cell_index].covered = false;
                    }
                } else {
                    open.remove(&start);
                }
            }
        }

        for row_index in 0..self.rows.len() {
            for cell_index in 0..self.rows[row_index].len() {
                if !self.rows[row_index][cell_index].merge_start {
                    continue;
                }
                let start = starts[row_index][cell_index];
                let mut rows = 1usize;
                for below in row_index + 1..self.rows.len() {
                    let covers = self.rows[below]
                        .iter()
                        .enumerate()
                        .find(|(index, cell)| {
                            let cell_start = starts[below][*index];
                            cell_start <= start && start < cell_start + cell.span.max(1)
                        })
                        .map(|(_, cell)| cell.covered)
                        .unwrap_or(false);
                    if !covers {
                        break;
                    }
                    rows += 1;
                }
                self.rows[row_index][cell_index].row_span = rows;
            }
        }
    }
}

/// Parse a `.docx` package.
pub fn load(package: &Package) -> Result<WordDocument, String> {
    let document = package
        .xml("word/document.xml")
        .ok_or_else(|| String::from("这个 Word 文档缺少 word/document.xml"))?;
    let document = parse_tree(&document).map(root_of)?;

    let styles = package
        .xml("word/styles.xml")
        .map(|xml| parse_tree(&xml).map(root_of))
        .and_then(Result::ok);
    let numbering = package
        .xml("word/numbering.xml")
        .map(|xml| parse_tree(&xml).map(root_of))
        .and_then(Result::ok);
    let styles = Styles::parse(styles.as_ref());
    let numbering = Numbering::parse(numbering.as_ref());

    let mut context = Context {
        styles,
        numbering,
        rels: package.rels("word/document.xml"),
        images: HashMap::new(),
        rotated: HashMap::new(),
        counters: HashMap::new(),
        content_width: DEFAULT_CONTENT_WIDTH,
        page_width: None,
        margin_left: 0.0,
        margin_right: 0.0,
        margin_top: 0.0,
        margin_bottom: 0.0,
    };

    // Which pictures the body draws rotated: baking the rotation costs as
    // much as the pixel count, so it happens here too, off-thread, instead of
    // serially while the body is walked.
    let mut rotations: HashMap<String, Vec<i32>> = HashMap::new();
    collect_rotations(&document, &mut rotations);

    let decoded: Vec<(String, Option<DecodedImage>, Vec<(i32, DecodedImage)>)> =
        std::thread::scope(|scope| {
            let handles: Vec<_> = context
                .rels
                .iter()
                .filter(|(_, path)| path.contains("media/"))
                .filter_map(|(id, path)| {
                    let bytes = package.bytes(path)?.to_vec();
                    let degrees = rotations.get(id).cloned().unwrap_or_default();
                    Some((
                        id.clone(),
                        scope.spawn(move || {
                            let base = decode_image(&bytes);
                            let baked = base
                                .as_ref()
                                .map(|base| {
                                    degrees
                                        .iter()
                                        .map(|degrees| (*degrees, bake(base, *degrees)))
                                        .collect::<Vec<_>>()
                                })
                                .unwrap_or_default();
                            (base, baked)
                        }),
                    ))
                })
                .collect();
            handles
                .into_iter()
                .map(|(id, handle)| {
                    let (base, baked) = handle.join().unwrap_or((None, Vec::new()));
                    (id, base, baked)
                })
                .collect()
        });
    for (id, base, baked) in decoded {
        if base.is_some() {
            context.images.insert(id.clone(), base);
        }
        for (degrees, image) in baked {
            context.rotated.insert((id.clone(), degrees), Some(image));
        }
    }

    let body = document
        .child("body")
        .ok_or_else(|| String::from("这个 Word 文档没有正文"))?;
    let blocks = context.blocks_of(body);
    if blocks.is_empty() {
        return Err(String::from("这份 Word 文档没有可显示的内容"));
    }
    let paragraphs = count_paragraphs(&blocks);

    Ok(WordDocument {
        blocks,
        content_width: context.content_width,
        page_width: context.page_width,
        margin_left: context.margin_left,
        margin_right: context.margin_right,
        margin_top: context.margin_top,
        margin_bottom: context.margin_bottom,
        paragraphs,
    })
}

fn count_paragraphs(blocks: &[Block]) -> usize {
    blocks
        .iter()
        .map(|block| match block {
            Block::Paragraph(_) => 1,
            Block::Table(table) => table
                .rows
                .iter()
                .flatten()
                .map(|cell| count_paragraphs(&cell.blocks))
                .sum(),
        })
        .sum()
}

struct Context {
    styles: Styles,
    numbering: Numbering,
    rels: HashMap<String, String>,
    images: HashMap<String, Option<DecodedImage>>,
    /// Rotation-baked copies of decoded images, keyed by relationship id and
    /// angle: baking is proportional to the pixel count and a picture
    /// referenced several times must not be rotated again per reference.
    rotated: HashMap<(String, i32), Option<DecodedImage>>,
    counters: HashMap<(String, usize), u32>,
    content_width: f32,
    /// Page width and side margins of the section, in points.
    page_width: Option<f32>,
    margin_left: f32,
    margin_right: f32,
    margin_top: f32,
    margin_bottom: f32,
}

impl Context {
    /// Paragraphs and tables of a container, in order.
    fn blocks_of(&mut self, node: &Node) -> Vec<Block> {
        let mut blocks = Vec::new();
        for child in &node.children {
            match child.name.as_str() {
                "p" => blocks.push(Block::Paragraph(self.paragraph(child))),
                "tbl" => blocks.push(Block::Table(self.table(child))),
                "sectPr" => self.read_section(child),
                // Content controls, tracked changes and fields only wrap real
                // content, so their children are read as if they were inline.
                "sdt" | "sdtContent" | "ins" | "moveTo" | "smartTag" | "customXml" | "body"
                | "tc" | "txbxContent" | "hdr" | "ftr" => {
                    blocks.extend(self.blocks_of(child));
                }
                _ => {}
            }
        }
        blocks
    }

    fn read_section(&mut self, node: &Node) {
        let Some(size) = node.child("pgSz") else {
            return;
        };
        let Some(width) = size.attr("w").and_then(|value| value.parse::<f32>().ok()) else {
            return;
        };
        let margins = node.child("pgMar");
        let left = margins
            .and_then(|mar| mar.attr("left"))
            .and_then(|value| value.parse::<f32>().ok())
            .unwrap_or(1440.0);
        let right = margins
            .and_then(|mar| mar.attr("right"))
            .and_then(|value| value.parse::<f32>().ok())
            .unwrap_or(1440.0);
        let width = (width - left - right) / TWIPS_PER_POINT;
        if width > 120.0 {
            let top = margins
                .and_then(|mar| mar.attr("top"))
                .and_then(|value| value.parse::<f32>().ok())
                .unwrap_or(1440.0);
            let bottom = margins
                .and_then(|mar| mar.attr("bottom"))
                .and_then(|value| value.parse::<f32>().ok())
                .unwrap_or(1440.0);
            self.content_width = width;
            self.page_width = Some(width + (left + right) / TWIPS_PER_POINT);
            self.margin_left = left / TWIPS_PER_POINT;
            self.margin_right = right / TWIPS_PER_POINT;
            self.margin_top = top / TWIPS_PER_POINT;
            self.margin_bottom = bottom / TWIPS_PER_POINT;
        }
    }

    fn paragraph(&mut self, node: &Node) -> Paragraph {
        let direct = node.child("pPr").map(paragraph_props).unwrap_or_default();
        let style = self.styles.paragraph(direct.style.as_deref());
        let para = merge_para(style, direct);

        let mut marker = None;
        if let (Some(num_id), Some(level)) = (para.num_id.as_deref(), para.level) {
            marker = self.numbering.marker(num_id, level, &mut self.counters);
        }

        let run_style = self.styles.run(para.style.as_deref());
        let size = run_style.size.unwrap_or(DEFAULT_FONT_SIZE);
        let mut paragraph = Paragraph {
            align: para.align.unwrap_or_default(),
            indent: para.indent.unwrap_or(0.0),
            first_line: para.first_line.unwrap_or(0.0),
            space_before: para.space_before.unwrap_or(0.0),
            space_after: para.space_after.unwrap_or(0.0),
            line_spacing: para.line_spacing.unwrap_or(1.0),
            marker,
            size,
            color: run_style.color,
            font: run_style.font.clone(),
            bold: run_style.bold.unwrap_or(false),
            italic: run_style.italic.unwrap_or(false),
            runs: Vec::new(),
            border: ParagraphBorder::default(),
            fill: para.fill,
        };

        if paragraph.marker.is_some() {
            let level = para.level.unwrap_or(0) as f32;
            paragraph.indent += (level + 1.0) * 18.0;
        }

        self.collect_runs(node, &run_style, &mut paragraph.runs);
        paragraph
    }

    fn collect_runs(&mut self, node: &Node, base: &RunProps, runs: &mut Vec<Run>) {
        for child in &node.children {
            match child.name.as_str() {
                "r" => self.run(child, base, runs),
                "hyperlink" | "ins" | "moveTo" | "smartTag" | "sdt" | "sdtContent"
                | "fldSimple" | "customXml" => self.collect_runs(child, base, runs),
                _ => {}
            }
        }
    }

    fn run(&mut self, node: &Node, base: &RunProps, runs: &mut Vec<Run>) {
        let direct = node.child("rPr").map(run_props).unwrap_or_default();
        let props = merge_run(base.clone(), direct);

        let mut text = String::new();
        for child in &node.children {
            match child.name.as_str() {
                "t" => text.push_str(&child.text),
                "tab" => text.push('\t'),
                "br" | "cr" => text.push('\n'),
                "noBreakHyphen" => text.push('-'),
                "sym" => {
                    if let Some(character) = child
                        .attr("char")
                        .and_then(|value| u32::from_str_radix(&value, 16).ok())
                        .and_then(symbol_character)
                    {
                        text.push(character);
                    }
                }
                "drawing" | "pict" | "object" => {
                    // A freely floating image (no text wrap) is positioned by
                    // page coordinates the reader does not reproduce, so
                    // inlining it would only drop a stray box into the flow.
                    let floating = child.descendant("anchor").is_some()
                        && child.descendant("wrapNone").is_some();
                    if floating {
                        continue;
                    }
                    if !text.is_empty() {
                        runs.push(props.to_run(std::mem::take(&mut text)));
                    }
                    if let Some(image) = self.image(child) {
                        runs.push(Run {
                            text: String::new(),
                            bold: false,
                            italic: false,
                            underline: false,
                            strike: false,
                            size: None,
                            color: None,
                            highlight: None,
                            font: None,
                            image: Some(image),
                        });
                    }
                }
                _ => {}
            }
        }

        if !text.is_empty() {
            runs.push(props.to_run(text));
        }
    }

    fn table(&mut self, node: &Node) -> Table {
        let mut rows = Vec::new();
        for row in node.children_named("tr") {
            let mut cells = Vec::new();
            for cell in row.children_named("tc") {
                let properties = cell.child("tcPr");
                let span = properties
                    .and_then(|properties| properties.child("gridSpan"))
                    .and_then(|span| span.attr("val"))
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, 64);
                let fill = properties
                    .and_then(|properties| properties.child("shd"))
                    .and_then(|shading| shading.attr("fill"))
                    .and_then(|value| parse_hex_color(&value));
                // `<w:vMerge/>` without a value continues a merge; only
                // "restart" marks the cell that starts one.
                let merge = properties.and_then(|properties| properties.child("vMerge"));
                let merge_start = merge
                    .and_then(|merge| merge.attr("val"))
                    .map(|value| value == "restart")
                    .unwrap_or(false);
                let covered = merge.is_some() && !merge_start;
                let v_align = properties
                    .and_then(|properties| properties.child("vAlign"))
                    .and_then(|align| align.attr("val"))
                    .map(|value| match value.as_str() {
                        "center" => VAlign::Center,
                        "bottom" => VAlign::Bottom,
                        _ => VAlign::Top,
                    })
                    .unwrap_or_default();
                cells.push(TableCell {
                    span,
                    merge_start,
                    covered,
                    row_span: 1,
                    v_align,
                    fill,
                    blocks: self.blocks_of(cell),
                });
            }
            if !cells.is_empty() {
                rows.push(cells);
            }
        }

        let columns = node
            .child("tblGrid")
            .map(|grid| {
                grid.children_named("gridCol")
                    .map(|column| {
                        column
                            .attr("w")
                            .and_then(|value| value.parse::<f32>().ok())
                            .map(|twips| twips / TWIPS_PER_POINT)
                            .unwrap_or(60.0)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let borders = node
            .child("tblPr")
            .and_then(|properties| properties.child("tblBorders"))
            .is_some();

        // Minimum row heights: `w:trHeight` is a dxa value; without an
        // "exact" rule the row grows past it when the contents need more.
        let row_heights: Vec<f32> = node
            .children_named("tr")
            .map(|row| {
                row.child("trPr")
                    .and_then(|properties| properties.child("trHeight"))
                    .and_then(|height| height.attr("val"))
                    .and_then(|value| value.parse::<f32>().ok())
                    .map(|twips| twips / TWIPS_PER_POINT)
                    .unwrap_or(0.0)
            })
            .collect();

        let mut table = Table {
            indent: node
                .child("tblPr")
                .and_then(|pr| pr.child("tblInd"))
                .and_then(|ind| ind.attr("w"))
                .and_then(|value| value.parse::<f32>().ok())
                .unwrap_or(0.0)
                / TWIPS_PER_POINT,
            rows,
            columns,
            row_heights,
            borders,
        };
        table.resolve_row_spans();
        table
    }

    /// The image of a drawing, decoded once per relationship id.
    fn image(&mut self, node: &Node) -> Option<InlineImage> {
        let blip = node.descendant("blip")?;
        let id = blip.attr("embed").or_else(|| blip.attr("link"))?.to_owned();

        // DrawingML may rotate the whole picture clockwise in 60 000ths of a
        // degree; the stored raster is upright, so the rotation is baked in
        // and the extent swapped to match. Baking costs as much as the pixel
        // count, so the baked copy is cached per picture and angle.
        let degrees = node
            .descendant("xfrm")
            .and_then(|xfrm| xfrm.attr("rot"))
            .and_then(|value| value.parse::<f32>().ok())
            .map(|value| ((value / 60000.0).round() as i32).rem_euclid(360))
            .unwrap_or(0);
        let decoded = if degrees == 0 {
            self.images.get(&id)?.clone()
        } else {
            match self.rotated.get(&(id.clone(), degrees)) {
                Some(rotated) => rotated.clone(),
                // Missed the predecode (an unusual angle, or the drawing is
                // written in a shape the scan does not recognise): bake here.
                None => {
                    let base = self.images.get(&id)?.clone()?;
                    let baked = bake(&base, degrees);
                    self.rotated
                        .insert((id.clone(), degrees), Some(baked.clone()));
                    Some(baked)
                }
            }
        }?;

        let image = decoded.render;
        let pixels = decoded.pixels;

        // `wp:extent` is in EMU; documents written by other tools sometimes
        // leave it out, in which case the raster size at 96 dpi is a good guess.
        let extent = node.descendant("extent");
        let (width, height) = match extent {
            Some(extent) => (
                extent
                    .attr("cx")
                    .and_then(|value| value.parse::<f32>().ok()),
                extent
                    .attr("cy")
                    .and_then(|value| value.parse::<f32>().ok()),
            ),
            None => (None, None),
        };

        let (mut width, mut height) = match (width, height) {
            (Some(width), Some(height)) if width >= 1.0 && height >= 1.0 => {
                (width / EMU_PER_POINT, height / EMU_PER_POINT)
            }
            _ => {
                let size = image.size(0);
                (
                    size.width.0 as f32 * 72.0 / 96.0,
                    size.height.0 as f32 * 72.0 / 96.0,
                )
            }
        };
        if degrees % 180 == 90 {
            std::mem::swap(&mut width, &mut height);
        }

        Some(InlineImage {
            image,
            pixels,
            width,
            height,
        })
    }
}

#[derive(Clone)]
struct DecodedImage {
    render: Arc<RenderImage>,
    pixels: Arc<RgbaImage>,
}

/// One rotation-baked copy of a decoded image: the pixels turned, and the
/// BGRA copy the window draws its texture from.
fn bake(base: &DecodedImage, degrees: i32) -> DecodedImage {
    let mut rotated = base.pixels.as_ref().clone();
    let rotated = match degrees {
        90 => image::imageops::rotate90(&rotated),
        180 => {
            image::imageops::rotate180_in_place(&mut rotated);
            rotated
        }
        270 => image::imageops::rotate270(&rotated),
        _ => rotated,
    };
    let display = crate::picture::to_render_pixels_copy(&rotated);
    DecodedImage {
        render: Arc::new(RenderImage::new(vec![Frame::new(display)])),
        pixels: Arc::new(rotated),
    }
}

/// Pictures the body draws rotated, as relationship id -> every angle they
/// are drawn at. Only non-zero angles are collected; the parallel predecode
/// bakes these before the body is walked.
fn collect_rotations(node: &Node, out: &mut HashMap<String, Vec<i32>>) {
    if matches!(node.name.as_str(), "drawing" | "pict") {
        if let Some(id) = node
            .descendant("blip")
            .and_then(|blip| blip.attr("embed").or_else(|| blip.attr("link")))
        {
            let degrees = node
                .descendant("xfrm")
                .and_then(|xfrm| xfrm.attr("rot"))
                .and_then(|value| value.parse::<f32>().ok())
                .map(|value| ((value / 60000.0).round() as i32).rem_euclid(360))
                .unwrap_or(0);
            if degrees != 0 {
                out.entry(id.to_owned()).or_default().push(degrees);
            }
        }
    }
    for child in &node.children {
        collect_rotations(child, out);
    }
}

fn decode_image(bytes: &[u8]) -> Option<DecodedImage> {
    let image = image::load_from_memory(bytes).ok()?;
    let pixels = image.to_rgba8();
    // The window reads a texture as BGRA, so the copy it draws is converted;
    // the printer keeps the straight RGBA one.
    let display = crate::picture::to_render_pixels_copy(&pixels);
    let render = RenderImage::new(vec![Frame::new(display)]);
    Some(DecodedImage {
        render: Arc::new(render),
        pixels: Arc::new(pixels),
    })
}

fn symbol_character(value: u32) -> Option<char> {
    // Wingdings/Symbol bullets live in the private use area.
    let mapped = match value {
        0xF0B7 | 0xF0A7 | 0xF0D8 | 0xF0FC => '•',
        0xF0A8 | 0xF0FE => '▪',
        0xF0D2 => '–',
        0xF0E0 => '→',
        _ => return char::from_u32(value).filter(|character| !character.is_control()),
    };
    Some(mapped)
}

// ---------------------------------------------------------------- properties

#[derive(Clone, Default)]
struct RunProps {
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    strike: Option<bool>,
    size: Option<f32>,
    color: Option<u32>,
    highlight: Option<u32>,
    font: Option<String>,
}

impl RunProps {
    fn to_run(&self, text: String) -> Run {
        Run {
            text,
            bold: self.bold.unwrap_or(false),
            italic: self.italic.unwrap_or(false),
            underline: self.underline.unwrap_or(false),
            strike: self.strike.unwrap_or(false),
            size: self.size,
            color: self.color,
            highlight: self.highlight,
            font: self.font.clone(),
            image: None,
        }
    }
}

#[derive(Clone, Default)]
struct ParaProps {
    style: Option<String>,
    align: Option<Align>,
    indent: Option<f32>,
    first_line: Option<f32>,
    space_before: Option<f32>,
    space_after: Option<f32>,
    line_spacing: Option<f32>,
    num_id: Option<String>,
    level: Option<usize>,
    /// Paragraph shading fill, RGB; `w:fill="auto"` reads as none.
    fill: Option<u32>,
}

#[derive(Clone, Default)]
struct StyleDef {
    based_on: Option<String>,
    run: RunProps,
    para: ParaProps,
}

#[derive(Default)]
struct Styles {
    run: RunProps,
    para: ParaProps,
    by_id: HashMap<String, StyleDef>,
}

impl Styles {
    fn parse(node: Option<&Node>) -> Self {
        let mut styles = Styles::default();
        let Some(node) = node else {
            return styles;
        };

        if let Some(defaults) = node.child("docDefaults") {
            if let Some(run) = defaults
                .child("rPrDefault")
                .and_then(|node| node.child("rPr"))
            {
                styles.run = run_props(run);
            }
            if let Some(para) = defaults
                .child("pPrDefault")
                .and_then(|node| node.child("pPr"))
            {
                styles.para = paragraph_props(para);
            }
        }

        for style in node.children_named("style") {
            let Some(id) = style.attr("styleId") else {
                continue;
            };
            styles.by_id.insert(
                id,
                StyleDef {
                    based_on: style.child("basedOn").and_then(|node| node.attr("val")),
                    run: style.child("rPr").map(run_props).unwrap_or_default(),
                    para: style.child("pPr").map(paragraph_props).unwrap_or_default(),
                },
            );
        }

        styles
    }

    fn chain(&self, style: Option<&str>) -> Vec<String> {
        let mut chain = Vec::new();
        let mut current = style.map(str::to_owned);
        while let Some(id) = current {
            if chain.len() > 16 || chain.contains(&id) {
                break;
            }
            current = self.by_id.get(&id).and_then(|def| def.based_on.clone());
            chain.push(id);
        }
        chain
    }

    fn run(&self, style: Option<&str>) -> RunProps {
        let mut props = self.run.clone();
        for id in self.chain(style).iter().rev() {
            if let Some(def) = self.by_id.get(id) {
                props = merge_run(props, def.run.clone());
            }
        }
        props
    }

    fn paragraph(&self, style: Option<&str>) -> ParaProps {
        let mut props = self.para.clone();
        for id in self.chain(style).iter().rev() {
            if let Some(def) = self.by_id.get(id) {
                props = merge_para(props, def.para.clone());
            }
        }
        props
    }
}

fn run_props(node: &Node) -> RunProps {
    let mut props = RunProps::default();
    for child in &node.children {
        match child.name.as_str() {
            "b" => props.bold = Some(bool_property_node(child)),
            "i" => props.italic = Some(bool_property_node(child)),
            "u" => props.underline = Some(child.attr("val").as_deref() != Some("none")),
            "strike" | "dstrike" => props.strike = Some(bool_property_node(child)),
            "sz" => {
                props.size = child
                    .attr("val")
                    .and_then(|value| value.parse::<f32>().ok())
                    .map(|half| half / 2.0)
            }
            "color" => props.color = child.attr("val").and_then(|value| parse_hex_color(&value)),
            "highlight" => {
                props.highlight = child.attr("val").and_then(|value| highlight_color(&value))
            }
            "shd" => {
                if props.highlight.is_none() {
                    props.highlight = child.attr("fill").and_then(|value| parse_hex_color(&value));
                }
            }
            "rFonts" => {
                props.font = child
                    .attr("eastAsia")
                    .or_else(|| child.attr("ascii"))
                    .or_else(|| child.attr("hAnsi"))
                    .filter(|name| !name.is_empty());
            }
            _ => {}
        }
    }
    props
}

fn bool_property_node(node: &Node) -> bool {
    match node.attr("val") {
        Some(value) => !matches!(value.as_str(), "0" | "false" | "off"),
        None => true,
    }
}

fn paragraph_props(node: &Node) -> ParaProps {
    let mut props = ParaProps::default();
    for child in &node.children {
        match child.name.as_str() {
            "pStyle" => props.style = child.attr("val"),
            "jc" => {
                props.align = Some(match child.attr("val").as_deref() {
                    Some("center") => Align::Center,
                    Some("right") | Some("end") => Align::Right,
                    Some("both") | Some("distribute") => Align::Justify,
                    _ => Align::Left,
                })
            }
            "ind" => {
                if let Some(left) = child.attr("left").or_else(|| child.attr("start")) {
                    props.indent = left
                        .parse::<f32>()
                        .ok()
                        .map(|twips| twips / TWIPS_PER_POINT);
                }
                if let Some(first) = child.attr("firstLine") {
                    props.first_line = first
                        .parse::<f32>()
                        .ok()
                        .map(|twips| twips / TWIPS_PER_POINT);
                }
                if let Some(hanging) = child.attr("hanging") {
                    props.first_line = hanging
                        .parse::<f32>()
                        .ok()
                        .map(|twips| -twips / TWIPS_PER_POINT);
                }
            }
            "spacing" => {
                if let Some(before) = child.attr("before") {
                    props.space_before = before
                        .parse::<f32>()
                        .ok()
                        .map(|twips| twips / TWIPS_PER_POINT);
                }
                if let Some(after) = child.attr("after") {
                    props.space_after = after
                        .parse::<f32>()
                        .ok()
                        .map(|twips| twips / TWIPS_PER_POINT);
                }
                if let Some(line) = child
                    .attr("line")
                    .and_then(|value| value.parse::<f32>().ok())
                {
                    props.line_spacing = Some(match child.attr("lineRule").as_deref() {
                        // "auto" is in 240ths of a line; exact/atLeast are in twips.
                        Some("exact") | Some("atLeast") => (line / TWIPS_PER_POINT / 11.0).max(0.8),
                        _ => line / 240.0,
                    });
                }
            }
            "numPr" => {
                props.num_id = child
                    .child("numId")
                    .and_then(|node| node.attr("val"))
                    .filter(|value| value != "0");
                props.level = child
                    .child("ilvl")
                    .and_then(|node| node.attr("val"))
                    .and_then(|value| value.parse::<usize>().ok());
            }
            "shd" => {
                props.fill = child
                    .attr("fill")
                    .filter(|value| !value.eq_ignore_ascii_case("auto"))
                    .and_then(|value| parse_hex_color(&value));
            }
            _ => {}
        }
    }
    props
}

fn merge_run(mut base: RunProps, over: RunProps) -> RunProps {
    macro_rules! overlay {
        ($($field:ident),* $(,)?) => {
            $(if over.$field.is_some() { base.$field = over.$field; })*
        };
    }
    overlay!(
        bold, italic, underline, strike, size, color, highlight, font
    );
    base
}

fn merge_para(mut base: ParaProps, over: ParaProps) -> ParaProps {
    macro_rules! overlay {
        ($($field:ident),* $(,)?) => {
            $(if over.$field.is_some() { base.$field = over.$field; }) *
        };
    }
    overlay!(
        style,
        align,
        indent,
        first_line,
        space_before,
        space_after,
        line_spacing,
        num_id,
        level,
        fill
    );
    base
}

fn highlight_color(name: &str) -> Option<u32> {
    Some(match name {
        "yellow" => 0xFFFF00,
        "green" => 0x00FF00,
        "cyan" => 0x00FFFF,
        "magenta" => 0xFF00FF,
        "blue" => 0x0000FF,
        "red" => 0xFF0000,
        "darkBlue" => 0x000080,
        "darkCyan" => 0x008080,
        "darkGreen" => 0x008000,
        "darkMagenta" => 0x800080,
        "darkRed" => 0x800000,
        "darkYellow" => 0x808000,
        "darkGray" => 0x808080,
        "lightGray" => 0xC0C0C0,
        "black" => 0x000000,
        "white" => 0xFFFFFF,
        _ => return None,
    })
}

// ------------------------------------------------------------------ numbering

#[derive(Default)]
struct Numbering {
    /// `w:num` id to `w:abstractNum` id.
    numbers: HashMap<String, String>,
    /// Abstract id to its levels: `(format, text)`.
    levels: HashMap<String, Vec<(String, String)>>,
}

impl Numbering {
    fn parse(node: Option<&Node>) -> Self {
        let mut numbering = Numbering::default();
        let Some(node) = node else {
            return numbering;
        };

        for abstract_num in node.children_named("abstractNum") {
            let Some(id) = abstract_num.attr("abstractNumId") else {
                continue;
            };
            let mut levels = vec![("decimal".to_string(), "%1.".to_string()); 9];
            for level in abstract_num.children_named("lvl") {
                let Some(index) = level
                    .attr("ilvl")
                    .and_then(|value| value.parse::<usize>().ok())
                else {
                    continue;
                };
                if index >= levels.len() {
                    continue;
                }
                let format = level
                    .child("numFmt")
                    .and_then(|node| node.attr("val"))
                    .unwrap_or_else(|| String::from("decimal"));
                let text = level
                    .child("lvlText")
                    .and_then(|node| node.attr("val"))
                    .unwrap_or_else(|| String::from("%1."));
                levels[index] = (format, text);
            }
            numbering.levels.insert(id, levels);
        }

        for num in node.children_named("num") {
            let Some(id) = num.attr("numId") else {
                continue;
            };
            if let Some(abstract_id) = num.child("abstractNumId").and_then(|node| node.attr("val"))
            {
                numbering.numbers.insert(id, abstract_id);
            }
        }

        numbering
    }

    /// The marker text for the next item of `num_id` at `level`.
    fn marker(
        &self,
        num_id: &str,
        level: usize,
        counters: &mut HashMap<(String, usize), u32>,
    ) -> Option<String> {
        let abstract_id = self.numbers.get(num_id)?.clone();
        let levels = self.levels.get(&abstract_id)?;
        let (format, text) = levels.get(level)?.clone();

        *counters.entry((abstract_id.clone(), level)).or_insert(0) += 1;
        for deeper in level + 1..9 {
            counters.remove(&(abstract_id.clone(), deeper));
        }

        if format == "bullet" || format == "none" {
            return Some(String::from("•"));
        }

        let mut marker = text;
        for depth in 0..=level {
            let value = counters
                .get(&(abstract_id.clone(), depth))
                .copied()
                .unwrap_or(1);
            let inner_format = levels
                .get(depth)
                .map(|(format, _)| format.as_str())
                .unwrap_or("decimal");
            marker = marker.replace(
                &format!("%{}", depth + 1),
                &format_counter(inner_format, value),
            );
        }
        Some(marker)
    }
}

pub(crate) fn format_counter(format: &str, value: u32) -> String {
    match format {
        "lowerLetter" => letters(value).to_lowercase(),
        "upperLetter" => letters(value),
        "lowerRoman" => roman(value).to_lowercase(),
        "upperRoman" => roman(value),
        "decimalZero" => format!("{value:02}"),
        "chineseCounting"
        | "chineseCountingThousand"
        | "ideographDigital"
        | "japaneseCounting"
        | "taiwaneseCounting" => chinese_number(value),
        _ => value.to_string(),
    }
}

fn letters(mut value: u32) -> String {
    let mut out = String::new();
    while value > 0 {
        let letter = ((value - 1) % 26) as u8 + b'A';
        out.insert(0, letter as char);
        value = (value - 1) / 26;
    }
    out
}

fn roman(mut value: u32) -> String {
    const TABLE: [(u32, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    for (number, numeral) in TABLE {
        while value >= number {
            out.push_str(numeral);
            value -= number;
        }
    }
    out
}

fn chinese_number(value: u32) -> String {
    const DIGITS: [&str; 10] = ["零", "一", "二", "三", "四", "五", "六", "七", "八", "九"];
    match value {
        0 => String::from("零"),
        1..=10 => DIGITS[value as usize].to_string(),
        11..=19 => format!("十{}", DIGITS[(value - 10) as usize]),
        20..=99 => {
            let tens = value / 10;
            let ones = value % 10;
            if ones == 0 {
                format!("{}十", DIGITS[tens as usize])
            } else {
                format!("{}十{}", DIGITS[tens as usize], DIGITS[ones as usize])
            }
        }
        _ => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(merge_start: bool, covered: bool) -> TableCell {
        TableCell {
            span: 1,
            merge_start,
            covered,
            row_span: 1,
            v_align: VAlign::Top,
            fill: None,
            blocks: Vec::new(),
        }
    }

    #[test]
    fn a_merge_covers_the_flagged_cells_below() {
        let mut table = Table {
            rows: vec![
                vec![cell(true, false), cell(false, false)],
                vec![cell(false, true), cell(false, false)],
                vec![cell(false, true), cell(false, false)],
                vec![cell(false, false), cell(false, false)],
            ],
            columns: vec![60.0; 2],
            row_heights: Vec::new(),
            borders: true,
            indent: 0.0,
        };
        table.resolve_row_spans();
        assert_eq!(table.rows[0][0].row_span, 3);
        assert!(table.rows[1][0].covered && table.rows[2][0].covered);
        assert_eq!(table.rows[3][0].row_span, 1);
        assert!(!table.rows[3][0].covered);
    }

    #[test]
    fn a_continuation_without_a_merge_above_is_a_plain_cell() {
        let mut table = Table {
            rows: vec![vec![cell(false, false), cell(false, true)]],
            columns: vec![60.0; 2],
            row_heights: Vec::new(),
            borders: true,
            indent: 0.0,
        };
        table.resolve_row_spans();
        assert!(!table.rows[0][1].covered);
    }
}

