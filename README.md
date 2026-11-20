# PoemReader

使用 [GPUI Kit](https://gpui-kit.com)（GPUI + GPUI Component）构建的极简阅读器和打印工具，方便简单查看和快速打印，没有任何编辑修改功能！
当前版本仅支持`Win64`环境。

![PoemReader 图标](assets/icon-256.png)

## 功能

- 自绘标题栏 + 菜单栏：`文件`（加载文件… / 打印预览… / 打印… / 纵向打印 / 横向打印 / 退出）、`视图`（显示行号，只在纯文本时出现）、`字体大小`（Markdown / PDF / Word / Excel / 图片为 `页面缩放`）、`窗口背景色`、`关于`
- `文件 → 加载文件…`（`Ctrl+O`）调用系统原生文件对话框选择文件；菜单项右侧也标出了快捷键
- 也可以把文件从系统（如资源管理器）拖入窗口直接打开
- 支持纯文本（UTF-8 含 BOM / GB18030），不限大小
  - 文本按当前字号和窗口宽度折行成显示行，只绘制屏幕内的行，
    上百 MB 的文件也能秒开，滚动与划选不受行数影响
  - 读文件与折行都在后台线程完成，窗口不会在打开文件时卡住；
    读入期间正文上方会显示「正在打开 …」，旧文档照常显示到新文档就绪为止
  - 调整字号或窗口大小时自动重新折行，并保持当前阅读位置
  - `视图 → 显示行号` 在正文左侧显示行号：按源文本行编号，折行出来的续行不
    重复编号，空行照样计数；行号比正文淡，开关状态会记住
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
  - 内嵌图片按原始尺寸显示，并跟随段落对齐；比正文更宽的图片会等比缩到正文宽度，
    不会顺着页面右边被裁掉
- 支持 Excel（`.xlsx`）：表格按单元格样式绘制
  - 字体（粗体 / 斜体 / 下划线 / 字号 / 颜色）、填充色、四边边框
  - 填充色与文字颜色支持 RGB、主题色（含淡色 / 深色变化）与索引色三种写法；
    只有格式没有文字的单元格（色块、表尾空行）同样保留
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
- 打印预览：`文件 → 打印预览…` 或 `Ctrl+Shift+P`，在单独的窗口中打开，`Esc` 或「关闭」退出
  - 预览窗口盖在主窗口中间，主窗口的阅读位置、选区与滚动完全不受影响
  - 预览使用与打印完全相同的排版代码，把每一页画到离屏的纸上再显示，
    因此预览里的页数、分页位置和每一行的落点就是打印出来的样子
  - 纸张尺寸取自默认打印机（含硬边距），工具栏会显示纸张规格与所用打印机；
    没有打印机时按 A4 估算排版。纸张在后台询问，预览界面不会等它
  - 位图按需生成：只有看得见的页才真的画，翻页时在后台排版，不阻塞阅读窗口
  - 排版可以提前收尾：先排到当前屏的最后一页就画出来，剩下的页数随后在后台补上，
    因此几百页的长文档也能立刻显示第一页；翻到第 N 页只排到第 N 页为止
  - 工具栏支持缩放（0.5×–2.5×）与页码指示，预览中也可以直接「打印…」；
    预览里可用 Esc 关闭、Ctrl+P 打印、Ctrl+= / Ctrl+- 缩放
  - 工具栏按钮带图标、淡底色与分组分隔线，指针在按钮上停留约 0.7 秒会弹出
    功能提示（说明这个按钮做什么、点下去会发生什么）
  - 工具栏的「适应纸张 / 原始大小」切换排版方式：前者把超出纸宽的内容整体缩小，
    后者保持原文尺寸（超出部分会被纸边裁掉）；切换后预览立即按新方式重排
  - 工具栏的「纵向 / 横向」切换纸张方向：纸横过来之后预览整页重排，
    工具栏的纸张规格（如 `297 × 210 毫米`）与页数会跟着变化，看到的就是打印的样子
  - 打开图片时工具栏出现「图片 N%」：缩放的是画面本身而不是版面——
    100% 就是适应纸张，缩小后四周留白；选项有 25%–400%，切换后预览立即重画
  - 放大到一页装不下时，可以在页上直接拖动图片设定基准点：拖动过程中画面
    跟着指针实时移动，松开左键即按新位置重画（页码旁有提示）；基准点存的是
    比例，换纸张、换比例之后仍然指在画面的同一处
  - 工具栏的「拼接分页」勾选后，装不下的画面自动分到多页上（海报式打印）：
    300% 就是 3 × 3 共 9 页，每页是同一张图往左上挪一格，打印出来拼回去
    就是完整原图；不勾选时只印基准点所在的那一页，超出部分由纸边裁掉
  - 工具栏的「边距」选择正文四周留多宽的白边：无边距 / 窄 / 普通 / 宽，
    选完立刻在纸上看到效果，缩印时每个小页各留一份
  - 工具栏的「N 合 1」选择每张纸印几页：1 / 2 / 4 / 6 / 8 / 9 合 1。
    每一页仍按自己的排版整页绘制，再缩小排进网格（纵向纸 2 合 1 左右并排、
    6 合 1 三列两行，9 合 1 三行三列，横向纸相应转置），页与页之间留出窄缝，
    不足一组时按实际页数收尾；页码指示显示的是纸数
  - 「打印视图…」只打印当前看得见的那一块：按预览的缩放把该页重排到 300–600 dpi，
    裁下可见区域后铺满一张纸，放大看图片细节时正好把细节印大
  - 换了文档后已打开的预览会自动关闭；预览已打开时再次触发只会把它带到前面
- 打印：`文件 → 打印…` 或 `Ctrl+P`，使用系统打印对话框选择打印机、份数与页码
  - 打印方向、每张纸的页数与图片设置都在「文件」菜单中选择：方向为
    「纵向打印 / 横向打印」，页数为「每张纸打印页数」（1 / 2 / 4 / 6 / 8 / 9 合 1），
    页边距为「打印页边距」（无边距 / 窄 0.6 厘米 / 普通 1.3 厘米 / 宽 2.5 厘米），
    打开图片时还有「图片打印缩放」（25%–400%）、
    「拼接分页（海报式多页）」与「图片位置居中」。
    预览里切换之后直接打印用的是同一组设置，两个窗口看到与打出的是同一张纸；
    横向由驱动完成（修改 `DEVMODE` 后 `ResetDC`），打印机自己负责出纸的方向
  - 所有格式都支持页码范围，可在对话框中选「页码」并输入单页或范围（如 `3`、`5-12`）
    - PDF 的对话框按文件真实页数限制可选范围
    - 其他格式要到排版时才知道总页数，因此对话框允许输入较大的页码；
      超出实际页数时只打印存在的页面，并提示文档总页数
  - 文本按当前字号排版并自动分页，按「打印页边距」留白（默认 1.3 厘米），
    超出部分自动换行
  - PDF 按打印机分辨率逐页栅格化后输出，保持原始比例并居中
  - 图片按纸张可打印区域等比缩放后居中输出，可用「图片打印缩放」
    在此基础上放大或缩小（超出部分由纸边裁掉，仍居中）
  - 打印不会改变阅读位置与当前选区
  - Word 按文档自身的页面宽度排版：字体、颜色、列表、表格与内嵌图片都会输出，
    超出页面宽度时整体缩小以适应纸张（「原始大小」模式下不缩小）
  - Markdown 与 Word 使用同一套排版输出，标题、列表、表格、代码块和图片都会保留
  - Excel 按列 / 行边界分页，每个工作表依次输出，保留填充色、边框与数字格式；
    没有边框的区域照屏幕上的样子画出浅灰网格线；
    单列比纸还宽时按比例缩小到能放下（最多缩到 25%），避免右侧被裁掉
  - 多页合一时每一页都先按整页排版，再整页缩小放进格子，
    所以换行位置与单页打印完全一致，只是字变小了
  - 内嵌图片与透明背景的图片统一铺在白底上输出，缩放使用平滑插值，不会出现黑底或锯齿
- 模式会随其他设置一起保存，下次启动时沿用
- 正文字号可调（14–28 px），四种阅读配色（默认 / 米黄 / 淡绿 / 夜间）
- 字号、PDF / 页面缩放、配色、上次打开的文件和阅读位置自动保存，下次启动时自动恢复
  - 配置文件：Windows `%APPDATA%\PoemReader\settings.json`，
    其他平台 `$XDG_CONFIG_HOME/poemreader/settings.json`
  - 每个文件按绝对路径分别记录阅读位置；PDF 记录「第几页 + 页内比例」，
    因此缩放或窗口尺寸变化后仍停在原来的位置
- 窗口与 exe 使用同一套图标：一页纸（正文横线 + 一张照片）从打印机出纸口伸出，
  对应「查看文档与图片、并打印」；16 / 24 像素另有简化画法，保证任务栏上不糊

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

# 打包后顺手把 dist 里的副本注册为文件打开候选（见「文件关联」）
powershell -NoProfile -ExecutionPolicy Bypass -File tools\package.ps1 -Associate
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
    install.cmd
    install.ps1
    register-associations.cmd
    register-associations.ps1
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

## 下载

预编译的发布包在 [Releases](../../releases) 页面，不需要自己编译：

- `PoemReader-0.1.0-win64.zip`（附带 `SHA256SUMS.txt`）

仓库里的 `releases/` 目录保存着同一份包，`gh` 或仓库源码下载也能拿到。
发布由 `.github/workflows/release.yml` 完成：打 `v*` tag 或手动触发时，
它会把 `releases/` 下的包挂到对应 Release 上。

## 安装

发布包里带一个免管理员的安装脚本。解压后**双击 `install.cmd`** 即可，它会：

- 把程序复制到 `%LOCALAPPDATA%\Programs\PoemReader`，装完可以删掉解压出来的文件夹
- 建立开始菜单快捷方式
- 在「设置 → 应用 → 已安装的应用」里登记，可以从那里卸载
- 把**安装后的副本**注册为支持格式的「打开方式」候选（见下一节）

也可以在 PowerShell 里带参数运行：

```powershell
# 指定安装目录
powershell -NoProfile -ExecutionPolicy Bypass -File install.ps1 -InstallDir "D:\Apps\PoemReader"

# 只装程序，不注册文件关联、不建快捷方式
powershell -NoProfile -ExecutionPolicy Bypass -File install.ps1 -NoAssociations -NoShortcuts

# 卸载（同时移除文件关联、快捷方式与注册表项）
powershell -NoProfile -ExecutionPolicy Bypass -File install.ps1 -Uninstall
```

写入范围只有 `HKCU` 和用户自己的目录，不需要管理员权限，也不影响本机其他账户。
卸载时会删除安装目录，因此**装到别的目录后不要往里面放自己的文件**。

## 文件关联

让 PoemReader 出现在「打开方式」里，可以自己选它来打开支持的格式。

安装时这一步默认已经完成——`install.ps1` 会调用下面这个脚本。如果不想安装、
想直接就地使用解压出来的副本，也可以单独运行它。发布包里自带注册脚本，就在
exe 旁边，解压到任意目录后：

- **双击 `register-associations.cmd`** 即可注册，结束后按任意键关闭窗口
- 取消注册：在同一目录执行 `register-associations.cmd -Unregister`

也可以用 PowerShell 直接运行脚本，效果相同：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\register-associations.ps1

# 取消注册
powershell -NoProfile -ExecutionPolicy Bypass -File .\register-associations.ps1 -Unregister
```

脚本按**自身所在目录**查找 `PoemReader.exe`，与你从哪个目录启动无关，所以文件夹解压到哪里都能直接用。注册表里记录的仍是那个 exe 的绝对路径，**移动文件夹后需要重新运行一次**。

在源码仓库里直接运行 `tools\register-associations.ps1` 时，同目录没有 exe，会依次回退到 `target\release` 和 `dist\`；也可以显式指定：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tools\register-associations.ps1 -ExePath "C:\Apps\PoemReader\PoemReader.exe"
```

- 覆盖全部支持的格式：纯文本、PDF、Word（`.docx` / `.docm` / `.dotx` / `.doc`）、
  Excel（`.xlsx` / `.xlsm` / `.xltx` / `.xls`）、Markdown、图片
- 只写 `HKCU`，不需要管理员权限，也不影响本机其他账户
- 不写 `UserChoice`（Windows 8 起有哈希保护，写了会被系统判为篡改并重置），
  也不改扩展名的默认值，因此不会和已有关联打架；取消注册时只删自己写入的值
- 注册后 PoemReader 会出现在「打开方式」和「设置 → 应用 → 默认应用」中。
  **把某个格式真正设为默认需要用户自己勾一次「始终使用此应用」**，
  这是 Windows 的规定，程序无法代替
- 注册表里记录的是 exe 的绝对路径，换了目录要重新运行脚本

## 图标

- `assets/icon.ico`：多尺寸 ICO（16/24/32/48/64/128/256），由 `build.rs`
  （`embed-resource`）嵌入 exe，因此资源管理器、任务栏和任务管理器进程列表
  显示的都是它
- `assets/icon.svg`：标题栏与空状态页使用的单色图标（跟随主题前景色），
  画的是同一件事：一张纸插在打印机里
- `assets/icons/`：手写的界面小图标，用 `include_bytes!` 内嵌进二进制
  - `printer.svg`：「文件 → 打印…」菜单项前的打印机
  - `page-number.svg`：预览工具栏「页号」按钮上叠放的 1 / 2 / 3
  - `margins.svg`：预览工具栏「边距」按钮上的一页纸与正文框
- `assets/icon-256.png`：预览图
- `tools/generate-icon.ps1`：重新生成 `icon.ico` 与预览图
  - 256 像素是完整画法：纸张上三行正文 + 一张照片，机身有出纸槽与指示灯
  - 48 / 64 去掉指示灯以外的细节靠缩放，32 及以下单独绘制（12 像素高的粗线、
    16 / 24 只保留一行正文），避免细线在小尺寸里糊成灰块

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tools\generate-icon.ps1
```

## 项目结构

按「界面 / 解析 / 输出」三部分划分：解析层不依赖界面，渲染与打印都只消费解析
结果，彼此之间没有调用关系。

```
src/
  main.rs         入口、文档模型（Document）、阅读视图与渲染编排
  window.rs       进程级 DPI 适配、窗口尺寸与位置、启动时打开哪个文件
  events.rs       阅读区事件：拖拽平移 / 划选、右键菜单、拖放打开
  shortcuts.rs    窗口级快捷键：Ctrl+O 加载、Ctrl+C 复制、Ctrl+P 打印、Ctrl+Shift+P 预览
  loader.rs       按文件魔数分派解析器，产出 DocumentKind + 内容

  render/         渲染层：只做排版与绘制
    mod.rs        可划选行的共用模型（SelectableLine / LinePlace / LineText）
    word.rs       Word 段落、表格与内嵌图片的排版
    excel.rs      工作表网格、单元格与合并单元格
    pdf.rs        页面布局与后台栅格化

  text.rs         纯文本折行（按字号与宽度缓存，每行带着源文本行号）
  word.rs         .docx 解析
  excel.rs        .xlsx 解析
  doc.rs          .doc 解析（Word 97-2003）
  xls.rs          .xls 解析（BIFF8）
  markdown.rs     Markdown 解析
  ooxml.rs        ZIP 与 XML 公共件（Word / Excel 共用）
  cfb.rs          OLE2 复合文档（.doc / .xls 的容器）
  pdf.rs          PDF 解析
  picture.rs      图片解码（含 SVG、GIF 动画）
  settings.rs     配置读写（字号、配色、阅读位置）

  print.rs        打印入口，文本与 PDF 的输出
  print_office.rs Word / Excel / Markdown 的打印排版
  preview.rs      打印预览：纸张尺寸、按需离屏排版与页面视图
```

视图状态集中在 `PoemReader` 里，由 `main.rs` 定义。`events` 与 `render` 直接
读写其中的字段（`pub(crate)`），解析器与加载器则完全不知道界面的存在。
注意 `src/pdf.rs` 与 `src/render/pdf.rs` 分工不同：前者解析 PDF，后者只管
页面落在哪儿、什么时候栅格化。

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
- 打印预览与打印共用同一套 GDI 排版（`src/preview.rs` 只负责纸张尺寸、按需
  调度与页面显示），页面画到离屏 DIB 上再交给 GPUI 显示。
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
