//! Reading Word 97-2003 (`.doc`) documents into the reader's block model.
//!
//! The binary format stores the text in a "piece table" and the formatting in
//! character and paragraph property runs. This reader walks the piece table to
//! recover the text, then applies the direct formatting found in the CHPX and
//! PAPX runs. Paragraph styles are consulted only for alignment and list
//! membership; images, headers and footnotes are not interpreted.

use crate::cfb::CompoundFile;
use crate::word::{
    Align, Block, Paragraph, ParagraphBorder, Run, Table, TableCell, WordDocument, format_counter,
};
use std::collections::HashMap;

/// Text column width of a Word 97 page with 1.25-inch margins, in points.
const CONTENT_WIDTH: f32 = 6.0 * 72.0;
/// Word's default body size, in points.
const DEFAULT_SIZE: f32 = 10.5;
const TWIPS_PER_POINT: f32 = 20.0;

/// Parse a `.doc` file.
pub fn load(data: Vec<u8>) -> Result<WordDocument, String> {
    let container = CompoundFile::open(data)?;
    let word_stream = container
        .stream("WordDocument")
        .ok_or_else(|| String::from("这个 Word 文件缺少 WordDocument 流"))?;
    if word_stream.len() < 0x200 || u16::from_le_bytes([word_stream[0], word_stream[1]]) != 0xA5EC {
        return Err(String::from("这不是有效的 Word 97-2003 文件"));
    }
    if read_u16(&word_stream, 2) < 0x00C1 {
        return Err(String::from("只支持 Word 97 及更新的 .doc 文件"));
    }
    let flags = read_u16(&word_stream, 0x0A);
    if flags & 0x0100 != 0 {
        return Err(String::from("加密的 Word 文档暂不支持"));
    }
    let table_name = if flags & 0x0200 != 0 {
        "1Table"
    } else {
        "0Table"
    };
    let table_stream = container
        .stream(table_name)
        .ok_or_else(|| String::from("这个 Word 文件缺少表格流"))?;

    let ccp_text = read_u32(&word_stream, 0x4C);
    let fc_clx = read_u32(&word_stream, 0x01A2) as usize;
    let lcb_clx = read_u32(&word_stream, 0x01A6) as usize;
    if lcb_clx == 0 {
        return Err(String::from("这个 Word 文件没有正文"));
    }
    let clx = table_stream
        .get(fc_clx..fc_clx.saturating_add(lcb_clx))
        .ok_or_else(|| String::from("Word 文档的文本索引损坏"))?;
    let pieces = piece_table(clx)?;
    if pieces.is_empty() {
        return Err(String::from("这个 Word 文件没有正文"));
    }

    let chpx = char_runs(&word_stream, &table_stream);
    let papx = para_runs(&word_stream, &table_stream);
    let fonts = fonts(&word_stream, &table_stream);
    let styles = style_props(&word_stream, &table_stream);
    let lists = Lists::parse(&word_stream, &table_stream);

    let mut raws = read_paragraphs(&word_stream, &pieces, &chpx, &papx, &fonts, ccp_text);
    apply_style_props(&mut raws, &styles);
    apply_markers(&mut raws, &lists);
    let blocks = build_blocks(raws);
    if blocks.is_empty() {
        return Err(String::from("这份 Word 文档没有可显示的内容"));
    }
    let paragraphs = count_paragraphs(&blocks);

    Ok(WordDocument {
        blocks,
        content_width: CONTENT_WIDTH,
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

// --------------------------------------------------------------- piece table

struct Piece {
    cp_start: u32,
    cp_end: u32,
    fc: u32,
    compressed: bool,
}

fn piece_table(clx: &[u8]) -> Result<Vec<Piece>, String> {
    let mut position = 0usize;
    // Property modifiers come first; only the last part (the piece table) is
    // needed to read the text.
    while position < clx.len() {
        match clx[position] {
            0x01 => {
                if position + 3 > clx.len() {
                    break;
                }
                let length = read_u16(clx, position + 1) as usize;
                position += 3 + length;
            }
            0x02 => {
                position += 1;
                break;
            }
            _ => return Err(String::from("Word 文档的文本索引无法识别")),
        }
    }
    if position + 4 > clx.len() {
        return Err(String::from("Word 文档的文本索引不完整"));
    }
    let length = read_u32(clx, position) as usize;
    let data = clx
        .get(position + 4..position + 4 + length)
        .ok_or_else(|| String::from("Word 文档的文本索引不完整"))?;
    let count = data.len().saturating_sub(4) / 12;
    if count == 0 {
        return Ok(Vec::new());
    }

    let mut pieces = Vec::with_capacity(count);
    for index in 0..count {
        let cp_start = read_u32(data, index * 4);
        let cp_end = read_u32(data, (index + 1) * 4);
        let pcd = 4 * (count + 1) + index * 8;
        let Some(entry) = data.get(pcd..pcd + 8) else {
            break;
        };
        let fc = read_u32(entry, 2);
        let compressed = fc & 0x4000_0000 != 0;
        pieces.push(Piece {
            cp_start,
            cp_end,
            fc: fc & 0x3FFF_FFFF,
            compressed,
        });
    }
    Ok(pieces)
}

// ------------------------------------------------------------- property runs

struct CharRun {
    start: u32,
    end: u32,
    props: CharProps,
}

struct ParaRun {
    start: u32,
    end: u32,
    props: ParaProps,
}

/// The FC bin table's list of FKP page numbers.
///
/// Word 97 stores one 4-byte FC per entry plus a terminator and 4-byte page
/// numbers, so a table with `n` entries is `8 * n + 4` bytes long.
fn bin_table(word: &[u8], table: &[u8], fc: usize, lcb: usize) -> Vec<u32> {
    let fc = read_u32(word, fc) as usize;
    let lcb = read_u32(word, lcb) as usize;
    let Some(data) = table.get(fc..fc.saturating_add(lcb)) else {
        return Vec::new();
    };
    let count = data.len().saturating_sub(4) / 8;
    let pages_start = 4 * (count + 1);
    (0..count)
        .filter_map(|index| {
            let offset = pages_start + index * 4;
            data.get(offset..offset + 4)
                .map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        })
        .filter(|page| *page != 0)
        .collect()
}

fn char_runs(word: &[u8], table: &[u8]) -> Vec<CharRun> {
    let mut runs = Vec::new();
    for page_number in bin_table(word, table, 0x00FA, 0x00FE) {
        let Some(page) = fkp(word, page_number) else {
            continue;
        };
        let count = page[511] as usize;
        if count == 0 || count > 101 {
            continue;
        }
        let offsets = 4 * (count + 1);
        for index in 0..count {
            let start = read_u32(page, index * 4);
            let end = read_u32(page, index * 4 + 4);
            let Some(&offset_byte) = page.get(offsets + index) else {
                break;
            };
            let props = if offset_byte == 0 {
                CharProps::default()
            } else {
                char_props(&page[offset_byte as usize * 2..])
            };
            runs.push(CharRun { start, end, props });
        }
    }
    runs
}

fn para_runs(word: &[u8], table: &[u8]) -> Vec<ParaRun> {
    let mut runs = Vec::new();
    for page_number in bin_table(word, table, 0x0102, 0x0106) {
        let Some(page) = fkp(word, page_number) else {
            continue;
        };
        let count = page[511] as usize;
        if count == 0 || count > 29 {
            continue;
        }
        let entries = 4 * (count + 1);
        for index in 0..count {
            let start = read_u32(page, index * 4);
            let end = read_u32(page, index * 4 + 4);
            let Some(&offset_byte) = page.get(entries + index * 13) else {
                break;
            };
            let props = if offset_byte == 0 {
                ParaProps::default()
            } else {
                // The PAPX area grows downwards, so the entry before this one
                // in the FKP marks where this one ends. The byte before the
                // trailing run count is the last usable byte of the page.
                let region_end = if index == 0 {
                    page.len() - 2
                } else {
                    page.get(entries + (index - 1) * 13).copied().unwrap_or(0) as usize * 2
                };
                para_props(page, offset_byte as usize * 2, region_end)
            };
            runs.push(ParaRun { start, end, props });
        }
    }
    runs
}

fn fkp(word: &[u8], page_number: u32) -> Option<&[u8]> {
    let start = page_number as usize * 512;
    word.get(start..start + 512)
}

// ------------------------------------------------------------- SPRM parsing

#[derive(Default, Clone, PartialEq)]
struct CharProps {
    bold: Option<bool>,
    italic: Option<bool>,
    strike: bool,
    underline: bool,
    size_half_points: Option<u16>,
    color: Option<u8>,
    highlight: Option<u8>,
    font: Option<u16>,
}

fn char_props(data: &[u8]) -> CharProps {
    let Some(&length) = data.first() else {
        return CharProps::default();
    };
    let grpprl = &data[1..1 + (length as usize).min(data.len() - 1)];
    let mut props = CharProps::default();
    let mut position = 0usize;
    while position + 2 <= grpprl.len() {
        let sprm = read_u16(grpprl, position);
        position += 2;
        let Some(size) = sprm_operand_size(sprm, &grpprl[position..]) else {
            break;
        };
        let Some(operand) = grpprl.get(position..position + size) else {
            break;
        };
        position += size;
        match sprm {
            0x0835 => props.bold = Some(operand[0] & 1 != 0),
            0x0836 => props.italic = Some(operand[0] & 1 != 0),
            0x0837 => props.strike = operand[0] & 1 != 0,
            0x2A3E => props.underline = operand[0] != 0,
            0x2A42 => props.color = Some(operand[0]),
            0x2A0C => props.highlight = Some(operand[0]),
            0x4A43 => props.size_half_points = Some(read_u16(operand, 0)),
            0x4A4F => props.font = Some(read_u16(operand, 0)),
            // Far East font; used for Chinese runs.
            0x4A50 => props.font = props.font.or_else(|| Some(read_u16(operand, 0))),
            _ => {}
        }
    }
    props
}

#[derive(Default, Clone)]
struct ParaProps {
    /// Style index stored at the start of the PAPX body.
    style: Option<u16>,
    in_table: bool,
    row_end: bool,
    align: Option<u8>,
    indent: Option<i16>,
    first_line: Option<i16>,
    space_before: Option<u16>,
    space_after: Option<u16>,
    line_spacing: Option<u16>,
    /// List instance the paragraph belongs to; `ilfo` is one-based and zero
    /// means "no list".
    ilfo: Option<u16>,
    /// Level of the list, zero-based.
    ilvl: Option<u8>,
}

fn para_props(page: &[u8], start: usize, region_end: usize) -> ParaProps {
    let region = page.get(start..region_end).unwrap_or_default();
    if region.is_empty() {
        return ParaProps::default();
    }

    // `cb` counts 2-byte units. Entries are padded to an even size, so a PAPX
    // whose body has an even length starts one byte into the region; compare
    // the candidates against the region length to find the real one.
    let mut pad = 0usize;
    let mut units = region[0] as usize;
    if region.len() != 2 * units && region.len() != 2 * units + 2 {
        let candidate = region.get(1).copied().unwrap_or(0) as usize;
        if region.len() == 2 * candidate || region.len() == 2 * candidate + 2 {
            pad = 1;
            units = candidate;
        }
    }

    let length = if pad == 1 {
        2 * units
    } else {
        (2 * units).saturating_sub(1)
    };
    let data = region.get(pad + 1..pad + 1 + length).unwrap_or_default();
    // The body starts with the paragraph's style index.
    let style = (data.len() >= 2).then(|| read_u16(data, 0));
    let grpprl = data.get(2..).unwrap_or_default();

    let mut props = ParaProps {
        style,
        ..ParaProps::default()
    };
    let mut position = 0usize;
    while position + 2 <= grpprl.len() {
        let sprm = read_u16(grpprl, position);
        position += 2;
        let Some(size) = sprm_operand_size(sprm, &grpprl[position..]) else {
            break;
        };
        let Some(operand) = grpprl.get(position..position + size) else {
            break;
        };
        position += size;
        match sprm {
            0x2416 => props.in_table = operand[0] & 1 != 0,
            0x2417 => props.row_end = operand[0] & 1 != 0,
            0x2403 | 0x2461 => props.align = Some(operand[0]),
            0x840F => props.indent = Some(read_u16(operand, 0) as i16),
            0x8411 => props.first_line = Some(read_u16(operand, 0) as i16),
            0xA413 => props.space_before = Some(read_u16(operand, 0)),
            0xA414 => props.space_after = Some(read_u16(operand, 0)),
            0x6412 => {
                let mode = read_u16(operand, 0) as i16;
                if mode == 0 {
                    props.line_spacing = Some(read_u16(operand, 2));
                }
            }
            0x460B => props.ilfo = Some(read_u16(operand, 0)),
            0x260A => props.ilvl = Some(operand[0]),
            _ => {}
        }
    }
    props
}

/// Paragraph properties declared by a style's UPX, reduced to what the reader
/// uses; other style properties stay untouched.
#[derive(Clone, Copy, Default)]
struct StyleProps {
    align: Option<u8>,
    ilfo: Option<u16>,
    ilvl: Option<u8>,
}

/// The paragraph properties each style declares, keyed by style index.
fn style_props(word: &[u8], table: &[u8]) -> HashMap<u16, StyleProps> {
    let mut styles = HashMap::new();
    let fc = read_u32(word, 0x00A2) as usize;
    let lcb = read_u32(word, 0x00A6) as usize;
    let Some(stsh) = table.get(fc..fc.saturating_add(lcb)) else {
        return styles;
    };
    if stsh.len() < 6 {
        return styles;
    }
    let cb_stshi = read_u16(stsh, 0) as usize;
    let cstd = read_u16(stsh, 2) as usize;
    let cb_base = read_u16(stsh, 4) as usize;
    if cb_base < 10 || cb_base > 64 || cstd > 4096 {
        return styles;
    }

    let mut position = 2 + cb_stshi;
    for _ in 0..cstd {
        // WPS inserts padding between some entries, so look a little ahead for
        // the next structure whose name length is plausible.
        let mut found = None;
        for shift in (0..=64).step_by(2) {
            let start = position + shift;
            if start + 2 > stsh.len() {
                break;
            }
            let size = read_u16(stsh, start) as usize;
            let body_start = start + 2;
            if size < cb_base + 4 || body_start + size > stsh.len() {
                continue;
            }
            let body = &stsh[body_start..body_start + size];
            let chars = read_u16(body, cb_base) as usize;
            if chars == 0 || chars > 64 || cb_base + 2 + chars * 2 + 4 > body.len() {
                continue;
            }
            found = Some((start, size, body));
            break;
        }
        let Some((start, size, body)) = found else {
            break;
        };

        let chars = read_u16(body, cb_base) as usize;
        // Skip the name, its null terminator, then read the first UPX, which
        // holds the paragraph properties of the style.
        let name_end = cb_base + 2 + chars * 2 + 2;
        if name_end + 4 <= body.len() {
            let upx_size = read_u16(body, name_end) as usize;
            let upx_end = name_end + 2 + upx_size;
            if upx_size >= 2 && upx_end <= body.len() {
                let upx = &body[name_end + 2..upx_end];
                let style = read_u16(upx, 0);
                styles.insert(style, style_paragraph_sprms(&upx[2..]));
            }
        }
        position = start + 2 + size;
    }
    styles
}

/// Paragraph properties from a paragraph-property grpprl.
fn style_paragraph_sprms(grpprl: &[u8]) -> StyleProps {
    let mut position = 0usize;
    let mut props = StyleProps::default();
    while position + 2 <= grpprl.len() {
        let sprm = read_u16(grpprl, position);
        position += 2;
        let Some(size) = sprm_operand_size(sprm, &grpprl[position..]) else {
            break;
        };
        let Some(operand) = grpprl.get(position..position + size) else {
            break;
        };
        position += size;
        match sprm {
            0x2403 | 0x2461 => props.align = Some(operand[0]),
            0x460B => props.ilfo = Some(read_u16(operand, 0)),
            0x260A => props.ilvl = Some(operand[0]),
            _ => {}
        }
    }
    props
}

/// Paragraphs without direct alignment or list properties inherit them from
/// their style.
fn apply_style_props(raws: &mut [RawParagraph], styles: &HashMap<u16, StyleProps>) {
    for raw in raws {
        let Some(style) = raw.para.style else {
            continue;
        };
        let Some(props) = styles.get(&style) else {
            continue;
        };
        if raw.para.align.is_none()
            && let Some(align) = props.align
        {
            raw.paragraph.align = match align {
                1 => Align::Center,
                2 => Align::Right,
                3 => Align::Justify,
                _ => Align::Left,
            };
        }
        if raw.para.ilfo.is_none() {
            raw.para.ilfo = props.ilfo;
        }
        if raw.para.ilvl.is_none() {
            raw.para.ilvl = props.ilvl;
        }
    }
}

// ------------------------------------------------------------------- lists

/// `nfc` of a bulleted level.
const BULLET: u8 = 23;
/// `nfc` of a level that draws no marker.
const NO_NUMBERING: u8 = 255;

/// One level of a list format (`LVL`), reduced to what a marker needs.
struct Level {
    /// Number the level starts at.
    start: u32,
    /// Number format code (`nfc`).
    format: u8,
    /// Left indent the level adds, in points.
    indent: f32,
}

/// The list formats of the document and the list instances that use them.
#[derive(Default)]
struct Lists {
    /// Level definitions of each list format, keyed by its id.
    formats: HashMap<u32, Vec<Level>>,
    /// List format of each list instance; `ilfo` is a one-based index here.
    instances: Vec<u32>,
}

impl Lists {
    fn parse(word: &[u8], table: &[u8]) -> Self {
        let mut lists = Lists::default();
        let fc = read_u32(word, 0x02E2) as usize;
        // WPS ends the recorded length after the list headers and writes the
        // level definitions right behind them, so the rest of the table is the
        // real bound; the parse stops on its own once every level is read.
        if let Some(data) = table.get(fc..) {
            lists.read_formats(data);
        }
        let fc = read_u32(word, 0x02EA) as usize;
        let lcb = read_u32(word, 0x02EE) as usize;
        if let Some(data) = table.get(fc..fc.saturating_add(lcb)) {
            let count = (read_u32(data, 0) as usize).min(data.len().saturating_sub(4) / 16);
            lists.instances = (0..count)
                .map(|index| read_u32(data, 4 + index * 16))
                .collect();
        }
        lists
    }

    /// Reads `PlfLst`: the list headers, followed by the levels of each list in
    /// the same order.
    fn read_formats(&mut self, data: &[u8]) {
        let count = read_u16(data, 0) as usize;
        if count == 0 || count > 0x1000 {
            return;
        }

        let mut headers = Vec::with_capacity(count);
        let mut position = 2usize;
        for _ in 0..count {
            let Some(entry) = data.get(position..position + 28) else {
                return;
            };
            headers.push((read_u32(entry, 0), entry[26] & 1 != 0));
            position += 28;
        }

        for (id, simple) in headers {
            let mut levels = Vec::new();
            for _ in 0..if simple { 1 } else { 9 } {
                let Some(level) = data.get(position..position + 28) else {
                    break;
                };
                // A length that does not fit or a number format that does not
                // exist means the levels are not where they should be; keep
                // what was read and leave the rest to the fallback marker.
                let size = 28 + level[24] as usize + level[25] as usize;
                if level[4] > 35 && level[4] != NO_NUMBERING {
                    break;
                }
                let Some(overrides) = data.get(position + 28..position + size) else {
                    break;
                };
                levels.push(Level {
                    start: read_u32(level, 0),
                    format: level[4],
                    // Character overrides come first, then the paragraph ones.
                    indent: level_indent(&overrides[level[24] as usize..]),
                });
                position += size;
            }
            if !levels.is_empty() {
                self.formats.insert(id, levels);
            }
        }
    }

    /// The level definition `ilfo` uses for `level`.
    fn level(&self, ilfo: u16, level: usize) -> Option<&Level> {
        let id = self
            .instances
            .get(ilfo as usize - 1)
            .copied()
            .unwrap_or(ilfo as u32);
        self.formats.get(&id)?.get(level)
    }
}

/// Left indent from a level's paragraph overrides.
fn level_indent(grpprl: &[u8]) -> f32 {
    let mut position = 0usize;
    while position + 2 <= grpprl.len() {
        let sprm = read_u16(grpprl, position);
        position += 2;
        let Some(size) = sprm_operand_size(sprm, &grpprl[position..]) else {
            break;
        };
        let Some(operand) = grpprl.get(position..position + size) else {
            break;
        };
        position += size;
        if sprm == 0x840F {
            let twips = read_u16(operand, 0) as i16;
            return (twips as f32 / TWIPS_PER_POINT).max(0.0);
        }
    }
    0.0
}

/// Adds the list markers, walking the paragraphs in document order so the
/// counters follow the document. Word restarts numbering whenever a paragraph
/// changes its list instance, so counters are keyed by `ilfo`.
fn apply_markers(raws: &mut [RawParagraph], lists: &Lists) {
    let mut counters = HashMap::new();
    for raw in raws {
        let Some(ilfo) = raw.para.ilfo else {
            continue;
        };
        if ilfo == 0 {
            continue;
        }
        let index = raw.para.ilvl.unwrap_or(0) as usize;

        let known = lists.level(ilfo, index);
        // WPS writes list headers without their level definitions. When the
        // document does define other lists, an unreadable one is still a list,
        // so a generic bullet is better than nothing; when no list could be
        // read at all, the paragraph is more likely stray data.
        let (format, start) = match known {
            Some(level) => (level.format, level.start),
            None if index == 0 && !lists.formats.is_empty() => (BULLET, 0),
            None => continue,
        };
        if format == NO_NUMBERING {
            continue;
        }
        if let Some(level) = known
            && raw.paragraph.indent <= 0.0
        {
            raw.paragraph.indent = level.indent;
        }

        let counter = counters.entry((ilfo, index)).or_insert(start);
        let value = *counter;
        *counter += 1;
        for deeper in index + 1..9 {
            counters.remove(&(ilfo, deeper));
        }

        raw.paragraph.marker = Some(if format == BULLET {
            String::from("•")
        } else {
            // A level stores no marker text, so numbered lists use the
            // conventional "<number>." form.
            let text = match format {
                1 => format_counter("upperRoman", value),
                2 => format_counter("lowerRoman", value),
                3 => format_counter("upperLetter", value),
                4 => format_counter("lowerLetter", value),
                10 => format_counter("ideographDigital", value),
                34 => format_counter("chineseCounting", value),
                _ => value.to_string(),
            };
            format!("{text}.")
        });
    }
}

/// Length of an SPRM's operand, from the size class in its opcode.
fn sprm_operand_size(sprm: u16, data: &[u8]) -> Option<usize> {
    Some(match sprm >> 13 {
        0 | 1 => 1,
        2 | 4 | 5 => 2,
        3 => 4,
        7 => 3,
        6 => {
            let first = *data.first()? as usize;
            match sprm {
                // A table definition stores its length in the first two bytes.
                0xD608 => 2 + read_u16(data, 1) as usize,
                _ => 1 + first,
            }
        }
        _ => 1,
    })
}

// ------------------------------------------------------------------ fonts

fn fonts(word: &[u8], table: &[u8]) -> Vec<String> {
    let fc = read_u32(word, 0x0112) as usize;
    let lcb = read_u32(word, 0x0116) as usize;
    let Some(data) = table.get(fc..fc.saturating_add(lcb)) else {
        return Vec::new();
    };
    if data.len() < 4 {
        return Vec::new();
    }
    // Word 2000 and later prefix the table with a 0xFFFF marker.
    let mut position = if read_u16(data, 0) == 0xFFFF { 4 } else { 2 };
    let mut names = Vec::new();
    while position + 2 <= data.len() {
        let size = data[position] as usize + 1;
        let Some(entry) = data.get(position + 1..position + size) else {
            break;
        };
        names.push(font_name(entry));
        position += size;
    }
    names
}

fn font_name(entry: &[u8]) -> String {
    // The name follows the fixed part of the FFN as a length-prefixed
    // UTF-16 string.
    if entry.len() < 41 {
        return String::new();
    }
    let length = entry[39] as usize;
    let Some(bytes) = entry.get(40..40 + length * 2) else {
        return String::new();
    };
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .take_while(|unit| *unit != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

// ------------------------------------------------------------- text reading

enum Terminator {
    Paragraph,
    Cell,
}

struct RawParagraph {
    paragraph: Paragraph,
    terminator: Terminator,
    para: ParaProps,
}

fn read_paragraphs(
    word: &[u8],
    pieces: &[Piece],
    chpx: &[CharRun],
    papx: &[ParaRun],
    fonts: &[String],
    ccp_text: u32,
) -> Vec<RawParagraph> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut runs: Vec<Run> = Vec::new();
    let mut current: Option<CharProps> = None;
    let mut size = DEFAULT_SIZE;
    let mut mark_fc = 0u32;
    let mut last_fc = 0u32;
    let mut in_instruction = false;

    for piece in pieces {
        if piece.cp_start >= ccp_text {
            break;
        }
        let count = piece.cp_end.min(ccp_text).saturating_sub(piece.cp_start) as usize;
        let width = if piece.compressed { 1 } else { 2 };
        let mut index = 0usize;
        while index < count {
            let fc = piece.fc + (index * width) as u32;
            let Some((ch, code_units)) = character(word, piece, index) else {
                break;
            };
            index += code_units;

            match ch {
                '\r' | '\u{0C}' => {
                    flush(&mut runs, &mut text, current.take(), fonts);
                    mark_fc = fc;
                    out.push(paragraph(runs, mark_fc, Terminator::Paragraph, papx, size));
                    runs = Vec::new();
                    size = DEFAULT_SIZE;
                    continue;
                }
                '\u{07}' => {
                    flush(&mut runs, &mut text, current.take(), fonts);
                    mark_fc = fc;
                    out.push(paragraph(runs, mark_fc, Terminator::Cell, papx, size));
                    runs = Vec::new();
                    size = DEFAULT_SIZE;
                    continue;
                }
                _ => {}
            }

            match ch {
                '\u{13}' => in_instruction = true,
                '\u{14}' | '\u{15}' => in_instruction = false,
                _ if in_instruction => {}
                _ => {
                    if let Some(ch) = visible(ch) {
                        let props = props_at(chpx, fc);
                        if let Some(half_points) = props.size_half_points {
                            size = size.max(half_points as f32 / 2.0);
                        }
                        if current.as_ref() != Some(&props) {
                            flush(&mut runs, &mut text, current.clone(), fonts);
                            current = Some(props);
                        }
                        text.push(ch);
                    }
                }
            }
            last_fc = fc;
        }
    }

    flush(&mut runs, &mut text, current, fonts);
    // Text that does not end with a paragraph mark still forms a paragraph.
    if !runs.is_empty() {
        let fc = if last_fc != 0 { last_fc } else { mark_fc };
        out.push(paragraph(runs, fc, Terminator::Paragraph, papx, size));
    }
    out
}

fn character(word: &[u8], piece: &Piece, index: usize) -> Option<(char, usize)> {
    if piece.compressed {
        let byte = *word.get(piece.fc as usize + index)?;
        Some((char::from(byte), 1))
    } else {
        let offset = piece.fc as usize + index * 2;
        let bytes = word.get(offset..offset + 2)?;
        let unit = u16::from_le_bytes([bytes[0], bytes[1]]);
        if (0xD800..0xDC00).contains(&unit)
            && let Some(next) = word.get(offset + 2..offset + 4)
        {
            let low = u16::from_le_bytes([next[0], next[1]]);
            if (0xDC00..0xE000).contains(&low) {
                let code = 0x10000 + ((unit as u32 - 0xD800) << 10) + (low as u32 - 0xDC00);
                return char::from_u32(code).map(|ch| (ch, 2));
            }
        }
        char::from_u32(unit as u32).map(|ch| (ch, 1))
    }
}

fn props_at(runs: &[CharRun], fc: u32) -> CharProps {
    if runs.is_empty() {
        return CharProps::default();
    }
    let index = runs
        .partition_point(|run| run.start <= fc)
        .saturating_sub(1);
    runs.get(index)
        .filter(|run| fc < run.end)
        .map(|run| run.props.clone())
        .unwrap_or_default()
}

/// Drops control characters that Word stores inline.
fn visible(ch: char) -> Option<char> {
    match ch {
        '\u{01}' | '\u{02}' | '\u{05}' | '\u{08}' => None,
        '\u{0B}' => Some('\n'),
        '\u{1E}' => Some('-'),
        '\u{1F}' => Some('\u{00AD}'),
        '\u{00A0}' => Some(' '),
        _ => Some(ch),
    }
}

fn flush(runs: &mut Vec<Run>, text: &mut String, props: Option<CharProps>, fonts: &[String]) {
    if text.is_empty() {
        return;
    }
    let props = props.unwrap_or_default();
    runs.push(Run {
        text: std::mem::take(text),
        bold: props.bold.unwrap_or(false),
        italic: props.italic.unwrap_or(false),
        underline: props.underline,
        strike: props.strike,
        size: None,
        color: props.color.and_then(color),
        highlight: props.highlight.and_then(highlight),
        font: props
            .font
            .and_then(|index| fonts.get(index as usize))
            .filter(|name| !name.is_empty())
            .cloned(),
        image: None,
    });
}

fn paragraph(
    runs: Vec<Run>,
    mark_fc: u32,
    terminator: Terminator,
    papx: &[ParaRun],
    size: f32,
) -> RawParagraph {
    let para = papx
        .iter()
        .find(|run| mark_fc >= run.start && mark_fc < run.end)
        .map(|run| run.props.clone())
        .unwrap_or_default();
    let align = match para.align {
        Some(1) => Align::Center,
        Some(2) => Align::Right,
        Some(3) => Align::Justify,
        _ => Align::Left,
    };
    let line_spacing = para
        .line_spacing
        .map(|value| (value as f32 / 240.0).clamp(0.8, 3.0))
        .unwrap_or(1.0);
    RawParagraph {
        paragraph: Paragraph {
            runs,
            align,
            indent: para.indent.unwrap_or(0) as f32 / TWIPS_PER_POINT,
            first_line: para.first_line.unwrap_or(0) as f32 / TWIPS_PER_POINT,
            space_before: para.space_before.unwrap_or(0) as f32 / TWIPS_PER_POINT,
            space_after: para.space_after.unwrap_or(0) as f32 / TWIPS_PER_POINT,
            line_spacing,
            marker: None,
            size,
            color: None,
            font: None,
            bold: false,
            italic: false,
            border: ParagraphBorder::default(),
            fill: None,
        },
        terminator,
        para,
    }
}

// ------------------------------------------------------------------- colors

/// Word's colour indexes, which are also used by highlighting.
fn color(index: u8) -> Option<u32> {
    Some(match index {
        0 => return None,
        1 => 0x000000,
        2 => 0x0000FF,
        3 => 0x00FFFF,
        4 => 0x00FF00,
        5 => 0xFF00FF,
        6 => 0xFF0000,
        7 => 0xFFFF00,
        8 => 0xFFFFFF,
        9 => 0x800000,
        10 => 0x008080,
        11 => 0x008000,
        12 => 0x800080,
        13 => 0x800000,
        14 => 0x808000,
        15 => 0x808080,
        16 => 0xC0C0C0,
        _ => return None,
    })
}

fn highlight(index: u8) -> Option<u32> {
    match index {
        0 => None,
        other => color(other),
    }
}

// ------------------------------------------------------------------- blocks

fn build_blocks(raws: Vec<RawParagraph>) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut rows: Vec<Vec<TableCell>> = Vec::new();
    let mut row: Vec<TableCell> = Vec::new();
    let mut cell: Vec<Paragraph> = Vec::new();
    let mut in_table = false;
    // Row marks carry `fTtp`; when a file does not carry paragraph properties
    // an empty cell-marked paragraph is taken as the end of the row instead.
    let ttp_seen = raws.iter().any(|raw| raw.para.row_end);

    for raw in raws {
        match raw.terminator {
            Terminator::Cell => {
                in_table = true;
                let row_end = raw.para.row_end
                    || (!ttp_seen && raw.paragraph.runs.is_empty() && !row.is_empty());
                if row_end {
                    if !cell.is_empty() {
                        row.push(cell_of(std::mem::take(&mut cell)));
                    }
                    rows.push(std::mem::take(&mut row));
                } else {
                    cell.push(raw.paragraph);
                    row.push(cell_of(std::mem::take(&mut cell)));
                }
            }
            Terminator::Paragraph => {
                // A cell or row that is still open means the paragraph belongs
                // to the table even when its properties omit `fInTable`. Once a
                // row has been closed, a paragraph without `fInTable` marks the
                // end of the table instead of extending its last cell.
                if raw.para.in_table || !(row.is_empty() && cell.is_empty()) {
                    in_table = true;
                    cell.push(raw.paragraph);
                } else {
                    if in_table || !rows.is_empty() || !row.is_empty() || !cell.is_empty() {
                        flush_table(&mut blocks, &mut rows, &mut row, &mut cell);
                    }
                    in_table = false;
                    blocks.push(Block::Paragraph(raw.paragraph));
                }
            }
        }
    }
    if in_table || !rows.is_empty() || !row.is_empty() || !cell.is_empty() {
        flush_table(&mut blocks, &mut rows, &mut row, &mut cell);
    }
    blocks
}

fn cell_of(paragraphs: Vec<Paragraph>) -> TableCell {
    TableCell {
        span: 1,
        fill: None,
        blocks: paragraphs.into_iter().map(Block::Paragraph).collect(),
    }
}

fn flush_table(
    blocks: &mut Vec<Block>,
    rows: &mut Vec<Vec<TableCell>>,
    row: &mut Vec<TableCell>,
    cell: &mut Vec<Paragraph>,
) {
    if !cell.is_empty() {
        row.push(cell_of(std::mem::take(cell)));
    }
    if !row.is_empty() {
        rows.push(std::mem::take(row));
    }
    if rows.is_empty() {
        return;
    }
    let columns = rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let width = CONTENT_WIDTH / columns as f32;
    blocks.push(Block::Table(Table {
        rows: std::mem::take(rows),
        columns: vec![width; columns],
        borders: true,
    }));
}

// ------------------------------------------------------------------ helpers

fn read_u16(data: &[u8], offset: usize) -> u16 {
    let bytes = data.get(offset..offset + 2).unwrap_or(&[0, 0]);
    u16::from_le_bytes([bytes[0], bytes[1]])
}

fn read_u32(data: &[u8], offset: usize) -> u32 {
    let bytes = data.get(offset..offset + 4).unwrap_or(&[0, 0, 0, 0]);
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}
