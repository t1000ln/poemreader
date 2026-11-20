//! 把文件读成可以渲染的内容。
//!
//! 先按魔数判断真实类型（扩展名可以骗人），再交给对应的解析器。这里不碰界面，
//! 也不关心窗口：启动时由 `window` 模块调用，运行时由视图的「加载文件…」调用。

use std::path::Path;
use std::sync::Arc;

use crate::cfb;
use crate::doc;
use crate::excel::{self, Workbook};
use crate::markdown;
use crate::ooxml::Package;
use crate::pdf::PdfSource;
use crate::picture::{self, Picture};
use crate::word::{self, WordDocument};
use crate::xls;

/// Every document is read into memory as a whole — text files included, since
/// they are decoded and wrapped after the read — so all of them share one cap.
/// Without it a large stray file (or an unknown extension) could make the
/// process allocate gigabytes before it even knows what it opened.
const MAX_DOCUMENT_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DocumentKind {
    Text,
    Pdf,
    Word,
    Excel,
    Markdown,
    Image,
}

/// What a background load produced. Held only long enough to build a
/// [`Document`] on the render thread.
pub(crate) enum LoadedContent {
    Text(String),
    Pdf(Arc<PdfSource>),
    Word(Arc<WordDocument>),
    Excel(Arc<Workbook>),
    Markdown(Arc<WordDocument>),
    Image(Arc<Picture>),
}

/// Load a text, PDF, Word, Excel or Markdown document from disk.
pub(crate) fn load_document(path: &Path) -> Result<(DocumentKind, LoadedContent), String> {
    let bytes = read_file_bytes(path)?;
    if is_pdf(path, &bytes) {
        return PdfSource::load(bytes)
            .map(|source| (DocumentKind::Pdf, LoadedContent::Pdf(Arc::new(source))));
    }
    if is_markdown_path(path) {
        let text = decode_text(bytes);
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        return markdown::load(&text, base).map(|document| {
            (
                DocumentKind::Markdown,
                LoadedContent::Markdown(Arc::new(document)),
            )
        });
    }
    if is_image_path(path) {
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        return Picture::load(bytes, base)
            .map(|picture| (DocumentKind::Image, LoadedContent::Image(Arc::new(picture))));
    }
    if bytes.starts_with(b"PK\x03\x04") {
        return load_office(bytes);
    }
    if is_compound_file(&bytes) {
        return load_compound_office(path, bytes);
    }

    // A file without a known extension may still be a picture.
    if picture::looks_like_image(&bytes) {
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        return Picture::load(bytes, base)
            .map(|picture| (DocumentKind::Image, LoadedContent::Image(Arc::new(picture))));
    }

    Ok((DocumentKind::Text, LoadedContent::Text(decode_text(bytes))))
}

/// Word and Excel documents are ZIP packages; which one it is depends on the
/// parts inside.
fn load_office(bytes: Vec<u8>) -> Result<(DocumentKind, LoadedContent), String> {
    let package = Package::open(&bytes)?;
    if package.has("word/document.xml") {
        let document = word::load(&package)?;
        Ok((DocumentKind::Word, LoadedContent::Word(Arc::new(document))))
    } else if package.has("xl/workbook.xml") {
        let workbook = excel::load(&package)?;
        Ok((
            DocumentKind::Excel,
            LoadedContent::Excel(Arc::new(workbook)),
        ))
    } else {
        Err(String::from("这是一个压缩包，但不是 Word 或 Excel 文档"))
    }
}

fn read_file_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = std::fs::metadata(path).map_err(|error| format!("无法读取文件信息：{error}"))?;
    // Applies to every extension: unknown suffixes fall through to the text
    // and image probes, and those are the ones that used to read a whole file
    // with no limit at all.
    if metadata.len() > MAX_DOCUMENT_BYTES {
        return Err(format!(
            "文件过大（{:.1} MB），最大支持 {} MB",
            metadata.len() as f64 / (1024.0 * 1024.0),
            MAX_DOCUMENT_BYTES / (1024 * 1024)
        ));
    }

    std::fs::read(path).map_err(|error| format!("读取文件失败：{error}"))
}

fn is_pdf_path(path: &Path) -> bool {
    has_extension(path, &["pdf"])
}

fn is_markdown_path(path: &Path) -> bool {
    has_extension(path, &["md", "markdown", "mdown", "mkd", "mkdn"])
}

fn is_image_path(path: &Path) -> bool {
    has_extension(
        path,
        &[
            "jpg", "jpeg", "jpe", "jfif", "png", "bmp", "dib", "webp", "ico", "gif", "svg", "svgz",
        ],
    )
}

/// Legacy Word and Excel documents share the OLE2 container; which one it is
/// depends on the streams inside or on the file extension.
fn load_compound_office(
    path: &Path,
    bytes: Vec<u8>,
) -> Result<(DocumentKind, LoadedContent), String> {
    if has_extension(path, &["xls"]) {
        return xls::load(bytes).map(|workbook| {
            (
                DocumentKind::Excel,
                LoadedContent::Excel(Arc::new(workbook)),
            )
        });
    }
    if has_extension(path, &["doc"]) {
        return doc::load(bytes)
            .map(|document| (DocumentKind::Word, LoadedContent::Word(Arc::new(document))));
    }

    // The extension is unknown, so pick the reader from the streams inside.
    let container = cfb::CompoundFile::open(bytes)?;
    if container.has("Workbook") || container.has("Book") {
        return Err(String::from("这是一个 Excel 文档，请使用 .xls 扩展名"));
    }
    if container.has("WordDocument") {
        return Err(String::from("这是一个 Word 文档，请使用 .doc 扩展名"));
    }
    Err(String::from("这是一个复合文档，但不是 Word 或 Excel 文件"))
}

fn is_compound_file(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1])
}

fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension().is_some_and(|extension| {
        extensions
            .iter()
            .any(|candidate| extension.eq_ignore_ascii_case(candidate))
    })
}

fn is_pdf(path: &Path, bytes: &[u8]) -> bool {
    is_pdf_path(path) || bytes.starts_with(b"%PDF-")
}

/// Decode a text file, accepting UTF-8 (with or without BOM) and falling back
/// to GB18030 so that Chinese text saved by older Windows editors works.
fn decode_text(mut bytes: Vec<u8>) -> String {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        bytes.drain(..3);
    }

    // Taking the buffer over keeps a large UTF-8 file from being copied once more.
    match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => {
            let bytes = error.into_bytes();
            let (decoded, _, had_errors) = encoding_rs::GB18030.decode(&bytes);
            if had_errors {
                String::from_utf8_lossy(&bytes).into_owned()
            } else {
                decoded.into_owned()
            }
        }
    }
}
