//! 配置的读写。
//!
//! 设置以 JSON 存在用户配置目录：Windows 是 `%APPDATA%\PoemReader\settings.json`，
//! 其他平台在 `$XDG_CONFIG_HOME/poemreader/`（回退到 `~/.config`）。文件缺失、
//! 无法读取或解析失败时整体退回默认值；文件存在但少了某些字段时，每一项各自
//! 取默认值（`#[serde(default)]`）。所以删掉配置文件只影响偏好，不影响启动。
//!
//! 阅读位置按文件的**绝对路径**保存，同一个文件用相对路径还是绝对路径打开都回到
//! 同一处；PDF 存的是「第几页 + 页内比例」，缩放与窗口尺寸变化后依然对得上。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::print::{ImagePrint, Orientation, PageMargins, PagesPerSheet, PrintFit};

/// Reading colour scheme, persisted by name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadingBackground {
    #[default]
    Default,
    Paper,
    Mint,
    Night,
}

/// How the left mouse button behaves in the reading area.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointerMode {
    /// Drag the content around.
    #[default]
    Pan,
    /// Drag to select text.
    Select,
}

/// 上一次打印用的参数，跨会话记忆，免得每次打印都要重新确认。
///
/// 与 [`crate::print`] 里的打印参数一一对应：适应方式、纸张方向、每张纸印几页、
/// 图片怎么印。预览里改过、或直接打印前改过的值，都会写回这里。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrintParams {
    /// 内容超出纸宽时收进纸里还是按原尺寸裁掉。
    pub fit: PrintFit,
    /// 纸张方向：纵向或横向。
    pub orientation: Orientation,
    /// 每张纸印几页：1 合 1、2 合 1、4 合 1……
    pub pages_per_sheet: PagesPerSheet,
    /// 图片怎么印：印多大、从画面哪一块开始、要不要拼成多页。
    pub image: ImagePrint,
    /// 页脚要不要印「n / m」页号。
    pub page_number: bool,
    /// 正文四周留多宽的白边。
    pub margins: PageMargins,
}

impl Default for PrintParams {
    fn default() -> Self {
        Self {
            fit: PrintFit::default(),
            orientation: Orientation::default(),
            pages_per_sheet: PagesPerSheet::default(),
            image: ImagePrint::default(),
            page_number: false,
            margins: PageMargins::default(),
        }
    }
}

impl PrintParams {
    /// 读回的参数夹回合法范围：拖动基准点万一存出界，印的时候也回得来。
    pub fn sanitized(&mut self) {
        self.image.anchor = self.image.anchor.clamped();
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub font_size: f32,
    /// Page zoom for PDF documents, as a factor of the fitted page width.
    pub pdf_zoom: f32,
    pub background: ReadingBackground,
    pub pointer_mode: PointerMode,
    /// 纯文本文档左侧要不要显示行号。
    pub line_numbers: bool,
    pub last_file: Option<PathBuf>,
    /// Reading position keyed by file: a scroll offset for text files, and a
    /// `page + fraction` anchor for PDFs.
    pub reading_positions: HashMap<String, f32>,
    /// 上一次打印用的参数，打印时先按它来。
    pub print_params: PrintParams,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_size: 18.0,
            pdf_zoom: 1.0,
            background: ReadingBackground::Default,
            pointer_mode: PointerMode::default(),
            line_numbers: false,
            last_file: None,
            reading_positions: HashMap::new(),
            print_params: PrintParams::default(),
        }
    }
}

impl Settings {
    pub fn font_size(&self) -> f32 {
        self.font_size.clamp(8.0, 72.0)
    }

    pub fn pdf_zoom(&self) -> f32 {
        if self.pdf_zoom.is_finite() {
            self.pdf_zoom.clamp(0.5, 4.0)
        } else {
            1.0
        }
    }
}

/// Settings loaded from disk, written back on the next [`Store::save`].
pub struct Store {
    path: Option<PathBuf>,
    settings: Settings,
}

impl Store {
    pub fn load() -> Self {
        let path = settings_path();
        let mut settings: Settings = path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        settings.print_params.sanitized();

        Self { path, settings }
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn settings_mut(&mut self) -> &mut Settings {
        &mut self.settings
    }

    pub fn position_for(&self, path: &Path) -> Option<f32> {
        self.settings
            .reading_positions
            .get(&position_key(path))
            .copied()
    }

    pub fn set_position(&mut self, path: &Path, offset: f32) {
        self.settings
            .reading_positions
            .insert(position_key(path), offset);
    }

    pub fn set_last_file(&mut self, path: &Path) {
        self.settings.last_file = Some(path.to_path_buf());
    }

    pub fn save(&self) {
        let Some(path) = self.path.as_ref() else {
            return;
        };

        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        if let Ok(text) = serde_json::to_string_pretty(&self.settings) {
            let _ = std::fs::write(path, text);
        }
    }
}

/// Absolute path used as the key for reading positions, so the same file opened
/// from a relative and an absolute path resumes at the same place.
fn position_key(path: &Path) -> String {
    std::path::absolute(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn settings_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA")
            .map(|dir| PathBuf::from(dir).join("PoemReader").join("settings.json"))
    }
    #[cfg(not(windows))]
    {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
        base.map(|dir| dir.join("poemreader").join("settings.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::print::{ImageAnchor, ImageScale};

    /// 打印参数跟着设置一起存一起取，字段名也不能悄悄变。
    #[test]
    fn print_params_round_trip_through_json() {
        let mut image = ImagePrint {
            scale: ImageScale(200),
            anchor: ImageAnchor { x: 0.25, y: 0.75 },
            poster: true,
        };
        image.anchor = image.anchor.clamped();
        let params = PrintParams {
            fit: PrintFit::ActualSize,
            orientation: Orientation::Landscape,
            pages_per_sheet: PagesPerSheet::Four,
            image,
            page_number: true,
            margins: PageMargins::Wide,
        };

        let text = serde_json::to_string(&params).expect("serialize");
        let back: PrintParams = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(back, params);
    }

    /// 老配置文件里没有打印参数这一节，读进来用默认值，不能报错。
    #[test]
    fn a_settings_file_without_print_params_still_loads() {
        let mut value = serde_json::to_value(Settings::default()).expect("serialize");
        value
            .as_object_mut()
            .expect("an object")
            .remove("print_params");
        let text = serde_json::to_string(&value).expect("serialize");

        let settings: Settings = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(settings.print_params, PrintParams::default());
    }

    /// 越界的拖动基准点读回来时被夹回画面里。
    #[test]
    fn an_out_of_range_anchor_is_sanitized_on_load() {
        let mut params = PrintParams::default();
        params.image.anchor = ImageAnchor { x: 3.0, y: -0.5 };
        params.sanitized();
        assert_eq!(params.image.anchor, ImageAnchor { x: 1.0, y: 0.0 });
    }
}
