//! editor-ui：eframe/egui 界面层。
//!
//! M1 架构：
//! - 每个标签页持有一个 `Document`（editor-core 的 rope + 撤销栈）；
//! - 每帧把 TextEdit 产生的文本变更 diff 同步回 Document（前缀/后缀对齐），
//!   Ctrl+Z / Ctrl+Y 由内核撤销接管（消费按键事件，避免与 egui 内建撤销叠加）；
//! - 多标签页、自动保存（30s）、崩溃会话恢复、外部修改检测与重载横幅、
//!   关闭/退出未保存确认、跳转到行、用户自定义语法（JSON）。
//!
//! 仍属边界（后续收敛）：同步文件对话框、mtime 轮询（而非 notify 推送）、
//! 脚本系统、打印、i18n 资源化。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use eframe::egui;
use eframe::egui::Color32;

use editor_core::document::Document;
use editor_core::encoding;
use editor_core::newline::LineEnding;
use editor_core::search::{self, Match, SearchOptions};
use editor_core::text as tx;
use syntax_engine::LanguageDef;

/// 超过此大小的文本不做语法高亮（扫描分词全量 O(n)，大文件每帧布局不划算）。
const MAX_HIGHLIGHT_BYTES: usize = 1_000_000;
/// 自动保存间隔（FR-1.5）。
const AUTO_SAVE_SECS: u64 = 30;
const MAIN_EDIT_ID: &str = "main_text_edit";
const SEARCH_BOX_ID: &str = "search_box";

/// 待应用的光标操作（在下一帧 TextEdit 渲染前写入其状态）。
enum PendingCursor {
    /// 光标放到字符索引处
    At(usize),
    /// 选中 [start, end)
    Select(usize, usize),
}

/// 单词补全弹窗状态（FR-6.1）。
struct Completion {
    items: Vec<String>,
    selected: usize,
}

// ---------- 标签页 ----------

struct Tab {
    /// TextEdit 状态隔离键（每个标签页独立的光标/滚动状态）
    edit_key: u64,
    untitled_no: u32,
    doc: Document,
    /// UI 工作副本（TextEdit 的编辑源），每帧 diff 回 Document
    text: String,
    /// 上次同步进 Document 的镜像
    synced_text: String,
    saved_text: String,
    /// 缓存的脏标记（编辑/撤销/保存时更新，避免每帧 O(n) 比较）
    dirty: bool,
    /// 上一帧 TextEdit 是否改动了文本（门控 diff 同步，空闲帧零开销）
    edit_changed: bool,
    /// 选区 (anchor, head)，字符索引（脚本 API 与未来多点编辑用）
    sel: Option<(usize, usize)>,
    language: Option<LanguageDef>,
    auto_language: bool,
    /// tree-sitter 高亮器（语言支持且有有效查询时存在；否则回退扫描器）
    ts_highlighter: Option<syntax_engine::ts::TsHighlighter>,
    /// 高亮 spans 缓存：(文本, spans)，文本变化才重算；Arc 供布局器无借用共享
    spans: Option<(String, Arc<Vec<syntax_engine::Span>>)>,
    mixed_newline: bool,
    /// 打开/保存时记录的文件指纹（mtime, len），用于外部修改检测
    file_sig: Option<(Option<SystemTime>, u64)>,
    externally_changed: bool,

    matches: Vec<Match>,
    matches_sig: Option<(String, SearchOptions, usize, usize, usize)>,
    match_idx: usize,

    cursor_char: usize,
    cursor_line_col: (usize, usize),
    cursor_cache_key: Option<(usize, usize)>,
    stats_cache: Option<((usize, usize, usize), editor_core::text::TextStats)>,
    /// 大纲缓存，键：(文本长度, undo 深度, redo 深度)
    outline_cache: Option<((usize, usize, usize), Vec<syntax_engine::OutlineItem>)>,
}

impl Tab {
    fn new_untitled(edit_key: u64, no: u32) -> Self {
        let doc = Document::from_text("");
        Self {
            edit_key,
            untitled_no: no,
            doc,
            text: String::new(),
            synced_text: String::new(),
            saved_text: String::new(),
            dirty: false,
            edit_changed: false,
            sel: None,
            language: None,
            auto_language: true,
            ts_highlighter: None,
            spans: None,
            mixed_newline: false,
            file_sig: None,
            externally_changed: false,
            matches: vec![],
            matches_sig: None,
            match_idx: 0,
            cursor_char: 0,
            cursor_line_col: (1, 1),
            cursor_cache_key: None,
            stats_cache: None,
            outline_cache: None,
        }
    }

    fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// 重新计算脏标记（仅在文本真实变化时调用，O(n)）。
    fn update_dirty(&mut self) {
        self.dirty = self.text.len() != self.saved_text.len() || self.text != self.saved_text;
    }

    fn display_name(&self) -> String {
        self.doc
            .path()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("未命名 {}", self.untitled_no))
    }

    /// 设置语言并同步重建 tree-sitter 高亮器。
    fn set_language(tab: &mut Tab, lang: Option<LanguageDef>) {
        tab.ts_highlighter = lang.as_ref().and_then(syntax_engine::ts::TsHighlighter::for_language);
        tab.spans = None; // 失效，下一帧重算
        tab.outline_cache = None; // 大纲规则随语言变化
        tab.language = lang;
    }

    /// FR-2.4：UTF-16/32 无 BOM 几乎不可用，保存时默认补 BOM；UTF-8 跟随原文件。
    fn bom_policy(&self) -> bool {
        match self.doc.encoding_name() {
            "UTF-16LE" | "UTF-16BE" | "UTF-32LE" | "UTF-32BE" => true,
            _ => self.doc.had_bom(),
        }
    }

    /// 以 Document 当前内容为准刷新 UI 文本（undo/redo/重载后调用）。
    fn reload_from_doc(&mut self) {
        self.text = self.doc.text();
        self.synced_text = self.text.clone();
        self.update_dirty();
    }

    /// 把本帧 TextEdit 产生的文本变更 diff 同步进 Document。
    /// 字节级公共前缀/后缀对齐，O(n) memcmp；编辑内核据此维护 rope 与撤销分组。
    fn sync_diff_to_doc(&mut self) {
        if self.text == self.synced_text {
            return;
        }
        let a = self.text.as_bytes();
        let b = self.synced_text.as_bytes();
        let min = a.len().min(b.len());
        let mut p = 0usize;
        while p < min && a[p] == b[p] {
            p += 1;
        }
        while p > 0 && !self.text.is_char_boundary(p) {
            p -= 1;
        }
        let mut s = 0usize;
        while s < a.len() - p && s < b.len() - p && a[a.len() - 1 - s] == b[b.len() - 1 - s] {
            s += 1;
        }
        while s > 0
            && (!self.text.is_char_boundary(a.len() - s)
                || !self.synced_text.is_char_boundary(b.len() - s))
        {
            s -= 1;
        }
        let old = &self.synced_text[p..self.synced_text.len() - s];
        let new = &self.text[p..self.text.len() - s];
        let char_pos = self.synced_text[..p].chars().count();
        if !old.is_empty() {
            self.doc.delete_range(char_pos, char_pos + old.chars().count());
        }
        if !new.is_empty() {
            self.doc.insert_text(char_pos, new);
        }
        self.synced_text = self.text.clone();
        self.update_dirty();
    }

    fn cached_stats(&mut self) -> editor_core::text::TextStats {
        // 键含撤销/重做深度：等长替换（脚本 setText 等）长度不变但内容已变
        let key = (self.text.len(), self.doc.undo_depth(), self.doc.redo_depth());
        if self.stats_cache.as_ref().map(|(k, _)| *k) != Some(key) {
            let st = tx::stats(&self.text);
            self.stats_cache = Some((key, st));
        }
        self.stats_cache.unwrap().1
    }

    fn refresh_matches(&mut self, query: &str, opts: SearchOptions) {
        let sig = (
            query.to_string(),
            opts,
            self.text.len(),
            self.doc.undo_depth(),
            self.doc.redo_depth(),
        );
        if self.matches_sig.as_ref() != Some(&sig) {
            self.matches = search::find_all(&self.text, query, &opts).unwrap_or_default();
            self.matches_sig = Some(sig);
            self.match_idx = 0;
        }
    }

    /// 对整篇文本应用一个变换（作为单步撤销），并刷新 UI 文本。
    fn apply_transform(&mut self, f: impl FnOnce(&str) -> String) {
        let new_text = f(&self.text);
        self.doc.replace_all_text(&new_text);
        self.reload_from_doc();
    }
}

// ---------- 应用 ----------

#[derive(Default)]
pub struct EditorApp {
    tabs: Vec<Tab>,
    active: usize,
    next_edit_key: u64,
    next_untitled: u32,
    /// CLI --line/--column 或会话恢复的待跳转位置（1 基行列）
    pending_goto: Option<(usize, usize)>,
    pending_cursor: Option<PendingCursor>,
    user_syntaxes: Vec<LanguageDef>,

    search_open: bool,
    just_opened_search: bool,
    search_query: String,
    replace_query: String,
    search_opts: SearchOptions,
    goto_open: bool,
    goto_input: String,

    pending_close: Option<usize>,
    pending_exit: bool,
    scripts: Vec<script_host::ScriptInfo>,
    show_outline: bool,
    completion: Option<Completion>,

    last_autosave: Option<Instant>,
    status_msg: Option<(String, Instant)>,
    focus_main: bool,
    last_title: String,
}

impl EditorApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        platform::fonts::install_cjk_fallback(&cc.egui_ctx);
        cc.egui_ctx
            .options_mut(|o| o.theme_preference = egui::ThemePreference::System);
        // 全局字体层级（菜单栏 15.5 基准的放大版）
        cc.egui_ctx.style_mut(|style| {
            style.text_styles.insert(
                egui::TextStyle::Monospace,
                egui::FontId::monospace(16.0),
            );
            style
                .text_styles
                .insert(egui::TextStyle::Body, egui::FontId::proportional(15.5));
            style
                .text_styles
                .insert(egui::TextStyle::Button, egui::FontId::proportional(15.5));
            style
                .text_styles
                .insert(egui::TextStyle::Small, egui::FontId::proportional(13.0));
        });
        let mut app = Self {
            next_edit_key: 1,
            next_untitled: 1,
            last_autosave: Some(Instant::now()),
            // 大纲侧栏默认关闭（查看 → 大纲 开启）
            show_outline: false,
            ..Default::default()
        };
        app.user_syntaxes = platform::session::config_dir()
            .map(|d| syntax_engine::load_user_syntaxes(&d.join("syntaxes")))
            .unwrap_or_default();
        app.scripts = platform::session::config_dir()
            .map(|d| script_host::load_scripts(&d.join("scripts")))
            .unwrap_or_default();
        app.new_tab_silent();
        app
    }

    fn status(&mut self, msg: impl Into<String>) {
        self.status_msg = Some((msg.into(), Instant::now()));
    }

    fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    fn main_edit_id(&self) -> egui::Id {
        egui::Id::new(MAIN_EDIT_ID).with(self.tabs[self.active].edit_key)
    }

    // ---------- 标签页管理 ----------

    fn new_tab_silent(&mut self) -> usize {
        let key = self.next_edit_key;
        let no = self.next_untitled;
        self.next_edit_key += 1;
        self.next_untitled += 1;
        self.tabs.push(Tab::new_untitled(key, no));
        self.tabs.len() - 1
    }

    fn new_tab(&mut self) {
        let idx = self.new_tab_silent();
        self.active = idx;
        self.focus_main = true;
        self.save_session();
    }

    fn switch_to(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        self.active = idx;
        self.completion = None; // 补全跟随原标签页上下文
        self.tabs[idx].cursor_cache_key = None; // 重新计算行列显示
        self.check_external_changed(idx);
        self.save_session(); // 记住活动标签，会话恢复时还原
    }

    fn request_close_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        if self.tabs[idx].is_dirty() {
            self.pending_close = Some(idx);
        } else {
            self.finish_close(idx);
        }
    }

    fn finish_close(&mut self, idx: usize) {
        self.pending_close = None;
        if idx >= self.tabs.len() {
            return;
        }
        self.tabs.remove(idx);
        // 被关闭的标签在活动标签之前：active 左移保持指向同一标签
        // （否则会跳到右边错误的标签上）
        if idx < self.active {
            self.active -= 1;
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
        if self.tabs.is_empty() {
            self.new_tab_silent();
            self.active = 0;
        }
        self.save_session();
    }

    fn request_exit(&mut self, ctx: &egui::Context) {
        if self.tabs.iter().any(|t| t.is_dirty()) {
            self.pending_exit = true;
        } else {
            self.save_session();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    // ---------- 文件 ----------

    /// 把文件读入指定标签页（打开/重载共用）。
    fn load_into(tab: &mut Tab, path: &Path) -> std::io::Result<()> {
        let bytes = std::fs::read(path)?;
        tab.doc.load_bytes(&bytes);
        tab.doc.set_path(Some(path.to_path_buf()));
        tab.reload_from_doc();
        tab.saved_text = tab.text.clone();
        let mut all = syntax_engine::builtin().to_vec();
        all.extend(
            platform::session::config_dir()
                .map(|d| syntax_engine::load_user_syntaxes(&d.join("syntaxes")))
                .unwrap_or_default(),
        );
        Tab::set_language(tab, syntax_engine::detect_for(&all, path).cloned());
        tab.auto_language = true;
        tab.mixed_newline = tab.doc.newline_info().mixed();
        tab.file_sig = file_sig(path);
        tab.externally_changed = false;
        tab.dirty = false;
        tab.sel = None;
        tab.matches.clear();
        tab.matches_sig = None;
        tab.match_idx = 0;
        tab.stats_cache = None;
        tab.cursor_char = 0;
        tab.cursor_line_col = (1, 1);
        tab.cursor_cache_key = None;
        Ok(())
    }

    fn open_path(&mut self, path: &Path) {
        // 已打开则切换过去
        for (i, t) in self.tabs.iter().enumerate() {
            if t.doc.path().map(|p| p == path).unwrap_or(false) {
                self.switch_to(i);
                self.status("该文件已在标签页中打开");
                return;
            }
        }
        // 唯一标签是干净、空白的未命名页 → 直接复用它加载（不残留空标签）
        if self.tabs.len() == 1 {
            let only = &mut self.tabs[0];
            if only.doc.path().is_none() && !only.is_dirty() && only.text.is_empty() {
                match Self::load_into(only, path) {
                    Ok(()) => {
                        self.active = 0;
                        self.focus_main = true;
                        let t = &self.tabs[0];
                        let lines = t.text.lines().count();
                        self.status(format!(
                            "已打开 {}（{}，{} 行）",
                            t.display_name(),
                            t.doc.encoding_name(),
                            lines
                        ));
                        self.save_session();
                    }
                    Err(e) => self.status(format!("打开失败：{e}")),
                }
                return;
            }
        }
        let idx = self.new_tab_silent();
        match Self::load_into(&mut self.tabs[idx], path) {
            Ok(()) => {
                self.active = idx;
                self.focus_main = true;
                let t = &self.tabs[idx];
                let lines = t.text.lines().count();
                self.status(format!(
                    "已打开 {}（{}，{} 行）",
                    t.display_name(),
                    t.doc.encoding_name(),
                    lines
                ));
                self.save_session();
            }
            Err(e) => {
                self.tabs.remove(idx);
                self.status(format!("打开失败：{e}"));
            }
        }
    }

    fn open_dialog(&mut self) {
        if let Some(path) = platform::dialogs::pick_open_file() {
            self.open_path(&path);
        }
    }

    fn reload_active(&mut self) {
        let dirty = self.tab().is_dirty();
        let path = self.tab().doc.path().cloned();
        match path {
            Some(p) if !dirty => {
                if Self::load_into(self.tab_mut(), &p).is_ok() {
                    self.status(format!("已重新加载 {}", self.tab().display_name()));
                } else {
                    self.status("重新加载失败");
                }
            }
            Some(_) => self.status("文档有未保存修改，重新加载前请先保存"),
            None => self.status("当前文档尚未保存到磁盘"),
        }
    }

    fn save(&mut self) {
        self.save_tab(self.active, false);
    }

    fn save_as(&mut self) {
        let default_name = self.tab().display_name();
        let default_name = if default_name.starts_with("未命名") {
            "untitled.txt".to_string()
        } else {
            default_name
        };
        if let Some(path) = platform::dialogs::pick_save_file(Some(&default_name)) {
            self.tabs[self.active].doc.set_path(Some(path));
            self.save_tab(self.active, false);
        }
    }

    /// 保存指定标签页。quiet=true 时只保存不弹状态（自动保存用）。
    fn save_tab(&mut self, idx: usize, quiet: bool) {
        if idx >= self.tabs.len() {
            return;
        }
        let Some(path) = self.tabs[idx].doc.path().cloned() else {
            if !quiet && idx == self.active {
                self.save_as();
            }
            return;
        };
        let (bytes, losses, bom) = {
            let t = &self.tabs[idx];
            let bom = t.bom_policy();
            let out = t.doc.to_bytes(bom);
            (out.bytes, out.losses.len(), bom)
        };
        match platform::fs::write_atomic(&path, &bytes) {
            Ok(()) => {
                let t = &mut self.tabs[idx];
                t.doc.set_had_bom(bom);
                t.saved_text = t.text.clone();
                t.dirty = false;
                t.file_sig = file_sig(&path);
                t.externally_changed = false;
                if !quiet {
                    let enc = t.doc.encoding_name().to_string();
                    let name = t.display_name();
                    self.status(if losses > 0 {
                        format!("已保存（警告：{losses} 个字符无法用 {enc} 表示，已丢弃）")
                    } else {
                        format!("已保存 {name}（{enc}）")
                    });
                }
            }
            Err(e) => {
                if !quiet {
                    self.status(format!("保存失败：{e}"));
                }
            }
        }
    }

    // ---------- 外部修改检测（FR-1.4）与自动保存（FR-1.5） ----------

    fn check_external_changed(&mut self, idx: usize) {
        if idx >= self.tabs.len() || self.tabs[idx].externally_changed {
            return;
        }
        let Some(path) = self.tabs[idx].doc.path().cloned() else { return };
        if self.tabs[idx].file_sig.is_none() {
            return;
        }
        match std::fs::metadata(&path) {
            Ok(m) => {
                let sig = (m.modified().ok(), m.len());
                if self.tabs[idx].file_sig != Some(sig) {
                    self.tabs[idx].externally_changed = true;
                    self.status("⚠ 文件已被外部程序修改");
                }
            }
            Err(_) => {
                self.tabs[idx].externally_changed = true;
                self.status("⚠ 文件已被删除或移动");
            }
        }
    }

    fn maybe_autosave(&mut self) {
        if self.last_autosave.map(|t| t.elapsed() < Duration::from_secs(AUTO_SAVE_SECS)).unwrap_or(false) {
            return;
        }
        self.last_autosave = Some(Instant::now());
        for i in 0..self.tabs.len() {
            if self.tabs[i].is_dirty() && self.tabs[i].doc.path().is_some() {
                self.save_tab(i, true);
            }
        }
        self.save_session();
    }

    fn save_session(&self) {
        let tabs: Vec<platform::session::SessionTab> = self
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
        let active = self.active.min(self.tabs.len().saturating_sub(1));
        platform::session::save_session(&platform::session::Session { tabs, active });
    }

    fn restore_session(&mut self) {
        let Some(s) = platform::session::load_session() else { return };
        for st in &s.tabs {
            let p = PathBuf::from(&st.path);
            if p.exists() {
                let idx = self.new_tab_silent();
                if Self::load_into(&mut self.tabs[idx], &p).is_err() {
                    self.tabs.remove(idx);
                }
            }
        }
        if !self.tabs.is_empty() {
            self.active = s.active.min(self.tabs.len() - 1);
            // 跳转位置取自激活标签页（曾误用第一个标签页的行列）
            if let Some(st) = s.tabs.get(self.active) {
                if st.line.is_some() {
                    self.pending_goto = Some((st.line.unwrap_or(1), st.col.unwrap_or(1)));
                }
            }
        }
        // 恢复出了文件标签时移除启动时的空白未命名标签（否则会残留一个空标签）
        if self.tabs.len() > 1 && self.tabs[0].doc.path().is_none() && !self.tabs[0].is_dirty() {
            self.tabs.remove(0);
            self.active = self.active.saturating_sub(1);
        }
    }

    // ---------- 撤销 / 重做（内核接管） ----------

    fn do_undo(&mut self) {
        self.tab_mut().sync_diff_to_doc();
        let t = self.tab_mut();
        if let Some(cur) = t.doc.undo() {
            t.reload_from_doc();
            self.pending_cursor = Some(PendingCursor::At(cur));
            self.focus_main = true;
        } else {
            self.status("没有可撤销的操作");
        }
    }

    fn do_redo(&mut self) {
        self.tab_mut().sync_diff_to_doc();
        let t = self.tab_mut();
        if let Some(cur) = t.doc.redo() {
            t.reload_from_doc();
            self.pending_cursor = Some(PendingCursor::At(cur));
            self.focus_main = true;
        } else {
            self.status("没有可重做的操作");
        }
    }

    // ---------- 查找 / 替换 / 跳转 ----------

    fn refresh_matches(&mut self) {
        let (q, opts) = (self.search_query.clone(), self.search_opts);
        self.tab_mut().refresh_matches(&q, opts);
    }

    fn jump_next(&mut self) {
        let t = self.tab_mut();
        if t.matches.is_empty() {
            return;
        }
        let idx = t.matches.partition_point(|m| m.start < t.cursor_char);
        let idx = if idx < t.matches.len() { idx } else { 0 };
        let m = t.matches[idx];
        t.match_idx = idx;
        t.cursor_char = m.end;
        self.pending_cursor = Some(PendingCursor::Select(m.start, m.end));
        self.focus_main = true;
    }

    fn jump_prev(&mut self) {
        let t = self.tab_mut();
        if t.matches.is_empty() {
            return;
        }
        let idx = t.matches.partition_point(|m| m.start < t.cursor_char);
        let idx = if idx == 0 { t.matches.len() - 1 } else { idx - 1 };
        let m = t.matches[idx];
        t.match_idx = idx;
        t.cursor_char = m.end;
        self.pending_cursor = Some(PendingCursor::Select(m.start, m.end));
        self.focus_main = true;
    }

    fn replace_everything(&mut self) {
        if self.search_query.is_empty() {
            self.status("查找内容为空");
            return;
        }
        let (q, r, opts) =
            (self.search_query.clone(), self.replace_query.clone(), self.search_opts);
        let t = self.tab_mut();
        match search::replace_all(&t.text, &q, &r, &opts) {
            Ok((new_text, n)) => {
                t.doc.replace_all_text(&new_text);
                t.reload_from_doc();
                self.status(format!("已替换 {n} 处"));
            }
            Err(e) => self.status(format!("替换失败：{e}")),
        }
    }

    fn do_goto(&mut self) {
        let Ok(line) = self.goto_input.trim().parse::<usize>() else {
            self.status("请输入有效行号");
            return;
        };
        if line == 0 {
            return;
        }
        let ch = self.tab().doc.line_to_char(line - 1);
        self.pending_cursor = Some(PendingCursor::At(ch));
        self.focus_main = true;
        self.goto_open = false;
    }

    // ---------- 脚本（FR-9） ----------

    /// 在当前标签页上执行脚本。文本变更走单步撤销；异常不影响主程序。
    fn run_script(&mut self, idx: usize) {
        if idx >= self.scripts.len() {
            return;
        }
        self.tab_mut().sync_diff_to_doc();
        let api = {
            let t = self.tab();
            script_host::ScriptApi {
                text: t.text.clone(),
                selection: t.sel,
                status: None,
            }
        };
        let source = self.scripts[idx].source.clone();
        let name = self.scripts[idx].name.clone();
        match script_host::run_script(&source, &api) {
            Ok(out) => {
                let t = self.tab_mut();
                if out.text != t.text {
                    t.doc.replace_all_text(&out.text);
                    t.reload_from_doc();
                }
                if let Some((a, h)) = out.selection {
                    self.pending_cursor = Some(PendingCursor::Select(a.min(h), a.max(h)));
                }
                self.status(out.status.unwrap_or_else(|| format!("脚本 {name} 执行完成")));
            }
            Err(e) => self.status(format!("脚本 {name} 出错：{e}")),
        }
    }

    // ---------- 单词补全（FR-6.1） ----------

    fn open_completion(&mut self) {
        self.tab_mut().sync_diff_to_doc();
        let cur = self.tab().cursor_char;
        let before: String = self.tab().text.chars().take(cur).collect();
        let n = before.chars().rev().take_while(|c| tx::is_word_char(*c)).count();
        if n == 0 {
            self.completion = None;
            self.status("光标处无单词前缀");
            return;
        }
        let prefix: String = before.chars().skip(before.chars().count() - n).collect();
        let items = tx::word_completions(&self.tab().text, &prefix, 30);
        if items.is_empty() {
            self.completion = None;
            self.status("无补全候选");
        } else {
            self.completion = Some(Completion { items, selected: 0 });
        }
    }

    fn apply_completion(&mut self, item: String) {
        let t = self.tab_mut();
        let cur = t.cursor_char;
        let before: String = t.text.chars().take(cur).collect();
        let n = before.chars().rev().take_while(|c| tx::is_word_char(*c)).count();
        let start = cur - n;
        let byte_start = byte_of_char(&t.text, start);
        let byte_end = byte_of_char(&t.text, cur);
        let new_cursor = start + item.chars().count();
        t.text.replace_range(byte_start..byte_end, &item);
        t.edit_changed = true; // 触发下一帧 diff 同步进内核
        self.completion = None;
        self.pending_cursor = Some(PendingCursor::At(new_cursor));
        self.focus_main = true;
    }

    // ---------- 菜单子面板 ----------

    fn encoding_menu(&mut self, ui: &mut egui::Ui) {
        for enc in encoding::COMMON_ENCODINGS {
            let current = self.tab().doc.encoding_name() == *enc;
            let label = if current { format!("● {enc}") } else { format!("　{enc}") };
            if ui.button(label).clicked() {
                self.apply_encoding(enc);
                ui.close();
            }
        }
    }

    /// 切换编码：干净且有文件 → 立即以新编码重新解码；否则仅设为保存目标编码（FR-2.2）。
    fn apply_encoding(&mut self, enc: &str) {
        if self.tab().doc.encoding_name() == enc {
            return;
        }
        if !self.tab().is_dirty() {
            if let Some(path) = self.tab().doc.path().cloned() {
                if let Ok(bytes) = std::fs::read(&path) {
                    let d = encoding::decode_with(&bytes, enc);
                    let t = self.tab_mut();
                    // 必须用显式解码结果加载：load_bytes 会自动检测，
                    // 与用户手选编码不一致时显示与保存都会错
                    t.doc.load_decoded(&d.text, &d.encoding, d.had_bom);
                    t.reload_from_doc();
                    t.saved_text = t.text.clone();
                    t.dirty = false;
                    t.mixed_newline = t.doc.newline_info().mixed();
                    self.status(format!("已以 {} 重新解码", d.encoding));
                    return;
                }
            }
        }
        self.tab_mut().doc.set_encoding(enc);
        self.status(format!("保存时将使用 {enc}（有损编码建议先确认不可表示字符）"));
    }

    fn newline_menu(&mut self, ui: &mut egui::Ui) {
        for le in [LineEnding::Lf, LineEnding::Crlf, LineEnding::Cr] {
            let current = self.tab().doc.newline() == le;
            let label = if current {
                format!("● {}", le.label())
            } else {
                format!("　{}", le.label())
            };
            if ui.button(label).clicked() {
                {
                    let t = self.tab_mut();
                    t.doc.set_newline(le);
                    t.reload_from_doc();
                    t.mixed_newline = t.doc.newline_info().mixed();
                }
                self.status(format!("换行符已转换为 {}", le.label()));
                ui.close();
            }
        }
    }

    fn language_menu(&mut self, ui: &mut egui::Ui) {
        let auto = self.tab().auto_language;
        let auto_label = if auto { "● 自动检测" } else { "　自动检测" };
        if ui.button(auto_label).clicked() {
            let mut all = syntax_engine::builtin().to_vec();
            all.extend(self.user_syntaxes.iter().cloned());
            let path = self.tab().doc.path().cloned();
            let detected = path.and_then(|p| syntax_engine::detect_for(&all, &p).cloned());
            let t = self.tab_mut();
            t.auto_language = true;
            Tab::set_language(t, detected);
            ui.close();
        }
        let builtin: Vec<LanguageDef> = syntax_engine::builtin().to_vec();
        let user: Vec<LanguageDef> = self.user_syntaxes.clone();
        for (group_label, defs) in [("内置", &builtin), ("自定义", &user)] {
            if defs.is_empty() {
                continue;
            }
            if group_label == "自定义" {
                ui.separator();
            }
            for l in defs {
                let t = self.tab();
                let selected = !t.auto_language && t.language.as_ref().map(|x| &x.name) == Some(&l.name);
                let label = if selected { format!("● {}", l.name) } else { format!("　{}", l.name) };
                if ui.button(label).clicked() {
                    let t = self.tab_mut();
                    t.auto_language = false;
                    Tab::set_language(t, Some(l.clone()));
                    ui.close();
                }
            }
        }
    }

    fn edit_menu(&mut self, ui: &mut egui::Ui) {
        if ui.add(egui::Button::new("撤销").shortcut_text("Ctrl+Z")).clicked() {
            self.do_undo();
            ui.close();
        }
        if ui.add(egui::Button::new("重做").shortcut_text("Ctrl+Y")).clicked() {
            self.do_redo();
            ui.close();
        }
        ui.separator();
        if ui.button("转为大写").clicked() {
            self.tab_mut().apply_transform(tx::to_upper);
            ui.close();
        }
        if ui.button("转为小写").clicked() {
            self.tab_mut().apply_transform(tx::to_lower);
            ui.close();
        }
        if ui.button("词首大写").clicked() {
            self.tab_mut().apply_transform(tx::to_title_case);
            ui.close();
        }
        ui.separator();
        if ui.button("全角 → 半角").clicked() {
            self.tab_mut().apply_transform(tx::full_to_half);
            ui.close();
        }
        if ui.button("半角 → 全角").clicked() {
            self.tab_mut().apply_transform(tx::half_to_full);
            ui.close();
        }
        ui.menu_button("Unicode 规范化", |ui| {
            for (label, form) in [
                ("NFC", tx::NormalForm::Nfc),
                ("NFD", tx::NormalForm::Nfd),
                ("NFKC", tx::NormalForm::Nfkc),
                ("NFKD", tx::NormalForm::Nfkd),
            ] {
                if ui.button(label).clicked() {
                    self.tab_mut().apply_transform(|s| tx::normalize(s, form));
                    ui.close();
                }
            }
        });
        ui.separator();
        if ui.button("Tab → 空格 (4)").clicked() {
            self.tab_mut().apply_transform(|s| tx::tabs_to_spaces(s, 4));
            ui.close();
        }
        if ui.button("行首空格 → Tab (4)").clicked() {
            self.tab_mut().apply_transform(|s| tx::leading_spaces_to_tabs(s, 4));
            ui.close();
        }
        ui.separator();
        if ui.button("行排序（升序）").clicked() {
            self.tab_mut().apply_transform(|s| tx::sort_lines(s, true, false, false));
            ui.close();
        }
        if ui.button("行排序（降序）").clicked() {
            self.tab_mut().apply_transform(|s| tx::sort_lines(s, false, false, false));
            ui.close();
        }
        if ui.button("行去重").clicked() {
            self.tab_mut().apply_transform(tx::unique_lines);
            ui.close();
        }
        if ui.button("行反转").clicked() {
            self.tab_mut().apply_transform(tx::reverse_lines);
            ui.close();
        }
        if ui.button("去行尾空白").clicked() {
            self.tab_mut().apply_transform(tx::trim_trailing_whitespace);
            ui.close();
        }
        ui.separator();
        let line = self.tab().cursor_line_col.0.saturating_sub(1);
        if ui.button("删除当前行").clicked() {
            self.tab_mut().apply_transform(|s| tx::delete_line(s, line));
            ui.close();
        }
        if ui.button("复制当前行").clicked() {
            self.tab_mut().apply_transform(|s| tx::duplicate_line(s, line));
            ui.close();
        }
        if ui.button("当前行上移").clicked() {
            self.tab_mut().apply_transform(|s| tx::move_line(s, line, -1));
            ui.close();
        }
        if ui.button("当前行下移").clicked() {
            self.tab_mut().apply_transform(|s| tx::move_line(s, line, 1));
            ui.close();
        }
        ui.separator();
        if ui.button("切换行注释").clicked() {
            let prefix = self
                .tab()
                .language
                .as_ref()
                .and_then(|l| l.line_comment.clone())
                .unwrap_or_else(|| "//".to_string());
            self.tab_mut().apply_transform(|s| tx::toggle_line_comment(s, &prefix));
            ui.close();
        }
    }

    // ---------- 快捷键 ----------

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        // 撤销/重做必须先于 TextEdit 消费按键，避免 egui 内建撤销叠加
        let (undo, redo) = ctx.input_mut(|i| {
            let undo = i.key_pressed(egui::Key::Z) && i.modifiers.command && !i.modifiers.shift;
            let redo = (i.key_pressed(egui::Key::Y) && i.modifiers.command)
                || (i.key_pressed(egui::Key::Z) && i.modifiers.command && i.modifiers.shift);
            if undo || redo {
                i.events.retain(|e| {
                    !matches!(
                        e,
                        egui::Event::Key { key: egui::Key::Z | egui::Key::Y, modifiers: m, pressed: true, .. }
                            if m.command
                    )
                });
            }
            (undo, redo)
        });
        if undo {
            self.completion = None; // 文本已变化，补全候选过期
            self.do_undo();
        }
        if redo {
            self.completion = None;
            self.do_redo();
        }

        let (f, s, o, n, t, w, g, space, esc) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::F),
                i.key_pressed(egui::Key::S),
                i.key_pressed(egui::Key::O),
                i.key_pressed(egui::Key::N),
                i.key_pressed(egui::Key::T),
                i.key_pressed(egui::Key::W),
                i.key_pressed(egui::Key::G),
                i.key_pressed(egui::Key::Space),
                i.key_pressed(egui::Key::Escape),
            )
        });
        let (cmd, shift) = ctx.input(|i| (i.modifiers.command, i.modifiers.shift));
        if cmd && f {
            self.completion = None; // 避免与查找栏叠加
            self.search_open = true;
            self.just_opened_search = true;
        }
        if cmd && s {
            if shift {
                self.save_as();
            } else {
                self.save();
            }
        }
        if cmd && o {
            self.open_dialog();
        }
        if cmd && n {
            self.new_tab();
        }
        if cmd && t {
            self.new_tab();
        }
        if cmd && w {
            let idx = self.active;
            self.request_close_tab(idx);
        }
        if cmd && g {
            self.completion = None;
            self.goto_open = true;
        }
        if cmd && space {
            self.open_completion();
        }

        // 补全弹窗打开时接管方向键 / Enter / Tab / Esc（否则 TextEdit 会插入换行等）
        if self.completion.is_some() {
            let (up, down, enter, tab, esc) = ctx.input_mut(|i| {
                let r = (
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::Enter),
                    i.key_pressed(egui::Key::Tab),
                    i.key_pressed(egui::Key::Escape),
                );
                if r.0 || r.1 || r.2 || r.3 || r.4 {
                    i.events.retain(|e| {
                        !matches!(
                            e,
                            egui::Event::Key {
                                key: egui::Key::ArrowUp
                                    | egui::Key::ArrowDown
                                    | egui::Key::Enter
                                    | egui::Key::Tab
                                    | egui::Key::Escape,
                                pressed: true,
                                ..
                            }
                        )
                    });
                }
                r
            });
            let mut action: Option<Option<usize>> = None; // None=关闭，Some(i)=应用第 i 项
            if let Some(comp) = self.completion.as_mut() {
                if up {
                    if comp.selected > 0 {
                        comp.selected -= 1;
                    }
                } else if down {
                    if comp.selected + 1 < comp.items.len() {
                        comp.selected += 1;
                    }
                } else if enter || tab {
                    action = Some(Some(comp.selected));
                } else if esc {
                    action = Some(None);
                }
            }
            match action {
                Some(Some(i)) => {
                    let item = self.completion.as_ref().unwrap().items[i].clone();
                    self.apply_completion(item);
                }
                Some(None) => self.completion = None,
                None => {}
            }
        }

        if esc && (self.search_open || self.goto_open) {
            self.search_open = false;
            self.goto_open = false;
        }
    }
}

fn file_sig(path: &Path) -> Option<(Option<SystemTime>, u64)> {
    std::fs::metadata(path).ok().map(|m| (m.modified().ok(), m.len()))
}

fn byte_of_char(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(s.len())
}

/// 菜单（栏按钮与弹出条目）的宽松内边距：条目更大更好点。
fn menu_padding_style(s: &mut egui::Style) {
    s.spacing.button_padding = egui::vec2(10.0, 5.0);
    s.spacing.item_spacing = egui::vec2(6.0, 3.0);
}

fn scan_line_col(text: &str, char_idx: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    for (i, c) in text.chars().enumerate() {
        if i >= char_idx {
            break;
        }
        if c == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

fn color_for(kind: syntax_engine::TokenKind, dark: bool) -> egui::Color32 {
    use syntax_engine::TokenKind::*;
    match (kind, dark) {
        (Keyword, true) => egui::Color32::from_rgb(0x56, 0x9C, 0xD6),
        (String, true) => egui::Color32::from_rgb(0xCE, 0x91, 0x78),
        (Comment, true) => egui::Color32::from_rgb(0x6A, 0x99, 0x55),
        (Number, true) => egui::Color32::from_rgb(0xB5, 0xCE, 0xA8),
        (Function, true) => egui::Color32::from_rgb(0xDC, 0xDC, 0xAA),
        (Type, true) => egui::Color32::from_rgb(0x4E, 0xC9, 0xB0),
        (Property, true) => egui::Color32::from_rgb(0x9C, 0xDC, 0xFE),
        (Constant, true) => egui::Color32::from_rgb(0x4F, 0xC1, 0xFF),
        (Operator, true) => egui::Color32::from_rgb(0xD4, 0xD4, 0xD4),
        (Punct, true) => egui::Color32::from_rgb(0x80, 0x80, 0x80),
        (Plain, true) => egui::Color32::from_rgb(0xD4, 0xD4, 0xD4),
        (Keyword, false) => egui::Color32::from_rgb(0x00, 0x00, 0xFF),
        (String, false) => egui::Color32::from_rgb(0xA3, 0x15, 0x15),
        (Comment, false) => egui::Color32::from_rgb(0x00, 0x80, 0x00),
        (Number, false) => egui::Color32::from_rgb(0x09, 0x86, 0x58),
        (Function, false) => egui::Color32::from_rgb(0x79, 0x5E, 0x26),
        (Type, false) => egui::Color32::from_rgb(0x26, 0x7F, 0x99),
        (Property, false) => egui::Color32::from_rgb(0x00, 0x10, 0x80),
        (Constant, false) => egui::Color32::from_rgb(0x00, 0x00, 0xFF),
        (Operator, false) => egui::Color32::from_rgb(0x1F, 0x1F, 0x1F),
        (Punct, false) => egui::Color32::from_rgb(0x66, 0x66, 0x66),
        (Plain, false) => egui::Color32::from_rgb(0x1F, 0x1F, 0x1F),
    }
}

fn build_job_from_spans(
    ui: &egui::Ui,
    text: &str,
    spans: &[syntax_engine::Span],
    dark: bool,
) -> egui::text::LayoutJob {
    let font_id = egui::TextStyle::Monospace.resolve(ui.style());
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    // 防护：spans 缓存可能在编辑帧内短暂过期（布局器看到的是应用编辑后的文本）。
    // 逐段 clamp + 字符边界校验，缝隙用 Plain 补齐——既不会越界 panic，
    // 也不会因跳过失效段导致文字消失。
    let text_len = text.len();
    let mut pos = 0usize;
    for s in spans {
        let start = s.start.min(text_len);
        let end = s.end.min(text_len);
        if end <= start
            || !text.is_char_boundary(start)
            || !text.is_char_boundary(end)
            || end <= pos
        {
            continue;
        }
        let start = start.max(pos);
        if start > pos {
            job.append(
                &text[pos..start],
                0.0,
                egui::TextFormat::simple(font_id.clone(), color_for(syntax_engine::TokenKind::Plain, dark)),
            );
        }
        job.append(&text[start..end], 0.0, egui::TextFormat::simple(font_id.clone(), color_for(s.kind, dark)));
        pos = end;
    }
    if pos < text_len {
        job.append(
            &text[pos..],
            0.0,
            egui::TextFormat::simple(font_id, color_for(syntax_engine::TokenKind::Plain, dark)),
        );
    }
    job
}

impl eframe::App for EditorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 1) 同步上一帧的编辑到 Document（仅在 TextEdit 报告变更时执行 diff，空闲帧零开销）
        {
            let t = self.tab_mut();
            if t.edit_changed {
                t.edit_changed = false;
                t.sync_diff_to_doc();
            }
        }
        // 2) 快捷键（撤销/重做在其中消费，避免双重撤销）
        self.handle_shortcuts(ctx);
        // 3) 外部修改检测 + 自动保存 + 查找缓存
        let active = self.active;
        self.check_external_changed(active);
        self.maybe_autosave();
        self.refresh_matches();
        if let Some((_, t)) = &self.status_msg {
            if t.elapsed() > Duration::from_secs(5) {
                self.status_msg = None;
            }
        }

        // ---------- 菜单栏（加大字号与条目间距，弹出菜单同一内边距） ----------
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::MenuBar::new()
                .style(menu_padding_style)
                .config(
                    egui::containers::menu::MenuConfig::new().style(menu_padding_style),
                )
                .ui(ui, |ui| {
                ui.style_mut().text_styles.insert(
                    egui::TextStyle::Button,
                    egui::FontId::proportional(14.5),
                );
                ui.style_mut().spacing.item_spacing = egui::vec2(10.0, 6.0);
                ui.menu_button("文件", |ui| {
                    if ui.add(egui::Button::new("新建标签页").shortcut_text("Ctrl+T")).clicked() {
                        self.new_tab();
                        ui.close();
                    }
                    if ui.add(egui::Button::new("打开…").shortcut_text("Ctrl+O")).clicked() {
                        self.open_dialog();
                        ui.close();
                    }
                    if ui.button("重新加载").clicked() {
                        self.reload_active();
                        ui.close();
                    }
                    ui.separator();
                    if ui.add(egui::Button::new("保存").shortcut_text("Ctrl+S")).clicked() {
                        self.save();
                        ui.close();
                    }
                    if ui
                        .add(egui::Button::new("另存为…").shortcut_text("Ctrl+Shift+S"))
                        .clicked()
                    {
                        self.save_as();
                        ui.close();
                    }
                    ui.separator();
                    if ui.add(egui::Button::new("关闭标签页").shortcut_text("Ctrl+W")).clicked() {
                        let idx = self.active;
                        self.request_close_tab(idx);
                        ui.close();
                    }
                    if ui.button("退出").clicked() {
                        self.request_exit(ctx);
                        ui.close();
                    }
                });
                ui.menu_button("编辑", |ui| self.edit_menu(ui));
                ui.menu_button("查找", |ui| {
                    if ui.add(egui::Button::new("查找 / 替换…").shortcut_text("Ctrl+F")).clicked() {
                        self.search_open = true;
                        self.just_opened_search = true;
                        ui.close();
                    }
                    if ui.add(egui::Button::new("跳转到行…").shortcut_text("Ctrl+G")).clicked() {
                        self.goto_open = true;
                        ui.close();
                    }
                });
                ui.menu_button("查看", |ui| {
                    let outline_label =
                        if self.show_outline { "● 大纲" } else { "　大纲" };
                    if ui.button(outline_label).clicked() {
                        self.show_outline = !self.show_outline;
                        ui.close();
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("单词补全：Ctrl+Space").weak());
                });
                ui.menu_button("格式", |ui| {
                    ui.menu_button("编码", |ui| self.encoding_menu(ui));
                    ui.menu_button("换行符", |ui| self.newline_menu(ui));
                    ui.menu_button("语言", |ui| self.language_menu(ui));
                });
                ui.menu_button("脚本", |ui| {
                    if self.scripts.is_empty() {
                        let dir = platform::session::config_dir()
                            .map(|d| d.join("scripts").display().to_string())
                            .unwrap_or_else(|| "用户配置目录/scripts".to_string());
                        ui.label(
                            egui::RichText::new(format!("暂无脚本。\n将 .js 文件放入：\n{dir}"))
                                .weak(),
                        );
                    } else {
                        for i in 0..self.scripts.len() {
                            let name = self.scripts[i].name.clone();
                            if ui.button(name).clicked() {
                                self.run_script(i);
                                ui.close();
                            }
                        }
                    }
                });
            });
        });

        // ---------- 标签页栏（一体化自绘标签：标题与关闭键同区、大号易点、中键关闭） ----------
        egui::TopBottomPanel::top("tab_strip").show(ctx, |ui| {
            ui.add_space(2.0);
            egui::ScrollArea::horizontal().show(ui, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let visuals = ui.style().visuals.clone();
                    // 即时模式纪律：遍历中只收集动作，循环结束后统一执行——
                    // 否则关闭标签会同步缩短 tabs，循环下一轮越界 panic
                    let mut close_request: Option<usize> = None;
                    let mut switch_request: Option<usize> = None;
                    for i in 0..self.tabs.len() {
                        let (mut name, is_active, edit_key) = {
                            let t = &self.tabs[i];
                            let name = if t.is_dirty() {
                                format!("● {}", t.display_name())
                            } else {
                                t.display_name()
                            };
                            (name, i == self.active, t.edit_key)
                        };
                        // 标题过宽时截断（带省略号）
                        let font = egui::FontId::proportional(14.5);
                        let mut galley = ui.painter().layout_no_wrap(
                            name.clone(),
                            font.clone(),
                            Color32::WHITE,
                        );
                        if galley.size().x > 150.0 {
                            let chars: Vec<char> = name.chars().collect();
                            for take in (1..=chars.len()).rev() {
                                let s: String = chars[..take].iter().collect();
                                galley = ui.painter().layout_no_wrap(
                                    format!("{s}…"),
                                    font.clone(),
                                    Color32::WHITE,
                                );
                                if galley.size().x <= 150.0 {
                                    name = format!("{s}…");
                                    break;
                                }
                            }
                        }
                        let close_w = 22.0_f32;
                        let tab_w = (galley.size().x + 10.0 + close_w + 4.0).max(76.0);
                        let (rect, resp) =
                            ui.allocate_exact_size(egui::vec2(tab_w, 30.0), egui::Sense::click());
                        let (fill, text_color) = if is_active {
                            (visuals.selection.bg_fill, visuals.strong_text_color())
                        } else if resp.hovered() {
                            (visuals.widgets.hovered.bg_fill, visuals.text_color())
                        } else {
                            (Color32::TRANSPARENT, visuals.text_color())
                        };
                        ui.painter().rect(
                            rect,
                            egui::CornerRadius::same(5),
                            fill,
                            egui::Stroke::NONE,
                            egui::StrokeKind::Inside,
                        );
                        ui.painter().text(
                            egui::pos2(rect.left() + 10.0, rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            &name,
                            font,
                            text_color,
                        );
                        // 内嵌关闭键
                        let close_rect = egui::Rect::from_center_size(
                            egui::pos2(rect.right() - 4.0 - close_w / 2.0, rect.center().y),
                            egui::vec2(close_w, 24.0),
                        );
                        let close_resp = ui
                            .interact(
                                close_rect,
                                egui::Id::new("tab_close").with(edit_key),
                                egui::Sense::click(),
                            )
                            .on_hover_text("关闭标签页 (Ctrl+W)");
                        let close_fill = if close_resp.hovered() {
                            visuals.widgets.hovered.bg_fill
                        } else {
                            Color32::TRANSPARENT
                        };
                        ui.painter().rect(
                            close_rect,
                            egui::CornerRadius::same(4),
                            close_fill,
                            egui::Stroke::NONE,
                            egui::StrokeKind::Inside,
                        );
                        ui.painter().text(
                            close_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            "×",
                            egui::FontId::proportional(13.5),
                            if close_resp.hovered() {
                                visuals.strong_text_color()
                            } else {
                                visuals.weak_text_color()
                            },
                        );
                        if close_resp.clicked() {
                            close_request = close_request.or(Some(i));
                        } else if resp.middle_clicked() {
                            close_request = close_request.or(Some(i)); // 中键关闭
                        } else if resp.clicked() && !is_active {
                            switch_request = switch_request.or(Some(i));
                        }
                        ui.add_space(3.0);
                    }
                    // 新建按钮（与标签同高，hover 高亮）
                    let (plus_rect, plus_resp) =
                        ui.allocate_exact_size(egui::vec2(30.0, 30.0), egui::Sense::click());
                    let fill = if plus_resp.hovered() {
                        visuals.widgets.hovered.bg_fill
                    } else {
                        Color32::TRANSPARENT
                    };
                    ui.painter().rect(
                        plus_rect,
                        egui::CornerRadius::same(5),
                        fill,
                        egui::Stroke::NONE,
                        egui::StrokeKind::Inside,
                    );
                    ui.painter().text(
                        plus_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "＋",
                        egui::FontId::proportional(16.0),
                        visuals.text_color(),
                    );
                    // 循环结束后统一执行标签切换与关闭（tabs 已不再被遍历持有）
                    if let Some(i) = switch_request {
                        self.switch_to(i);
                    }
                    if let Some(i) = close_request {
                        self.request_close_tab(i);
                    }
                    if plus_resp.clicked() {
                        self.new_tab();
                    }
                });
            });
            ui.add_space(2.0);
        });

        // ---------- 查找 / 替换栏（两行布局，输入框自适应窗口宽度） ----------
        if self.search_open {
            egui::TopBottomPanel::top("search_panel").show(ctx, |ui| {
                ui.add_space(3.0);
                // 行 1：查找
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("查找:").strong());
                    let box_w = (ui.available_width() - 300.0).max(140.0);
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.search_query)
                            .id(egui::Id::new(SEARCH_BOX_ID))
                            .hint_text("输入查找内容…")
                            .desired_width(box_w),
                    );
                    if self.just_opened_search {
                        resp.request_focus();
                        self.just_opened_search = false;
                    }
                    ui.checkbox(&mut self.search_opts.case_sensitive, "Aa");
                    ui.checkbox(&mut self.search_opts.regex, ".*");
                    ui.checkbox(&mut self.search_opts.whole_word, "整词");
                    let (total, idx) = (self.tab().matches.len(), self.tab().match_idx);
                    ui.label(
                        egui::RichText::new(format!(
                            "{}/{}",
                            if total == 0 { 0 } else { idx + 1 },
                            total
                        ))
                        .strong(),
                    );
                    if ui
                        .add(egui::Button::new(egui::RichText::new("◀").small()))
                        .on_hover_text("上一处 (Shift+Enter)")
                        .clicked()
                    {
                        self.jump_prev();
                    }
                    if ui
                        .add(egui::Button::new(egui::RichText::new("▶").small()))
                        .on_hover_text("下一处 (Enter)")
                        .clicked()
                    {
                        self.jump_next();
                    }
                    if ui
                        .add(egui::Button::new(egui::RichText::new("×").small()))
                        .on_hover_text("关闭 (Esc)")
                        .clicked()
                    {
                        self.search_open = false;
                    }
                });
                // 行 2：替换
                ui.horizontal(|ui| {
                    ui.label("替换为:");
                    let box_w = (ui.available_width() - 340.0).max(140.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut self.replace_query)
                            .id(egui::Id::new("replace_box"))
                            .hint_text("支持 $1 捕获组（正则模式）")
                            .desired_width(box_w),
                    );
                    if ui.button("全部替换").clicked() {
                        self.replace_everything();
                    }
                    ui.label(
                        egui::RichText::new("Enter 下一处 · Shift+Enter 上一处").weak().small(),
                    );
                });
                ui.add_space(2.0);
                let on_search_box = ctx.memory(|m| m.has_focus(egui::Id::new(SEARCH_BOX_ID)));
                if on_search_box {
                    let (enter, shift) = ctx
                        .input(|i| (i.key_pressed(egui::Key::Enter), i.modifiers.shift));
                    if enter {
                        if shift {
                            self.jump_prev();
                        } else {
                            self.jump_next();
                        }
                    }
                }
            });
        }

        // ---------- 外部修改横幅 ----------
        if self.tab().externally_changed {
            egui::TopBottomPanel::top("ext_banner").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        "⚠ 文件已被外部程序修改或删除",
                    );
                    if ui.button("重新加载").clicked() {
                        self.reload_active();
                    }
                    if ui.button("忽略").clicked() {
                        let t = self.tab_mut();
                        t.externally_changed = false;
                        t.file_sig = t.doc.path().and_then(|p| file_sig(p.as_path()));
                    }
                });
            });
        }

        // ---------- 大纲侧栏（FR-4.4） ----------
        if self.show_outline {
            egui::SidePanel::left("outline_panel")
                .resizable(true)
                .default_width(200.0)
                .show(ctx, |ui| {
                    // 重算（缓存键：文本长度 + 撤销/重做深度）
                    let items: Vec<syntax_engine::OutlineItem> = {
                        let t = self.tab_mut();
                        let key = (t.text.len(), t.doc.undo_depth(), t.doc.redo_depth());
                        let stale = t
                            .outline_cache
                            .as_ref()
                            .map(|c| c.0)
                            .map(|k| k != key)
                            .unwrap_or(true);
                        if stale {
                            let items = syntax_engine::outline(&t.text, t.language.as_ref());
                            t.outline_cache = Some((key, items));
                        }
                        t.outline_cache.as_ref().unwrap().1.clone()
                    };
                    ui.add_space(3.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("大纲").strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(format!("{} 项", items.len())).weak().small(),
                            );
                        });
                    });
                    ui.separator();
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        if items.is_empty() {
                            ui.label(
                                egui::RichText::new("（当前语言无大纲项）\n提示：Markdown\n标题、源码函数/类会出现在这里")
                                    .weak(),
                            );
                        }
                        for it in &items {
                            // 全宽按钮形成列表感
                            let text =
                                egui::RichText::new(format!("{}  {}", it.kind, it.label)).small();
                            if ui
                                .add_sized(
                                    [ui.available_width(), 20.0],
                                    egui::Button::selectable(false, text),
                                )
                                .clicked()
                            {
                                let ch = self.tab().doc.line_to_char(it.line);
                                self.pending_cursor = Some(PendingCursor::At(ch));
                                self.focus_main = true;
                            }
                        }
                    });
                });
        }

        // ---------- 状态栏 ----------
        let dirty = self.tab().is_dirty();
        let (stats, (line, col)) = {
            let t = self.tab_mut();
            (t.cached_stats(), t.cursor_line_col)
        };
        let enc = self.tab().doc.encoding_name().to_string();
        let mixed = self.tab().mixed_newline;
        let nl_label = if mixed {
            format!("⚠ 换行: 混合（按 {} 保存）", self.tab().doc.newline().label())
        } else {
            format!("换行: {}", self.tab().doc.newline().label())
        };
        let lang_name = self
            .tab()
            .language
            .as_ref()
            .map(|l| l.name.clone())
            .unwrap_or_else(|| "纯文本".to_string());
        let status_msg = self.status_msg.as_ref().map(|(m, _)| m.clone());

        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                // 状态栏整体 14pt（比正文略小、比 small 大）
                // 位置：加粗突出
                ui.label(
                    egui::RichText::new(format!("Ln {line}, Col {col}"))
                        .size(14.0)
                        .strong(),
                );
                ui.separator();
                // 统计：合并为一组，减少碎片感
                ui.label(
                    egui::RichText::new(format!(
                        "{} 行　{} 字符　{} 词",
                        stats.lines, stats.chars, stats.words
                    ))
                    .size(14.0),
                );
                ui.separator();
                // 快捷菜单
                ui.menu_button(
                    egui::RichText::new(format!("编码: {enc}")).size(14.0),
                    |ui| self.encoding_menu(ui),
                );
                ui.menu_button(egui::RichText::new(nl_label).size(14.0), |ui| {
                    self.newline_menu(ui)
                });
                ui.menu_button(
                    egui::RichText::new(format!("语言: {lang_name}")).size(14.0),
                    |ui| self.language_menu(ui),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(msg) = &status_msg {
                        ui.label(egui::RichText::new(msg).weak().size(14.0));
                    }
                    if dirty {
                        ui.label(
                            egui::RichText::new("未保存")
                                .size(14.0)
                                .color(egui::Color32::YELLOW),
                        );
                    }
                });
            });
        });

        // ---------- 编辑区（带内边距；显式填充面板底色——自定义 Frame 会覆盖默认填充） ----------
        let panel_fill = ctx.style().visuals.panel_fill;
        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(panel_fill)
                    .inner_margin(egui::Margin::same(6)),
            )
            .show(ctx, |ui| {
            // 刷新高亮 spans 缓存（文本变化才重算；tree-sitter 优先，超限降级 Plain）
            let spans_arc: Arc<Vec<syntax_engine::Span>> = {
                let t = self.tab_mut();
                let over_limit = t.text.len() > MAX_HIGHLIGHT_BYTES;
                let stale = t
                    .spans
                    .as_ref()
                    .map(|(txt, _)| txt.as_str() != t.text.as_str())
                    .unwrap_or(true);
                if stale {
                    let spans = if over_limit {
                        vec![syntax_engine::Span {
                            start: 0,
                            end: t.text.len(),
                            kind: syntax_engine::TokenKind::Plain,
                        }]
                    } else {
                        match &mut t.ts_highlighter {
                            Some(h) => h.spans_for(&t.text),
                            None => syntax_engine::highlight(&t.text, t.language.as_ref()),
                        }
                    };
                    t.spans = Some((t.text.clone(), Arc::new(spans)));
                }
                t.spans
                    .as_ref()
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| Arc::new(vec![]))
            };
            let edit_key = {
                let t = self.tab_mut();
                t.edit_key
            };
            let dark = ui.style().visuals.dark_mode;
            let mut layouter = |ui: &egui::Ui,
                                buf: &dyn egui::TextBuffer,
                                _wrap_width: f32|
             -> Arc<egui::Galley> {
                let job = build_job_from_spans(ui, buf.as_str(), &spans_arc, dark);
                ui.fonts(|f| f.layout_job(job))
            };
            let output = {
                let t = self.tab_mut();
                egui::TextEdit::multiline(&mut t.text)
                    .id(egui::Id::new(MAIN_EDIT_ID).with(edit_key))
                    .frame(false)
                    .code_editor()
                    .min_size(egui::vec2(ui.available_width(), ui.available_height()))
                    .layouter(&mut layouter)
                    .show(ui)
            };
            if self.focus_main {
                output.response.request_focus();
                self.focus_main = false;
            }
            if output.response.changed() {
                self.tab_mut().edit_changed = true;
            }
            if let Some(range) = &output.cursor_range {
                let idx = range.primary.index;
                let t = self.tab_mut();
                t.sel = Some((range.secondary.index, range.primary.index));
                let key = (idx, t.text.len());
                if t.cursor_cache_key != Some(key) {
                    t.cursor_char = idx;
                    t.cursor_line_col = scan_line_col(&t.text, idx);
                    t.cursor_cache_key = Some(key);
                }
            }
        });

        // ---------- 待应用光标（撤销/重做/跳转/查找） ----------
        if let Some(pc) = self.pending_cursor.take() {
            let id = self.main_edit_id();
            let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
            match pc {
                PendingCursor::At(i) => {
                    state.cursor.set_char_range(Some(egui::text::CCursorRange::one(
                        egui::text::CCursor::new(i),
                    )));
                }
                PendingCursor::Select(s, e) => {
                    state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                        egui::text::CCursor::new(e),
                        egui::text::CCursor::new(s),
                    )));
                }
            }
            egui::TextEdit::store_state(ctx, id, state);
            self.focus_main = true;
        }
        if let Some((line, col)) = self.pending_goto.take() {
            let ch = self.tab().doc.line_to_char(line.saturating_sub(1)) + col.saturating_sub(1);
            self.pending_cursor = Some(PendingCursor::At(ch));
        }

        // ---------- 跳转到行 ----------
        if self.goto_open {
            egui::Window::new("跳转到行")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_TOP, [0.0, 60.0])
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("行号:");
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.goto_input)
                                .id(egui::Id::new("goto_box"))
                                .desired_width(120.0),
                        );
                        if resp.changed() {
                            self.goto_input.retain(|c| c.is_ascii_digit());
                        }
                        let enter = ctx.input(|i| i.key_pressed(egui::Key::Enter))
                            && ctx.memory(|m| m.has_focus(egui::Id::new("goto_box")));
                        if ui.button("跳转").clicked() || enter {
                            self.do_goto();
                        }
                        if ui.button("取消").clicked() {
                            self.goto_open = false;
                        }
                    });
                });
        }

        // ---------- 单词补全弹窗（FR-6.1） ----------
        if let Some(comp) = &self.completion {
            let items = comp.items.clone();
            let selected = comp.selected;
            let mut apply_idx: Option<usize> = None;
            egui::Window::new(egui::RichText::new(format!("单词补全（{} 项）", items.len())).small())
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_TOP, [0.0, 90.0])
                .show(ctx, |ui| {
                    for (i, item) in items.iter().enumerate() {
                        let text = if i == selected {
                            format!("▶ {item}")
                        } else {
                            format!("　 {item}")
                        };
                        if ui
                            .add(egui::Button::selectable(i == selected, text).small())
                            .clicked()
                        {
                            apply_idx = Some(i);
                        }
                    }
                    ui.label(
                        egui::RichText::new("↑↓ 选择 · Enter/Tab 应用 · Esc 关闭").weak().small(),
                    );
                });
            if let Some(i) = apply_idx {
                let item = self.completion.as_ref().unwrap().items[i].clone();
                self.apply_completion(item);
            }
        }

        // ---------- 未保存确认 ----------
        if let Some(idx) = self.pending_close {
            let name = self.tabs.get(idx).map(|t| t.display_name()).unwrap_or_default();
            egui::Window::new("未保存的修改")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("“{name}” 有未保存的修改。"));
                    ui.horizontal(|ui| {
                        if ui.button("保存并关闭").clicked() {
                            self.save_tab(idx, true);
                            self.finish_close(idx);
                        }
                        if ui.button("不保存").clicked() {
                            self.finish_close(idx);
                        }
                        if ui.button("取消").clicked() {
                            self.pending_close = None;
                        }
                    });
                });
        }
        if self.pending_exit {
            let dirty_count = self.tabs.iter().filter(|t| t.is_dirty()).count();
            egui::Window::new("退出 Cote")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("有 {dirty_count} 个标签页有未保存的修改。"));
                    ui.horizontal(|ui| {
                        if ui.button("保存全部并退出").clicked() {
                            for i in 0..self.tabs.len() {
                                if self.tabs[i].is_dirty() && self.tabs[i].doc.path().is_some() {
                                    self.save_tab(i, true);
                                }
                            }
                            // 未命名的脏标签页无处可存：阻止退出并提示，
                            // 否则内容会被静默丢弃
                            if self.tabs.iter().any(|t| t.is_dirty()) {
                                self.status("存在未命名的未保存标签页，请先另存或关闭");
                            } else {
                                self.pending_exit = false;
                                self.save_session();
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                        }
                        if ui.button("不保存并退出").clicked() {
                            self.pending_exit = false;
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        if ui.button("取消").clicked() {
                            self.pending_exit = false;
                        }
                    });
                });
        }

        // ---------- 标题 ----------
        let title = format!(
            "{}{} — Cote",
            if dirty { "● " } else { "" },
            self.tab().display_name()
        );
        if title != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
    }
}

/// CLI 打开请求。
#[derive(Debug, Default, Clone)]
pub struct OpenRequest {
    pub files: Vec<PathBuf>,
    pub line: Option<usize>,
    pub column: Option<usize>,
}

/// 启动 GUI。
pub fn run(open: OpenRequest) -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 720.0])
            .with_min_inner_size([640.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Cote",
        options,
        Box::new(move |cc| {
            let mut app = EditorApp::new(cc);
            let mut opened = false;
            for p in &open.files {
                if p.exists() {
                    app.open_path(p);
                    opened = true;
                } else {
                    app.status(format!("文件不存在：{}", p.display()));
                }
            }
            if !opened && open.files.is_empty() {
                app.restore_session();
            }
            if open.line.is_some() || open.column.is_some() {
                app.pending_goto = Some((open.line.unwrap_or(1), open.column.unwrap_or(1)));
            }
            Ok(Box::new(app))
        }),
    )
}
