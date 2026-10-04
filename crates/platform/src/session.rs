//! 会话持久化（FR-1.5 崩溃恢复）：记录打开的文件与光标位置，
//! 启动时恢复。存储于用户配置目录 `cote/session.json`。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionTab {
    pub path: String,
    #[serde(default)]
    pub line: Option<usize>,
    #[serde(default)]
    pub col: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub tabs: Vec<SessionTab>,
    #[serde(default)]
    pub active: usize,
    /// 外观偏好："dark" / "light" / "system"（缺省跟随系统）
    #[serde(default)]
    pub theme: Option<String>,
}

/// 用户配置目录（Windows: %APPDATA%\cote；macOS: ~/Library/Application Support/cote；Linux: ~/.config/cote）。
pub fn config_dir() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("cote"))
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Library").join("Application Support").join("cote"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config").join("cote")))
    }
}

fn session_file() -> Option<PathBuf> {
    config_dir().map(|d| d.join("session.json"))
}

/// 保存会话（原子写入，防崩溃半写；失败静默——会话持久化不应影响主流程）。
pub fn save_session(session: &Session) {
    let Some(file) = session_file() else { return };
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(session) {
        let _ = crate::fs::write_atomic(&file, json.as_bytes());
    }
}

/// 读取会话；损坏或不存在时返回 None。
pub fn load_session() -> Option<Session> {
    let file = session_file()?;
    let content = std::fs::read_to_string(file).ok()?;
    serde_json::from_str(&content).ok()
}
