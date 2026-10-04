//! 原生文件对话框封装（rfd 同步版）。
//!
//! 注意：同步对话框会阻塞事件循环。Windows/Linux 上可接受；
//! macOS 上 M1+ 应换成 `rfd::AsyncFileDialog`（见需求文档 M3 项）。

use std::path::PathBuf;

pub fn pick_open_file() -> Option<PathBuf> {
    rfd::FileDialog::new().pick_file()
}

pub fn pick_save_file(default_name: Option<&str>) -> Option<PathBuf> {
    let mut dlg = rfd::FileDialog::new();
    if let Some(name) = default_name {
        dlg = dlg.set_file_name(name);
    }
    dlg.save_file()
}
