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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub font_size: f32,
    /// Page zoom for PDF documents, as a factor of the fitted page width.
    pub pdf_zoom: f32,
    pub background: ReadingBackground,
    pub pointer_mode: PointerMode,
    pub last_file: Option<PathBuf>,
    /// Reading position keyed by file: a scroll offset for text files, and a
    /// `page + fraction` anchor for PDFs.
    pub reading_positions: HashMap<String, f32>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_size: 18.0,
            pdf_zoom: 1.0,
            background: ReadingBackground::Default,
            pointer_mode: PointerMode::default(),
            last_file: None,
            reading_positions: HashMap::new(),
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
        let settings = path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();

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
