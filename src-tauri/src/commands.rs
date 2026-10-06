//! Tauri commands：前端可调用的全部后端接口。
//!
//! 约定：所有命令同步执行、通过 `AppState` 的互斥锁串行化；
//! 前端负责以 promise 队列保证 invoke 顺序（编辑序列不可乱序）。
//! 凡改变文本/元数据的命令都返回受影响标签的最新 meta、必要时全文，
//! 以及整个标签列表快照（`tabs` + `active_id`），前端据此整体更新。

use std::path::PathBuf;

use editor_core::encoding;
use editor_core::search::{self, SearchOptions};
use editor_core::text as tx;
use script_host::ScriptApi;
use serde::Serialize;
use syntax_engine::Span;
use tauri::State;

use crate::state::{
    adjust_active_after_close, char_to_byte, char_to_utf16, load_into, new_tab_silent, open_path,
    save_session, save_tab, utf16_to_char, AppInner, AppState, SaveOutcome, TabList, TabMeta,
    TabPayload, TabState, MAX_HIGHLIGHT_BYTES,
};

fn lock<'a>(state: &'a State<'_, AppState>) -> std::sync::MutexGuard<'a, AppInner> {
    // 中毒恢复而非崩溃：某次命令 panic 后应用仍可继续（状态一致性由命令粒度保证）
    state
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn tab_idx(inner: &AppInner, id: u64) -> Option<usize> {
    inner.tabs.iter().position(|t| t.id == id)
}

#[derive(Debug, Clone, Serialize)]
pub struct StatsPayload {
    pub lines: usize,
    pub chars: usize,
    pub words: usize,
}

fn stats_of(t: &TabState) -> StatsPayload {
    let st = t.doc.stats();
    StatsPayload {
        lines: st.lines,
        chars: st.chars,
        words: st.words,
    }
}

// ---------- 启动 ----------

#[derive(Debug, Serialize)]
pub struct LangInfo {
    pub name: String,
    pub builtin: bool,
}

#[derive(Debug, Serialize)]
pub struct ScriptEntry {
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct InitPayload {
    pub tabs: Vec<TabPayload>,
    pub active_id: u64,
    pub theme: String,
    pub encodings: Vec<String>,
    pub languages: Vec<LangInfo>,
    pub scripts: Vec<ScriptEntry>,
    pub scripts_dir: String,
    pub notices: Vec<String>,
    /// 启动跳转 (line, col)，1 基，作用于激活标签
    pub goto: Option<(usize, usize)>,
}

fn load_scripts() -> Vec<ScriptEntry> {
    let dir = platform::session::config_dir()
        .map(|d| d.join("scripts"))
        .unwrap_or_default();
    script_host::load_scripts(&dir)
        .into_iter()
        .map(|s| ScriptEntry { name: s.name })
        .collect()
}

#[tauri::command]
pub fn get_init(state: State<'_, AppState>) -> InitPayload {
    let mut inner = lock(&state);
    let tabs = inner.tabs.iter().map(TabPayload::of).collect();
    let active_id = inner.tabs.get(inner.active).map(|t| t.id).unwrap_or(0);
    let languages = inner
        .languages
        .iter()
        .map(|(d, builtin)| LangInfo {
            name: d.name.clone(),
            builtin: *builtin,
        })
        .collect();
    let scripts_dir = platform::session::config_dir()
        .map(|d| d.join("scripts").to_string_lossy().into_owned())
        .unwrap_or_default();
    InitPayload {
        tabs,
        active_id,
        theme: inner.theme.clone(),
        encodings: encoding::COMMON_ENCODINGS
            .iter()
            .map(|s| s.to_string())
            .collect(),
        languages,
        scripts: load_scripts(),
        scripts_dir,
        notices: std::mem::take(&mut inner.notices),
        goto: inner.pending_goto.take(),
    }
}

// ---------- 标签页 ----------

#[derive(Debug, Serialize)]
pub struct NewTabResult {
    pub tab: TabPayload,
    pub list: TabList,
}

#[tauri::command]
pub fn new_tab(state: State<'_, AppState>) -> NewTabResult {
    let mut inner = lock(&state);
    let idx = new_tab_silent(&mut inner);
    inner.active = idx;
    save_session(&inner);
    NewTabResult {
        tab: TabPayload::of(&inner.tabs[idx]),
        list: TabList::of(&inner),
    }
}

#[derive(Debug, Serialize)]
pub struct OpenResult {
    /// 打开失败（文件不可读）时为 None，此时只有 message
    pub tab: Option<TabPayload>,
    pub list: TabList,
    pub message: String,
    /// 已在别的标签页打开，前端只需切换过去
    pub switched: bool,
}

#[tauri::command]
pub fn open_file(state: State<'_, AppState>, path: String) -> OpenResult {
    let mut inner = lock(&state);
    let p = PathBuf::from(&path);
    let was_open = inner
        .tabs
        .iter()
        .any(|t| t.doc.path().map(|q| q == &p).unwrap_or(false));
    let (idx, message) = open_path(&mut inner, &p);
    save_session(&inner);
    // 注意：open_path 失败路径会移除新建标签，idx 可能越界——用 get 安全取
    OpenResult {
        tab: inner.tabs.get(idx).map(TabPayload::of),
        list: TabList::of(&inner),
        message,
        switched: was_open,
    }
}

#[derive(Debug, Serialize)]
pub struct CloseResult {
    pub closed: bool,
    /// 保存被要求但文档无路径 → 前端先走另存为对话框
    pub need_path: bool,
    pub list: TabList,
    pub message: Option<String>,
}

/// 关闭标签页。save=true 时先保存（无路径则返回 need_path=true 不关闭）。
#[tauri::command]
pub fn close_tab(state: State<'_, AppState>, tab_id: u64, save: bool) -> CloseResult {
    let mut inner = lock(&state);
    let deny = |message: Option<String>, inner: &AppInner| CloseResult {
        closed: false,
        need_path: false,
        list: TabList::of(inner),
        message,
    };
    let Some(idx) = tab_idx(&inner, tab_id) else {
        return deny(None, &inner);
    };
    if inner.tabs[idx].is_dirty() && save {
        match save_tab(&mut inner, idx, false) {
            Some(SaveOutcome::NeedPath) => {
                return CloseResult {
                    closed: false,
                    need_path: true,
                    list: TabList::of(&inner),
                    message: None,
                };
            }
            Some(SaveOutcome::Failed(e)) => return deny(Some(e), &inner),
            _ => {}
        }
    }
    let idx = tab_idx(&inner, tab_id).unwrap();
    inner.tabs.remove(idx);
    adjust_active_after_close(&mut inner, idx);
    save_session(&inner);
    CloseResult {
        closed: true,
        need_path: false,
        list: TabList::of(&inner),
        message: None,
    }
}

#[tauri::command]
pub fn set_active(state: State<'_, AppState>, tab_id: u64) -> TabList {
    let mut inner = lock(&state);
    if let Some(idx) = tab_idx(&inner, tab_id) {
        inner.active = idx;
        save_session(&inner);
    }
    TabList::of(&inner)
}

#[derive(Debug, Serialize)]
pub struct SaveResult {
    pub meta: TabMeta,
    pub list: TabList,
    pub message: String,
}

fn save_message(inner: &AppInner, idx: usize, losses: usize) -> String {
    let enc = inner.tabs[idx].doc.encoding_name();
    let name = inner.tabs[idx].display_name();
    if losses > 0 {
        format!("已保存（警告：{losses} 个字符无法用 {enc} 表示，已丢弃）")
    } else {
        format!("已保存 {name}（{enc}）")
    }
}

#[tauri::command]
pub fn save(state: State<'_, AppState>, tab_id: u64) -> SaveResult {
    let mut inner = lock(&state);
    let idx = tab_idx(&inner, tab_id).unwrap_or(inner.active);
    let meta = TabMeta::of(&inner.tabs[idx]);
    let list = TabList::of(&inner);
    match save_tab(&mut inner, idx, false) {
        Some(SaveOutcome::Saved { losses }) => {
            let list = TabList::of(&inner);
            SaveResult {
                meta: TabMeta::of(&inner.tabs[idx]),
                list,
                message: save_message(&inner, idx, losses),
            }
        }
        Some(SaveOutcome::NeedPath) => SaveResult {
            meta,
            list,
            message: "__need_path__".to_string(),
        },
        Some(SaveOutcome::Failed(e)) => SaveResult {
            meta,
            list,
            message: e,
        },
        None => SaveResult {
            meta,
            list,
            message: String::new(),
        },
    }
}

#[tauri::command]
pub fn save_as(state: State<'_, AppState>, tab_id: u64, path: String) -> SaveResult {
    let mut inner = lock(&state);
    let Some(idx) = tab_idx(&inner, tab_id) else {
        let list = TabList::of(&inner);
        return SaveResult {
            meta: TabMeta::of(&inner.tabs[inner.active]),
            list,
            message: String::new(),
        };
    };
    inner.tabs[idx].doc.set_path(Some(PathBuf::from(&path)));
    match save_tab(&mut inner, idx, false) {
        Some(SaveOutcome::Saved { losses }) => {
            save_session(&inner);
            let list = TabList::of(&inner);
            SaveResult {
                meta: TabMeta::of(&inner.tabs[idx]),
                list,
                message: save_message(&inner, idx, losses),
            }
        }
        _ => {
            let list = TabList::of(&inner);
            SaveResult {
                meta: TabMeta::of(&inner.tabs[idx]),
                list,
                message: "保存失败：无法写入所选路径".to_string(),
            }
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ReloadResult {
    pub tab: TabPayload,
    pub list: TabList,
    pub message: String,
    pub ok: bool,
}

/// 重新加载（对照 egui 版 reload_active：仅干净文档允许）。
#[tauri::command]
pub fn reload_tab(state: State<'_, AppState>, tab_id: u64) -> ReloadResult {
    let mut inner = lock(&state);
    let Some(idx) = tab_idx(&inner, tab_id) else {
        return ReloadResult {
            tab: TabPayload::of(&inner.tabs[inner.active]),
            list: TabList::of(&inner),
            message: String::new(),
            ok: false,
        };
    };
    if inner.tabs[idx].is_dirty() {
        return ReloadResult {
            tab: TabPayload::of(&inner.tabs[idx]),
            list: TabList::of(&inner),
            message: "文档有未保存修改，重新加载前请先保存".to_string(),
            ok: false,
        };
    }
    let Some(path) = inner.tabs[idx].doc.path().cloned() else {
        return ReloadResult {
            tab: TabPayload::of(&inner.tabs[idx]),
            list: TabList::of(&inner),
            message: "当前文档尚未保存到磁盘".to_string(),
            ok: false,
        };
    };
    let languages = inner.languages.clone();
    match load_into(&mut inner.tabs[idx], &path, &languages) {
        Ok(()) => ReloadResult {
            tab: TabPayload::of(&inner.tabs[idx]),
            list: TabList::of(&inner),
            message: format!("已重新加载 {}", inner.tabs[idx].display_name()),
            ok: true,
        },
        Err(_) => ReloadResult {
            tab: TabPayload::of(&inner.tabs[idx]),
            list: TabList::of(&inner),
            message: "重新加载失败".to_string(),
            ok: false,
        },
    }
}

#[derive(Debug, Serialize)]
pub struct ExitCheck {
    /// 有路径的脏标签数（保存全部可直接进行）
    pub dirty_with_path: usize,
    /// 未命名脏标签数（保存需逐个另存为对话框）
    pub dirty_untitled: usize,
}

#[tauri::command]
pub fn exit_check(state: State<'_, AppState>) -> ExitCheck {
    let inner = lock(&state);
    let mut r = ExitCheck {
        dirty_with_path: 0,
        dirty_untitled: 0,
    };
    for t in &inner.tabs {
        if t.is_dirty() {
            if t.doc.path().is_some() {
                r.dirty_with_path += 1;
            } else {
                r.dirty_untitled += 1;
            }
        }
    }
    r
}

#[tauri::command]
pub fn save_all(state: State<'_, AppState>) -> TabList {
    let mut inner = lock(&state);
    for i in 0..inner.tabs.len() {
        if inner.tabs[i].is_dirty() && inner.tabs[i].doc.path().is_some() {
            save_tab(&mut inner, i, true);
        }
    }
    save_session(&inner);
    TabList::of(&inner)
}

// ---------- 编辑（增量通路） ----------

#[derive(Debug, Clone, serde::Deserialize)]
pub struct EditOp {
    /// UTF-16 偏移（CodeMirror 坐标）
    pub from: usize,
    pub to: usize,
    pub inserted: String,
}

/// 应用前端产生的增量变更。多个 op 按 CodeMirror 事务语义顺序执行
/// （每个 op 的坐标基于前一个 op 应用后的文本，前端已做位移累计）。
#[tauri::command]
pub fn apply_edit(
    state: State<'_, AppState>,
    tab_id: u64,
    ops: Vec<EditOp>,
    anchor: usize,
    head: usize,
) {
    let mut inner = lock(&state);
    let Some(idx) = tab_idx(&inner, tab_id) else {
        return;
    };
    // Document 的删除/插入用字符索引；逐 op 转换（文本随 op 演化）。
    // O(n)/op：与 egui 版每帧 O(n) diff 同步同阶，编辑场景足够。
    for op in ops {
        let text = inner.tabs[idx].doc.text();
        let from = utf16_to_char(&text, op.from);
        let to = utf16_to_char(&text, op.to);
        if from < to {
            inner.tabs[idx].doc.delete_range(from, to);
        }
        if !op.inserted.is_empty() {
            inner.tabs[idx].doc.insert_text(from, &op.inserted);
        }
    }
    let text = inner.tabs[idx].doc.text();
    let a = utf16_to_char(&text, anchor.min(head));
    let h = utf16_to_char(&text, anchor.max(head));
    inner.tabs[idx].selection = Some((a, h));
}

#[derive(Debug, Serialize)]
pub struct StatusResult {
    pub meta: TabMeta,
    pub stats: StatsPayload,
}

/// 上报光标行列（会话恢复用）并取回统计。前端在编辑/移动光标后防抖调用。
#[tauri::command]
pub fn doc_status(
    state: State<'_, AppState>,
    tab_id: u64,
    line: usize,
    col: usize,
) -> StatusResult {
    let mut inner = lock(&state);
    let idx = tab_idx(&inner, tab_id).unwrap_or(inner.active);
    inner.tabs[idx].cursor_line_col = (line.max(1), col.max(1));
    let tab = &inner.tabs[idx];
    StatusResult {
        meta: TabMeta::of(tab),
        stats: stats_of(tab),
    }
}

// ---------- 撤销 / 重做 ----------

#[derive(Debug, Serialize)]
pub struct HistoryEdit {
    pub from: usize,
    pub to: usize,
    pub inserted: String,
}

#[derive(Debug, Serialize)]
pub struct HistoryResult {
    pub edits: Vec<HistoryEdit>,
    /// 撤销/重做后的光标（UTF-16 偏移）
    pub cursor: usize,
    pub list: TabList,
}

/// 撤销并产出增量编辑序列（前端逐条 dispatch 给 CodeMirror）。
/// 编辑坐标从撤销前文本出发逐步演化：第 i 条作用于前 i-1 条应用后的文本。
#[tauri::command]
pub fn undo(state: State<'_, AppState>, tab_id: u64) -> Option<HistoryResult> {
    let mut inner = lock(&state);
    let idx = tab_idx(&inner, tab_id)?;
    let before = inner.tabs[idx].doc.text();
    let (ops, cursor_char) = inner.tabs[idx].doc.undo_with_ops()?;
    let edits = replay_to_utf16(&before, &ops, false);
    Some(HistoryResult {
        edits,
        cursor: char_to_utf16(&inner.tabs[idx].doc.text(), cursor_char),
        list: TabList::of(&inner),
    })
}

#[tauri::command]
pub fn redo(state: State<'_, AppState>, tab_id: u64) -> Option<HistoryResult> {
    let mut inner = lock(&state);
    let idx = tab_idx(&inner, tab_id)?;
    let before = inner.tabs[idx].doc.text();
    let (ops, cursor_char) = inner.tabs[idx].doc.redo_with_ops()?;
    let edits = replay_to_utf16(&before, &ops, true);
    Some(HistoryResult {
        edits,
        cursor: char_to_utf16(&inner.tabs[idx].doc.text(), cursor_char),
        list: TabList::of(&inner),
    })
}

/// 把内核编辑组换算成前端增量序列。
/// redo=false：ops 为逆时间序，每条把 [start, start+插入数) 替换为 removed；
/// redo=true：ops 为正时间序，每条把 [start, start+删除数) 替换为 inserted。
fn replay_to_utf16(
    start_text: &str,
    ops: &[editor_core::undo::Edit],
    redo: bool,
) -> Vec<HistoryEdit> {
    let mut text = start_text.to_string();
    let mut out = Vec::with_capacity(ops.len());
    for e in ops {
        let (old_len, new_text) = if redo {
            (e.chars_removed(), e.inserted.clone())
        } else {
            (e.chars_inserted(), e.removed.clone())
        };
        let from_c = e.start;
        let to_c = e.start + old_len;
        let from = char_to_utf16(&text, from_c);
        let to = char_to_utf16(&text, to_c);
        out.push(HistoryEdit {
            from,
            to,
            inserted: new_text.clone(),
        });
        // 文本演化到该条编辑应用后的状态
        let b_from = char_to_byte(&text, from_c);
        let b_to = char_to_byte(&text, to_c);
        text.replace_range(b_from..b_to, &new_text);
    }
    out
}

// ---------- 查找 / 替换 / 跳转 ----------

#[derive(Debug, serde::Deserialize)]
pub struct SearchOpts {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
}

#[derive(Debug, Serialize)]
pub struct SearchMatch {
    pub from: usize,
    pub to: usize,
}

#[derive(Debug, Serialize)]
pub struct SearchResult {
    pub matches: Vec<SearchMatch>,
    pub error: Option<String>,
}

fn search_options(o: &SearchOpts) -> SearchOptions {
    SearchOptions {
        case_sensitive: o.case_sensitive,
        whole_word: o.whole_word,
        regex: o.regex,
    }
}

#[tauri::command]
pub fn search(
    state: State<'_, AppState>,
    tab_id: u64,
    query: String,
    opts: SearchOpts,
) -> SearchResult {
    let inner = lock(&state);
    let Some(idx) = tab_idx(&inner, tab_id) else {
        return SearchResult {
            matches: vec![],
            error: None,
        };
    };
    let text = inner.tabs[idx].doc.text();
    match search::find_all(&text, &query, &search_options(&opts)) {
        Ok(ms) => SearchResult {
            matches: ms
                .into_iter()
                .map(|m| SearchMatch {
                    from: char_to_utf16(&text, m.start),
                    to: char_to_utf16(&text, m.end),
                })
                .collect(),
            error: None,
        },
        Err(e) => SearchResult {
            matches: vec![],
            error: Some(e.to_string()),
        },
    }
}

#[derive(Debug, Serialize)]
pub struct ReplaceAllResult {
    pub count: usize,
    pub text: String,
    pub message: String,
    pub list: TabList,
}

#[tauri::command]
pub fn replace_all(
    state: State<'_, AppState>,
    tab_id: u64,
    query: String,
    replacement: String,
    opts: SearchOpts,
) -> ReplaceAllResult {
    let mut inner = lock(&state);
    let Some(idx) = tab_idx(&inner, tab_id) else {
        return ReplaceAllResult {
            count: 0,
            text: String::new(),
            message: String::new(),
            list: TabList::of(&inner),
        };
    };
    if query.is_empty() {
        return ReplaceAllResult {
            count: 0,
            text: inner.tabs[idx].doc.text(),
            message: "查找内容为空".to_string(),
            list: TabList::of(&inner),
        };
    }
    let text = inner.tabs[idx].doc.text();
    match search::replace_all(&text, &query, &replacement, &search_options(&opts)) {
        Ok((new_text, n)) => {
            {
                let t = &mut inner.tabs[idx];
                t.doc.replace_all_text(&new_text);
                t.mixed_newline = t.doc.newline_info().mixed();
            }
            ReplaceAllResult {
                count: n,
                text: new_text.clone(),
                message: format!("已替换 {n} 处"),
                list: TabList::of(&inner),
            }
        }
        Err(e) => ReplaceAllResult {
            count: 0,
            text,
            message: format!("替换失败：{e}"),
            list: TabList::of(&inner),
        },
    }
}

#[derive(Debug, Serialize)]
pub struct GotoResult {
    /// 行首的 UTF-16 偏移
    pub from: usize,
    pub line: usize,
}

#[tauri::command]
pub fn goto_line(state: State<'_, AppState>, tab_id: u64, line: usize) -> Option<GotoResult> {
    let inner = lock(&state);
    let idx = tab_idx(&inner, tab_id)?;
    if line == 0 {
        return None;
    }
    let ch = inner.tabs[idx].doc.line_to_char(line - 1);
    let text = inner.tabs[idx].doc.text();
    Some(GotoResult {
        from: char_to_utf16(&text, ch),
        line,
    })
}

// ---------- 文本变换 ----------

#[derive(Debug, Serialize)]
pub struct TransformResult {
    pub text: String,
    pub message: Option<String>,
    pub list: TabList,
}

#[tauri::command]
pub fn transform(
    state: State<'_, AppState>,
    tab_id: u64,
    kind: String,
    line: Option<usize>,
) -> TransformResult {
    use editor_core::text::NormalForm;
    let mut inner = lock(&state);
    let Some(idx) = tab_idx(&inner, tab_id) else {
        return TransformResult {
            text: String::new(),
            message: None,
            list: TabList::of(&inner),
        };
    };
    let text = inner.tabs[idx].doc.text();
    let comment_prefix = inner.tabs[idx]
        .language
        .as_ref()
        .and_then(|l| l.line_comment.clone())
        .unwrap_or_else(|| "//".to_string());
    let line_idx = line.unwrap_or(0);
    let applied: Result<String, String> = match kind.as_str() {
        "upper" => Ok(tx::to_upper(&text)),
        "lower" => Ok(tx::to_lower(&text)),
        "title" => Ok(tx::to_title_case(&text)),
        "sentence" => Ok(tx::to_sentence_case(&text)),
        "full_to_half" => Ok(tx::full_to_half(&text)),
        "half_to_full" => Ok(tx::half_to_full(&text)),
        "nfc" => Ok(tx::normalize(&text, NormalForm::Nfc)),
        "nfd" => Ok(tx::normalize(&text, NormalForm::Nfd)),
        "nfkc" => Ok(tx::normalize(&text, NormalForm::Nfkc)),
        "nfkd" => Ok(tx::normalize(&text, NormalForm::Nfkd)),
        "tabs_to_spaces" => Ok(tx::tabs_to_spaces(&text, 4)),
        "spaces_to_tabs" => Ok(tx::leading_spaces_to_tabs(&text, 4)),
        "sort_asc" => Ok(tx::sort_lines(&text, true, false, false)),
        "sort_desc" => Ok(tx::sort_lines(&text, false, false, false)),
        "unique_lines" => Ok(tx::unique_lines(&text)),
        "reverse_lines" => Ok(tx::reverse_lines(&text)),
        "trim_trailing" => Ok(tx::trim_trailing_whitespace(&text)),
        "delete_line" => Ok(tx::delete_line(&text, line_idx)),
        "duplicate_line" => Ok(tx::duplicate_line(&text, line_idx)),
        "move_up" => Ok(tx::move_line(&text, line_idx, -1)),
        "move_down" => Ok(tx::move_line(&text, line_idx, 1)),
        "toggle_comment" => Ok(tx::toggle_line_comment(&text, &comment_prefix)),
        _ => Err(format!("未知变换：{kind}")),
    };
    match applied {
        Ok(new_text) => {
            let t = &mut inner.tabs[idx];
            t.doc.replace_all_text(&new_text);
            t.mixed_newline = t.doc.newline_info().mixed();
            TransformResult {
                text: new_text,
                message: None,
                list: TabList::of(&inner),
            }
        }
        Err(e) => TransformResult {
            text,
            message: Some(e),
            list: TabList::of(&inner),
        },
    }
}

// ---------- 编码 / 换行 / 语言 ----------

#[derive(Debug, Serialize)]
pub struct EncodingResult {
    /// true = 已按新编码重新解码
    pub text: String,
    pub message: String,
    pub list: TabList,
}

/// 切换编码（FR-2.2）：干净且有文件 → 立即以新编码重新解码；
/// 否则仅设为保存目标编码。
#[tauri::command]
pub fn set_encoding(state: State<'_, AppState>, tab_id: u64, enc: String) -> EncodingResult {
    let mut inner = lock(&state);
    let Some(idx) = tab_idx(&inner, tab_id) else {
        return EncodingResult {
            text: String::new(),
            message: String::new(),
            list: TabList::of(&inner),
        };
    };
    if inner.tabs[idx].doc.encoding_name() == enc {
        return EncodingResult {
            text: inner.tabs[idx].doc.text(),
            message: String::new(),
            list: TabList::of(&inner),
        };
    }
    if !inner.tabs[idx].is_dirty() {
        if let Some(path) = inner.tabs[idx].doc.path().cloned() {
            if let Ok(bytes) = std::fs::read(&path) {
                // 必须用显式解码结果加载（egui 版教训：load_bytes 会自动检测）
                let d = encoding::decode_with(&bytes, &enc);
                inner.tabs[idx]
                    .doc
                    .load_decoded(&d.text, &d.encoding, d.had_bom);
                inner.tabs[idx].mark_saved();
                inner.tabs[idx].mixed_newline = inner.tabs[idx].doc.newline_info().mixed();
                return EncodingResult {
                    text: inner.tabs[idx].doc.text(),
                    message: format!("已以 {} 重新解码", d.encoding),
                    list: TabList::of(&inner),
                };
            }
        }
    }
    let (text, message) = if inner.tabs[idx].doc.set_encoding(&enc) {
        (
            inner.tabs[idx].doc.text(),
            format!("保存时将使用 {enc}（有损编码建议先确认不可表示字符）"),
        )
    } else {
        (inner.tabs[idx].doc.text(), format!("未知编码：{enc}"))
    };
    EncodingResult {
        text,
        message,
        list: TabList::of(&inner),
    }
}

#[derive(Debug, Serialize)]
pub struct NewlineResult {
    pub text: String,
    pub message: String,
    pub list: TabList,
}

#[tauri::command]
pub fn set_newline(state: State<'_, AppState>, tab_id: u64, le: String) -> NewlineResult {
    let mut inner = lock(&state);
    let Some(idx) = tab_idx(&inner, tab_id) else {
        return NewlineResult {
            text: String::new(),
            message: String::new(),
            list: TabList::of(&inner),
        };
    };
    let Some(le) = crate::state::newline_from_str(&le) else {
        return NewlineResult {
            text: inner.tabs[idx].doc.text(),
            message: format!("未知换行符：{le}"),
            list: TabList::of(&inner),
        };
    };
    let label = le.label().to_string();
    inner.tabs[idx].doc.set_newline(le);
    inner.tabs[idx].mixed_newline = inner.tabs[idx].doc.newline_info().mixed();
    NewlineResult {
        text: inner.tabs[idx].doc.text(),
        message: format!("换行符已转换为 {label}"),
        list: TabList::of(&inner),
    }
}

#[tauri::command]
pub fn list_languages(state: State<'_, AppState>) -> Vec<LangInfo> {
    let inner = lock(&state);
    inner
        .languages
        .iter()
        .map(|(d, builtin)| LangInfo {
            name: d.name.clone(),
            builtin: *builtin,
        })
        .collect()
}

#[tauri::command]
pub fn set_language(state: State<'_, AppState>, tab_id: u64, name: Option<String>) -> TabList {
    let mut inner = lock(&state);
    if let Some(idx) = tab_idx(&inner, tab_id) {
        match name {
            None => {
                // 自动检测
                let detected = inner.tabs[idx].doc.path().and_then(|p| {
                    syntax_engine::detect_for(
                        &inner
                            .languages
                            .iter()
                            .map(|(d, _)| d.clone())
                            .collect::<Vec<_>>(),
                        p,
                    )
                    .cloned()
                });
                inner.tabs[idx].auto_language = true;
                inner.tabs[idx].set_language(detected);
            }
            Some(n) => {
                let def = inner
                    .languages
                    .iter()
                    .find(|(d, _)| d.name == n)
                    .map(|(d, _)| d.clone());
                inner.tabs[idx].auto_language = false;
                inner.tabs[idx].set_language(def);
            }
        }
    }
    TabList::of(&inner)
}

// ---------- 高亮 / 大纲 / 补全 ----------

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SpanKind {
    Plain,
    Keyword,
    Str,
    Comment,
    Number,
    Function,
    Type,
    Property,
    Constant,
    Operator,
    Punct,
}

impl SpanKind {
    fn of(k: syntax_engine::TokenKind) -> Self {
        use syntax_engine::TokenKind::*;
        match k {
            Plain => SpanKind::Plain,
            Keyword => SpanKind::Keyword,
            String => SpanKind::Str,
            Comment => SpanKind::Comment,
            Number => SpanKind::Number,
            Function => SpanKind::Function,
            Type => SpanKind::Type,
            Property => SpanKind::Property,
            Constant => SpanKind::Constant,
            Operator => SpanKind::Operator,
            Punct => SpanKind::Punct,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct HighlightSpan {
    pub from: usize,
    pub to: usize,
    pub kind: SpanKind,
}

#[derive(Debug, Serialize)]
pub struct HighlightResult {
    pub spans: Vec<HighlightSpan>,
    /// 文本超过上限时为 true（不做高亮）
    pub truncated: bool,
}

fn byte_spans_to_utf16(text: &str, spans: &[Span]) -> Vec<HighlightSpan> {
    // 字节偏移 → UTF-16 偏移：建 (字节起点 → u16 起点) 有序表后二分
    // （高亮为防抖操作，O(n) 建表可接受）
    let mut entries: Vec<(usize, usize)> = text
        .char_indices()
        .scan(0usize, |u16acc, (b, c)| {
            let u16start = *u16acc;
            *u16acc += c.len_utf16();
            Some((b, u16start))
        })
        .collect();
    let total_u16 = text.chars().map(|c| c.len_utf16()).sum();
    entries.push((text.len(), total_u16));
    let byte_to_u16 = |b: usize| -> usize {
        match entries.binary_search_by_key(&b, |&(bb, _)| bb) {
            Ok(i) => entries[i].1,
            Err(i) => entries.get(i).map(|x| x.1).unwrap_or(total_u16),
        }
    };
    spans
        .iter()
        .map(|s| HighlightSpan {
            from: byte_to_u16(s.start.min(text.len())),
            to: byte_to_u16(s.end.min(text.len())),
            kind: SpanKind::of(s.kind),
        })
        .collect()
}

#[tauri::command]
pub fn highlight(state: State<'_, AppState>, tab_id: u64) -> HighlightResult {
    let mut inner = lock(&state);
    let Some(idx) = tab_idx(&inner, tab_id) else {
        return HighlightResult {
            spans: vec![],
            truncated: true,
        };
    };
    let text = inner.tabs[idx].doc.text();
    if text.len() > MAX_HIGHLIGHT_BYTES {
        return HighlightResult {
            spans: vec![],
            truncated: true,
        };
    }
    let lang = inner.tabs[idx].language.clone();
    let spans: Vec<Span> = match &mut inner.tabs[idx].ts_highlighter {
        Some(ts) => ts.spans_for(&text),
        None => syntax_engine::highlight(&text, lang.as_ref()),
    };
    HighlightResult {
        spans: byte_spans_to_utf16(&text, &spans),
        truncated: false,
    }
}

#[derive(Debug, Serialize)]
pub struct OutlineEntry {
    pub line: usize,
    pub label: String,
    pub kind: String,
}

#[tauri::command]
pub fn outline(state: State<'_, AppState>, tab_id: u64) -> Vec<OutlineEntry> {
    let inner = lock(&state);
    let Some(idx) = tab_idx(&inner, tab_id) else {
        return vec![];
    };
    let t = &inner.tabs[idx];
    syntax_engine::outline(&t.doc.text(), t.language.as_ref())
        .into_iter()
        .map(|i| OutlineEntry {
            line: i.line,
            label: i.label,
            kind: i.kind,
        })
        .collect()
}

#[tauri::command]
pub fn completions(state: State<'_, AppState>, tab_id: u64, prefix: String) -> Vec<String> {
    let inner = lock(&state);
    let Some(idx) = tab_idx(&inner, tab_id) else {
        return vec![];
    };
    tx::word_completions(&inner.tabs[idx].doc.text(), &prefix, 30)
}

// ---------- 脚本 ----------

#[tauri::command]
pub fn list_scripts() -> Vec<ScriptEntry> {
    load_scripts()
}

#[derive(Debug, Serialize)]
pub struct ScriptResult {
    pub text: String,
    /// 选区 (anchor, head)，UTF-16 偏移
    pub sel: Option<(usize, usize)>,
    pub message: String,
    pub changed: bool,
    pub list: TabList,
}

#[tauri::command]
pub fn run_script(
    state: State<'_, AppState>,
    tab_id: u64,
    name: String,
    anchor: Option<usize>,
    head: Option<usize>,
) -> ScriptResult {
    let mut inner = lock(&state);
    let Some(idx) = tab_idx(&inner, tab_id) else {
        return ScriptResult {
            text: String::new(),
            sel: None,
            message: String::new(),
            changed: false,
            list: TabList::of(&inner),
        };
    };
    let dir = platform::session::config_dir()
        .map(|d| d.join("scripts"))
        .unwrap_or_default();
    let scripts = script_host::load_scripts(&dir);
    let Some(s) = scripts.iter().find(|s| s.name == name) else {
        return ScriptResult {
            text: inner.tabs[idx].doc.text(),
            sel: None,
            message: format!("找不到脚本 {name}"),
            changed: false,
            list: TabList::of(&inner),
        };
    };
    let text = inner.tabs[idx].doc.text();
    // 选区以前端传入的实时值为准（TabState.selection 仅在编辑时更新，会过期）
    let selection = match (anchor, head) {
        (Some(a), Some(h)) => {
            let (lo, hi) = if a <= h { (a, h) } else { (h, a) };
            Some((utf16_to_char(&text, lo), utf16_to_char(&text, hi)))
        }
        _ => inner.tabs[idx].selection,
    };
    let api = ScriptApi {
        text: text.clone(),
        selection,
        status: None,
    };
    match script_host::run_script(&s.source, &api) {
        Ok(out) => {
            let changed = out.text != text;
            if changed {
                let t = &mut inner.tabs[idx];
                t.doc.replace_all_text(&out.text);
                t.mixed_newline = t.doc.newline_info().mixed();
            }
            let sel = out.selection.map(|(a, h)| {
                let new_text = inner.tabs[idx].doc.text();
                (
                    char_to_utf16(&new_text, a.min(h)),
                    char_to_utf16(&new_text, a.max(h)),
                )
            });
            ScriptResult {
                text: inner.tabs[idx].doc.text(),
                sel,
                message: out
                    .status
                    .unwrap_or_else(|| format!("脚本 {} 执行完成", s.name)),
                changed,
                list: TabList::of(&inner),
            }
        }
        Err(e) => ScriptResult {
            text,
            sel: None,
            message: format!("脚本 {} 出错：{e}", s.name),
            changed: false,
            list: TabList::of(&inner),
        },
    }
}

// ---------- 外观 ----------

#[tauri::command]
pub fn set_theme(state: State<'_, AppState>, theme: String) {
    let mut inner = lock(&state);
    inner.theme = theme;
    save_session(&inner);
}

/// 只持久化会话（不动文件）。退出前由前端调用，保证光标位置最新。
#[tauri::command]
pub fn save_session_now(state: State<'_, AppState>) {
    let inner = lock(&state);
    save_session(&inner);
}
