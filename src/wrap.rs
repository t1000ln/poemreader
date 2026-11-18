//! Text wrapping with a batch-measured width cache.
//!
//! gpui's [`LineWrapper`] measures the width of every character it has not
//! seen yet through the platform text system. On Windows each measurement
//! takes a write lock on the DirectWrite state, so opening a large Chinese
//! novel — roughly five thousand distinct characters — means five thousand
//! serialized platform calls (~300 ms) that also contend with the UI thread's
//! own text shaping and make the whole window stutter.
//!
//! This module replaces that path for plain text documents. CJK characters —
//! the bulk of a Chinese document — are measured with GDI
//! (`GetTextExtentPoint32W`), which is independent of gpui's DirectWrite state
//! and needs no locks; their advances are em-sized, so GDI's integer widths
//! equal DirectWrite's exactly. The remaining characters (ASCII, Latin, …) are
//! few and are measured through gpui itself, because GDI's integer rounding
//! would differ from DirectWrite's fractional advances there. A sample of the
//! GDI-measured characters is always checked against DirectWrite: if the two
//! disagree, the GDI font is not the font gpui renders with and the whole
//! cache falls back to gpui's own per-character measurement. The wrap
//! algorithm below is a verbatim port of gpui's `LineWrapper::wrap_line` in
//! the `IndentAdjustment::NoIndent` configuration — the only one the text
//! reader uses — so wrap points match what gpui would have produced.

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::{Font, FontId, FontStyle, Pixels, TextSystem};
use icu_segmenter::LineSegmenter;
use unicode_segmentation::UnicodeSegmentation;

/// How many sample characters are compared against DirectWrite.
const SAMPLE_SIZE: usize = 64;
/// Per-character width difference (px) counted as a mismatch.
const WIDTH_EPSILON: f32 = 0.6;
/// Share of sample characters allowed to differ between GDI and DirectWrite
/// before the GDI measurement is rejected.
const GDI_MISMATCH_TOLERANCE: f32 = 0.08;
/// Characters from here up are measured with GDI; everything below — ASCII,
/// Latin, Greek, punctuation — goes through gpui, whose fractional advances
/// GDI's integer widths would round away.
const CJK_RANGE_START: u32 = 0x2E80;

/// Character widths for one font and size.
pub struct CharWidths {
    widths: HashMap<char, f32>,
}

impl CharWidths {
    /// An empty cache; every width reads as zero. Used by tests.
    #[cfg(test)]
    pub(crate) fn empty() -> Self {
        Self {
            widths: HashMap::new(),
        }
    }

    /// A cache from explicit widths. Used by tests.
    #[cfg(test)]
    pub(crate) fn from_map(widths: HashMap<char, f32>) -> Self {
        Self { widths }
    }
}

impl CharWidths {
    /// Measure every distinct character of `text` for `font` at `font_size`.
    ///
    /// The GDI path covers the whole character set after spot-checking a
    /// sample against DirectWrite; a rejected check falls back to measuring
    /// every character through gpui's text system.
    pub fn measure(
        text: &str,
        font: &Font,
        font_size: Pixels,
        text_system: &Arc<TextSystem>,
    ) -> Self {
        let mut distinct: Vec<char> = text.chars().collect();
        distinct.sort_unstable();
        distinct.dedup();

        let font_id = text_system.resolve_font(font);

        // Non-CJK characters are few in Chinese text and are measured through
        // gpui directly — GDI's integer rounding differs from DirectWrite's
        // fractional advances on proportional Latin glyphs, which would
        // occasionally shift a wrap point. CJK glyphs are all em-sized, where
        // GDI and DirectWrite agree exactly, so the bulk goes through GDI.
        let mut widths = HashMap::with_capacity(distinct.len());
        let mut cjk = Vec::new();
        for &c in &distinct {
            if (c as u32) >= CJK_RANGE_START {
                cjk.push(c);
            } else {
                widths.insert(c, text_system.layout_width(font_id, font_size, c).as_f32());
            }
        }

        match gdi_widths(&cjk, font, font_size, text_system, font_id) {
            Some(gdi) => widths.extend(gdi),
            None => {
                widths.extend(direct_write_widths(&cjk, font_id, font_size, text_system));
            }
        }
        Self { widths }
    }

    /// Width of `c`. Characters always come from the text the cache was built
    /// from, so a miss can only be a bug; zero keeps the layout degenerate
    /// instead of panicking.
    #[inline]
    pub fn width(&self, c: char) -> f32 {
        self.widths.get(&c).copied().unwrap_or(0.)
    }
}

/// Measure all characters through gpui's text system, one platform layout per
/// character — the same numbers [`LineWrapper`] would cache.
fn direct_write_widths(
    distinct: &[char],
    font_id: FontId,
    font_size: Pixels,
    text_system: &Arc<TextSystem>,
) -> HashMap<char, f32> {
    let mut widths = HashMap::with_capacity(distinct.len());
    for &c in distinct {
        widths.insert(c, text_system.layout_width(font_id, font_size, c).as_f32());
    }
    widths
}

/// Wrap `line` to `wrap_width`, returning the byte index each row ends at
/// (exclusive), like `LineWrapper::wrap_line` with `NoIndent`.
///
/// `line` must not contain `\n`.
pub fn wrap_line(line: &str, wrap_width: f32, widths: &CharWidths) -> Vec<usize> {
    let mut width = 0.;
    let mut first_non_whitespace_ix = None;
    let mut last_candidate_ix = 0usize;
    let mut last_candidate_width = 0.;
    let mut last_wrap_ix = 0usize;
    let mut index = 0usize;

    let mut break_indices = break_indices(line).peekable();
    let mut grapheme_indices = line.grapheme_indices(true).map(|(at, _)| at).peekable();
    let mut grapheme_index = 0usize;
    let mut width_before_grapheme = 0.;
    let mut rows = Vec::new();

    for c in line.chars() {
        let ix = index;
        index += c.len_utf8();

        while grapheme_indices.next_if(|at| *at < ix).is_some() {}
        let is_grapheme_boundary = grapheme_indices.next_if_eq(&ix).is_some();
        if is_grapheme_boundary {
            grapheme_index = ix;
            width_before_grapheme = width;
        }

        if break_indices.next_if_eq(&ix).is_some()
            && is_grapheme_boundary
            && first_non_whitespace_ix.is_some()
        {
            last_candidate_ix = ix;
            last_candidate_width = width;
        }

        if c == '\n' {
            continue;
        }
        if c != ' ' && first_non_whitespace_ix.is_none() {
            first_non_whitespace_ix = Some(ix);
        }

        width += widths.width(c);

        if width > wrap_width && grapheme_index > last_wrap_ix {
            let wrap_at_candidate =
                last_candidate_ix > last_wrap_ix && width - last_candidate_width <= wrap_width;

            if wrap_at_candidate {
                last_wrap_ix = last_candidate_ix;
                width -= last_candidate_width;
                width_before_grapheme -= last_candidate_width;
            } else {
                // An oversized grapheme stays intact, even if it exceeds the
                // line width.
                last_wrap_ix = grapheme_index;
                width -= width_before_grapheme;
                width_before_grapheme = 0.;
            }
            last_candidate_ix = 0;

            rows.push(last_wrap_ix);
        }
    }

    rows
}

/// Line break opportunities, byte indices a row may end at.
fn break_indices(text: &str) -> impl Iterator<Item = usize> + '_ {
    LineSegmenter::new_for_non_complex_scripts(Default::default()).segment_str(text)
}

/// Measure all characters with GDI, verify a sample against DirectWrite and
/// return `None` when the two disagree — the GDI font then is not the font
/// gpui renders with, and the widths would be wrong.
#[cfg(windows)]
fn gdi_widths(
    distinct: &[char],
    font: &Font,
    font_size: Pixels,
    text_system: &Arc<TextSystem>,
    font_id: FontId,
) -> Option<HashMap<char, f32>> {
    use std::mem;

    use windows::core::HSTRING;
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateFontW, DeleteDC, DeleteObject, GetDC, GetTextExtentPoint32W,
        GetTextMetricsW, ReleaseDC, SelectObject, DEFAULT_CHARSET, DEFAULT_QUALITY, OUT_TT_PRECIS,
        TEXTMETRICW,
    };

    let face = gdi_face_name(font)?;
    let face: HSTRING = face.as_str().into();

    // A screen DC is enough: no blitting, only metrics are queried.
    let screen_dc = unsafe { GetDC(None) };
    if screen_dc.is_invalid() {
        return None;
    }
    let font_handle = unsafe {
        CreateFontW(
            -(font_size.as_f32().round() as i32),
            0,
            0,
            0,
            gdi_weight(font.weight),
            u32::from(font.style == FontStyle::Italic),
            0,
            0,
            DEFAULT_CHARSET,
            OUT_TT_PRECIS,
            Default::default(),
            DEFAULT_QUALITY,
            Default::default(),
            &face,
        )
    };
    if font_handle.is_invalid() {
        unsafe { ReleaseDC(None, screen_dc) };
        return None;
    }

    // A compatible DC carries the font for measurement.
    let dc = unsafe { CreateCompatibleDC(Some(screen_dc)) };
    let previous = unsafe { SelectObject(dc, font_handle.into()) };

    let mut metrics = unsafe { mem::zeroed::<TEXTMETRICW>() };
    let metrics_ok = unsafe { GetTextMetricsW(dc, &mut metrics) }.as_bool();

    let mut widths = HashMap::with_capacity(distinct.len());
    let mut failed = !metrics_ok;
    if metrics_ok {
        for &c in distinct {
            let mut buffer = [0u8; 4];
            let encoded = c.encode_utf8(&mut buffer);
            let wide: Vec<u16> = encoded.encode_utf16().collect();
            let mut size = SIZE::default();
            let ok = unsafe { GetTextExtentPoint32W(dc, &wide, &mut size) }.as_bool();
            if !ok {
                failed = true;
                break;
            }
            widths.insert(c, size.cx as f32);
        }
    }

    unsafe { SelectObject(dc, previous) };
    let _ = unsafe { DeleteObject(font_handle.into()) };
    let _ = unsafe { DeleteDC(dc) };
    unsafe { ReleaseDC(None, screen_dc) };

    if failed || widths.len() != distinct.len() {
        return None;
    }
    if !sample_agrees(distinct, &widths, font_id, font_size, text_system) {
        return None;
    }
    Some(widths)
}

#[cfg(not(windows))]
fn gdi_widths(
    _distinct: &[char],
    _font: &Font,
    _font_size: Pixels,
    _text_system: &Arc<TextSystem>,
    _font_id: FontId,
) -> Option<HashMap<char, f32>> {
    None
}

/// Compare a spread-out sample of the characters against DirectWrite.
fn sample_agrees(
    distinct: &[char],
    widths: &HashMap<char, f32>,
    font_id: FontId,
    font_size: Pixels,
    text_system: &Arc<TextSystem>,
) -> bool {
    if distinct.is_empty() {
        return true;
    }
    let step = distinct.len().div_ceil(SAMPLE_SIZE);
    let mut checked = 0usize;
    let mut mismatched = 0usize;
    for &c in distinct.iter().step_by(step) {
        let expected = text_system.layout_width(font_id, font_size, c);
        if let Some(actual) = widths.get(&c)
            && (actual - expected.as_f32()).abs() > WIDTH_EPSILON
        {
            mismatched += 1;
        }
        checked += 1;
    }
    checked > 0 && mismatched as f32 / checked as f32 <= GDI_MISMATCH_TOLERANCE
}

#[cfg(windows)]
mod gdi {
    use gpui_kit::{Font, FontWeight};

    /// The face name GDI should use. gpui's `.SystemUIFont` alias resolves to
    /// the system message font, which is what the NONCLIENTMETRICS carry.
    pub(crate) fn face_name(font: &Font) -> Option<String> {
        use std::mem;

        use windows::Win32::UI::WindowsAndMessaging::{
            SystemParametersInfoW, NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS,
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
        };

        const SYSTEM_UI_FONT_NAME: &str = ".SystemUIFont";
        if font.family != SYSTEM_UI_FONT_NAME {
            return Some(font.family.to_string());
        }

        let mut metrics = NONCLIENTMETRICSW {
            cbSize: mem::size_of::<NONCLIENTMETRICSW>() as u32,
            ..unsafe { mem::zeroed() }
        };
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETNONCLIENTMETRICS,
                metrics.cbSize,
                Some(&mut metrics as *mut NONCLIENTMETRICSW as *mut _),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
        }
        .is_ok();
        if !ok {
            return None;
        }
        let face = &metrics.lfMessageFont.lfFaceName;
        let len = face.iter().position(|&unit| unit == 0).unwrap_or(face.len());
        Some(String::from_utf16_lossy(&face[..len]))
    }

    /// gpui and GDI share the 100–900 weight scale.
    pub(crate) fn weight(weight: FontWeight) -> i32 {
        weight.0 as i32
    }
}

#[cfg(windows)]
use gdi::{face_name as gdi_face_name, weight as gdi_weight};
