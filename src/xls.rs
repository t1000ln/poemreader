//! Reading `.xls` (BIFF8) workbooks into the same grid the reader draws.
//!
//! The binary format is a stream of records grouped into a "globals" part
//! (fonts, styles, shared strings, sheet list) and one substream per sheet.
//! Only what the reader can draw is understood: cell values with their number
//! format, fonts, fills, borders, alignment, merges and row/column sizes.

use std::collections::HashMap;

use crate::cfb::CompoundFile;
use crate::excel::{
    self, CellStyle, HorizontalAlign, LegacyCell, LegacySheet, Merge, VerticalAlign, Workbook,
};

// Record identifiers.
const BOF: u16 = 0x0809;
const EOF: u16 = 0x000A;
const FILEPASS: u16 = 0x002F;
const CODEPAGE: u16 = 0x0042;
const DATE1904: u16 = 0x0022;
const FONT: u16 = 0x0031;
const FORMAT: u16 = 0x041E;
const XF: u16 = 0x00E0;
const BOUNDSHEET: u16 = 0x0085;
const SST: u16 = 0x00FC;
const PALETTE: u16 = 0x0092;
const CONTINUE: u16 = 0x003C;
const XFEXT: u16 = 0x087D;

const DIMENSIONS: u16 = 0x0200;
const ROW: u16 = 0x0208;
const COLINFO: u16 = 0x007D;
const ROW_HEIGHT_RECORD: u16 = 0x0225;
const DEF_COL_WIDTH: u16 = 0x0055;
const MERGED_CELLS: u16 = 0x00E5;

const NUMBER: u16 = 0x0203;
const RK: u16 = 0x027E;
const MUL_RK: u16 = 0x00BD;
const LABEL_SST: u16 = 0x00FD;
const LABEL: u16 = 0x0204;
const RSTRING: u16 = 0x00D6;
const BOOL_ERR: u16 = 0x0205;
const FORMULA: u16 = 0x0006;
const STRING: u16 = 0x0207;

/// Excel's default column width, in characters.
const DEFAULT_COLUMN_WIDTH: f32 = 8.43;
/// Excel's default row height, in points.
const DEFAULT_ROW_HEIGHT: f32 = 15.0;
/// The BIFF8 colour palette for indices 8 to 63, as 0x00RRGGBB.
const DEFAULT_PALETTE: [u32; 56] = [
    0x000000, 0xFFFFFF, 0xFF0000, 0x00FF00, 0x0000FF, 0xFFFF00, 0xFF00FF, 0x00FFFF, 0x800000,
    0x008000, 0x000080, 0x808000, 0x800080, 0x008080, 0xC0C0C0, 0x808080, 0x9999FF, 0x993366,
    0xFFFFCC, 0xCCFFFF, 0x660066, 0xFF8080, 0x0066CC, 0xCCCCFF, 0x000080, 0xFF00FF, 0xFFFF00,
    0x00FFFF, 0x800080, 0x800000, 0x008080, 0x0000FF, 0x00CCFF, 0xCCFFFF, 0xCCFFCC, 0xFFFF99,
    0x99CCFF, 0xFF99CC, 0xCC99FF, 0xFFCC99, 0x3366FF, 0x33CCCC, 0x99CC00, 0xFFCC00, 0xFF9900,
    0xFF6600, 0x666699, 0x969696, 0x003366, 0x339966, 0x003300, 0x333300, 0x993300, 0x993366,
    0x333399, 0x333333,
];

/// Parse an `.xls` file.
pub fn load(data: Vec<u8>) -> Result<Workbook, String> {
    let container = CompoundFile::open(data)?;
    let stream = container
        .stream("Workbook")
        .or_else(|| container.stream("Book"))
        .ok_or_else(|| String::from("这个 Excel 文件缺少 Workbook 流"))?;
    parse(&stream)
}

fn parse(stream: &[u8]) -> Result<Workbook, String> {
    if stream.len() < 4 || read_u16(stream, 0) != BOF {
        return Err(String::from("这不是有效的 Excel 97-2003 文件"));
    }
    let version = read_u16(stream, 4);
    if version < 0x0600 {
        // BIFF5 and older use a different string encoding; reading them as
        // BIFF8 would show broken text, so refuse politely.
        return Err(String::from("只支持 Excel 97（BIFF8）及更新的 .xls 文件"));
    }

    let mut formats: HashMap<u16, String> = HashMap::new();
    let mut fonts: Vec<FontInfo> = Vec::new();
    let mut xfs: Vec<Xf> = Vec::new();
    let mut palette: Vec<u32> = DEFAULT_PALETTE.to_vec();
    let mut extended: HashMap<u16, XfExt> = HashMap::new();
    let mut sst: Vec<String> = Vec::new();
    let mut sheets: Vec<(usize, String)> = Vec::new();
    let mut date1904 = false;

    for record in records(stream) {
        match record.typ {
            FILEPASS => {
                if read_u16(record.data, 0) != 0 {
                    return Err(String::from("加密的 Excel 文件暂不支持"));
                }
            }
            CODEPAGE => {}
            DATE1904 => date1904 = read_u16(record.data, 0) == 1,
            FONT => {
                if let Some(font) = parse_font(&record) {
                    fonts.push(font);
                }
            }
            FORMAT => {
                if let Some((id, code)) = parse_format(&record) {
                    formats.insert(id, code);
                }
            }
            XF => {
                if let Some(xf) = parse_xf(&record) {
                    xfs.push(xf);
                }
            }
            XFEXT => {
                if let Some((ixfe, ext)) = parse_xfext(&record) {
                    extended.insert(ixfe, ext);
                }
            }
            BOUNDSHEET => {
                if let Some((position, name)) = parse_boundsheet(&record) {
                    sheets.push((position, name));
                }
            }
            SST => sst = parse_sst(&record),
            PALETTE => {
                let count = (read_u16(record.data, 0) as usize).min(56);
                for index in 0..count {
                    let Some(bytes) = record.data.get(2 + index * 4..6 + index * 4) else {
                        break;
                    };
                    // Stored as 0x00BBGGRR.
                    let value = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                    palette[index] = rgb(value);
                }
            }
            EOF => break,
            _ => {}
        }
    }

    if sheets.is_empty() {
        return Err(String::from("这个 Excel 文件没有可显示的工作表"));
    }

    let styles = build_styles(&xfs, &fonts, &formats, &palette, &extended);
    let globals = Globals {
        date1904,
        styles: &styles,
        strings: &sst,
    };

    let mut worksheets = Vec::new();
    for (position, name) in sheets {
        if position >= stream.len() {
            continue;
        }
        worksheets.push(parse_sheet(stream, position, &name, &globals));
    }

    if worksheets.is_empty() {
        return Err(String::from("这个 Excel 文件没有可显示的工作表"));
    }

    let sheets = worksheets
        .into_iter()
        .map(|sheet| excel::legacy_sheet(sheet, &styles))
        .collect();

    Ok(Workbook { sheets, styles })
}

struct Globals<'a> {
    date1904: bool,
    styles: &'a [CellStyle],
    strings: &'a [String],
}

// --------------------------------------------------------------------- styles

#[derive(Default, Clone)]
struct FontInfo {
    /// Height in twips.
    height: u16,
    bold: bool,
    italic: bool,
    underline: bool,
    /// Palette index of the text colour.
    color: u16,
}

#[derive(Default, Clone)]
struct Xf {
    font: u16,
    format: u16,
    alignment: u8,
    wrap: bool,
    vertical: u8,
    border: u32,
    fill: u32,
    fill_extra: u16,
}

/// Colours from an `XFEXT` record that override the palette indices stored in
/// the cell format. Excel 2007 and later write these when a document saved as
/// `.xls` uses colours that the BIFF8 palette cannot represent exactly.
#[derive(Default, Clone)]
struct XfExt {
    font: Option<u32>,
    fill: Option<u32>,
    border_left: Option<u32>,
    border_right: Option<u32>,
    border_top: Option<u32>,
    border_bottom: Option<u32>,
}

fn parse_font(record: &Record) -> Option<FontInfo> {
    let data = record.data;
    if data.len() < 15 {
        return None;
    }
    let flags = read_u16(data, 2);
    let weight = read_u16(data, 6);
    Some(FontInfo {
        height: read_u16(data, 0),
        bold: flags & 0x0001 != 0 || weight >= 700,
        italic: flags & 0x0002 != 0,
        underline: data[10] != 0,
        color: read_u16(data, 4),
    })
}

fn parse_format(record: &Record) -> Option<(u16, String)> {
    let data = record.data;
    if data.len() < 2 {
        return None;
    }
    let id = read_u16(data, 0);
    let code = read_unicode_string(&data[2..])?;
    if code.is_empty() || code == "General" {
        return None;
    }
    Some((id, code))
}

fn parse_xf(record: &Record) -> Option<Xf> {
    let data = record.data;
    if data.len() < 10 {
        return None;
    }
    // XF starts with ifnt, ifmt and one flags word; the CellXF structure that
    // follows begins at byte 6.
    let alignment = read_u32(data, 6);
    let border = read_u32(data, 10);
    let fill = read_u32(data, 14);
    let fill_extra = if data.len() >= 20 {
        read_u16(data, 18)
    } else {
        0
    };
    Some(Xf {
        font: read_u16(data, 0),
        format: read_u16(data, 2),
        alignment: (alignment & 0x7) as u8,
        wrap: alignment & 0x8 != 0,
        vertical: ((alignment >> 4) & 0x7) as u8,
        border,
        fill,
        fill_extra,
    })
}

/// Parse an `XFEXT` record: the cell format index followed by blocks of
/// `{ type: u16, size: u16, payload }`. Colour blocks carry a four-byte flag
/// word and then a `0x00BBGGRR` colour.
fn parse_xfext(record: &Record) -> Option<(u16, XfExt)> {
    let data = record.data;
    if data.len() < 20 {
        return None;
    }
    let ixfe = read_u16(data, 14);
    let count = read_u16(data, 18) as usize;
    let mut ext = XfExt::default();
    let mut position = 20usize;
    for _ in 0..count {
        if position + 4 > data.len() {
            break;
        }
        let block = read_u16(data, position);
        let size = read_u16(data, position + 2) as usize;
        if size < 4 || position + size > data.len() {
            break;
        }
        if size >= 12 {
            let color = rgb(read_u32(data, position + 8));
            match block {
                0x04 => ext.fill = Some(color),
                0x07 => ext.border_top = Some(color),
                0x08 => ext.border_bottom = Some(color),
                0x09 => ext.border_left = Some(color),
                0x0A => ext.border_right = Some(color),
                0x0D => ext.font = Some(color),
                _ => {}
            }
        }
        position += size;
    }
    Some((ixfe, ext))
}

fn build_styles(
    xfs: &[Xf],
    fonts: &[FontInfo],
    formats: &HashMap<u16, String>,
    palette: &[u32],
    extended: &HashMap<u16, XfExt>,
) -> Vec<CellStyle> {
    xfs.iter()
        .enumerate()
        .map(|(index, xf)| {
            let ext = extended.get(&(index as u16));
            let font = fonts.get(xf.font as usize).cloned().unwrap_or_default();
            let code = formats
                .get(&xf.format)
                .cloned()
                .or_else(|| excel::builtin_number_format(xf.format as usize).map(str::to_string))
                .filter(|code| code != "General" && code != "@");

            let pattern = (xf.fill >> 26) & 0x3F;
            // The fill foreground colour is stored in the last word of CellXF.
            let foreground = xf.fill_extra & 0x7F;
            let fill = if pattern != 0 {
                ext.and_then(|ext| ext.fill)
                    .or_else(|| color(foreground, palette))
            } else {
                None
            };

            CellStyle {
                bold: font.bold,
                italic: font.italic,
                underline: font.underline,
                size: (font.height as f32 / 20.0 * 4.0 / 3.0).max(6.0),
                color: ext
                    .and_then(|ext| ext.font)
                    .or_else(|| color(font.color, palette)),
                fill,
                align: match xf.alignment {
                    1 | 4 => Some(HorizontalAlign::Left),
                    2 => Some(HorizontalAlign::Center),
                    3 => Some(HorizontalAlign::Right),
                    6 => Some(HorizontalAlign::Center),
                    _ => None,
                },
                valign: match xf.vertical {
                    0 => VerticalAlign::Top,
                    1 => VerticalAlign::Center,
                    _ => VerticalAlign::Bottom,
                },
                wrap: xf.wrap,
                border_left: border((xf.border & 0xF) as u8, ext.and_then(|ext| ext.border_left)),
                border_right: border(
                    ((xf.border >> 4) & 0xF) as u8,
                    ext.and_then(|ext| ext.border_right),
                ),
                border_top: border(
                    ((xf.border >> 8) & 0xF) as u8,
                    ext.and_then(|ext| ext.border_top),
                ),
                border_bottom: border(
                    ((xf.border >> 12) & 0xF) as u8,
                    ext.and_then(|ext| ext.border_bottom),
                ),
                num_fmt: code,
            }
        })
        .collect()
}

fn color(index: u16, palette: &[u32]) -> Option<u32> {
    match index {
        // Automatic, system foreground and system background.
        0 | 0x40 | 0x41 | 0x7FFF | 0x8000 => None,
        // Palette indices 8..63 map to the palette entries recorded in the file.
        0x08..=0x3F => palette.get((index - 0x08) as usize).copied(),
        _ => None,
    }
}

fn border(style: u8, color: Option<u32>) -> Option<u32> {
    if style == 0 {
        None
    } else {
        Some(color.unwrap_or(0x000000))
    }
}

fn rgb(value: u32) -> u32 {
    // 0x00BBGGRR to 0x00RRGGBB.
    ((value & 0xFF) << 16) | (value & 0xFF00) | ((value >> 16) & 0xFF)
}

// -------------------------------------------------------------------- sheets

fn parse_boundsheet(record: &Record) -> Option<(usize, String)> {
    let data = record.data;
    if data.len() < 8 {
        return None;
    }
    let position = read_u32(data, 0) as usize;
    let kind = data[5];
    if kind != 0 {
        // Chart, macro and VBA sheets have no cells to draw.
        return None;
    }
    let name = read_short_string(&data[6..])?;
    Some((position, name.trim_end_matches('\0').to_string()))
}

fn parse_sheet(stream: &[u8], start: usize, name: &str, globals: &Globals) -> LegacySheet {
    let mut cells: Vec<LegacyCell> = Vec::new();
    let mut merges: Vec<Merge> = Vec::new();
    let mut column_widths: HashMap<u32, f32> = HashMap::new();
    let mut row_heights: HashMap<u32, f32> = HashMap::new();
    let mut default_column_width = DEFAULT_COLUMN_WIDTH * 7.0 + 5.0;
    let mut default_row_height = DEFAULT_ROW_HEIGHT * 4.0 / 3.0;
    let mut pending_string: Option<(u32, u32, usize)> = None;
    let mut depth = 0usize;

    for record in records(&stream[start..]) {
        match record.typ {
            BOF => {
                depth += 1;
                if depth > 1 {
                    // Embedded chart macro sheet; its cells are not ours.
                    continue;
                }
            }
            EOF => {
                if depth <= 1 {
                    break;
                }
                depth -= 1;
            }
            _ if depth > 1 => {}
            DIMENSIONS => {}
            ROW_HEIGHT_RECORD => {
                let height = read_u16(record.data, 2);
                if height > 0 {
                    default_row_height = (height as f32 / 20.0 * 4.0 / 3.0).max(4.0);
                }
            }
            DEF_COL_WIDTH => {
                let width = read_u16(record.data, 0);
                if width > 0 {
                    default_column_width = (width as f32 * 7.0 + 5.0).max(8.0);
                }
            }
            COLINFO => {
                let data = record.data;
                if data.len() < 8 {
                    continue;
                }
                let first = read_u16(data, 0) as u32;
                let last = read_u16(data, 2) as u32;
                let width = read_u16(data, 4) as f32 / 256.0;
                let pixels = (width * 7.0 + 5.0).max(8.0);
                for column in first..=last.min(first.saturating_add(1024)) {
                    column_widths.insert(column, pixels);
                }
            }
            ROW => {
                let data = record.data;
                if data.len() < 8 {
                    continue;
                }
                let row = read_u16(data, 0) as u32;
                let height = read_u16(data, 6);
                if height > 0 && height < 0x7FFF {
                    row_heights.insert(row, (height as f32 / 20.0 * 4.0 / 3.0).max(4.0));
                }
            }
            MERGED_CELLS => {
                let count = read_u16(record.data, 0) as usize;
                for index in 0..count {
                    let offset = 2 + index * 8;
                    let Some(entry) = record.data.get(offset..offset + 8) else {
                        break;
                    };
                    let first_row = read_u16(entry, 0);
                    let last_row = read_u16(entry, 2);
                    let first_column = read_u16(entry, 4);
                    let last_column = read_u16(entry, 6);
                    if last_row >= first_row && last_column >= first_column {
                        merges.push(Merge {
                            row: first_row as u32,
                            column: first_column as u32,
                            rows: (last_row - first_row) as u32 + 1,
                            columns: (last_column - first_column) as u32 + 1,
                        });
                    }
                }
            }
            NUMBER => {
                let data = record.data;
                if data.len() >= 14 {
                    let value = read_f64(data, 6);
                    push_number(&mut cells, data, value, globals);
                }
            }
            RK => {
                let data = record.data;
                if data.len() >= 10 {
                    let value = rk_value(&data[6..10]);
                    push_number(&mut cells, data, value, globals);
                }
            }
            MUL_RK => {
                let data = record.data;
                if data.len() >= 6 {
                    let row = read_u16(data, 0) as u32;
                    let first = read_u16(data, 2) as u32;
                    let last = read_u16(data, data.len() - 2) as u32;
                    let count = (last.saturating_sub(first) + 1) as usize;
                    for index in 0..count {
                        let offset = 4 + index * 6;
                        let Some(entry) = data.get(offset..offset + 6) else {
                            break;
                        };
                        let xf = read_u16(entry, 0) as usize;
                        let value = rk_value(&entry[2..6]);
                        cells.push(number_cell(row, first + index as u32, xf, value, globals));
                    }
                }
            }
            LABEL_SST => {
                let data = record.data;
                if data.len() >= 10 {
                    let index = read_u32(data, 6) as usize;
                    let text = globals.strings.get(index).cloned().unwrap_or_default();
                    cells.push(LegacyCell {
                        row: read_u16(data, 0) as u32,
                        column: read_u16(data, 2) as u32,
                        text,
                        style: (read_u16(data, 4) as usize)
                            .min(globals.styles.len().saturating_sub(1)),
                        numeric: false,
                    });
                }
            }
            LABEL | RSTRING => {
                let data = record.data;
                if let Some(text) = read_unicode_string(data.get(6..).unwrap_or_default()) {
                    cells.push(LegacyCell {
                        row: read_u16(data, 0) as u32,
                        column: read_u16(data, 2) as u32,
                        text,
                        style: (read_u16(data, 4) as usize)
                            .min(globals.styles.len().saturating_sub(1)),
                        numeric: false,
                    });
                }
            }
            BOOL_ERR => {
                let data = record.data;
                if data.len() >= 8 {
                    let flag = data[7];
                    let value = data[6];
                    let text = if flag == 0 {
                        if value == 0 {
                            String::from("FALSE")
                        } else {
                            String::from("TRUE")
                        }
                    } else {
                        error_text(value).to_string()
                    };
                    cells.push(LegacyCell {
                        row: read_u16(data, 0) as u32,
                        column: read_u16(data, 2) as u32,
                        text,
                        style: (read_u16(data, 4) as usize)
                            .min(globals.styles.len().saturating_sub(1)),
                        numeric: false,
                    });
                }
            }
            FORMULA => {
                let data = record.data;
                if data.len() < 14 {
                    continue;
                }
                let row = read_u16(data, 0) as u32;
                let column = read_u16(data, 2) as u32;
                let xf = (read_u16(data, 4) as usize).min(globals.styles.len().saturating_sub(1));
                let result = &data[6..14];
                match result {
                    // A string result arrives in the next STRING record.
                    [0x00, ..] => pending_string = Some((row, column, xf)),
                    [0x01, _, flag, ..] => cells.push(LegacyCell {
                        row,
                        column,
                        text: if *flag == 0 {
                            String::from("FALSE")
                        } else {
                            String::from("TRUE")
                        },
                        style: xf,
                        numeric: false,
                    }),
                    [0x02, _, flag, ..] => cells.push(LegacyCell {
                        row,
                        column,
                        text: error_text(*flag).to_string(),
                        style: xf,
                        numeric: false,
                    }),
                    [0x03, _, ..] => cells.push(LegacyCell {
                        row,
                        column,
                        text: String::new(),
                        style: xf,
                        numeric: false,
                    }),
                    _ => {
                        let value = read_f64(result, 0);
                        cells.push(number_cell(row, column, xf, value, globals));
                    }
                }
            }
            STRING => {
                if let Some((row, column, xf)) = pending_string.take()
                    && let Some(text) = read_unicode_string(record.data)
                {
                    cells.push(LegacyCell {
                        row,
                        column,
                        text,
                        style: xf,
                        numeric: false,
                    });
                }
            }
            _ => {}
        }
    }

    LegacySheet {
        name: name.to_string(),
        cells,
        merges,
        column_widths,
        default_column_width,
        row_heights,
        default_row_height,
    }
}

fn push_number(cells: &mut Vec<LegacyCell>, data: &[u8], value: f64, globals: &Globals) {
    let row = read_u16(data, 0) as u32;
    let column = read_u16(data, 2) as u32;
    let xf = (read_u16(data, 4) as usize).min(globals.styles.len().saturating_sub(1));
    cells.push(number_cell(row, column, xf, value, globals));
}

fn number_cell(row: u32, column: u32, style: usize, value: f64, globals: &Globals) -> LegacyCell {
    let code = globals
        .styles
        .get(style)
        .and_then(|style| style.num_fmt.as_deref());
    LegacyCell {
        row,
        column,
        text: excel::formatted_number(value, code, globals.date1904),
        style,
        numeric: true,
    }
}

fn rk_value(bytes: &[u8]) -> f64 {
    let rk = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let divided = rk & 1 != 0;
    let integer = rk & 2 != 0;
    let value = if integer {
        ((rk as i32) >> 2) as f64
    } else {
        let mut raw = [0u8; 8];
        raw[4..].copy_from_slice(&rk.to_le_bytes());
        raw[4] &= 0xFC;
        f64::from_le_bytes(raw)
    };
    if divided { value / 100.0 } else { value }
}

fn error_text(code: u8) -> &'static str {
    match code {
        0x00 => "#NULL!",
        0x07 => "#DIV/0!",
        0x0F => "#VALUE!",
        0x17 => "#REF!",
        0x1D => "#NAME?",
        0x24 => "#NUM!",
        0x2A => "#N/A",
        _ => "#ERR!",
    }
}

// ------------------------------------------------------------------- strings

fn read_short_string(data: &[u8]) -> Option<String> {
    let length = *data.first()? as usize;
    let flags = *data.get(1)?;
    read_chars(&data[2..], length, flags & 0x1 != 0)
}

fn read_unicode_string(data: &[u8]) -> Option<String> {
    if data.len() < 3 {
        return None;
    }
    let length = read_u16(data, 0) as usize;
    let flags = data[2];
    read_chars(&data[3..], length, flags & 0x1 != 0)
}

fn read_chars(data: &[u8], length: usize, wide: bool) -> Option<String> {
    if wide {
        let bytes = data.get(..length * 2)?;
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        Some(String::from_utf16_lossy(&units))
    } else {
        let bytes = data.get(..length)?;
        Some(bytes.iter().map(|byte| char::from(*byte)).collect())
    }
}

fn parse_sst(record: &Record) -> Vec<String> {
    let mut chunks: Vec<&[u8]> = Vec::with_capacity(1 + record.continuations.len());
    chunks.push(record.data);
    chunks.extend(record.continuations.iter().copied());
    let mut cursor = Cursor::new(chunks);

    // cstTotal and cstUnique.
    if cursor.u32().is_none() || cursor.u32().is_none() {
        return Vec::new();
    }

    let mut strings = Vec::new();
    while cursor.has_data() {
        let Some(length) = cursor.u16() else {
            break;
        };
        let Some(flags) = cursor.u8() else {
            break;
        };
        let mut runs = 0usize;
        let mut extended = 0usize;
        if flags & 0x08 != 0 {
            runs = cursor.u16().unwrap_or(0) as usize;
        }
        if flags & 0x04 != 0 {
            extended = cursor.u32().unwrap_or(0) as usize;
        }
        strings.push(cursor.chars(length as usize, flags & 0x01 != 0));
        cursor.skip(runs * 4);
        cursor.skip(extended);
    }
    strings
}

/// A reader over a record plus its continuation records.
struct Cursor<'a> {
    chunks: Vec<&'a [u8]>,
    current: usize,
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(mut chunks: Vec<&'a [u8]>) -> Self {
        chunks.reverse();
        Self {
            chunks,
            current: 0,
            offset: 0,
        }
    }

    fn chunk(&self) -> Option<&'a [u8]> {
        self.chunks.get(self.current).copied()
    }

    fn has_data(&self) -> bool {
        self.chunk().is_some()
    }

    fn take(&mut self, length: usize) -> Option<&'a [u8]> {
        loop {
            let bytes = self.chunk()?;
            if self.offset < bytes.len() {
                break;
            }
            self.current += 1;
            self.offset = 0;
        }
        let bytes = self.chunk()?;
        let end = (self.offset + length).min(bytes.len());
        let slice = &bytes[self.offset..end];
        self.offset = end;
        Some(slice)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).and_then(|bytes| bytes.first().copied())
    }

    fn u16(&mut self) -> Option<u16> {
        let bytes = self.take(2)?;
        Some(u16::from_le_bytes([bytes[0], *bytes.get(1)?]))
    }

    fn u32(&mut self) -> Option<u32> {
        let bytes = self.take(4)?;
        Some(u32::from_le_bytes([
            bytes[0],
            *bytes.get(1)?,
            *bytes.get(2)?,
            *bytes.get(3)?,
        ]))
    }

    fn skip(&mut self, mut length: usize) {
        while length > 0 {
            let Some(bytes) = self.chunk() else {
                return;
            };
            let available = bytes.len().saturating_sub(self.offset);
            if available == 0 {
                self.current += 1;
                self.offset = 0;
                continue;
            }
            let step = available.min(length);
            self.offset += step;
            length -= step;
        }
    }

    /// Reads `count` characters, honouring the flag byte at each continuation.
    fn chars(&mut self, mut count: usize, mut wide: bool) -> String {
        let mut text = String::new();
        while count > 0 {
            let Some(bytes) = self.chunk() else {
                break;
            };
            if self.offset >= bytes.len() {
                // Continuation records start with their own flag byte.
                self.current += 1;
                self.offset = 0;
                match self.u8() {
                    Some(flags) => wide = flags & 0x01 != 0,
                    None => break,
                }
                continue;
            }
            let width = if wide { 2 } else { 1 };
            let available = (bytes.len() - self.offset) / width;
            if available == 0 {
                self.offset = bytes.len();
                continue;
            }
            let take = available.min(count);
            let end = self.offset + take * width;
            let slice = &bytes[self.offset..end];
            if wide {
                let units: Vec<u16> = slice
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect();
                text.push_str(&String::from_utf16_lossy(&units));
            } else {
                text.extend(slice.iter().map(|byte| char::from(*byte)));
            }
            self.offset = end;
            count -= take;
        }
        text
    }
}

// ------------------------------------------------------------------- records

struct Record<'a> {
    typ: u16,
    data: &'a [u8],
    continuations: Vec<&'a [u8]>,
}

fn records(stream: &[u8]) -> Records<'_> {
    Records {
        stream,
        position: 0,
    }
}

struct Records<'a> {
    stream: &'a [u8],
    position: usize,
}

impl<'a> Iterator for Records<'a> {
    type Item = Record<'a>;

    fn next(&mut self) -> Option<Record<'a>> {
        let header = self.stream.get(self.position..self.position + 4)?;
        let typ = u16::from_le_bytes([header[0], header[1]]);
        let length = u16::from_le_bytes([header[2], header[3]]) as usize;
        let start = self.position + 4;
        let data = self.stream.get(start..start + length)?;
        self.position = start + length;

        let mut continuations = Vec::new();
        while let Some(next) = self.stream.get(self.position..self.position + 4) {
            if u16::from_le_bytes([next[0], next[1]]) != CONTINUE {
                break;
            }
            let length = u16::from_le_bytes([next[2], next[3]]) as usize;
            let start = self.position + 4;
            let Some(bytes) = self.stream.get(start..start + length) else {
                break;
            };
            continuations.push(bytes);
            self.position = start + length;
        }

        Some(Record {
            typ,
            data,
            continuations,
        })
    }
}

// ------------------------------------------------------------------ primitives

fn read_u16(data: &[u8], offset: usize) -> u16 {
    let bytes = data.get(offset..offset + 2).unwrap_or(&[0, 0]);
    u16::from_le_bytes([bytes[0], bytes[1]])
}

fn read_u32(data: &[u8], offset: usize) -> u32 {
    let bytes = data.get(offset..offset + 4).unwrap_or(&[0, 0, 0, 0]);
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn read_f64(data: &[u8], offset: usize) -> f64 {
    let bytes = data.get(offset..offset + 8).unwrap_or(&[0; 8]);
    f64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xfext_record(ixfe: u16, blocks: &[(u16, u32)]) -> Vec<u8> {
        let mut data = vec![0u8; 20];
        data[0..2].copy_from_slice(&XFEXT.to_le_bytes());
        data[14..16].copy_from_slice(&ixfe.to_le_bytes());
        data[18..20].copy_from_slice(&(blocks.len() as u16).to_le_bytes());
        for (block, color) in blocks {
            let mut entry = Vec::with_capacity(20);
            entry.extend_from_slice(&block.to_le_bytes());
            entry.extend_from_slice(&20u16.to_le_bytes());
            entry.extend_from_slice(&2u32.to_le_bytes());
            // Stored as 0x00BBGGRR.
            let value = ((color & 0xFF) << 16) | (color & 0xFF00) | ((color >> 16) & 0xFF);
            entry.extend_from_slice(&value.to_le_bytes());
            entry.extend_from_slice(&[0u8; 8]);
            data.extend_from_slice(&entry);
        }
        data
    }

    #[test]
    fn parses_extended_colours() {
        let data = xfext_record(64, &[(0x04, 0x2F5496), (0x0D, 0xFFFFFF), (0x07, 0xBFBFBF)]);
        let record = Record {
            typ: XFEXT,
            data: &data,
            continuations: Vec::new(),
        };
        let (ixfe, ext) = parse_xfext(&record).unwrap();
        assert_eq!(ixfe, 64);
        assert_eq!(ext.fill, Some(0x2F5496));
        assert_eq!(ext.font, Some(0xFFFFFF));
        assert_eq!(ext.border_top, Some(0xBFBFBF));
        assert_eq!(ext.border_bottom, None);
    }
}
