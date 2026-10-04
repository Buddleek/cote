//! Cote Tauri 后端：命令注册、状态初始化、后台定时器
//! （自动保存 30s / 外部修改检测 2s）。

mod commands;
mod state;

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use syntax_engine::LanguageDef;
use tauri::{Emitter, Manager};

use state::{
    autosave_all, check_external_changed, new_tab_silent, open_path, restore_session, AppInner,
    AppState, AUTO_SAVE_SECS, EXTERNAL_CHECK_SECS,
};

pub fn run(
    files: Vec<PathBuf>,
    line: Option<usize>,
    column: Option<usize>,
) -> Result<(), Box<dyn std::error::Error>> {
    // ---- 状态初始化（窗口出现前完成，get_init 一次性下发） ----
    let mut inner = AppInner {
        tabs: vec![],
        active: 0,
        next_tab_id: 0,
        // 对照 egui 版：未命名从 1 开始计数
        next_untitled_no: 1,
        languages: Vec::new(),
        theme: "system".to_string(),
        pending_goto: None,
        notices: vec![],
    };
    let mut languages: Vec<(LanguageDef, bool)> = syntax_engine::builtin()
        .iter()
        .cloned()
        .map(|d| (d, true))
        .collect();
    if let Some(dir) = platform::session::config_dir() {
        for d in syntax_engine::load_user_syntaxes(&dir.join("syntaxes")) {
            languages.push((d, false));
        }
    }
    inner.languages = languages;
    new_tab_silent(&mut inner);

    let mut opened = false;
    for p in &files {
        if p.exists() {
            open_path(&mut inner, p);
            opened = true;
        } else {
            inner.notices.push(format!("文件不存在：{}", p.display()));
        }
    }
    if !opened && files.is_empty() {
        restore_session(&mut inner);
    }
    if line.is_some() || column.is_some() {
        inner.pending_goto = Some((line.unwrap_or(1), column.unwrap_or(1)));
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState(Mutex::new(inner)))
        .invoke_handler(tauri::generate_handler![
            commands::get_init,
            commands::new_tab,
            commands::open_file,
            commands::close_tab,
            commands::set_active,
            commands::save,
            commands::save_as,
            commands::save_all,
            commands::reload_tab,
            commands::exit_check,
            commands::apply_edit,
            commands::doc_status,
            commands::undo,
            commands::redo,
            commands::search,
            commands::replace_all,
            commands::goto_line,
            commands::transform,
            commands::set_encoding,
            commands::set_newline,
            commands::list_languages,
            commands::set_language,
            commands::highlight,
            commands::outline,
            commands::completions,
            commands::list_scripts,
            commands::run_script,
            commands::set_theme,
            commands::save_session_now,
        ])
        .setup(|app| {
            spawn_timers(app.handle().clone());
            Ok(())
        })
        .run(tauri::generate_context!())?;
    Ok(())
}

/// 后台线程：外部修改检测（2s 轮询，事件推送前端）+ 自动保存（30s）。
/// 与命令共用 AppState 互斥锁；锁持有时间均为短临界区。
fn spawn_timers(app: tauri::AppHandle) {
    let app_for_check = app.clone();
    // 外部修改检测
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(EXTERNAL_CHECK_SECS));
        let state = app_for_check.state::<AppState>();
        let mut inner = match state.0.lock() {
            Ok(g) => g,
            Err(_) => continue,
        };
        let mut changes = vec![];
        for i in 0..inner.tabs.len() {
            if let Some((tab_id, kind)) = check_external_changed(&mut inner, i) {
                changes.push((tab_id, kind));
            }
        }
        drop(inner);
        for (tab_id, kind) in changes {
            let _ = app_for_check.emit(
                "file-changed",
                serde_json::json!({ "tabId": tab_id, "kind": kind }),
            );
        }
    });
    // 自动保存 + 会话
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(AUTO_SAVE_SECS));
        let state = app.state::<AppState>();
        let mut inner = match state.0.lock() {
            Ok(g) => g,
            Err(_) => continue,
        };
        autosave_all(&mut inner);
    });
}
