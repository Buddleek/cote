# Tauri 迁移交接文档

> 本文档供**新会话**接手 UI 技术栈迁移（egui → Tauri 2）使用。
> 迁移原因：egui 为即时模式自绘渲染，各操作系统界面外观不一致；
> Tauri 2 使用系统 WebView（Windows: WebView2 / macOS: WKWebView / Linux: WebKitGTK），
> 原生观感一致、安装包更小。

## 1. 当前项目状态（迁移基准）

- 版本 v0.1.0，仓库 `Buddleek/cote`（public，main 分支）
- 85 个单元测试全绿，clippy 零警告
- 最新提交：`b452a7a`（深浅色主题切换 Ctrl+Shift+L + winresource 修复）
- 已实现功能全集（迁移时逐项对照，勿丢失）：

| 功能 | 现状要点 |
|---|---|
| 编辑内核 | Ropey rope + 分组撤销（连续输入/退格合并、显式分组）、grapheme 安全光标 |
| 编码 | 三级自动检测（BOM→UTF-8→chardetng）、18+ 编码手动重解码、有损字符预警、UTF-16/32 手写实现 |
| 换行 | LF/CRLF/CR 识别、混合提示、可撤销转换 |
| 查找替换 | 正则（fancy-regex）/大小写/整词、$1 捕获组、匹配计数、跳转到行 |
| 文本变换 | 大小写、全半角（含半角片假名）、NFC/NFD/NFKC/NFKD、Tab↔空格、行排序/去重/移动/注释切换 |
| 语法高亮 | tree-sitter 8 语言（函数/类型/属性/常量/运算符/标签）+ 扫描式 10 语言，失败自动回退；亮暗双主题色板 |
| 大纲 | 逐语言正则规则提取，侧栏跳转；用户 JSON 可定义规则 |
| 补全 | Ctrl+Space 文档内单词补全（CJK 词支持） |
| 脚本 | boa_engine，`cote.text()/setText()/selection()/setSelection()/status()`，沙箱隔离 + 迭代上限 |
| 文件 | 多标签、原子写入（临时文件+rename）、30s 自动保存、会话恢复（含光标位置）、外部修改检测、关闭/退出确认 |
| CLI | 多文件多标签、`--line/--column` |
| 其他 | CJK 字体回退、CJK 字数统计、应用图标（exe 资源嵌入）、状态栏（行列/统计/编码/换行/语言快捷菜单） |

## 2. 资产复用评估（关键：大部分内核可直接保留）

| crate | 处置 | 说明 |
|---|---|---|
| `editor-core` | ✅ **完整保留** | 纯库无 GUI 依赖；作为 Tauri commands 的后端被直接调用 |
| `syntax-engine` | ✅ **完整保留** | 高亮 spans（字节区间）序列化为 JSON 供前端着色 |
| `script-host` | ✅ **完整保留** | 脚本执行是纯函数式（ScriptApi 进出） |
| `platform/fs` | ✅ 保留 | 原子写入 |
| `platform/session` | ✅ 保留 | 会话持久化（serde_json） |
| `platform/dialogs` | ⚠️ 替换 | rfd 同步对话框 → Tauri 自带 dialog plugin（异步） |
| `platform/fonts` | ❌ 废弃 | WebView 自管字体 |
| `editor-ui` | ❌ **整体废弃重做** | egui 界面 → Web 前端 + Tauri commands |
| `cli` | ♻️ 改造 | CLI 参数解析保留，入口改为启动 Tauri app |

**注意**：egui 版实现在 git 历史中（main 分支），迁移期间建议开 `tauri` 分支开发，
移植时随时对照旧实现语义（如撤销分组边界、diff 同步的门控策略）。

## 3. 目标架构

```
┌─ Web 前端（Vite + TypeScript + Svelte 或 Preact）─────────┐
│  CodeMirror 6 编辑器组件（多实例 = 多标签）                    │
│  查找栏 / 大纲侧栏 / 状态栏 / 补全 / 标签栏 / 设置页            │
│  主题（亮/暗/跟随系统）— CSS 变量                              │
└──────────────┬ invoke / event ────────────┘
               │
┌─ Tauri 2 Rust 后端 ───────────────────────────────────────┐
│  commands: open_file / save_file / apply_edit / undo / redo │
│            search / transform / outline / completions       │
│            run_script / set_encoding / set_newline …        │
│  状态: Vec<DocumentState>（标签页，含 editor-core::Document）│
│  事件: file_changed(外部修改) / session_restored / status    │
└──────────────────────────────────────────────────────────┘
```

### 关键设计决策（新会话定夺前先读）

1. **编辑数据流（性能关键）**：不要整篇文本来回传。
   前端 CodeMirror 产生增量变更 → `invoke("apply_edit", {start, end, inserted})`
   → 后端 `Document.insert_text/delete_range`（自带撤销分组）→ 广播新的高亮 spans。
   全量文本仅在打开/切换标签时传输一次。
2. **高亮**：优先把 `syntax-engine` 的 spans（字节区间→行/列或 UTF-16 偏移）
   注入 CodeMirror 的装饰系统；若 CodeMirror lezer 方案更顺也可用其自带
   语言包（18 语言覆盖度需重新评估——决策 spike 时定）。
3. **多标签**：后端持有 `Vec<DocumentState>`（Document + 元数据），
   前端只持有视图状态；标签元数据（路径/dirty/编码/换行）随 state 事件同步。
4. **自动保存/会话/外部检测**：后端定时器 + notify（可顺带把轮询升级为推送）。

## 4. 环境风险（迁移第一优先级处理）

⚠️ **Tauri 2 官方不支持 `windows-gnu` 工具链**。本机当前正是 windows-gnu
（无 Visual Studio，用 w64devkit）。必须：

1. 安装 VS Build Tools（C++ 桌面开发 workload，约 6GB）：
   `winget install Microsoft.VisualStudio.2022.BuildTools --override "--add Microsoft.VisualStudio.Workload.VCTools --includeRecommended --quiet"`
2. `rustup toolchain install stable-msvc` 并设为默认（或项目级 override）
3. Node.js（前端构建）：`winget install OpenJS.NodeJS.LTS`
4. WebView2 Runtime：Win10/11 一般自带（`winget install Microsoft.EdgeWebView2Runtime` 备用）

GitHub Actions 的 `windows-latest` 自带 MSVC，CI 侧无需改动（`windows-gnu` 相关
文档章节随之过时，可删）。

## 5. 新会话实施清单（建议顺序）

- [ ] **Phase 0 环境与 spike**：装 VS Build Tools/Node、切 msvc、`cargo tauri init`
      空窗口 + 一个 `invoke("greet")` 打通；`cargo test --workspace` 保持全绿（内核未动）
- [ ] **Phase 1 编辑回路**：单标签 打开/编辑/保存/撤销/重做（apply_edit 增量通路 + 撤销接管）
- [ ] **Phase 2 功能迁移**（每项对照 §1 表格逐个验收）：编码/换行菜单与状态栏、
      查找替换、文本变换、高亮注入、大纲、补全、多标签、自动保存/会话/外部检测、
      脚本菜单、CLI 参数、图标与打包（tauri bundle：msi/nsis，图标用 `assets/app.ico`）
- [ ] **Phase 3 收尾**：CI 更新（Linux 需 webkit2gtk 系统包）、README 更新、
      删除 editor-ui（或移入 `legacy/` 分支）、版本号 0.2.0
- [ ] **验收**：迁移完成定义 = §1 功能表格逐行在 Tauri 版中可复现 + 测试全绿

## 6. 本会话遗留说明

- 工作区根目录的 `lib.rs`、`pelican-bike.html` 是无关杂项文件，已加入 `.gitignore`，勿提交
- 最近一次 bug 修复（标签关闭越界 panic）的教训：**即时模式遍历中不得修改集合**；
  Tauri 前端为保留模式 DOM，此类问题不再存在，但后端 commands 仍需注意状态锁
- `installer/cote.iss`（Inno Setup）在 Tauri 下可由 tauri bundler 取代；
  若保留 Inno 亦可，产物路径指向 tauri 构建的 exe
