# PoemReader

使用 [GPUI Kit](https://gpui-kit.com)（GPUI + GPUI Component）构建的极简阅读器。

![PoemReader 图标](assets/icon-256.png)

## 功能

- 自绘标题栏 + 菜单栏：`文件`（加载文件… / 打印… / 退出）、`字体大小`（Markdown / PDF / Word / Excel / 图片为 `页面缩放`）、`窗口背景色`、`关于`
- `文件 → 加载文件…` 调用系统原生文件对话框选择文件
- 也可以把文件从系统（如资源管理器）拖入窗口直接打开
- 支持纯文本（UTF-8 含 BOM / GB18030），不限大小
  - 文本按当前字号和窗口宽度折行成显示行，只绘制屏幕内的行，
    上百 MB 的文件也能秒开，滚动与划选不受行数影响
  - 调整字号或窗口大小时自动重新折行，并保持当前阅读位置
- 支持 PDF：按页把页面栅格化后显示，字体、表格、矢量图和内嵌图片都会完整呈现
  - 页面在后台线程逐页渲染，只渲染视口附近的页面，打开大文件不必等待
  - 缩放 75% / 100% / 125% / 150% / 200%，超过 100% 时可用底部滚动条左右查看
  - 加密 / 需要密码的 PDF 暂不支持
- 支持 Markdown（`.md` / `.markdown`）：按文档结构排版，不显示 Markdown 标记
  - 标题层级、段落、加粗 / 斜体 / 删除线、行内代码
  - 有序 / 无序 / 任务列表（多级缩进）、引用、分隔线、脚注
  - GFM 表格：表头底色、按列对齐、边框
  - 围栏代码块：等宽字体、浅色底纹与圆角外边框，保留缩进与空行
  - 本地相对路径图片按原比例显示，并限制在正文宽度内；远程图片显示替代文字
  - 链接显示为带下划线的文字，复制时只复制文字
  - 与 Word 一样支持页面缩放、划选复制与打印
- 支持 Word（`.docx`）：按文档自身的排版模型绘制，保留段落样式
  - 段落：标题样式、对齐方式、缩进（含首行缩进）、段前段后间距、行距
  - 字符：加粗、斜体、下划线、删除线、字号、字体、文字颜色、高亮
  - 项目符号与编号列表、多级缩进
  - 表格：列宽、单元格底纹、合并单元格、边框，表格内可含多段文字
  - 内嵌图片按原始尺寸显示，并跟随段落对齐
- 支持 Excel（`.xlsx`）：表格按单元格样式绘制
  - 字体（粗体 / 斜体 / 下划线 / 字号 / 颜色）、填充色、四边边框
  - 水平 / 垂直对齐、自动换行（行高会随之撑开）
  - 列宽与行高、合并单元格、多工作表（顶部标签切换）
  - 数字格式：整数、千分位、小数、百分比、货币、日期与时间
  - 只绘制可见区域的单元格，大表滚动仍然流畅
- 支持图片：JPEG（`.jpg` / `.jpeg` / `.jfif`）、PNG、BMP、WebP、ICO、GIF（含动画）与 SVG（含 `.svgz`）
  - 打开时按窗口大小等比适配；可用「页面缩放」或鼠标滚轮缩放，以指针所在位置为中心
  - 超出窗口时用鼠标拖拽或滚动条上下左右查看
  - SVG 以较高分辨率栅格化，适配到窗口大小后依然清晰；GIF 动图会自动播放
  - 透明背景的图片显示在当前阅读底色上，打印时自动铺上白色背景
- 支持旧版 Word（`.doc`，Word 97-2003）与 Excel（`.xls`，Excel 97 / BIFF8）
  - `.doc`：读取正文、直接字符格式（加粗 / 斜体 / 下划线 / 删除线 / 字号 / 颜色 / 高亮）、
    段落对齐与缩进、列表符号与编号、表格边框；命名样式只继承段落对齐与列表归属，其余样式属性、
    图片、页眉页脚、脚注与单元格合并暂不呈现
  - `.xls`：读取单元格、数字格式、字体、填充、边框、对齐与自动换行、合并单元格、行列尺寸和多工作表；
    颜色优先使用 `XFEXT` 扩展记录中的精确值，缺失时按 BIFF8 调色板近似，
    加密文件与 BIFF5 及更早的版本暂不支持
- 正文按行渲染，右侧常驻滚动条，支持滚轮 / 拖动滚动条
- 鼠标左键有两种模式，通过右键菜单切换：
  - `拖拽平移`：按住左键拖动即可上下左右移动内容（手形光标）
  - `划选文字`：按住左键拖动即可选中文字（I 形光标），支持跨行、跨换行与自动折行
    - 纯文本按字符选中，Markdown、Word（`.docx` 与旧版 `.doc`）按段落文字选中，
      Excel（`.xlsx` 与旧版 `.xls`）按单元格矩形选中
- 复制所选文字：`Ctrl+C`，或右键菜单中的「复制所选文字」
  - Markdown / Word 复制段落文本，同段内不额外换行；Excel 复制为制表符分隔的表格文本
- 打印：`文件 → 打印…` 或 `Ctrl+P`，使用系统打印对话框选择打印机、份数与页码
  - 所有格式都支持页码范围，可在对话框中选「页码」并输入单页或范围（如 `3`、`5-12`）
    - PDF 的对话框按文件真实页数限制可选范围
    - 其他格式要到排版时才知道总页数，因此对话框允许输入较大的页码；
      超出实际页数时只打印存在的页面，并提示文档总页数
  - 文本按当前字号排版并自动分页，0.5 英寸页边距，超出部分自动换行
  - PDF 按打印机分辨率逐页栅格化后输出，保持原始比例并居中
  - 图片按纸张可打印区域等比缩放后居中输出
  - 打印不会改变阅读位置与当前选区
  - Word 按文档自身的页面宽度排版：字体、颜色、列表、表格与内嵌图片都会输出，
    超出页面宽度时整体缩小以适应纸张
  - Markdown 与 Word 使用同一套排版输出，标题、列表、表格、代码块和图片都会保留
  - Excel 按列 / 行边界分页，每个工作表依次输出，保留填充色、边框与数字格式
- 模式会随其他设置一起保存，下次启动时沿用
- 正文字号可调（14–28 px），四种阅读配色（默认 / 米黄 / 淡绿 / 夜间）
- 字号、PDF / 页面缩放、配色、上次打开的文件和阅读位置自动保存，下次启动时自动恢复
  - 配置文件：Windows `%APPDATA%\PoemReader\settings.json`，
    其他平台 `$XDG_CONFIG_HOME/poemreader/settings.json`
  - 每个文件按绝对路径分别记录阅读位置；PDF 记录「第几页 + 页内比例」，
    因此缩放或窗口尺寸变化后仍停在原来的位置
- 窗口与 exe 使用同一套「打开的书」图标

## 运行

```sh
cargo run
# 也可以直接打开某个文件
cargo run -- samples/sample.txt
cargo run -- samples/sample.pdf
cargo run -- samples/sample-rich.pdf
cargo run -- samples/sample.docx
cargo run -- samples/sample.xlsx
cargo run -- samples/sample.md
cargo run -- samples/sample-image.png
```

首次编译 GPUI 依赖需要几分钟。程序使用 Windows GUI 子系统，
调试构建和发布构建都不会弹出控制台窗口。

## 打包发布版

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tools\package.ps1
```

脚本会执行 `cargo build --release`，验证 exe 使用 GUI 子系统（无控制台窗口），
然后生成：

```
dist/
  PoemReader-0.1.0-win64/
    PoemReader.exe
    README.md
    LICENSE-MIT
    LICENSE-APACHE
    THIRD-PARTY-NOTICES.md
    samples/sample.txt
    samples/sample.pdf
    samples/sample-rich.pdf
    samples/sample.docx
    samples/sample.xlsx
    samples/sample.md
    samples/sample-image.png
    samples/_legacy-sample.doc
    samples/_legacy.doc
    samples/_legacy.xls
  PoemReader-0.1.0-win64.zip
```

发布版 exe 已内嵌图标与版本信息（ProductName / FileVersion 等）。
目标机器需要已安装 Microsoft Visual C++ 运行库（绝大多数 Windows 10/11 自带）。

## 图标

- `assets/icon.ico`：多尺寸 ICO，由 `build.rs`（`embed-resource`）嵌入 exe
- `assets/icon.svg`：标题栏与空状态页使用的单色图标（跟随主题前景色）
- `assets/icon-256.png`：预览图
- `tools/generate-icon.ps1`：重新生成 `icon.ico` 与预览图

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tools\generate-icon.ps1
```

## 说明

- 依赖 `gpui-kit = "0.9.1"`；PDF 由 `hayro` 栅格化（纯 Rust，无系统依赖、无需 DLL）。
- 图片由 `image` 解码，SVG 由 `resvg` 解析并按窗口尺寸栅格化为位图（`src/picture.rs`），
  因此 JPEG、PNG、BMP、WebP、ICO、GIF 与 SVG 都能直接打开。
- Word / Excel 由 `src/word.rs`、`src/excel.rs` 直接读取 OOXML（`src/ooxml.rs` 负责 ZIP 与 XML），
  不依赖 Office 或 LibreOffice，也不需要任何 COM 组件。
- Markdown 由 `markdown`（markdown-rs）的 GFM 解析器解析，再转换成 Word 的块模型
  （`src/markdown.rs`），因此显示、划选与打印复用同一套排版代码。
- 文件对话框由 GPUI 平台层（Windows 上的 `IFileOpenDialog`）提供，不额外依赖第三方对话框库。
- 打印使用 Windows GDI 实现（`src/print.rs`，Word / Excel / Markdown 的排版在 `src/print_office.rs`），
  其他平台目前会提示暂不支持。
- PDF 页面与图片只支持拖拽平移，不支持划选与复制；
  右键菜单在这两种文档下只提供拖拽平移。
- 纯文本文件没有大小上限；Markdown、PDF、Word、Excel 与图片上限 128 MB。
- 单边超过 8192 像素的图片会在载入时缩小，避免超大照片耗尽内存。
- Markdown 中的图片只读取本地相对路径或绝对路径；网络图片不会被下载，而是显示替代文字。
- 已经渲染的页面最多缓存 12 页、约 256 MB，超出后按与当前位置的距离淘汰。
- 单页栅格最大 2400 万像素，超大尺寸页面会自动降低渲染分辨率。
- `samples/sample.pdf` 由 `tools/generate-sample-pdf.ps1` 生成；
  `samples/sample-rich.pdf`（含内嵌图片与表格）由 `tools/generate-rich-pdf.ps1` 生成。
- `samples/sample.docx` 与 `samples/sample.xlsx` 的 OOXML 内容放在 `tools/sample-word/`、
  `tools/sample-excel/`，由 `tools/pack-samples.ps1` 打包；改完 XML 重新运行该脚本即可。

## 许可证

PoemReader 以 MIT 或 Apache-2.0 双许可证发布，可任选其一，详见 `LICENSE-MIT` 与 `LICENSE-APACHE`。
第三方依赖的许可证见 `THIRD-PARTY-NOTICES.md`，它由 `tools/generate-notices.ps1` 根据
`Cargo.lock` 生成；升级依赖后重新运行该脚本即可更新。
