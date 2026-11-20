//! Reading `.xlsx` workbooks into a grid the reader can draw.
//!
//! Worksheets can be very large, so the sheet parts are streamed instead of
//! being turned into an element tree; the small parts (styles, workbook, shared
//! strings) use the tree parser.

use std::collections::HashMap;

use quick_xml::events::Event;

use crate::ooxml::{
    self, Node, Package, attr, attr_f32, end_name, name_of, parse_hex_color, parse_tree, root_of,
    text_of,
};

/// Height of one line of cell text, in pixels (11 pt at 96 dpi).
const DEFAULT_FONT_SIZE: f32 = 11.0 * 4.0 / 3.0;
/// Excel's default column width, in characters.
const DEFAULT_COLUMN_WIDTH: f32 = 8.43;
/// Excel's default row height, in points.
const DEFAULT_ROW_HEIGHT: f32 = 15.0;
/// Horizontal space a cell spends on padding and borders.
const CELL_INSET: f32 = 8.0;
/// Extra room a column keeps when it is widened for the value in it, so the
/// text does not end up flush against the border.
const CELL_CLEARANCE: f32 = 4.0;
/// Vertical padding above and below the text of a cell, in pixels.
const CELL_VPAD: f32 = 2.0;
/// Height of one line of cell text relative to the font size. The renderer
/// draws at 1.25; the extra tenth is slack so a wrapped row is never a line
/// short and clips its last line.
const LINE_HEIGHT_FACTOR: f32 = 1.35;
/// A hopeless cell — one long run of text in a very narrow column — would
/// otherwise ask for tens of thousands of rows. The cap keeps a malformed
/// worksheet from exploding the layout; real cells never come close.
const MAX_WRAP_LINES: usize = 500;
/// Excel's own limits. A malformed worksheet may claim a cell reference far
/// beyond them; clamping keeps `row_tops`/`column_lefts` from allocating
/// gigabytes (or looping billions of times) for a few bytes of XML.
const MAX_SHEET_ROWS: u32 = 1_048_576;
const MAX_SHEET_COLUMNS: u32 = 16_384;

pub struct Workbook {
    pub sheets: Vec<Sheet>,
    pub styles: Vec<CellStyle>,
}

pub struct Sheet {
    pub name: String,
    /// Left edge of every column, in logical pixels; the last entry is the
    /// right edge of the used range.
    pub column_lefts: Vec<f32>,
    /// Top edge of every row, in logical pixels; the last entry is the bottom
    /// edge of the used range.
    pub row_tops: Vec<f32>,
    /// Rows holding at least one cell, sorted by row index.
    pub rows: Vec<SheetRow>,
    pub merges: Vec<Merge>,
}

pub struct SheetRow {
    pub index: u32,
    /// Cells sorted by column.
    pub cells: Vec<Cell>,
}

pub struct Cell {
    pub column: u32,
    pub text: String,
    /// Index into [`Workbook::styles`].
    pub style: usize,
    /// True for numbers, which Excel aligns to the right by default.
    pub numeric: bool,
    /// Whether the text is drawn on several lines. A cell wraps when its style
    /// asks for it, when the text carries a line break, or when one line would
    /// be wider than the cell — the reader shows the whole cell rather than
    /// cutting the overflow off.
    pub wrap: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Merge {
    pub row: u32,
    pub column: u32,
    pub rows: u32,
    pub columns: u32,
}

impl Sheet {
    pub fn width(&self) -> f32 {
        self.column_lefts.last().copied().unwrap_or(0.0)
    }

    pub fn height(&self) -> f32 {
        self.row_tops.last().copied().unwrap_or(0.0)
    }

    pub fn column_count(&self) -> usize {
        self.column_lefts.len().saturating_sub(1)
    }

    pub fn row_count(&self) -> usize {
        self.row_tops.len().saturating_sub(1)
    }

    /// The row covering `offset` pixels from the top.
    pub fn row_at(&self, offset: f32) -> usize {
        index_at(&self.row_tops, offset)
    }

    /// The column covering `offset` pixels from the left.
    pub fn column_at(&self, offset: f32) -> usize {
        index_at(&self.column_lefts, offset)
    }

    /// The cell at `(row, column)`.
    pub fn cell(&self, row: u32, column: u32) -> Option<&Cell> {
        let index = self.rows.partition_point(|entry| entry.index < row);
        let entry = self.rows.get(index).filter(|entry| entry.index == row)?;
        let cell = entry
            .cells
            .binary_search_by_key(&column, |cell| cell.column)
            .ok()?;
        entry.cells.get(cell)
    }

    /// The merge whose top-left cell is `(row, column)`.
    pub fn merge_at(&self, row: u32, column: u32) -> Option<&Merge> {
        self.merges
            .iter()
            .find(|merge| merge.row == row && merge.column == column)
    }

    /// True when `(row, column)` lies inside a merge but is not its anchor.
    pub fn is_covered(&self, row: u32, column: u32) -> bool {
        self.merges.iter().any(|merge| {
            (merge.row, merge.column) != (row, column)
                && row >= merge.row
                && row < merge.row.saturating_add(merge.rows)
                && column >= merge.column
                && column < merge.column.saturating_add(merge.columns)
        })
    }

    /// The columns a merge hides at the boundary above `row`, as half-open
    /// `(first, end)` ranges. Excel draws no grid inside a merged range, so the
    /// horizontal line at that boundary has to be broken there.
    pub fn row_grid_holes(&self, row: usize) -> Vec<(usize, usize)> {
        let row = row as u32;
        let columns = self.column_count();
        self.merges
            .iter()
            .filter(|merge| merge.row < row && row < merge.row.saturating_add(merge.rows))
            .filter_map(|merge| {
                let start = (merge.column as usize).min(columns);
                let end = start.saturating_add(merge.columns as usize).min(columns);
                (end > start).then_some((start, end))
            })
            .collect()
    }

    /// The rows a merge hides at the boundary left of `column`.
    pub fn column_grid_holes(&self, column: usize) -> Vec<(usize, usize)> {
        let column = column as u32;
        let rows = self.row_count();
        self.merges
            .iter()
            .filter(|merge| {
                merge.column < column && column < merge.column.saturating_add(merge.columns)
            })
            .filter_map(|merge| {
                let start = (merge.row as usize).min(rows);
                let end = start.saturating_add(merge.rows as usize).min(rows);
                (end > start).then_some((start, end))
            })
            .collect()
    }
}

fn index_at(edges: &[f32], offset: f32) -> usize {
    if edges.len() < 2 || offset <= 0.0 {
        return 0;
    }
    let index = edges.partition_point(|edge| *edge <= offset);
    index.saturating_sub(1).min(edges.len() - 2)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HorizontalAlign {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum VerticalAlign {
    #[default]
    Bottom,
    Center,
    Top,
}

#[derive(Clone)]
pub struct CellStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub size: f32,
    pub color: Option<u32>,
    pub fill: Option<u32>,
    pub align: Option<HorizontalAlign>,
    pub valign: VerticalAlign,
    pub wrap: bool,
    pub border_top: Option<u32>,
    pub border_bottom: Option<u32>,
    pub border_left: Option<u32>,
    pub border_right: Option<u32>,
    /// Number format code, used while the value is turned into text.
    pub num_fmt: Option<String>,
}

impl Default for CellStyle {
    fn default() -> Self {
        Self {
            bold: false,
            italic: false,
            underline: false,
            size: DEFAULT_FONT_SIZE,
            color: None,
            fill: None,
            align: None,
            valign: VerticalAlign::Bottom,
            wrap: false,
            border_top: None,
            border_bottom: None,
            border_left: None,
            border_right: None,
            num_fmt: None,
        }
    }
}

impl CellStyle {
    /// Alignment Excel applies when the cell does not set one.
    pub fn horizontal(&self, numeric: bool) -> HorizontalAlign {
        self.align.unwrap_or(if numeric {
            HorizontalAlign::Right
        } else {
            HorizontalAlign::Left
        })
    }
}

/// A worksheet assembled by a legacy `.xls` reader.
pub(crate) struct LegacySheet {
    pub name: String,
    pub cells: Vec<LegacyCell>,
    pub merges: Vec<Merge>,
    pub column_widths: HashMap<u32, f32>,
    pub default_column_width: f32,
    pub row_heights: HashMap<u32, f32>,
    pub default_row_height: f32,
}

pub(crate) struct LegacyCell {
    pub row: u32,
    pub column: u32,
    pub text: String,
    /// Index into the workbook's styles.
    pub style: usize,
    pub numeric: bool,
}

/// Turn a legacy worksheet into the same shape the reader draws.
pub(crate) fn legacy_sheet(sheet: LegacySheet, styles: &[CellStyle]) -> Sheet {
    let mut max_row = 0;
    let mut max_column = 0;
    for cell in &sheet.cells {
        max_row = max_row.max(cell.row);
        max_column = max_column.max(cell.column);
    }

    finish_sheet(
        SheetData {
            name: sheet.name,
            column_widths: sheet.column_widths,
            default_column_width: sheet.default_column_width,
            row_heights: sheet.row_heights,
            default_row_height: sheet.default_row_height,
            cells: sheet
                .cells
                .into_iter()
                .map(|cell| RawCell {
                    row: cell.row,
                    column: cell.column,
                    style: cell.style,
                    text: cell.text,
                    numeric: cell.numeric,
                    wrap: false,
                })
                .collect(),
            merges: sheet.merges,
            max_row,
            max_column,
        },
        styles,
    )
}

/// Display text of a numeric value with an Excel number format.
pub(crate) fn formatted_number(value: f64, code: Option<&str>, date1904: bool) -> String {
    match code {
        Some(code) if code != "@" => format_number(value, code, date1904),
        _ => general(value),
    }
}

/// One of Excel's built-in number format codes.
pub(crate) fn builtin_number_format(id: usize) -> Option<&'static str> {
    builtin_format(id)
}

/// Parse an `.xlsx` package.
pub fn load(package: &Package) -> Result<Workbook, String> {
    let workbook = package
        .xml("xl/workbook.xml")
        .ok_or_else(|| String::from("这个 Excel 文档缺少 xl/workbook.xml"))?;
    let workbook = parse_tree(&workbook).map(root_of)?;

    let shared = package
        .xml("xl/sharedStrings.xml")
        .map(|xml| shared_strings(&xml))
        .unwrap_or_default();
    let style_tree = package
        .xml("xl/styles.xml")
        .map(|xml| parse_tree(&xml).map(root_of))
        .and_then(Result::ok);
    let date1904 = workbook
        .child("workbookPr")
        .and_then(|properties| properties.attr("date1904"))
        .map(|value| !matches!(value.as_str(), "0" | "false"))
        .unwrap_or(false);
    let theme = theme_of(package_theme(package));
    let styles = styles_of(style_tree.as_ref(), &theme);

    let rels = package.rels("xl/workbook.xml");
    let mut sheets = Vec::new();
    let sheet_nodes = workbook
        .child("sheets")
        .map(|sheets| sheets.children_named("sheet").collect::<Vec<_>>())
        .unwrap_or_default();
    for sheet in sheet_nodes {
        let (Some(name), Some(id)) = (sheet.attr("name"), sheet.attr("id")) else {
            continue;
        };
        let Some(path) = rels.get(&id) else {
            continue;
        };
        let Some(xml) = package.xml(path) else {
            continue;
        };
        sheets.push(parse_sheet(&name, &xml, &shared, &styles, date1904));
    }

    if sheets.is_empty() {
        return Err(String::from("这份 Excel 文档没有可显示的工作表"));
    }

    Ok(Workbook { sheets, styles })
}

// ------------------------------------------------------------------ worksheets

struct RawCell {
    row: u32,
    column: u32,
    style: usize,
    text: String,
    numeric: bool,
    /// Filled in by [`finish_sheet`] once the column widths are known.
    wrap: bool,
}

struct SheetData {
    name: String,
    column_widths: HashMap<u32, f32>,
    default_column_width: f32,
    row_heights: HashMap<u32, f32>,
    default_row_height: f32,
    cells: Vec<RawCell>,
    merges: Vec<Merge>,
    max_row: u32,
    max_column: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CellKind {
    Number,
    Shared,
    Inline,
    Text,
    Boolean,
    Error,
}

struct CellBuilder {
    row: u32,
    column: u32,
    style: usize,
    kind: CellKind,
    raw: String,
}

fn parse_sheet(
    name: &str,
    xml: &str,
    shared: &[String],
    styles: &[CellStyle],
    date1904: bool,
) -> Sheet {
    let mut data = SheetData {
        name: name.to_string(),
        column_widths: HashMap::new(),
        default_column_width: DEFAULT_COLUMN_WIDTH * 7.0 + 5.0,
        row_heights: HashMap::new(),
        default_row_height: DEFAULT_ROW_HEIGHT * 4.0 / 3.0,
        cells: Vec::new(),
        merges: Vec::new(),
        max_row: 0,
        max_column: 0,
    };

    let mut reader = ooxml::reader(xml);
    let mut row_index = 0u32;
    let mut next_column = 0u32;
    let mut cell: Option<CellBuilder> = None;
    let mut in_value = false;
    // Phonetic guides (`rPh`) inside inline strings are not displayed.
    let mut phonetic_depth = 0usize;

    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => match name_of(&event) {
                b"row" => {
                    row_index = row_of(&event, &mut data, row_index);
                    next_column = 0;
                }
                b"c" => {
                    cell = Some(cell_builder(
                        &event,
                        styles,
                        &mut row_index,
                        &mut next_column,
                    ));
                    in_value = false;
                }
                b"v" => in_value = true,
                b"rPh" => phonetic_depth += 1,
                b"col" => read_columns(&event, &mut data),
                b"mergeCell" => read_merge(&event, &mut data),
                b"sheetFormatPr" => read_sheet_format(&event, &mut data),
                _ => {}
            },
            Ok(Event::Empty(event)) => match name_of(&event) {
                b"row" => {
                    row_index = row_of(&event, &mut data, row_index);
                    next_column = 0;
                }
                b"c" => {
                    // Excel writes a cell that holds only formatting as an
                    // empty element, so it never reaches the `Start` arm above.
                    let builder = cell_builder(&event, styles, &mut row_index, &mut next_column);
                    push_cell(&mut data, builder, String::new(), styles);
                }
                b"col" => read_columns(&event, &mut data),
                b"mergeCell" => read_merge(&event, &mut data),
                b"sheetFormatPr" => read_sheet_format(&event, &mut data),
                _ => {}
            },
            Ok(Event::Text(event)) => {
                if phonetic_depth == 0
                    && let Some(builder) = cell.as_mut()
                    && (in_value || builder.kind == CellKind::Inline)
                {
                    builder.raw.push_str(&text_of(&event));
                }
            }
            Ok(Event::GeneralRef(event)) => {
                if phonetic_depth == 0
                    && let Some(builder) = cell.as_mut()
                    && (in_value || builder.kind == CellKind::Inline)
                    && let Some(text) = ooxml::text_of_ref(&event)
                {
                    builder.raw.push_str(&text);
                }
            }
            Ok(Event::End(event)) => match end_name(&event) {
                b"c" => {
                    if let Some(builder) = cell.take() {
                        let text = resolve_text(&builder, shared, styles, date1904);
                        push_cell(&mut data, builder, text, styles);
                    }
                    in_value = false;
                    phonetic_depth = 0;
                }
                b"v" => in_value = false,
                b"rPh" => phonetic_depth = phonetic_depth.saturating_sub(1),
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    finish_sheet(data, styles)
}

/// Read the attributes of a `<c>` element and note where the next cell sits.
fn cell_builder(
    event: &quick_xml::events::BytesStart<'_>,
    styles: &[CellStyle],
    row_index: &mut u32,
    next_column: &mut u32,
) -> CellBuilder {
    // `cell_reference` returns (row, column); the fallback for a missing or
    // invalid `r` must use that same order, or the sheet is transposed.
    let (row, column) = match attr(event, "r") {
        Some(reference) => cell_reference(&reference).unwrap_or((*row_index, *next_column)),
        None => (*row_index, *next_column),
    };
    let row = row.min(MAX_SHEET_ROWS - 1);
    let column = column.min(MAX_SHEET_COLUMNS - 1);
    *next_column = column.saturating_add(1);
    *row_index = row;
    let style = attr(event, "s")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
        .min(styles.len().saturating_sub(1));
    let kind = match attr(event, "t").as_deref() {
        Some("s") => CellKind::Shared,
        Some("inlineStr") => CellKind::Inline,
        Some("str") => CellKind::Text,
        Some("b") => CellKind::Boolean,
        Some("e") => CellKind::Error,
        _ => CellKind::Number,
    };
    CellBuilder {
        row,
        column,
        style,
        kind,
        raw: String::new(),
    }
}

/// Add a finished cell to the sheet.
///
/// A cell without text is worth keeping when it is shaded or ruled: on the
/// sheet it is a coloured block or a piece of the table's grid, not a blank.
fn push_cell(data: &mut SheetData, builder: CellBuilder, text: String, styles: &[CellStyle]) {
    let text = text.trim_end().to_string();
    let decorated = styles.get(builder.style).is_some_and(|style| {
        style.fill.is_some()
            || style.border_top.is_some()
            || style.border_bottom.is_some()
            || style.border_left.is_some()
            || style.border_right.is_some()
    });
    if !text.is_empty() || decorated {
        data.cells.push(RawCell {
            row: builder.row,
            column: builder.column,
            style: builder.style,
            text,
            numeric: builder.kind == CellKind::Number,
            wrap: false,
        });
    }
    data.max_row = data.max_row.max(builder.row.min(MAX_SHEET_ROWS - 1));
    data.max_column = data
        .max_column
        .max(builder.column.min(MAX_SHEET_COLUMNS - 1));
}

/// The display text of a cell, from its stored value and type.
fn resolve_text(
    cell: &CellBuilder,
    shared: &[String],
    styles: &[CellStyle],
    date1904: bool,
) -> String {
    let raw = cell.raw.trim();
    if raw.is_empty() {
        return String::new();
    }

    match cell.kind {
        CellKind::Shared => raw
            .parse::<usize>()
            .ok()
            .and_then(|index| shared.get(index))
            .cloned()
            .unwrap_or_default(),
        CellKind::Inline | CellKind::Text | CellKind::Error => raw.to_string(),
        CellKind::Boolean => {
            if raw == "1" || raw.eq_ignore_ascii_case("true") {
                String::from("TRUE")
            } else {
                String::from("FALSE")
            }
        }
        CellKind::Number => {
            let style = styles.get(cell.style);
            format_value(
                raw,
                style.and_then(|style| style.num_fmt.as_deref()),
                date1904,
            )
        }
    }
}

fn row_of(event: &quick_xml::events::BytesStart<'_>, data: &mut SheetData, fallback: u32) -> u32 {
    let index = attr(event, "r")
        .and_then(|value| value.parse::<u32>().ok())
        .map(|value| value.saturating_sub(1))
        .unwrap_or(fallback)
        .min(MAX_SHEET_ROWS - 1);
    if let Some(height) = attr_f32(event, "ht") {
        data.row_heights
            .insert(index, (height * 4.0 / 3.0).max(6.0));
    }
    index
}

fn read_columns(event: &quick_xml::events::BytesStart<'_>, data: &mut SheetData) {
    let min = attr(event, "min")
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(1)
        .saturating_sub(1)
        .min(MAX_SHEET_COLUMNS - 1);
    let max = attr(event, "max")
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(min + 1)
        .saturating_sub(1)
        .min(MAX_SHEET_COLUMNS - 1);
    let width = attr_f32(event, "width").unwrap_or(DEFAULT_COLUMN_WIDTH);
    let pixels = (width * 7.0 + 5.0).max(8.0);
    for index in min..=max.min(min.saturating_add(1024)) {
        data.column_widths.insert(index, pixels);
    }
}

fn read_merge(event: &quick_xml::events::BytesStart<'_>, data: &mut SheetData) {
    if let Some(reference) = attr(event, "ref")
        && let Some(merge) = parse_merge(&reference)
    {
        data.merges.push(merge);
    }
}

fn read_sheet_format(event: &quick_xml::events::BytesStart<'_>, data: &mut SheetData) {
    if let Some(height) = attr_f32(event, "defaultRowHeight") {
        data.default_row_height = (height * 4.0 / 3.0).max(6.0);
    }
    if let Some(width) = attr_f32(event, "defaultColWidth") {
        data.default_column_width = (width * 7.0 + 5.0).max(8.0);
    }
}

fn finish_sheet(mut data: SheetData, styles: &[CellStyle]) -> Sheet {
    for merge in &data.merges {
        data.max_row = data.max_row.max(
            merge
                .row
                .saturating_add(merge.rows)
                .saturating_sub(1)
                .min(MAX_SHEET_ROWS - 1),
        );
        data.max_column = data.max_column.max(
            merge
                .column
                .saturating_add(merge.columns)
                .saturating_sub(1)
                .min(MAX_SHEET_COLUMNS - 1),
        );
    }
    for row in data.row_heights.keys().copied().collect::<Vec<_>>() {
        data.max_row = data.max_row.max(row.min(MAX_SHEET_ROWS - 1));
    }
    for column in data.column_widths.keys().copied().collect::<Vec<_>>() {
        data.max_column = data.max_column.max(column.min(MAX_SHEET_COLUMNS - 1));
    }

    let rows = data.max_row.min(MAX_SHEET_ROWS - 1) as usize + 1;
    let columns = data.max_column.min(MAX_SHEET_COLUMNS - 1) as usize + 1;

    // The column edges come first: a cell's column width is what decides where
    // its text has to wrap, and wrapping is what keeps the text from being cut
    // off at the cell border.
    //
    // A column that is only a little too narrow for a single-line value gets
    // wider instead of breaking that value in two: a phone number split as
    // `187295750` / `26` is complete but unreadable. Anything much longer than
    // the column wraps, so a long address cannot stretch the sheet sideways.
    let mut widths = (0..columns)
        .map(|column| {
            data.column_widths
                .get(&(column as u32))
                .copied()
                .unwrap_or(data.default_column_width)
                .max(8.0)
        })
        .collect::<Vec<_>>();

    for cell in &data.cells {
        if cell.text.is_empty() || cell.text.contains('\n') {
            continue;
        }
        let Some(style) = styles.get(cell.style) else {
            continue;
        };
        // Text the sheet already wraps, and text spanning several columns,
        // are handled by wrapping; a merge would have to share any extra
        // width between the columns it covers.
        if style.wrap || merged_span(&data.merges, cell.row, cell.column) != (1, 1) {
            continue;
        }
        // Chinese text can break between any two characters, so wrapping it
        // costs nothing; a short ASCII run (a phone number, a code) has no
        // break point and reads badly when cut in two.
        if !is_unbreakable(&cell.text) {
            continue;
        }
        let column = (cell.column as usize).min(columns - 1);
        let available = (widths[column] - CELL_INSET).max(8.0);
        let single = text_width(&cell.text, style.size);
        if single > available && single <= available * 2.0 {
            // Half a pixel of slack: adding the inset and taking it off again
            // is not exact in floating point, and a cell that lands exactly on
            // the width must not be wrapped for a rounding error. The clearance
            // is what keeps the last digit of a phone number off the border.
            widths[column] = widths[column].max(single + CELL_INSET + CELL_CLEARANCE + 0.5);
        }
    }

    let mut column_lefts = Vec::with_capacity(columns + 1);
    let mut left = 0.0;
    column_lefts.push(0.0);
    for width in &widths {
        left += *width;
        column_lefts.push(left);
    }

    // Each cell is marked with the wrapping it needs, and every row starts at
    // the height the sheet gave it. A row that was sized for one line but now
    // carries a wrapped cell grows to fit: the reader would rather show a
    // taller row than the top half of a name.
    let mut heights = (0..rows)
        .map(|row| {
            data.row_heights
                .get(&(row as u32))
                .copied()
                .unwrap_or(data.default_row_height)
                .max(6.0)
        })
        .collect::<Vec<_>>();
    let mut required = vec![0.0f32; rows];
    let mut merged_growth = Vec::new();
    let merges = &data.merges;

    for cell in &mut data.cells {
        if cell.text.is_empty() {
            continue;
        }
        let Some(style) = styles.get(cell.style) else {
            continue;
        };
        let span = merged_span(merges, cell.row, cell.column);
        let start = (cell.column as usize).min(columns);
        let end = (start + span.1 as usize).min(columns);
        let available = (column_lefts[end] - column_lefts[start] - CELL_INSET).max(8.0);

        // A line break is a break whether the style asks for wrapping or not,
        // and text that does not fit on one line is wrapped instead of being
        // clipped. Only a cell that is both short enough and break-free keeps
        // Excel's single-line look.
        let wrap = style.wrap
            || cell.text.contains('\n')
            || text_width(&cell.text, style.size) > available + 0.5;
        cell.wrap = wrap;
        if !wrap {
            continue;
        }

        let lines = wrapped_lines(&cell.text, style.size, available);
        let needed = lines as f32 * style.size * LINE_HEIGHT_FACTOR + CELL_VPAD;
        if span.0 <= 1 {
            let row = (cell.row as usize).min(rows - 1);
            required[row] = required[row].max(needed);
        } else {
            merged_growth.push((cell.row, span.0, needed));
        }
    }

    for (height, needed) in heights.iter_mut().zip(required.iter()) {
        *height = height.max(*needed);
    }
    // A merged cell that needs more room than its rows have pushes the bottom
    // row down. Growing only the last row keeps the rows above it exactly
    // where Excel put them.
    for (row, span, needed) in merged_growth {
        let start = (row as usize).min(rows - 1);
        let end = (start + span as usize).min(rows);
        if end <= start {
            continue;
        }
        let current: f32 = heights[start..end].iter().sum();
        if current + 0.5 < needed {
            heights[end - 1] += needed - current;
        }
    }

    let mut row_tops = Vec::with_capacity(rows + 1);
    let mut top = 0.0;
    row_tops.push(0.0);
    for height in &heights {
        top += *height;
        row_tops.push(top);
    }

    // Group the cells by row, keeping columns ordered.
    data.cells.sort_by_key(|cell| (cell.row, cell.column));
    let mut grouped: Vec<SheetRow> = Vec::new();
    let mut current_row: Option<u32> = None;
    for cell in data.cells {
        let RawCell {
            row,
            column,
            style,
            text,
            numeric,
            wrap,
        } = cell;
        let cell = Cell {
            column,
            text,
            style,
            numeric,
            wrap,
        };
        if current_row == Some(row) {
            if let Some(last) = grouped.last_mut() {
                last.cells.push(cell);
            }
        } else {
            current_row = Some(row);
            grouped.push(SheetRow {
                index: row,
                cells: vec![cell],
            });
        }
    }

    Sheet {
        name: data.name,
        column_lefts,
        row_tops,
        rows: grouped,
        merges: data.merges,
    }
}

/// True for a value that should stay on one line if a column is only a little
/// too narrow: a short run of plain ASCII such as `18729575026`, or a short
/// Chinese label such as `是否有异常`. A long sentence still wraps rather than
/// widening its column.
fn is_unbreakable(text: &str) -> bool {
    let count = text.chars().count();
    if text.is_ascii() {
        count <= 20
    } else {
        count <= 8
    }
}

/// Width of `text` drawn on one line, in logical pixels.
fn text_width(text: &str, size: f32) -> f32 {
    text.chars()
        .map(|character| char_advance(character, size))
        .sum()
}

/// Advance width of one character, in logical pixels.
///
/// The em is rounded up to a whole pixel because neither renderer draws at the
/// exact fractional size: GDI creates its font at `round(pt * dpi / 72)` and a
/// 11 pt cell is therefore a 15 px font on a 96 dpi page, every glyph a
/// fraction wider than the point size suggests. Estimating without that
/// rounding left a phone number 2 px narrower than it is drawn, and the last
/// digit was clipped by the cell it sat in.
///
/// Inside the em a CJK glyph is a full em wide; ASCII in the fonts Excel uses
/// runs about six tenths of an em — `18729575026` measures 99 px at 15 px, not
/// the 91 px a five-tenths estimate gives.
fn char_advance(character: char, size: f32) -> f32 {
    let em = size.ceil();
    match character {
        // A tab is a column stop rather than a glyph; two ems is a fair guess
        // and errs on the wide side, which only ever adds slack.
        '\t' => em * 2.0,
        // Line breaks take no room on the line they end.
        '\n' | '\r' => 0.0,
        _ if character.is_ascii() => em * 0.6,
        _ => em,
    }
}

/// The number of lines `text` takes when drawn `width` pixels wide.
///
/// Each hard line break starts a new line, and a line that runs past `width`
/// wraps. The estimate counts every character, so it never comes out below the
/// renderer's word-aware wrapping — the row is a touch too tall rather than a
/// line short.
fn wrapped_lines(text: &str, size: f32, width: f32) -> usize {
    let width = width.max(1.0);
    let mut lines = 0usize;
    for line in text.split('\n') {
        lines += wrapped_lines_of(line, size, width);
    }
    lines.max(1).min(MAX_WRAP_LINES)
}

/// Lines one hard line of text takes at `width`.
fn wrapped_lines_of(line: &str, size: f32, width: f32) -> usize {
    let mut lines = 1usize;
    let mut used = 0.0;
    for character in line.chars() {
        let advance = char_advance(character, size);
        // A single glyph wider than the column still has to go somewhere; it
        // gets its own line instead of wrapping to an empty one forever.
        if used > 0.0 && used + advance > width {
            lines += 1;
            used = 0.0;
        }
        used += advance;
    }
    lines
}

/// Row and column span of the merge anchored at `(row, column)`, or `(1, 1)`.
fn merged_span(merges: &[Merge], row: u32, column: u32) -> (u32, u32) {
    merges
        .iter()
        .find(|merge| merge.row == row && merge.column == column)
        .map(|merge| (merge.rows, merge.columns))
        .unwrap_or((1, 1))
}

fn parse_merge(reference: &str) -> Option<Merge> {
    let (start, end) = reference.split_once(':').unwrap_or((reference, reference));
    let (row, column) = cell_reference(start)?;
    let (end_row, end_column) = cell_reference(end)?;
    if end_row < row || end_column < column {
        return None;
    }
    Some(Merge {
        row,
        column,
        rows: (end_row - row + 1).min(MAX_SHEET_ROWS),
        columns: (end_column - column + 1).min(MAX_SHEET_COLUMNS),
    })
}

/// `B12` -> (row 11, column 1).
fn cell_reference(reference: &str) -> Option<(u32, u32)> {
    let mut column = 0u32;
    let mut row = String::new();
    let mut seen_letter = false;
    for character in reference.chars() {
        if character.is_ascii_alphabetic() {
            if !row.is_empty() {
                return None;
            }
            seen_letter = true;
            column = column
                .saturating_mul(26)
                .saturating_add(character.to_ascii_uppercase() as u32 - 'A' as u32 + 1);
        } else if character.is_ascii_digit() {
            row.push(character);
        } else if character != '$' {
            return None;
        }
    }
    if !seen_letter {
        return None;
    }
    let row = row.parse::<u32>().ok()?.saturating_sub(1);
    Some((
        row.min(MAX_SHEET_ROWS - 1),
        column.saturating_sub(1).min(MAX_SHEET_COLUMNS - 1),
    ))
}

fn shared_strings(xml: &str) -> Vec<String> {
    let mut reader = ooxml::reader(xml);
    let mut strings = Vec::new();
    let mut current: Option<String> = None;
    let mut phonetic_depth = 0usize;

    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => match name_of(&event) {
                b"si" => current = Some(String::new()),
                b"rPh" => phonetic_depth += 1,
                _ => {}
            },
            Ok(Event::Text(event)) => {
                if phonetic_depth == 0
                    && let Some(text) = current.as_mut()
                {
                    text.push_str(&text_of(&event));
                }
            }
            Ok(Event::GeneralRef(event)) => {
                if phonetic_depth == 0
                    && let Some(text) = current.as_mut()
                    && let Some(reference) = ooxml::text_of_ref(&event)
                {
                    text.push_str(&reference);
                }
            }
            Ok(Event::End(event)) => match end_name(&event) {
                b"si" => strings.push(current.take().unwrap_or_default()),
                b"rPh" => phonetic_depth = phonetic_depth.saturating_sub(1),
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    strings
}

// --------------------------------------------------------------------- colours

/// The colours of the workbook theme.
///
/// A colour in the style sheet can name a theme entry by index instead of an
/// RGB value, which is what Excel writes for every fill picked from its palette.
/// The indexes follow the order the palette shows — the two background/text
/// pairs first, then the six accents and the two link colours — and not the
/// order the theme part stores them in.
struct Theme {
    colors: Vec<u32>,
}

/// Office's own theme, used when a package carries no theme part.
const DEFAULT_THEME: [u32; 12] = [
    0xFFFFFF, 0x000000, // background 1, text 1
    0xE7E6E6, 0x44546A, // background 2, text 2
    0x4472C4, 0xED7D31, 0xA5A5A5, 0xFFC000, 0x5B9BD5, 0x70AD47, // accents 1 to 6
    0x0563C1, 0x954F72, // hyperlink, followed hyperlink
];

/// Excel's legacy palette, which an `indexed` colour refers to.
const INDEXED_COLORS: [u32; 64] = [
    0x000000, 0xFFFFFF, 0xFF0000, 0x00FF00, 0x0000FF, 0xFFFF00, 0xFF00FF, 0x00FFFF, 0x000000,
    0xFFFFFF, 0xFF0000, 0x00FF00, 0x0000FF, 0xFFFF00, 0xFF00FF, 0x00FFFF, 0x800000, 0x008000,
    0x000080, 0x808000, 0x800080, 0x008080, 0xC0C0C0, 0x808080, 0x9999FF, 0x993366, 0xFFFFCC,
    0xCCFFFF, 0x660066, 0xFF8080, 0x0066CC, 0xCCCCFF, 0x000080, 0xFF00FF, 0xFFFF00, 0x00FFFF,
    0x800080, 0x800000, 0x008080, 0x0000FF, 0x00CCFF, 0xCCFFFF, 0xCCFFCC, 0xFFFF99, 0x99CCFF,
    0xFF99CC, 0xCC99FF, 0xFFCC99, 0x3366FF, 0x33CCCC, 0x99CC00, 0xFFCC00, 0xFF9900, 0xFF6600,
    0x666699, 0x969696, 0x003366, 0x339966, 0x003300, 0x333300, 0x993300, 0x993366, 0x333399,
    0x333333,
];

impl Theme {
    fn fallback() -> Self {
        Self {
            colors: DEFAULT_THEME.to_vec(),
        }
    }

    fn get(&self, index: usize) -> Option<u32> {
        self.colors.get(index).copied()
    }
}

fn theme_of(xml: Option<String>) -> Theme {
    let mut theme = Theme::fallback();
    let Some(root) = xml.as_deref().and_then(|xml| parse_tree(xml).ok()) else {
        return theme;
    };
    let Some(scheme) = root.descendant("clrScheme") else {
        return theme;
    };
    for entry in &scheme.children {
        let Some(slot) = theme_slot(&entry.name) else {
            continue;
        };
        // `srgbClr` carries the colour itself; `sysClr` names a system colour
        // and keeps the one it stood for in `lastClr`.
        let value = entry
            .children
            .iter()
            .find_map(|color| match color.name.as_str() {
                "srgbClr" => color.attr("val"),
                "sysClr" => color.attr("lastClr"),
                _ => None,
            });
        if let Some(color) = value.and_then(|value| parse_hex_color(&value)) {
            theme.colors[slot] = color;
        }
    }
    theme
}

/// Where a colour scheme entry sits in Excel's palette order.
fn theme_slot(name: &str) -> Option<usize> {
    Some(match name {
        "lt1" => 0,
        "dk1" => 1,
        "lt2" => 2,
        "dk2" => 3,
        "accent1" => 4,
        "accent2" => 5,
        "accent3" => 6,
        "accent4" => 7,
        "accent5" => 8,
        "accent6" => 9,
        "hlink" => 10,
        "folHlink" => 11,
        _ => return None,
    })
}

/// The theme part of a package, wherever the writer put it.
fn package_theme(package: &Package) -> Option<String> {
    ["xl/theme/theme1.xml", "xl/theme/theme2.xml"]
        .into_iter()
        .find_map(|path| package.xml(path))
}

/// An `rgb`, `indexed` or `theme` colour, as `0xRRGGBB`.
fn resolve_color(node: &Node, theme: &Theme) -> Option<u32> {
    let tint = node
        .attr("tint")
        .and_then(|value| value.trim().parse::<f32>().ok())
        .unwrap_or(0.0);

    if let Some(rgb) = node.attr("rgb").and_then(|value| parse_hex_color(&value)) {
        return Some(apply_tint(rgb, tint));
    }
    if let Some(index) = node
        .attr("indexed")
        .and_then(|value| value.trim().parse::<usize>().ok())
    {
        // 64 and 65 are the system window colours, which on paper are the
        // ordinary black and white.
        let rgb = match index {
            64 => Some(0x000000),
            65 => Some(0xFFFFFF),
            _ => INDEXED_COLORS.get(index).copied(),
        };
        return rgb.map(|rgb| apply_tint(rgb, tint));
    }
    if let Some(index) = node
        .attr("theme")
        .and_then(|value| value.trim().parse::<usize>().ok())
    {
        return theme.get(index).map(|rgb| apply_tint(rgb, tint));
    }
    None
}

/// Lighten or darken a colour the way Excel does: the tint moves the
/// lightness of the colour in HLS, leaving its hue and saturation alone.
fn apply_tint(rgb: u32, tint: f32) -> u32 {
    if !tint.is_finite() || tint == 0.0 {
        return rgb;
    }
    let red = ((rgb >> 16) & 0xFF) as f64 / 255.0;
    let green = ((rgb >> 8) & 0xFF) as f64 / 255.0;
    let blue = (rgb & 0xFF) as f64 / 255.0;

    let (hue, lightness, saturation) = rgb_to_hls(red, green, blue);
    let tint = tint as f64;
    let lightness = if tint < 0.0 {
        lightness * (1.0 + tint)
    } else {
        lightness * (1.0 - tint) + tint
    };
    let (red, green, blue) = hls_to_rgb(hue, lightness, saturation);
    let channel = |value: f64| (value.clamp(0.0, 1.0) * 255.0).round() as u32;
    (channel(red) << 16) | (channel(green) << 8) | channel(blue)
}

fn rgb_to_hls(red: f64, green: f64, blue: f64) -> (f64, f64, f64) {
    let max = red.max(green).max(blue);
    let min = red.min(green).min(blue);
    let lightness = (max + min) / 2.0;
    if max == min {
        return (0.0, lightness, 0.0);
    }
    let delta = max - min;
    let saturation = if lightness < 0.5 {
        delta / (max + min)
    } else {
        delta / (2.0 - max - min)
    };
    let hue = if max == red {
        (green - blue) / delta + if green < blue { 6.0 } else { 0.0 }
    } else if max == green {
        (blue - red) / delta + 2.0
    } else {
        (red - green) / delta + 4.0
    };
    (hue / 6.0, lightness, saturation)
}

fn hls_to_rgb(hue: f64, lightness: f64, saturation: f64) -> (f64, f64, f64) {
    if saturation <= 0.0 {
        return (lightness, lightness, lightness);
    }
    let other = if lightness < 0.5 {
        lightness * (1.0 + saturation)
    } else {
        lightness + saturation - lightness * saturation
    };
    let first = 2.0 * lightness - other;
    (
        hue_channel(first, other, hue + 1.0 / 3.0),
        hue_channel(first, other, hue),
        hue_channel(first, other, hue - 1.0 / 3.0),
    )
}

fn hue_channel(first: f64, other: f64, hue: f64) -> f64 {
    let hue = if hue < 0.0 {
        hue + 1.0
    } else if hue > 1.0 {
        hue - 1.0
    } else {
        hue
    };
    if hue * 6.0 < 1.0 {
        first + (other - first) * hue * 6.0
    } else if hue * 2.0 < 1.0 {
        other
    } else if hue * 3.0 < 2.0 {
        first + (other - first) * (2.0 / 3.0 - hue) * 6.0
    } else {
        first
    }
}

// ---------------------------------------------------------------------- styles

fn styles_of(node: Option<&Node>, theme: &Theme) -> Vec<CellStyle> {
    let Some(node) = node else {
        return vec![CellStyle::default()];
    };

    let fonts = node
        .child("fonts")
        .map(|fonts| {
            fonts
                .children_named("font")
                .map(|font| font_of(font, theme))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let fills = node
        .child("fills")
        .map(|fills| {
            fills
                .children_named("fill")
                .map(|fill| fill_of(fill, theme))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let borders = node
        .child("borders")
        .map(|borders| {
            borders
                .children_named("border")
                .map(|border| border_of(border, theme))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let custom_formats = number_formats(node);

    let Some(cell_xfs) = node.child("cellXfs") else {
        return vec![CellStyle::default()];
    };

    let mut styles = Vec::new();
    for xf in cell_xfs.children_named("xf") {
        let mut style = CellStyle::default();
        if let Some(font) = xf
            .attr("fontId")
            .and_then(|value| value.parse::<usize>().ok())
            .and_then(|index| fonts.get(index))
        {
            style.bold = font.bold;
            style.italic = font.italic;
            style.underline = font.underline;
            style.size = font.size;
            style.color = font.color;
        }
        if let Some(fill) = xf
            .attr("fillId")
            .and_then(|value| value.parse::<usize>().ok())
            .and_then(|index| fills.get(index))
        {
            style.fill = *fill;
        }
        if let Some(border) = xf
            .attr("borderId")
            .and_then(|value| value.parse::<usize>().ok())
            .and_then(|index| borders.get(index))
        {
            style.border_top = border.top;
            style.border_bottom = border.bottom;
            style.border_left = border.left;
            style.border_right = border.right;
        }
        if let Some(id) = xf
            .attr("numFmtId")
            .and_then(|value| value.parse::<usize>().ok())
        {
            style.num_fmt = custom_formats
                .get(&id)
                .cloned()
                .or_else(|| builtin_format(id).map(str::to_string))
                .filter(|code| code != "General");
        }
        if let Some(alignment) = xf.child("alignment") {
            style.align = alignment
                .attr("horizontal")
                .map(|value| match value.as_str() {
                    "center" | "centerContinuous" => HorizontalAlign::Center,
                    "right" => HorizontalAlign::Right,
                    _ => HorizontalAlign::Left,
                });
            style.valign = match alignment.attr("vertical").as_deref() {
                Some("top") => VerticalAlign::Top,
                Some("center") => VerticalAlign::Center,
                _ => VerticalAlign::Bottom,
            };
            style.wrap = alignment
                .attr("wrapText")
                .map(|value| !matches!(value.as_str(), "0" | "false"))
                .unwrap_or(false);
        }

        style.size = style.size.clamp(6.0, 96.0);
        styles.push(style);
    }

    if styles.is_empty() {
        styles.push(CellStyle::default());
    }
    styles
}

struct FontData {
    bold: bool,
    italic: bool,
    underline: bool,
    size: f32,
    color: Option<u32>,
}

fn font_of(node: &Node, theme: &Theme) -> FontData {
    FontData {
        bold: is_true(node.child("b")),
        italic: is_true(node.child("i")),
        underline: node
            .child("u")
            .is_some_and(|node| node.attr("val").as_deref() != Some("none")),
        size: node
            .child("sz")
            .and_then(|size| size.attr("val"))
            .and_then(|value| value.parse::<f32>().ok())
            .map(|points| points * 4.0 / 3.0)
            .unwrap_or(DEFAULT_FONT_SIZE),
        color: node
            .child("color")
            .and_then(|color| resolve_color(color, theme)),
    }
}

fn is_true(node: Option<&Node>) -> bool {
    node.is_some_and(|node| match node.attr("val") {
        Some(value) => !matches!(value.as_str(), "0" | "false" | "off"),
        None => true,
    })
}

fn fill_of(node: &Node, theme: &Theme) -> Option<u32> {
    if let Some(pattern) = node.child("patternFill") {
        if pattern.attr("patternType").as_deref() == Some("none") {
            return None;
        }
        // A solid fill names its colour in `fgColor`, but some writers put it
        // in `bgColor` and leave the first one out.
        return ["fgColor", "bgColor"].into_iter().find_map(|name| {
            pattern
                .child(name)
                .and_then(|color| resolve_color(color, theme))
        });
    }

    // A gradient is painted with its first stop, which is close enough to the
    // shading a cell shows on screen.
    node.child("gradientFill")
        .and_then(|gradient| gradient.children_named("stop").next())
        .and_then(|stop| stop.child("color"))
        .and_then(|color| resolve_color(color, theme))
}

struct BorderData {
    top: Option<u32>,
    bottom: Option<u32>,
    left: Option<u32>,
    right: Option<u32>,
}

fn border_of(node: &Node, theme: &Theme) -> BorderData {
    let side = |name: &str| -> Option<u32> {
        let element = node.child(name)?;
        let style = element.attr("style").unwrap_or_default();
        if style.is_empty() || style == "none" {
            return None;
        }
        Some(
            element
                .child("color")
                .and_then(|color| resolve_color(color, theme))
                .unwrap_or(0x808080),
        )
    };
    BorderData {
        top: side("top"),
        bottom: side("bottom"),
        left: side("left"),
        right: side("right"),
    }
}

fn number_formats(node: &Node) -> HashMap<usize, String> {
    let mut formats = HashMap::new();
    if let Some(node) = node.child("numFmts") {
        for format in node.children_named("numFmt") {
            if let (Some(id), Some(code)) = (
                format
                    .attr("numFmtId")
                    .and_then(|value| value.parse::<usize>().ok()),
                format.attr("formatCode"),
            ) {
                formats.insert(id, code);
            }
        }
    }
    formats
}

fn builtin_format(id: usize) -> Option<&'static str> {
    Some(match id {
        0 => "General",
        1 => "0",
        2 => "0.00",
        3 => "#,##0",
        4 => "#,##0.00",
        9 => "0%",
        10 => "0.00%",
        11 => "0.00E+00",
        12 => "# ?/?",
        13 => "# ??/??",
        14 => "m/d/yyyy",
        15 => "d-mmm-yy",
        16 => "d-mmm",
        17 => "mmm-yy",
        18 => "h:mm AM/PM",
        19 => "h:mm:ss AM/PM",
        20 => "h:mm",
        21 => "h:mm:ss",
        22 => "m/d/yyyy h:mm",
        37 => "#,##0;(#,##0)",
        38 => "#,##0;[Red](#,##0)",
        39 => "#,##0.00;(#,##0.00)",
        40 => "#,##0.00;[Red](#,##0.00)",
        45 => "mm:ss",
        46 => "[h]:mm:ss",
        47 => "mm:ss.0",
        48 => "##0.0E+0",
        49 => "@",
        _ => return None,
    })
}

// ------------------------------------------------------------ value formatting

/// Display text of a numeric cell, honouring its number format.
fn format_value(raw: &str, code: Option<&str>, date1904: bool) -> String {
    let Ok(value) = raw.parse::<f64>() else {
        return raw.to_string();
    };
    match code {
        Some(code) if code != "@" => format_number(value, code, date1904),
        _ => general(value),
    }
}

fn general(value: f64) -> String {
    if !value.is_finite() {
        return String::new();
    }
    if value == 0.0 {
        return String::from("0");
    }
    let magnitude = value.abs();
    if magnitude >= 1e11 || magnitude < 1e-10 {
        return tidy_exponent(format!("{value:E}"));
    }

    let mut text = format!("{value:.10}");
    if text.contains('.') {
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
    }
    if text == "-0" {
        String::from("0")
    } else {
        text
    }
}

fn tidy_exponent(text: String) -> String {
    let (mantissa, exponent) = text.split_once('E').unwrap_or((text.as_str(), "0"));
    let (sign, digits) = match exponent.strip_prefix('-') {
        Some(digits) => ('-', digits),
        None => ('+', exponent),
    };
    format!("{mantissa}E{sign}{digits:0>2}")
}

fn format_number(value: f64, code: &str, date1904: bool) -> String {
    let sections = split_sections(code);
    let negative = value < 0.0;
    let section = if negative && sections.len() > 1 {
        sections[1]
    } else {
        sections[0]
    };

    if looks_like_date(section) {
        return format_date(value, section, date1904);
    }

    let percent = section.contains('%');
    let scaled = if percent {
        value.abs() * 100.0
    } else {
        value.abs()
    };
    let decimals = decimal_places(section);
    let (prefix, suffix) = literals(section);

    let mut text = if section.to_ascii_uppercase().contains('E') {
        let text = tidy_exponent(format!("{scaled:.decimals$E}"));
        return format!("{prefix}{text}");
    } else if has_thousands(section) {
        grouped(scaled, decimals)
    } else {
        format!("{scaled:.decimals$}")
    };

    if negative && sections.len() == 1 {
        text.insert(0, '-');
    }
    format!("{prefix}{text}{suffix}")
}

fn split_sections(code: &str) -> Vec<&str> {
    let mut sections = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    for (index, byte) in code.as_bytes().iter().enumerate() {
        match byte {
            b'"' => quoted = !quoted,
            b';' if !quoted => {
                sections.push(&code[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    sections.push(&code[start..]);
    if sections
        .first()
        .is_none_or(|section| section.trim().is_empty())
    {
        sections[0] = "General";
    }
    sections
}

/// Text around the numeric placeholders: currency symbols, quoted strings and
/// escaped characters.
fn literals(section: &str) -> (String, String) {
    let mut prefix = String::new();
    let mut suffix = String::new();
    let mut seen_placeholder = false;
    let mut chars = section.chars().peekable();

    while let Some(character) = chars.next() {
        let mut literal = String::new();
        match character {
            '"' => {
                for inner in chars.by_ref() {
                    if inner == '"' {
                        break;
                    }
                    literal.push(inner);
                }
            }
            '\\' => {
                if let Some(next) = chars.next() {
                    literal.push(next);
                }
            }
            '_' => {
                if chars.next().is_some() {
                    literal.push(' ');
                }
            }
            '*' => {
                chars.next();
            }
            '[' => {
                let mut content = String::new();
                for inner in chars.by_ref() {
                    if inner == ']' {
                        break;
                    }
                    content.push(inner);
                }
                if let Some(rest) = content.strip_prefix('$') {
                    literal.extend(rest.chars().take_while(|symbol| *symbol != '-'));
                }
            }
            '0' | '#' | '?' | '.' | ',' | 'E' | 'e' => seen_placeholder = true,
            other => literal.push(other),
        }

        if !literal.is_empty() {
            if seen_placeholder {
                suffix.push_str(&literal);
            } else {
                prefix.push_str(&literal);
            }
        }
    }

    (prefix, suffix)
}

fn has_thousands(section: &str) -> bool {
    // Grouping is on when a comma sits between digit placeholders, as in
    // "#,##0"; "0," on its own scales the number by a thousand instead.
    let mut previous: Option<char> = None;
    for character in section.chars() {
        if character == ',' && matches!(previous, Some('0' | '#' | '?')) {
            return true;
        }
        if !character.is_whitespace() {
            previous = Some(character);
        }
    }
    false
}

fn decimal_places(section: &str) -> usize {
    match section.find('.') {
        Some(index) => section[index + 1..]
            .chars()
            .take_while(|character| matches!(character, '0' | '#' | '?'))
            .count(),
        None => 0,
    }
}

fn grouped(value: f64, decimals: usize) -> String {
    let text = format!("{value:.decimals$}");
    let (integer, fraction) = match text.split_once('.') {
        Some((integer, fraction)) => (integer.to_string(), Some(fraction.to_string())),
        None => (text, None),
    };

    let mut out = String::new();
    for (index, character) in integer.chars().enumerate() {
        if index > 0 && (integer.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(character);
    }
    match fraction {
        Some(fraction) => format!("{out}.{fraction}"),
        None => out,
    }
}

fn looks_like_date(section: &str) -> bool {
    let mut quoted = false;
    let mut chars = section.chars();
    while let Some(character) = chars.next() {
        match character {
            '"' => quoted = !quoted,
            '\\' => {
                chars.next();
            }
            _ if quoted => {}
            'y' | 'Y' | 'd' | 'D' | 'h' | 'H' | 's' | 'S' | 'm' | 'M' => return true,
            '[' => {
                for inner in chars.by_ref() {
                    if inner == ']' {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    false
}

fn format_date(value: f64, code: &str, date1904: bool) -> String {
    let (year, month, day, hour, minute, second) = serial_to_date(value, date1904);
    let elapsed_hours = value * 24.0;
    let twelve_hour = code.to_ascii_uppercase().contains("AM/PM");
    let (hour12, half) = match hour {
        0 => (12, "AM"),
        1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        _ => (hour - 12, "PM"),
    };

    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    const WEEKDAYS: [&str; 7] = [
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ];

    let mut out = String::new();
    let mut chars = code.chars().peekable();
    let mut after_hour = false;

    while let Some(character) = chars.next() {
        match character {
            '"' => {
                for inner in chars.by_ref() {
                    if inner == '"' {
                        break;
                    }
                    out.push(inner);
                }
            }
            '\\' => {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            }
            '[' => {
                let mut content = String::new();
                for inner in chars.by_ref() {
                    if inner == ']' {
                        break;
                    }
                    content.push(inner);
                }
                if content.eq_ignore_ascii_case("h") {
                    out.push_str(&(elapsed_hours.floor() as i64).to_string());
                }
            }
            'y' | 'Y' => {
                let count = 1 + take_while(&mut chars, 'y');
                if count >= 3 {
                    out.push_str(&year.to_string());
                } else {
                    out.push_str(&format!("{:02}", year.rem_euclid(100)));
                }
                after_hour = false;
            }
            'm' | 'M' => {
                let count = 1 + take_while(&mut chars, 'm');
                let next = chars.peek().map(|value| value.to_ascii_lowercase());
                if after_hour || next == Some('s') {
                    out.push_str(&format_time(minute, count));
                } else {
                    let month = (month as usize).clamp(1, 12) - 1;
                    match count {
                        1 => out.push_str(&(month + 1).to_string()),
                        2 => out.push_str(&format!("{:02}", month + 1)),
                        3 => out.push_str(&MONTHS[month][..3]),
                        _ => out.push_str(MONTHS[month]),
                    }
                    after_hour = false;
                }
            }
            'd' | 'D' => {
                let count = 1 + take_while(&mut chars, 'd');
                match count {
                    1 => out.push_str(&day.to_string()),
                    2 => out.push_str(&format!("{day:02}")),
                    3 => out.push_str(&WEEKDAYS[weekday_of(value, date1904)][..3]),
                    _ => out.push_str(WEEKDAYS[weekday_of(value, date1904)]),
                }
                after_hour = false;
            }
            'h' | 'H' => {
                let count = 1 + take_while(&mut chars, 'h');
                let hour = if twelve_hour { hour12 } else { hour };
                if count >= 2 {
                    out.push_str(&format!("{hour:02}"));
                } else {
                    out.push_str(&hour.to_string());
                }
                after_hour = true;
            }
            's' | 'S' => {
                let count = 1 + take_while(&mut chars, 's');
                out.push_str(&format_time(second, count));
                after_hour = false;
            }
            'a' | 'A' => {
                let mut rest = String::new();
                for _ in 0..4 {
                    if let Some(next) = chars.next() {
                        rest.push(next);
                    }
                }
                if rest.eq_ignore_ascii_case("m/pm") {
                    out.push_str(half);
                }
                after_hour = false;
            }
            other => {
                out.push(other);
                after_hour = false;
            }
        }
    }

    out
}

fn take_while(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, character: char) -> usize {
    let mut count = 0;
    while chars.peek().map(|next| next.to_ascii_lowercase()) == Some(character) {
        chars.next();
        count += 1;
    }
    count
}

fn format_time(value: u32, count: usize) -> String {
    if count >= 2 {
        format!("{value:02}")
    } else {
        value.to_string()
    }
}

/// Excel serial date to a calendar date and time.
fn serial_to_date(value: f64, date1904: bool) -> (i64, u32, u32, u32, u32, u32) {
    let days = value.floor() as i64;
    let fraction = value - value.floor();
    let seconds = (fraction * 86_400.0).round() as i64;

    let epoch = if date1904 { 24_107 } else { 25_569 };
    // Excel pretends 1900 was a leap year; serials before the phantom
    // 1900-02-29 are shifted by one day.
    let adjusted = if !date1904 && days < 61 {
        days + 1
    } else {
        days
    };
    let (year, month, day) = civil_from_days(adjusted - epoch);
    let hour = (seconds / 3600).rem_euclid(24) as u32;
    let minute = ((seconds % 3600) / 60) as u32;
    let second = (seconds % 60) as u32;
    (year, month, day, hour, minute, second)
}

fn weekday_of(value: f64, date1904: bool) -> usize {
    let days = value.floor() as i64;
    let epoch_weekday = if date1904 { 5 } else { 6 };
    ((days + epoch_weekday).rem_euclid(7)) as usize
}

/// Days since 1970-01-01 to a calendar date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (y + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(xml: &str) -> Node {
        root_of(parse_tree(xml).expect("well formed xml"))
    }

    /// The theme part of a workbook written by Excel.
    const THEME: &str = r#"<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
        <a:themeElements>
            <a:clrScheme name="Office">
                <a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1>
                <a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1>
                <a:dk2><a:srgbClr val="44546A"/></a:dk2>
                <a:lt2><a:srgbClr val="E7E6E6"/></a:lt2>
                <a:accent1><a:srgbClr val="4472C4"/></a:accent1>
                <a:accent2><a:srgbClr val="ED7D31"/></a:accent2>
                <a:accent3><a:srgbClr val="A5A5A5"/></a:accent3>
                <a:accent4><a:srgbClr val="FFC000"/></a:accent4>
                <a:accent5><a:srgbClr val="5B9BD5"/></a:accent5>
                <a:accent6><a:srgbClr val="70AD47"/></a:accent6>
                <a:hlink><a:srgbClr val="0563C1"/></a:hlink>
                <a:folHlink><a:srgbClr val="954F72"/></a:folHlink>
            </a:clrScheme>
        </a:themeElements>
    </a:theme>"#;

    /// Two cell formats: plain, and one shaded with a theme colour.
    const STYLES: &str = r#"<styleSheet>
        <fonts count="1"><font><sz val="11"/><color theme="1"/></font></fonts>
        <fills count="2">
            <fill><patternFill patternType="none"/></fill>
            <fill><patternFill patternType="solid">
                <fgColor theme="4" tint="-0.249977111117893"/><bgColor indexed="64"/>
            </patternFill></fill>
        </fills>
        <borders count="1"><border><left/><right/><top/><bottom/></border></borders>
        <cellXfs count="3">
            <xf numFmtId="0" fontId="0" fillId="0" borderId="0"/>
            <xf numFmtId="0" fontId="0" fillId="1" borderId="0"/>
            <xf numFmtId="0" fontId="0" fillId="0" borderId="0">
                <alignment wrapText="1"/>
            </xf>
        </cellXfs>
    </styleSheet>"#;

    fn styles() -> Vec<CellStyle> {
        styles_of(Some(&root(STYLES)), &theme_of(Some(THEME.to_string())))
    }

    #[test]
    fn a_theme_colour_resolves_to_its_rgb_value() {
        let theme = theme_of(Some(THEME.to_string()));
        assert_eq!(theme.get(0), Some(0xFFFFFF), "background 1 is white");
        assert_eq!(theme.get(1), Some(0x000000), "text 1 is black");
        assert_eq!(theme.get(4), Some(0x4472C4), "accent 1");
    }

    #[test]
    fn a_shaded_cell_keeps_its_fill() {
        let styles = styles();
        // Blue, accent 1, darker 25%, which is what Excel paints for a header.
        assert_eq!(styles[1].fill, Some(0x2F5597));
    }

    #[test]
    fn a_tint_moves_the_lightness_of_the_colour() {
        assert_eq!(apply_tint(0x4472C4, 0.4), 0x8FAADC);
        assert_eq!(apply_tint(0x4472C4, -0.5), 0x203864);
        assert_eq!(apply_tint(0x4472C4, 0.0), 0x4472C4);
    }

    #[test]
    fn an_indexed_colour_uses_excels_palette() {
        let theme = Theme::fallback();
        let color = root(r#"<fgColor indexed="10"/>"#);
        assert_eq!(resolve_color(&color, &theme), Some(0xFF0000));
    }

    #[test]
    fn a_cell_with_a_fill_but_no_text_is_kept() {
        let xml = r#"<worksheet><sheetData>
            <row r="1"><c r="A1" s="1"/><c r="B1" s="1" t="inlineStr"><is><t>标题</t></is></c></row>
            <row r="2"><c r="A2" s="0" t="inlineStr"><is><t>1</t></is></c></row>
        </sheetData></worksheet>"#;
        let styles = styles();
        let sheet = parse_sheet("表一", xml, &[], &styles, false);

        let empty = sheet.cell(0, 0).expect("the shaded empty cell is kept");
        assert_eq!(empty.text, "", "it still holds no text");
        assert_eq!(empty.style, 1);
        assert_eq!(
            sheet.cell(0, 1).map(|cell| cell.text.as_str()),
            Some("标题")
        );
        // A cell with nothing to show and nothing to paint stays out of the model.
        assert_eq!(sheet.row_count(), 2);
        assert_eq!(sheet.column_count(), 2);
    }

    #[test]
    fn a_gradient_fill_takes_its_first_stop() {
        let fill = root(
            r#"<fill><gradientFill type="linear">
            <stop position="0"><color indexed="6"/></stop>
            <stop position="1"><color indexed="4"/></stop>
        </gradientFill></fill>"#,
        );
        assert_eq!(fill_of(&fill, &Theme::fallback()), Some(0xFF00FF));
    }

    /// OOXML allows `<c>` without an `r` attribute. The fallback used to swap
    /// row and column, silently transposing the whole sheet.
    #[test]
    fn cells_without_a_reference_keep_their_row_and_column() {
        let xml = r#"<worksheet><sheetData>
            <row r="2"><c t="inlineStr"><is><t>甲</t></is></c><c t="inlineStr"><is><t>乙</t></is></c></row>
            <row r="3"><c t="inlineStr"><is><t>丙</t></is></c></row>
        </sheetData></worksheet>"#;
        let sheet = parse_sheet("表一", xml, &[], &styles(), false);

        assert_eq!(cell_reference("B12"), Some((11, 1)));
        assert_eq!(sheet.cell(1, 0).map(|cell| cell.text.as_str()), Some("甲"));
        assert_eq!(sheet.cell(1, 1).map(|cell| cell.text.as_str()), Some("乙"));
        assert_eq!(sheet.cell(2, 0).map(|cell| cell.text.as_str()), Some("丙"));
    }

    /// A few bytes of XML must not be able to make the layout allocate
    /// gigabytes for `column_lefts` or loop billions of times.
    #[test]
    fn a_huge_cell_reference_cannot_inflate_the_sheet() {
        let xml = r#"<worksheet><sheetData>
            <row r="1"><c r="AAAAAAAA1" t="inlineStr"><is><t>x</t></is></c></row>
        </sheetData></worksheet>"#;
        let sheet = parse_sheet("表一", xml, &[], &styles(), false);

        assert!(sheet.row_count() <= MAX_SHEET_ROWS as usize);
        assert!(sheet.column_count() <= MAX_SHEET_COLUMNS as usize);
        assert_eq!(
            sheet
                .cell(0, MAX_SHEET_COLUMNS - 1)
                .map(|cell| cell.text.as_str()),
            Some("x"),
            "越界引用被夹回合法的最后一列"
        );
    }

    // ------------------------------------------------------- fitting the text

    /// A sentence far wider than the column wraps instead of stretching it,
    /// and the row grows to hold every line.
    #[test]
    fn text_wider_than_its_column_wraps_and_grows_the_row() {
        let xml = r#"<worksheet><sheetData>
            <row r="1"><c r="A1" s="0" t="inlineStr"><is><t>一二三四五六七八九十</t></is></c></row>
        </sheetData></worksheet>"#;
        let sheet = parse_sheet("表一", xml, &[], &styles(), false);

        assert!(sheet.cell(0, 0).expect("cell").wrap, "十个汉字要折行");
        assert!(
            sheet.row_tops[1] >= DEFAULT_FONT_SIZE * LINE_HEIGHT_FACTOR * 2.0,
            "折行后行高要留出多行"
        );
        assert!(
            (sheet.column_lefts[1] - DEFAULT_COLUMN_WIDTH * 7.0 - 5.0).abs() < 0.01,
            "这么长的文本不该把列撑宽"
        );
    }

    #[test]
    fn a_short_cell_keeps_excels_single_line() {
        let xml = r#"<worksheet><sheetData>
            <row r="1"><c r="A1" s="0" t="inlineStr"><is><t>甲</t></is></c></row>
        </sheetData></worksheet>"#;
        let sheet = parse_sheet("表一", xml, &[], &styles(), false);

        assert!(!sheet.cell(0, 0).expect("cell").wrap);
        assert!(
            (sheet.row_tops[1] - DEFAULT_ROW_HEIGHT * 4.0 / 3.0).abs() < 0.01,
            "放得下的单行不该被动过"
        );
    }

    /// Excel writes a height for nearly every row. Keeping it as-is is how the
    /// text ended up taller than its cell and got cut in half.
    #[test]
    fn a_wrapped_cell_overrides_a_row_height_that_is_too_short() {
        let xml = r#"<worksheet><sheetData>
            <row r="1" ht="10" customHeight="1"><c r="A1" s="0" t="inlineStr"><is><t>一二三四五六七八</t></is></c></row>
        </sheetData></worksheet>"#;
        let sheet = parse_sheet("表一", xml, &[], &styles(), false);

        // ht=10pt is 13.3px, while eight characters need three lines.
        assert!(sheet.row_tops[1] > 13.4, "内容比固定行高高，行要跟着长");
    }

    /// A line break is a line even when the style does not ask for wrapping.
    #[test]
    fn a_manual_line_break_wraps_and_counts_as_two_lines() {
        let xml = r#"<worksheet><sheetData>
            <row r="1"><c r="A1" s="0" t="inlineStr"><is><t>甲&#10;乙</t></is></c></row>
        </sheetData></worksheet>"#;
        let sheet = parse_sheet("表一", xml, &[], &styles(), false);
        let cell = sheet.cell(0, 0).expect("cell");

        assert_eq!(cell.text, "甲\n乙", "换行符留在了文本里");
        assert!(cell.wrap, "有换行就得按两行画");
        assert!(sheet.row_tops[1] >= DEFAULT_FONT_SIZE * LINE_HEIGHT_FACTOR * 2.0);
    }

    /// A merge widens the cell, so text that would wrap in one column fits on
    /// one line once two columns are joined.
    #[test]
    fn a_merged_cell_wraps_to_its_whole_width() {
        let wrap_of = |merges: &str| {
            let xml = format!(
                r#"<worksheet><sheetData>
                    <row r="1"><c r="A1" s="0" t="inlineStr"><is><t>一二三四五六七八</t></is></c></row>
                </sheetData>{merges}</worksheet>"#
            );
            parse_sheet("表一", &xml, &[], &styles(), false)
                .cell(0, 0)
                .expect("cell")
                .wrap
        };

        assert!(wrap_of(""), "一列装不下八个汉字");
        assert!(
            !wrap_of(r#"<mergeCells><mergeCell ref="A1:B1"/></mergeCells>"#),
            "两列合起来就装得下"
        );
    }

    /// The rows a merge spans may still be shorter than its text; the extra
    /// height goes to the bottom row so the rows above it stay where they were.
    #[test]
    fn a_tall_merged_cell_grows_the_last_row_it_covers() {
        let xml = r#"<worksheet><sheetData>
            <row r="1"><c r="A1" s="0" t="inlineStr"><is><t>一二三四五六七八九十</t></is></c></row>
            <row r="2"/>
        </sheetData><mergeCells><mergeCell ref="A1:A2"/></mergeCells></worksheet>"#;
        let sheet = parse_sheet("表一", xml, &[], &styles(), false);

        assert_eq!(sheet.row_count(), 2);
        let first = sheet.row_tops[1] - sheet.row_tops[0];
        let total = sheet.row_tops[2] - sheet.row_tops[0];
        assert!(
            (first - DEFAULT_ROW_HEIGHT * 4.0 / 3.0).abs() < 0.01,
            "第一行保持原高"
        );
        assert!(total > 2.0 * DEFAULT_ROW_HEIGHT * 4.0 / 3.0, "合并区被撑高");
    }

    /// A phone number has no break point, so a column that is just too narrow
    /// grows instead of splitting the number across two lines.
    #[test]
    fn a_narrow_column_grows_for_a_phone_number() {
        let xml = r#"<worksheet><sheetData>
            <row r="1"><c r="A1" s="0" t="inlineStr"><is><t>18729575026</t></is></c></row>
        </sheetData></worksheet>"#;
        let sheet = parse_sheet("表一", xml, &[], &styles(), false);
        let cell = sheet.cell(0, 0).expect("cell");

        assert!(!cell.wrap, "电话号码不折行");
        let width = sheet.column_lefts[1] - sheet.column_lefts[0];
        assert!(
            width > DEFAULT_COLUMN_WIDTH * 7.0 + 5.0,
            "默认列宽放不下，列要加宽"
        );
        assert!(
            width >= text_width(&cell.text, DEFAULT_FONT_SIZE) + CELL_INSET + CELL_CLEARANCE - 0.01,
            "加宽后还要留出框内空白，数字不会贴着边框"
        );
    }

    /// A short Chinese label that misses its column by a few pixels gets a
    /// wider column too: `是否有异常` split after four characters reads as two
    /// unrelated words.
    #[test]
    fn a_short_chinese_label_keeps_one_line_by_widening_its_column() {
        let xml = r#"<worksheet><sheetData>
            <row r="1"><c r="A1" s="0" t="inlineStr"><is><t>是否有异常</t></is></c></row>
        </sheetData></worksheet>"#;
        let sheet = parse_sheet("表一", xml, &[], &styles(), false);

        assert!(!sheet.cell(0, 0).expect("cell").wrap, "五个字不该折行");
        assert!(
            sheet.column_lefts[1] > DEFAULT_COLUMN_WIDTH * 7.0 + 5.0,
            "列被放宽到装得下"
        );
    }

    /// Growing a column has a limit: a run much wider than the column wraps,
    /// or one long value would stretch the whole sheet sideways.
    #[test]
    fn a_long_run_without_breaks_wraps_instead_of_stretching_the_column() {
        let xml = r#"<worksheet><sheetData>
            <row r="1"><c r="A1" s="0" t="inlineStr"><is><t>ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789</t></is></c></row>
        </sheetData></worksheet>"#;
        let sheet = parse_sheet("表一", xml, &[], &styles(), false);

        assert!(sheet.cell(0, 0).expect("cell").wrap, "过长的串改为折行");
        assert!(
            (sheet.column_lefts[1] - DEFAULT_COLUMN_WIDTH * 7.0 - 5.0).abs() < 0.01,
            "列宽保持原样"
        );
    }

    /// quick-xml hands `&#10;` over as a general reference rather than text, and
    /// the shared-string reader used to drop it — every Alt+Enter line break in
    /// a cell was lost before the cell ever reached the layout.
    #[test]
    fn a_shared_string_keeps_its_line_break_and_entities() {
        let xml = r#"<sst><si><t>甲&#10;乙 &amp; 丙</t></si></sst>"#;
        assert_eq!(shared_strings(xml), vec![String::from("甲\n乙 & 丙")]);
    }

    #[test]
    fn wrapped_lines_counts_hard_breaks_and_overlong_lines() {
        assert_eq!(wrapped_lines("甲乙", 15.0, 100.0), 1);
        assert_eq!(wrapped_lines("甲\n乙\n丙", 15.0, 100.0), 3);
        // Six characters fit in a hundred pixels; the seventh wraps.
        assert_eq!(wrapped_lines("一二三四五六七", 15.0, 100.0), 2);
        assert_eq!(wrapped_lines("", 15.0, 100.0), 1);
    }

    #[test]
    fn a_glyph_wider_than_the_column_gets_its_own_line() {
        assert_eq!(wrapped_lines("甲乙丙", 15.0, 1.0), 3);
    }

    #[test]
    fn a_cjk_glyph_is_a_full_em_and_ascii_is_narrower() {
        assert!((text_width("中", 10.0) - 10.0).abs() < 0.01);
        assert!((text_width("a", 10.0) - 6.0).abs() < 0.01);
    }

    /// GDI rounds the font up to whole pixels, so 11 pt cells are drawn with a
    /// 15 px font. The estimate has to do the same or the drawn text is wider
    /// than the column it was measured against.
    #[test]
    fn the_estimate_uses_the_whole_pixel_font_height() {
        let eleven_pt = DEFAULT_FONT_SIZE; // 14.67 px
        assert!((text_width("中", eleven_pt) - 15.0).abs() < 0.01);
        assert!((text_width("18729575026", eleven_pt) - 99.0).abs() < 0.01);
    }
}
