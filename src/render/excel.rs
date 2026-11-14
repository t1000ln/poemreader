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
    for row in first_row..=last_row.min(rows.saturating_sub(1)) {
        let top = sheet.row_tops[row] * zoom;
        elements.push(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(px(top))
                .h(px(1.))
                .bg(grid)
                .into_any_element(),
        );
        positions.push(None);
    }
    for column in first_column..=last_column.min(columns.saturating_sub(1)) {
        let left = sheet.column_lefts[column] * zoom;
        elements.push(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(left))
                .w(px(1.))
                .bg(grid)
                .into_any_element(),
        );
        positions.push(None);
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

    let mut element = div()
        .absolute()
        .left(px(left))
        .top(px(top))
        .w(px(width))
        .h(px(height))
        .overflow_hidden();

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
    let wrap = style.is_some_and(|style| style.wrap);
    let bold = style.is_some_and(|style| style.bold);
    let italic = style.is_some_and(|style| style.italic);
    let underline = style.is_some_and(|style| style.underline);

    let mut text = div()
        .size_full()
        .flex()
        .flex_col()
        .px(px(3.))
        .py(px(1.))
        .text_size(px(size))
        .line_height(px(size * 1.25))
        .text_color(color)
        .text_align(match align {
            excel::HorizontalAlign::Center => TextAlign::Center,
            excel::HorizontalAlign::Right => TextAlign::Right,
            excel::HorizontalAlign::Left => TextAlign::Left,
        })
        .child(cell.text.clone());

    text = match valign {
        excel::VerticalAlign::Top => text.justify_start(),
        excel::VerticalAlign::Center => text.justify_center(),
        excel::VerticalAlign::Bottom => text.justify_end(),
    };
    if wrap {
        text = text.whitespace_normal();
    } else {
        text = text.whitespace_nowrap();
    }
    if bold || italic || underline {
        if bold {
            text = text.font_weight(FontWeight::BOLD);
        }
        if italic {
            text = text.italic();
        }
        if underline {
            text = text.underline();
        }
        text = text.font_family(base.family.clone());
    }

    element.child(text).into_any_element()
}
