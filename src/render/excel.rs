//! Excel 工作表的网格与单元格绘制。
//!
//! 只画落在可视范围内的行列，合并单元格按跨行跨列放大，选区覆盖在单元格
//! 填充色之上。

use gpui_kit::*;

use crate::events::CellSelection;
use crate::excel;

// --------------------------------------------------------------- Excel layout

/// State shared by the Excel builders: the sheet, its styles and the current
/// selection.
pub(crate) struct ExcelRender<'a> {
    pub(crate) sheet: &'a excel::Sheet,
    pub(crate) styles: &'a [excel::CellStyle],
    pub(crate) zoom: f32,
    pub(crate) foreground: Hsla,
    pub(crate) base: &'a Font,
    /// Colour painted behind the selected cells.
    pub(crate) highlight: Hsla,
    pub(crate) selection: Option<CellSelection>,
}

/// The gridlines and cell elements that are visible in a sheet.
pub(crate) fn excel_cells(
    render: ExcelRender,
    first_row: usize,
    last_row: usize,
    first_column: usize,
    last_column: usize,
    // Sheet coordinates of each element, `None` for the gridlines.
    positions: &mut Vec<Option<(u32, u32)>>,
) -> Vec<AnyElement> {
    let sheet = render.sheet;
    let zoom = render.zoom;
    let foreground = render.foreground;
    let mut elements = Vec::new();
    let grid = foreground.opacity(0.15);
    let rows = sheet.row_count();
    let columns = sheet.column_count();

    // Gridlines for the visible part of the sheet, painted under the cells.
    // `row_tops`/`column_lefts` hold one extra edge, so the last visible row
    // and column get their bottom/right border too. A merged range hides the
    // grid inside it — Excel draws no line between the cells it joined — so
    // each line is cut into the parts the merges leave visible.
    let left = sheet.column_lefts[first_column] * zoom;
    let right = sheet.column_lefts[(last_column + 1).min(columns)] * zoom;
    let top = sheet.row_tops[first_row] * zoom;
    let bottom = sheet.row_tops[(last_row + 1).min(rows)] * zoom;

    for row in first_row..=last_row.min(rows) {
        let y = sheet.row_tops[row] * zoom;
        let holes = sheet
            .row_grid_holes(row)
            .into_iter()
            .map(|(start, end)| {
                (
                    sheet.column_lefts[start] * zoom,
                    sheet.column_lefts[end] * zoom,
                )
            })
            .collect();
        for (start, end) in grid_segments(left, right, holes) {
            elements.push(
                div()
                    .absolute()
                    .left(px(start))
                    .top(px(y))
                    .w(px(end - start))
                    .h(px(1.))
                    .bg(grid)
                    .into_any_element(),
            );
            positions.push(None);
        }
    }
    for column in first_column..=last_column.min(columns) {
        let x = sheet.column_lefts[column] * zoom;
        let holes = sheet
            .column_grid_holes(column)
            .into_iter()
            .map(|(start, end)| (sheet.row_tops[start] * zoom, sheet.row_tops[end] * zoom))
            .collect();
        for (start, end) in grid_segments(top, bottom, holes) {
            elements.push(
                div()
                    .absolute()
                    .top(px(start))
                    .left(px(x))
                    .w(px(1.))
                    .h(px(end - start))
                    .bg(grid)
                    .into_any_element(),
            );
            positions.push(None);
        }
    }

    let start = sheet
        .rows
        .partition_point(|row| (row.index as usize) < first_row);
    for row in &sheet.rows[start..] {
        let row_index = row.index as usize;
        if row_index > last_row {
            break;
        }
        let cells_start = row
            .cells
            .partition_point(|cell| (cell.column as usize) < first_column);
        for cell in &row.cells[cells_start..] {
            let column = cell.column as usize;
            if column > last_column || column >= columns {
                break;
            }
            if sheet.is_covered(row.index, cell.column) {
                continue;
            }

            let (row_span, column_span) = sheet
                .merge_at(row.index, cell.column)
                .map(|merge| (merge.rows as usize, merge.columns as usize))
                .unwrap_or((1, 1));
            let bottom = (row_index + row_span).min(rows);
            let right = (column + column_span).min(columns);

            let left = sheet.column_lefts[column] * zoom;
            let top = sheet.row_tops[row_index] * zoom;
            let width = (sheet.column_lefts[right] - sheet.column_lefts[column]) * zoom;
            let height = (sheet.row_tops[bottom] - sheet.row_tops[row_index]) * zoom;

            elements.push(excel_cell(
                cell,
                &render,
                render
                    .selection
                    .is_some_and(|selection| selection.contains(row.index, cell.column)),
                left,
                top,
                width,
                height,
            ));
            positions.push(Some((row.index, cell.column)));
        }
    }

    elements
}

/// Cuts the line `start..end` into the parts that `holes` leave visible.
///
/// The holes arrive in merge order, so they can be unsorted and can overlap;
/// the parts are laid out after sorting, which is what keeps a line from being
/// drawn twice over the same stretch.
pub(crate) fn grid_segments(start: f32, end: f32, mut holes: Vec<(f32, f32)>) -> Vec<(f32, f32)> {
    if end <= start {
        return Vec::new();
    }
    holes.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut parts = Vec::with_capacity(holes.len() + 1);
    let mut cursor = start;
    for (hole_start, hole_end) in holes {
        let hole_start = hole_start.max(start);
        let hole_end = hole_end.min(end);
        if hole_end <= cursor || hole_start >= end {
            continue;
        }
        if hole_start > cursor {
            parts.push((cursor, hole_start));
        }
        cursor = hole_end;
    }
    if cursor < end {
        parts.push((cursor, end));
    }
    parts
}

fn excel_cell(
    cell: &excel::Cell,
    render: &ExcelRender,
    selected: bool,
    left: f32,
    top: f32,
    width: f32,
    height: f32,
) -> AnyElement {
    let zoom = render.zoom;
    let foreground = render.foreground;
    let base = render.base;
    let style = render.styles.get(cell.style);

    // No `overflow_hidden` here: the row is grown to fit its wrapped text, and
    // clipping is what used to cut the ends off a cell that ran wide.
    let mut element = div()
        .absolute()
        .left(px(left))
        .top(px(top))
        .w(px(width))
        .h(px(height));

    // The selection covers the cell fill, so the selected cells stand out even
    // when they are shaded.
    if selected {
        element = element.bg(render.highlight);
    } else if let Some(fill) = style.and_then(|style| style.fill) {
        element = element.bg(rgb(fill));
    }

    if let Some(style) = style {
        let border_color = [
            style.border_top,
            style.border_right,
            style.border_bottom,
            style.border_left,
        ]
        .into_iter()
        .flatten()
        .next();
        if let Some(color) = border_color {
            element = element.border_color(rgb(color));
            if style.border_top.is_some() {
                element = element.border_t_1();
            }
            if style.border_bottom.is_some() {
                element = element.border_b_1();
            }
            if style.border_left.is_some() {
                element = element.border_l_1();
            }
            if style.border_right.is_some() {
                element = element.border_r_1();
            }
        }
    }

    let size = (style.map(|style| style.size).unwrap_or(15.0) * zoom).max(4.0);
    let color = style
        .and_then(|style| style.color)
        .map(|color| rgb(color).into())
        .unwrap_or(foreground);
    let align = style
        .map(|style| style.horizontal(cell.numeric))
        .unwrap_or_default();
    let valign = style.map(|style| style.valign).unwrap_or_default();
    let wrap = cell.wrap;
    let bold = style.is_some_and(|style| style.bold);
    let italic = style.is_some_and(|style| style.italic);
    let underline = style.is_some_and(|style| style.underline);
    let text_align = match align {
        excel::HorizontalAlign::Center => TextAlign::Center,
        excel::HorizontalAlign::Right => TextAlign::Right,
        excel::HorizontalAlign::Left => TextAlign::Left,
    };

    // The padding matches `CELL_INSET` in the sheet model: the row heights and
    // the wrap points were measured with four pixels on each side, so drawing
    // with less would push the text past the width it was fitted to.
    let mut column = div().size_full().flex().flex_col().px(px(4.)).py(px(1.));
    column = match valign {
        excel::VerticalAlign::Top => column.justify_start(),
        excel::VerticalAlign::Center => column.justify_center(),
        excel::VerticalAlign::Bottom => column.justify_end(),
    };

    // One child per line the cell stores. gpui's wrapper skips a bare `\n`
    // rather than breaking on it, so a cell like `第一站\n(7:10-7:13)` would
    // otherwise run together on a single line and lose its second half.
    let line = |content: String| {
        let mut line = div()
            .w_full()
            .min_w(px(0.))
            .text_size(px(size))
            .line_height(px(size * 1.25))
            .text_color(color)
            .text_align(text_align);
        if bold {
            line = line.font_weight(FontWeight::BOLD);
        }
        if italic {
            line = line.italic();
        }
        if underline {
            line = line.underline();
        }
        if bold || italic || underline {
            line = line.font_family(base.family.clone());
        }
        line.child(content)
    };

    if wrap {
        for part in cell.text.split('\n') {
            column = column.child(line(part.to_string()).whitespace_normal());
        }
    } else {
        column = column.child(line(cell.text.clone()).whitespace_nowrap());
    }

    element.child(column).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::grid_segments;
    use crate::excel::{Merge, Sheet};

    /// A three-by-three grid with `B1:C2` merged.
    fn sheet() -> Sheet {
        Sheet {
            name: String::from("测试"),
            column_lefts: vec![0.0, 10.0, 20.0, 30.0],
            row_tops: vec![0.0, 10.0, 20.0, 30.0],
            rows: Vec::new(),
            merges: vec![Merge {
                row: 0,
                column: 1,
                rows: 2,
                columns: 2,
            }],
        }
    }

    /// Excel paints no grid between the cells it joined, so the line inside a
    /// merge has to be dropped while the merge's own edges stay.
    #[test]
    fn a_merge_hides_the_grid_inside_it() {
        let sheet = sheet();
        assert!(sheet.row_grid_holes(0).is_empty(), "顶边要留着");
        assert_eq!(sheet.row_grid_holes(1), vec![(1, 3)], "合并区中间不画横线");
        assert!(sheet.row_grid_holes(2).is_empty(), "底边要留着");

        assert!(sheet.column_grid_holes(1).is_empty(), "左边要留着");
        assert_eq!(
            sheet.column_grid_holes(2),
            vec![(0, 2)],
            "合并区中间不画竖线"
        );
        assert!(sheet.column_grid_holes(3).is_empty(), "右边要留着");
    }

    #[test]
    fn a_line_without_merges_is_drawn_whole() {
        assert_eq!(grid_segments(0.0, 100.0, Vec::new()), vec![(0.0, 100.0)]);
    }

    #[test]
    fn a_line_is_broken_around_every_merge_it_crosses() {
        assert_eq!(
            grid_segments(0.0, 100.0, vec![(20.0, 40.0), (60.0, 80.0)]),
            vec![(0.0, 20.0), (40.0, 60.0), (80.0, 100.0)]
        );
    }

    #[test]
    fn overlapping_merges_do_not_cut_the_line_twice() {
        assert_eq!(
            grid_segments(0.0, 100.0, vec![(60.0, 80.0), (20.0, 50.0)]),
            vec![(0.0, 20.0), (50.0, 60.0), (80.0, 100.0)]
        );
    }

    #[test]
    fn a_merge_covering_the_whole_line_leaves_nothing_to_draw() {
        assert!(grid_segments(0.0, 100.0, vec![(-10.0, 110.0)]).is_empty());
    }
}
