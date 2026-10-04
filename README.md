<div align="center">

<img src="assets/icon.png" width="96" alt="Cote 图标" />

# Cote

**轻量、快速、开箱即用的跨平台纯文本编辑器** — CotEditor 风格，Rust + Tauri 实现

[![CI](https://github.com/Buddleek/cote/actions/workflows/ci.yml/badge.svg)](https://github.com/Buddleek/cote/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](#license)
[![Rust](https://img.shields.io/badge/Rust-stable-orange.svg)](https://www.rust-lang.org)

Windows · macOS · Linux ｜ x86_64 · aarch64

</div>

---

Cote 是一款面向纯文本与轻量代码编辑的桌面编辑器：**语法树级高亮、完善的编码/换行符管理、
多标签、大纲导航、JS 脚本自动化**——保持轻快的同时覆盖日常编辑所需。
对标 [CotEditor](https://coteditor.com)（macOS），将这套体验带到所有桌面平台。

自 v0.2.0 起界面迁移到 **Tauri 2 + 系统 WebView**（Windows: WebView2 / macOS: WKWebView /
Linux: WebKitGTK），前端使用 Preact + CodeMirror 6；编辑内核不变。
egui 版实现保留在 git 历史与 `main` 分支。

## 特性一览

| | |
|---|---|
| 🌈 **语法树级高亮** | tree-sitter 驱动 8 种语言（Rust / Python / JavaScript / C / JSON / Java / C++ / HTML·XML），函数、类型、属性、常量独立着色；另有扫描式分词 10 种（Go / C# / Kotlin / Ruby / Shell / SQL / YAML / TOML / Markdown…），共 18 种内置语言。后端产出高亮 spans 注入 CodeMirror 装饰系统 |
| 🈶 **编码与换行** | BOM → UTF-8 校验 → 统计检测三级自动识别（GBK / Big5 / Shift_JIS / UTF-16 / UTF-32…）；编码一键切换（干净文档即时重解码）、有损字符预警；LF / CRLF / CR 识别与转换 |
| ↩️ **可靠的撤销** | rope 数据结构 + 分组撤销：连续输入/退格合并为单步，批量变换整组回退，Ctrl+Z 由编辑内核接管（CodeMirror 自带历史关闭），撤销以增量编辑序列同步到视图 |
| 🗂️ **多标签编辑** | 标签栏、未保存确认、30s 自动保存（原子写入）、崩溃会话恢复（含光标位置）、外部修改检测（横幅提示 + 重新加载/忽略） |
| 🔍 **查找替换** | 纯文本与正则（fancy-regex）、大小写/整词、捕获组 `$1`、匹配高亮与计数、跳转到行 |
| 📑 **大纲与补全** | 按语言规则提取函数/类/标题，侧栏点击跳转；Ctrl+Space 文档内单词补全（CJK 词支持） |
| 🧩 **JS 脚本** | `scripts/*.js` 自动进菜单，`cote.text()` / `cote.setText()` / `cote.selection()` 等 API，沙箱隔离（无文件/网络能力，防死循环） |
| 🎨 **可定制语法** | JSON 定义自定义语言（关键词/注释符/大纲正则/标记模式），放入配置目录即生效 |
| ♿ **细节** | Unicode 安全（grapheme 光标、NFC/NFD/NFKC/NFKD）、全角↔半角、CJK 字数统计、深浅色切换（跟随系统 / 手动，Ctrl+Shift+L，CSS 变量双主题） |

## 快速开始

依赖：Rust（stable，Windows 需 MSVC 工具链 + VS Build Tools）、Node.js ≥ 18、
WebView2 Runtime（Win10/11 一般自带）。

```bash
git clone https://github.com/Buddleek/cote.git
cd cote
npm install
npm run tauri dev                # 开发模式（Vite 热更新）
```

构建与打包：

```bash
npm run tauri build              # 产出 msi/nsis 安装包与绿色 exe
```

测试与静态检查：

```bash
cargo test --workspace           # 内核单元测试（86 个）
cargo clippy --workspace --all-targets   # 零警告门禁
npx tsc --noEmit                 # 前端类型检查
```

## CLI

```
cote [文件...]          打开一个或多个文件（多标签页）
cote 文件 --line N      打开后跳转到第 N 行
cote 文件 --column M    打开后跳转到第 M 列（配合 --line）
cote --version          显示版本
```

## 架构

```
crates/                      纯 Rust 库（无 GUI 依赖，可独立测试）
├── editor-core/             编辑内核：rope 缓冲、撤销分组、编码、换行、查找、变换
├── syntax-engine/           语法定义（内置 + 用户 JSON）+ tree-sitter/扫描双引擎高亮 + 大纲
├── script-host/             boa_engine JS 运行时 + cote.* 编辑器 API
└── platform/                原子写入、会话持久化

src-tauri/                   Tauri 2 后端 + 应用入口
└── src/
    ├── state.rs             标签页状态（Document + 元数据 + 已保存镜像）、偏移转换
    ├── commands.rs          全部 Tauri commands（打开/编辑/撤销/查找/变换/编码/脚本…）
    └── lib.rs               启动、定时器（自动保存 30s / 外部修改检测 2s 轮询）

src/                         Web 前端（Vite + TypeScript + Preact）
├── cm.ts                    CodeMirror 6 装配：后端 spans → 装饰、按键、补全源
├── backend.ts               invoke 桥：promise 队列串行化编辑序列
└── App.tsx                  标签页/菜单/查找/大纲/状态栏编排
```

数据流（性能关键）：CodeMirror 产生**增量变更** → `apply_edit` 写入内核 Document
（自带撤销分组）→ 高亮 spans / 大纲防抖重算回传；全量文本仅在打开/切换/整体替换时传输。
CodeMirror 坐标（UTF-16 偏移）与内核字符索引在后端边界互转。

| 领域 | 选型 |
|---|---|
| GUI | Tauri 2 + 系统 WebView（安装包小、观感原生一致） |
| 编辑器组件 | CodeMirror 6（历史关闭，撤销由内核接管） |
| 文本缓冲 | Ropey |
| 高亮 | tree-sitter（8 语言）+ 扫描分词（其余），失败自动回退 |
| 编码 | encoding_rs + chardetng（+ 手写 UTF-16/32） |
| 正则 | fancy-regex |
| 脚本 | boa_engine（纯 Rust，无 C 工具链依赖） |

## 需求文档

完整软件需求（FR/NFR 编号、里程碑、验收标准）见
[需求文档](./需求文档-仿CotEditor的Rust跨平台文本编辑器.md)。

## Windows 构建环境要点

Windows 上 Tauri 2 需要 **MSVC 工具链**（不支持 windows-gnu）：

1. VS Build Tools（C++ 桌面开发 workload）：
   `winget install Microsoft.VisualStudio.2022.BuildTools --override "--add Microsoft.VisualStudio.Workload.VCTools --includeRecommended --quiet"`
2. `rustup default stable-msvc`
3. Node.js：`winget install OpenJS.NodeJS.LTS`
4. WebView2 Runtime：Win10/11 一般自带（`winget install Microsoft.EdgeWebView2Runtime` 备用）

macOS / Linux 直接构建（Linux 需 webkit2gtk 等系统包，见 CI 配置）。

## 已知边界

- tree-sitter 为整体重解析（≤1MB 每次编辑约 1–30ms；未做 InputEdit 增量），>1MB 降级纯文本
- 外部修改检测为 mtime 轮询（未用 notify 推送）；编辑偏移转换 O(n)/op（超大文件可再优化）
- 打印、i18n 资源化、自定义主题、代码折叠未实现；脚本无墙钟超时
- 混合换行符文件以 `\n` 显示编辑（内核保存时按所选风格整体转换）

## 许可

MIT
