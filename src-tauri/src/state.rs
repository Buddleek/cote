//! 应用状态：标签页（Document + 元数据）与共享数据。
//!
//! 数据流（迁移文档 §3 决策 1）：前端 CodeMirror 产生增量变更 →
//! `apply_edit` 命令写入 Document（自带撤销分组）→ 高亮/大纲按需重算。
//! 全量文本仅在打开/切换/整体替换类操作时传输。
//!
//! dirty 语义与 egui 版一致：与「已保存镜像」（文本/编码/BOM）比对，
//! 撤销回已保存状态自动变干净；Document 内部的 dirty 标记仅供内核测试。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use editor_core::document::Document;
use editor_core::newline::LineEnding;
use serde::Serialize;
use syntax_engine::ts::TsHighlighter;
use syntax_engine::LanguageDef;

/// 超过此大小的文本不做语法高亮（与 egui 版一致）。
pub const MAX_HIGHLIGHT_BYTES: usize = 1_000_000;
/// 自动保存间隔（FR-1.5）。
pub const AUTO_SAVE_SECS: u64 = 30;
/// 外部修改检测轮询间隔。
pub const EXTERNAL_CHECK_SECS: u64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalChange {
    Modified,
    Deleted,
}

pub struct TabState {
    pub id: u64,
    pub untitled_no: u32,
    pub doc: Document,
    /// 已保存镜像（文本 + 编码 + BOM），dirty 依此比对
    saved_text: String,
    saved_encoding: String,
    saved_bom: bool,
    pub language: Option<LanguageDef>,
    pub auto_language: bool,
    pub ts_highlighter: Option<TsHighlighter>,
    pub mixed_newline: bool,
    /// 打开/保存时记录的文件指纹 (mtime, len)，用于外部修改检测
    pub file_sig: Option<(Option<SystemTime>, u64)>,
    /// 外部修改状态：文件被外部程序修改 / 被删除或移动
    pub externally_changed: Option<ExternalChange>,
    /// 选区 (anchor, head)，字符索引（脚本 API 用）
    pub selection: Option<(usize, usize)>,
    /// 光标行列（1 基，会话恢复用），由 doc_status 命令上报
    pub cursor_line_col: (usize, usize),
}

impl TabState {
    pub fn new_untitled(id: u64, no: u32) -> Self {
        Self {
            id,
            untitled_no: no,
            doc: Document::new(),
            saved_text: String::new(),
            saved_encoding: "UTF-8".to_string(),
            saved_bom: false,
            language: None,
            auto_language: true,
            ts_highlighter: None,
            mixed_newline: false,
            file_sig: None,
            externally_changed: None,
            selection: None,
            cursor_line_col: (1, 1),
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.doc.text() != self.saved_text
            || self.doc.encoding_name() != self.saved_encoding
            || self.doc.had_bom() != self.saved_bom
    }

    /// 把当前内容/元数据记为「已保存」状态（打开与保存成功后调用）。
    pub fn mark_saved(&mut self) {
        self.saved_text = self.doc.text();
        self.saved_encoding = self.doc.encoding_name().to_string();
        self.saved_bom = self.doc.had_bom();
        self.doc.set_dirty(false);
    }

    /// FR-2.4：UTF-16/32 无 BOM 几乎不可用，保存时默认补 BOM；UTF-8 跟随原文件。
    pub fn bom_policy(&self) -> bool {
        match self.doc.encoding_name() {
            "UTF-16LE" | "UTF-16BE" | "UTF-32LE" | "UTF-32BE" => true,
            _ => self.doc.had_bom(),
        }
    }

    pub fn display_name(&self) -> String {
        self.doc
            .path()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("未命名 {}", self.untitled_no))
    }

    /// 设置语言并同步重建 tree-sitter 高亮器（大纲规则随语言变化）。
    pub fn set_language(&mut self, lang: Option<LanguageDef>) {
        self.ts_highlighter = lang.as_ref().and_then(TsHighlighter::for_language);
        self.language = lang;
    }
}

#[derive(Default)]
pub struct AppInner {
    pub tabs: Vec<TabState>,
    /// 当前激活标签（按下标）
    pub active: usize,
    pub next_tab_id: u64,
    pub next_untitled_no: u32,
    /// 内置 + 用户自定义语法（builtin 标记供语言菜单分组）
    pub languages: Vec<(LanguageDef, bool)>,
    /// 外观偏好："dark" / "light" / "system"
    pub theme: String,
    /// 启动待跳转 (line, col)，1 基；get_init 时消费
    pub pending_goto: Option<(usize, usize)>,
    /// 启动提示（CLI 文件不存在等），get_init 时消费
    pub notices: Vec<String>,
}

pub struct AppState(pub Mutex<AppInner>);

// ---------- 序列化载荷 ----------

#[derive(Debug, Clone, Serialize)]
pub struct TabMeta {
    pub id: u64,
    pub title: String,
    pub dirty: bool,
    pub path: Option<String>,
    pub encoding: String,
    /// "LF" / "CRLF" / "CR"
    pub newline: String,
    pub newline_label: String,
    pub mixed_newline: bool,
    pub language: Option<String>,
    pub auto_language: bool,
    /// None | "modified" | "deleted"
    pub externally_changed: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TabPayload {
    pub meta: TabMeta,
    pub text: String,
}

/// 标签列表快照：所有变更类命令都随响应携带，前端整体替换本地列表。
#[derive(Debug, Clone, Serialize)]
pub struct TabList {
    pub tabs: Vec<TabMeta>,
    pub active_id: u64,
}

impl TabList {
    pub fn of(inner: &AppInner) -> Self {
        TabList {
            tabs: inner.tabs.iter().map(TabMeta::of).collect(),
            active_id: inner.tabs.get(inner.active).map(|t| t.id).unwrap_or(0),
        }
    }
}

impl TabMeta {
    pub fn of(t: &TabState) -> Self {
        let nl = t.doc.newline();
        Self {
            id: t.id,
            title: t.display_name(),
            dirty: t.is_dirty(),
            path: t.doc.path().map(|p| p.to_string_lossy().into_owned()),
            encoding: t.doc.encoding_name().to_string(),
            newline: nl.as_str().to_string(),
            newline_label: nl.label().to_string(),
            mixed_newline: t.mixed_newline,
            language: t.language.as_ref().map(|l| l.name.clone()),
            auto_language: t.auto_language,
            externally_changed: t.externally_changed.map(|k| {
                match k {
                    ExternalChange::Modified => "modified",
                    ExternalChange::Deleted => "deleted",
                }
                .to_string()
            }),
        }
    }
}

impl TabPayload {
    pub fn of(t: &TabState) -> Self {
        Self {
            meta: TabMeta::of(t),
            text: t.doc.text(),
        }
    }
}

// ---------- 偏移转换 ----------

/// 字符索引 → UTF-16 偏移（前端 CodeMirror 的坐标系）。
pub fn char_to_utf16(text: &str, char_idx: usize) -> usize {
    text.chars().take(char_idx).map(|c| c.len_utf16()).sum()
}

/// UTF-16 偏移 → 字符索引（越界夹紧到文本末尾）。
pub fn utf16_to_char(text: &str, u16_idx: usize) -> usize {
    let mut acc = 0usize;
    for (i, c) in text.chars().enumerate() {
        if acc >= u16_idx {
            return i;
        }
        acc += c.len_utf16();
    }
    text.chars().count()
}

/// 字符索引 → 字节偏移。
pub fn char_to_byte(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(text.len())
}

// ---------- 文件与标签操作（命令层与启动逻辑共用） ----------

pub fn file_sig(path: &Path) -> Option<(Option<SystemTime>, u64)> {
    std::fs::metadata(path)
        .ok()
        .map(|m| (m.modified().ok(), m.len()))
}

/// 把文件读入指定标签页（打开/重载共用）。对照 egui 版 load_into。
pub fn load_into(
    tab: &mut TabState,
    path: &Path,
    languages: &[(LanguageDef, bool)],
) -> std::io::Result<()> {
    let bytes = std::fs::read(path)?;
    tab.doc.load_bytes(&bytes);
    tab.doc.set_path(Some(path.to_path_buf()));
    tab.mixed_newline = tab.doc.newline_info().mixed();
    let detected = syntax_engine::detect_for(
        &languages.iter().map(|(d, _)| d.clone()).collect::<Vec<_>>(),
        path,
    )
    .cloned();
    tab.set_language(detected);
    tab.auto_language = true;
    tab.file_sig = file_sig(path);
    tab.externally_changed = None;
    tab.selection = None;
    tab.cursor_line_col = (1, 1);
    tab.mark_saved();
    Ok(())
}

/// 打开路径的核心语义（对照 egui 版 open_path）：
/// 已打开 → 切换；唯一标签是干净空白未命名页 → 复用；否则新建。
/// 返回 (标签下标, 状态消息)。
pub fn open_path(inner: &mut AppInner, path: &Path) -> (usize, String) {
    for (i, t) in inner.tabs.iter().enumerate() {
        if t.doc.path().map(|p| p == path).unwrap_or(false) {
            inner.active = i;
            return (i, "该文件已在标签页中打开".to_string());
        }
    }
    if inner.tabs.len() == 1 {
        let reuse = {
            let only = &inner.tabs[0];
            only.doc.path().is_none() && !only.is_dirty() && only.doc.is_empty()
        };
        if reuse {
            match load_into(&mut inner.tabs[0], path, &inner.languages) {
                Ok(()) => {
                    inner.active = 0;
                    let t = &inner.tabs[0];
                    let lines = t.doc.line_count();
                    return (
                        0,
                        format!(
                            "已打开 {}（{}，{} 行）",
                            t.display_name(),
                            t.doc.encoding_name(),
                            lines
                        ),
                    );
                }
                Err(e) => return (0, format!("打开失败：{e}")),
            }
        }
    }
    let idx = new_tab_silent(inner);
    match load_into(&mut inner.tabs[idx], path, &inner.languages) {
        Ok(()) => {
            inner.active = idx;
            let t = &inner.tabs[idx];
            let lines = t.doc.line_count();
            (
                idx,
                format!(
                    "已打开 {}（{}，{} 行）",
                    t.display_name(),
                    t.doc.encoding_name(),
                    lines
                ),
            )
        }
        Err(e) => {
            inner.tabs.remove(idx);
            inner.active = inner.active.min(inner.tabs.len().saturating_sub(1));
            (idx, format!("打开失败：{e}"))
        }
    }
}

pub fn new_tab_silent(inner: &mut AppInner) -> usize {
    let id = inner.next_tab_id;
    let no = inner.next_untitled_no;
    inner.next_tab_id += 1;
    inner.next_untitled_no += 1;
    inner.tabs.push(TabState::new_untitled(id, no));
    inner.tabs.len() - 1
}

/// 关闭标签后调整 active 下标（对照 egui 版 finish_close）。
pub fn adjust_active_after_close(inner: &mut AppInner, idx: usize) {
    if idx < inner.active {
        inner.active -= 1;
    }
    if inner.active >= inner.tabs.len() {
        inner.active = inner.tabs.len().saturating_sub(1);
    }
    if inner.tabs.is_empty() {
        new_tab_silent(inner);
        inner.active = 0;
    }
}

/// 保存结果。
#[derive(Debug, Clone, Serialize)]
pub enum SaveOutcome {
    Saved {
        losses: usize,
    },
    /// 文档无路径，需要另存为对话框
    NeedPath,
    Failed(String),
}

/// 保存指定标签页。quiet=true 时静默（自动保存用），失败不产消息。
pub fn save_tab(inner: &mut AppInner, idx: usize, quiet: bool) -> Option<SaveOutcome> {
    if idx >= inner.tabs.len() {
        return None;
    }
    let Some(path) = inner.tabs[idx].doc.path().cloned() else {
        return (!quiet).then_some(SaveOutcome::NeedPath);
    };
    let (bytes, losses, bom) = {
        let t = &inner.tabs[idx];
        let bom = t.bom_policy();
        let out = t.doc.to_bytes(bom);
        (out.bytes, out.losses.len(), bom)
    };
    match platform::fs::write_atomic(&path, &bytes) {
        Ok(()) => {
            let t = &mut inner.tabs[idx];
            t.doc.set_had_bom(bom);
            t.file_sig = file_sig(&path);
            t.externally_changed = None;
            t.mark_saved();
            (!quiet).then_some(SaveOutcome::Saved { losses })
        }
        Err(e) => (!quiet).then_some(SaveOutcome::Failed(format!("保存失败：{e}"))),
    }
}

/// 外部修改检测（FR-1.4）。返回 Some((tab_id, kind)) 表示刚检测到变化。
pub fn check_external_changed(inner: &mut AppInner, idx: usize) -> Option<(u64, &'static str)> {
    if idx >= inner.tabs.len() || inner.tabs[idx].externally_changed.is_some() {
        return None;
    }
    let path = inner.tabs[idx].doc.path().cloned()?;
    inner.tabs[idx].file_sig?;
    let kind = match std::fs::metadata(&path) {
        Ok(m) => {
            let sig = (m.modified().ok(), m.len());
            if inner.tabs[idx].file_sig != Some(sig) {
                Some((ExternalChange::Modified, "modified"))
            } else {
                None
            }
        }
        Err(_) => Some((ExternalChange::Deleted, "deleted")),
    };
    if let Some((k, name)) = kind {
        inner.tabs[idx].externally_changed = Some(k);
        return Some((inner.tabs[idx].id, name));
    }
    None
}

/// 自动保存（FR-1.5）：保存所有有路径的脏标签页 + 会话。
pub fn autosave_all(inner: &mut AppInner) {
    for i in 0..inner.tabs.len() {
        if inner.tabs[i].is_dirty() && inner.tabs[i].doc.path().is_some() {
            save_tab(inner, i, true);
        }
    }
    save_session(inner);
}

/// 会话持久化（含光标位置与外观偏好）。
pub fn save_session(inner: &AppInner) {
    let tabs: Vec<platform::session::SessionTab> = inner
        .tabs
        .iter()
        .filter_map(|t| {
            t.doc.path().map(|p| platform::session::SessionTab {
                path: p.to_string_lossy().into_owned(),
                line: Some(t.cursor_line_col.0),
                col: Some(t.cursor_line_col.1),
            })
        })
        .collect();
    let active = inner.active.min(inner.tabs.len().saturating_sub(1));
    platform::session::save_session(&platform::session::Session {
        tabs,
        active,
        theme: Some(inner.theme.clone()),
    });
}

/// 启动时恢复会话（对照 egui 版 restore_session，含空标签清理与跳转位置）。
pub fn restore_session(inner: &mut AppInner) {
    let Some(s) = platform::session::load_session() else {
        return;
    };
    if let Some(theme) = &s.theme {
        inner.theme = theme.clone();
    }
    for st in &s.tabs {
        let p = PathBuf::from(&st.path);
        if p.exists() {
            let idx = new_tab_silent(inner);
            if load_into(&mut inner.tabs[idx], &p, &inner.languages).is_err() {
                inner.tabs.remove(idx);
            }
        }
    }
    if !inner.tabs.is_empty() {
        inner.active = s.active.min(inner.tabs.len() - 1);
        // 跳转位置取自激活标签页（egui 版教训：不能用第一个标签页的行列）
        if let Some(st) = s.tabs.get(inner.active) {
            if st.line.is_some() {
                inner.pending_goto = Some((st.line.unwrap_or(1), st.col.unwrap_or(1)));
            }
        }
    }
    // 恢复出文件标签时移除启动时的空白未命名标签（否则会残留一个空标签）
    if inner.tabs.len() > 1 && inner.tabs[0].doc.path().is_none() && !inner.tabs[0].is_dirty() {
        inner.tabs.remove(0);
        inner.active = inner.active.saturating_sub(1);
    }
}

pub fn newline_from_str(s: &str) -> Option<LineEnding> {
    match s {
        "LF" => Some(LineEnding::Lf),
        "CRLF" => Some(LineEnding::Crlf),
        "CR" => Some(LineEnding::Cr),
        _ => None,
    }
}
