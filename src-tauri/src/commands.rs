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
    save_session, save_tab, AppInner, AppState, SaveOutcome, TabList, TabMeta, TabPayload,
    TabState, MAX_HIGHLIGHT_BYTES,
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
}

#[tauri::command]
pub fn open_file(state: State<'_, AppState>, path: String) -> OpenResult {
    let mut inner = lock(&state);
    let p = PathBuf::from(&path);
    let (idx, message) = open_path(&mut inner, &p);
    save_session(&inner);
    // 失败路径 open_path 已移除新建标签，idx 为 None——不得按下标误取其他标签
    OpenResult {
        tab: idx.and_then(|i| inner.tabs.get(i).map(TabPayload::of)),
        list: TabList::of(&inner),
        message,
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
    let Some(idx) = tab_idx(&inner, tab_id) else {
        return deny(None, &inner);
    };
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
    // UTF-16 ↔ 字符索引走 rope 原生 O(log n) 转换，不再物化全文。
    for op in ops {
        let doc = &mut inner.tabs[idx].doc;
        let from = doc.utf16_to_char(op.from);
        let to = doc.utf16_to_char(op.to);
        if from < to {
            doc.delete_range(from, to);
        }
        if !op.inserted.is_empty() {
            doc.insert_text(from, &op.inserted);
        }
    }
    let doc = &inner.tabs[idx].doc;
    let a = doc.utf16_to_char(anchor.min(head));
    let h = doc.utf16_to_char(anchor.max(head));
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
    // 撤销可能回到换行风格不同的旧文本（如 set_newline 前的状态），元数据须跟随
    inner.tabs[idx].mixed_newline = inner.tabs[idx].doc.newline_info().mixed();
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
    inner.tabs[idx].mixed_newline = inner.tabs[idx].doc.newline_info().mixed();
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
        Ok(ms) => {
            // 匹配按字符序递增：单遍游标换算 UTF-16，避免每匹配从头计数
            let mut cur = crate::state::Utf16Cursor::new(&text);
            SearchResult {
                matches: ms
                    .into_iter()
                    .map(|m| SearchMatch {
                        from: cur.char_to_u16(m.start),
                        to: cur.char_to_u16(m.end),
                    })
                    .collect(),
                error: None,
            }
        }
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

/// 文本变换（CotEditor 语义）：
/// - 字符类变换（大小写/全半角/规范化）：有选区只作用于选区，否则全文；
/// - 行类变换（排序/去重/反转/去尾空白/Tab 转换）：有选区作用于所选行，否则全文；
/// - 切换行注释：有选区作用于所选行，否则当前行；
/// - 行操作（删除/复制/移动行）：作用于光标行。
/// `pos` 为光标（UTF-16 偏移），行号由后端按 \n/\r\n/\r 统一推导——
/// 前端 CodeMirror 只按 \n 分行，CR 换行文件中两套行号不一致。
#[tauri::command]
pub fn transform(
    state: State<'_, AppState>,
    tab_id: u64,
    kind: String,
    pos: Option<usize>,
    sel: Option<(usize, usize)>,
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

    // 选区（UTF-16 → 字符索引），空/退化选区视为无选区
    let (sel_from, sel_to) = match sel {
        Some((a, b)) => {
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            (Some(lo), Some(hi))
        }
        None => (None, None),
    };
    let has_sel = matches!((sel_from, sel_to), (Some(a), Some(b)) if a < b);

    let text = inner.tabs[idx].doc.text();
    let comment_prefix = inner.tabs[idx]
        .language
        .as_ref()
        .and_then(|l| l.line_comment.clone())
        .unwrap_or_else(|| "//".to_string());

    // 各变换的文本函数（toggle_comment 需捕获注释前缀，统一装 Box）
    let f: Box<dyn FnOnce(&str) -> String> = match kind.as_str() {
        "upper" => Box::new(tx::to_upper),
        "lower" => Box::new(tx::to_lower),
        "title" => Box::new(tx::to_title_case),
        "sentence" => Box::new(tx::to_sentence_case),
        "full_to_half" => Box::new(tx::full_to_half),
        "half_to_full" => Box::new(tx::half_to_full),
        "nfc" => Box::new(|s| tx::normalize(s, NormalForm::Nfc)),
        "nfd" => Box::new(|s| tx::normalize(s, NormalForm::Nfd)),
        "nfkc" => Box::new(|s| tx::normalize(s, NormalForm::Nfkc)),
        "nfkd" => Box::new(|s| tx::normalize(s, NormalForm::Nfkd)),
        "tabs_to_spaces" => Box::new(|s| tx::tabs_to_spaces(s, 4)),
        "spaces_to_tabs" => Box::new(|s| tx::leading_spaces_to_tabs(s, 4)),
        "sort_asc" => Box::new(|s| tx::sort_lines(s, true, false, false)),
        "sort_desc" => Box::new(|s| tx::sort_lines(s, false, false, false)),
        "unique_lines" => Box::new(tx::unique_lines),
        "reverse_lines" => Box::new(tx::reverse_lines),
        "trim_trailing" => Box::new(tx::trim_trailing_whitespace),
        "toggle_comment" => {
            let prefix = comment_prefix;
            Box::new(move |s| tx::toggle_line_comment(s, &prefix))
        }
        "delete_line" | "duplicate_line" | "move_up" | "move_down" => {
            // 行操作走下方独立路径（需行索引）
            let line = tx::line_index_at_byte(
                &text,
                char_to_byte(&text, inner.tabs[idx].doc.utf16_to_char(pos.unwrap_or(0))),
            );
            let new_text = match kind.as_str() {
                "delete_line" => tx::delete_line(&text, line),
                "duplicate_line" => tx::duplicate_line(&text, line),
                "move_up" => tx::move_line(&text, line, -1),
                _ => tx::move_line(&text, line, 1),
            };
            apply_whole_doc(&mut inner, idx, new_text);
            return transform_result(&inner, idx);
        }
        _ => {
            return TransformResult {
                text,
                message: Some(format!("未知变换：{kind}")),
                list: TabList::of(&inner),
            }
        }
    };

    let doc = &mut inner.tabs[idx].doc;
    if has_sel {
        // 有选区：字符类直接作用于选区；行类/注释先扩展到完整行
        let cs = doc.utf16_to_char(sel_from.unwrap());
        let ce = doc.utf16_to_char(sel_to.unwrap());
        let is_line_kind = !matches!(
            kind.as_str(),
            "upper" | "lower" | "title" | "sentence" | "full_to_half" | "half_to_full"
                | "nfc" | "nfd" | "nfkc" | "nfkd"
        );
        if is_line_kind {
            let (bs, be) = (char_to_byte(&text, cs), char_to_byte(&text, ce));
            let (ls, le) = tx::expand_to_full_lines(&text, bs, be);
            doc.transform_range(
                text[..ls].chars().count(),
                text[..le].chars().count(),
                f,
            );
        } else {
            doc.transform_range(cs, ce, f);
        }
    } else if kind == "toggle_comment" {
        // 无选区的注释切换只作用当前行（CotEditor 语义）
        let cursor = doc.utf16_to_char(pos.unwrap_or(0));
        let bc = char_to_byte(&text, cursor);
        let (ls, le) = tx::expand_to_full_lines(&text, bc, bc);
        doc.transform_range(
            text[..ls].chars().count(),
            text[..le].chars().count(),
            f,
        );
    } else {
        apply_whole_doc(&mut inner, idx, f(&text));
    }
    transform_result(&inner, idx)
}

fn apply_whole_doc(inner: &mut crate::state::AppInner, idx: usize, new_text: String) {
    let t = &mut inner.tabs[idx];
    t.doc.replace_all_text(&new_text);
    t.mixed_newline = t.doc.newline_info().mixed();
}

fn transform_result(inner: &crate::state::AppInner, idx: usize) -> TransformResult {
    TransformResult {
        text: inner.tabs[idx].doc.text(),
        message: None,
        list: TabList::of(inner),
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
    // （高亮为防抖操作，O(n) 建表可接受）；终点在扫描中一并得出，避免二次遍历
    let mut total_u16 = 0usize;
    let mut entries: Vec<(usize, usize)> = text
        .char_indices()
        .scan(0usize, |u16acc, (b, c)| {
            let u16start = *u16acc;
            *u16acc += c.len_utf16();
            total_u16 = *u16acc;
            Some((b, u16start))
        })
        .collect();
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
    // 不相交字段借用：ts_highlighter 可变、language 只读，无需克隆整个 LanguageDef
    let spans: std::sync::Arc<Vec<Span>> = {
        let tab = &mut inner.tabs[idx];
        match &mut tab.ts_highlighter {
            Some(ts) => ts.spans_for(&text),
            None => std::sync::Arc::new(syntax_engine::highlight(&text, tab.language.as_ref())),
        }
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
    // 阶段一（持锁）：快照脚本输入。执行期间放锁——慢/死循环脚本
    // 不再冻结自动保存、外部修改检测与其他命令。
    let snapshot = {
        let inner = lock(&state);
        let Some(idx) = tab_idx(&inner, tab_id) else {
            return ScriptResult {
                text: String::new(),
                sel: None,
                message: String::new(),
                changed: false,
                list: TabList::of(&inner),
            };
        };
        let text = inner.tabs[idx].doc.text();
        // 选区以前端传入的实时值为准（TabState.selection 仅在编辑时更新，会过期）
        let selection = match (anchor, head) {
            (Some(a), Some(h)) => {
                let (lo, hi) = if a <= h { (a, h) } else { (h, a) };
                Some((
                    inner.tabs[idx].doc.utf16_to_char(lo),
                    inner.tabs[idx].doc.utf16_to_char(hi),
                ))
            }
            _ => inner.tabs[idx].selection,
        };
        (text, selection, inner.tabs[idx].doc.revision())
    };
    let (text, selection, revision) = snapshot;

    // 阶段二（无锁）：读取并执行脚本。
    // 按名直读单文件（不再每次执行全目录扫描）；名字即文件名，
    // 拒绝路径分隔符与 ".." 防路径注入。
    let source = platform::session::config_dir()
        .map(|d| d.join("scripts"))
        .and_then(|dir| read_script_source(&dir, &name));
    let Some(source) = source else {
        let inner = lock(&state);
        return ScriptResult {
            text: current_text_or_empty(&inner, tab_id),
            sel: None,
            message: format!("找不到脚本 {name}"),
            changed: false,
            list: TabList::of(&inner),
        };
    };
    let api = ScriptApi {
        text: text.clone(),
        selection,
        status: None,
    };
    let outcome = script_host::run_script(&source, &api);

    // 阶段三（持锁）：校验内容未被并发修改后写回
    let mut inner = lock(&state);
    let Some(idx) = tab_idx(&inner, tab_id) else {
        return ScriptResult {
            text: String::new(),
            sel: None,
            message: format!("脚本 {name} 执行期间标签页已关闭"),
            changed: false,
            list: TabList::of(&inner),
        };
    };
    match outcome {
        Ok(out) => {
            let changed = out.text != text;
            if changed {
                if inner.tabs[idx].doc.revision() != revision {
                    // 执行期间文档被编辑：基于旧文本的结果直接丢弃，防止覆盖用户输入
                    return ScriptResult {
                        text: inner.tabs[idx].doc.text(),
                        sel: None,
                        message: format!(
                            "脚本 {name} 执行期间文档已修改，其结果已被丢弃（可重新运行）"
                        ),
                        changed: false,
                        list: TabList::of(&inner),
                    };
                }
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
                    .unwrap_or_else(|| format!("脚本 {} 执行完成", name)),
                changed,
                list: TabList::of(&inner),
            }
        }
        Err(e) => ScriptResult {
            text,
            sel: None,
            message: format!("脚本 {name} 出错：{e}"),
            changed: false,
            list: TabList::of(&inner),
        },
    }
}

fn current_text_or_empty(inner: &crate::state::AppInner, tab_id: u64) -> String {
    inner
        .tabs
        .iter()
        .find(|t| t.id == tab_id)
        .map(|t| t.doc.text())
        .unwrap_or_default()
}

/// 按脚本名读取单个脚本源码。名字来自前端菜单，必须视作不可信输入：
/// 拒绝路径分隔符 / ".."，只允许拼到 `scripts/<name>.js`。
fn read_script_source(dir: &std::path::Path, name: &str) -> Option<String> {
    if name.is_empty() || name.contains(['/', '\\']) || name.contains("..") {
        return None;
    }
    std::fs::read_to_string(dir.join(format!("{name}.js"))).ok()
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
