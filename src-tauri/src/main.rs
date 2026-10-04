// release 版使用 Windows GUI 子系统：双击/启动器打开时不弹控制台窗口。
// debug 版保留控制台，便于开发时看 eprintln/panic 输出。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! cote 应用入口：CLI 参数解析（FR-9.4）+ 启动 Tauri app。
//! CLI 语义与 egui 版一致：多文件多标签、`--line/--column` 跳转。

use std::path::PathBuf;

/// release 版无控制台子系统时的输出：
/// - stdout 被重定向（管道/文件）→ 直接走 Rust stdout；
/// - 从已有终端直接运行（无重定向）→ 附着父进程控制台并写入 CONOUT$；
/// - 双击启动（无父终端）→ 输出丢弃（GUI 场景）。
#[cfg(all(windows, not(debug_assertions)))]
fn print_to_parent_console(msg: &str) {
    use std::io::Write;
    unsafe {
        use windows_sys::Win32::System::Console::{
            AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE,
        };
        // 已重定向？→ 管道/文件句柄有效，println 正常可用
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        let redirected = !h.is_null() && h as usize != usize::MAX;
        if redirected {
            print!("{msg}");
            return;
        }
        // 从 cmd/PowerShell 直接运行：附着父终端
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        // 重开控制台输出设备（std 以 \\.\ 设备路径走 CreateFileW）
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .write(true)
            .open("\\\\.\\CONOUT$")
        {
            let _ = f.write_all(msg.as_bytes());
            let _ = f.flush();
        }
    }
}

#[cfg(not(all(windows, not(debug_assertions))))]
fn print_to_parent_console(msg: &str) {
    print!("{msg}");
}

fn main() {
    let mut files: Vec<PathBuf> = vec![];
    let mut line: Option<usize> = None;
    let mut column: Option<usize> = None;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--version" | "-V" => {
                print_to_parent_console(&format!("cote {}\n", env!("CARGO_PKG_VERSION")));
                return;
            }
            "--help" | "-h" => {
                let mut help = String::new();
                help.push_str("cote — 轻量跨平台纯文本编辑器（CotEditor 风格，Rust 实现）\n");
                help.push('\n');
                help.push_str("用法:\n");
                help.push_str("  cote [文件...]         打开一个或多个文件（多标签页）\n");
                help.push_str("  cote 文件 --line N     打开后跳转到第 N 行\n");
                help.push_str("  cote 文件 --column M   打开后跳转到第 M 列（配合 --line）\n");
                help.push_str("  cote --version         显示版本\n");
                help.push_str("  cote --help            显示帮助\n");
                print_to_parent_console(&help);
                return;
            }
            "--line" | "-l" => {
                line = args.next().and_then(|v| v.parse().ok());
            }
            "--column" | "-c" => {
                column = args.next().and_then(|v| v.parse().ok());
            }
            other => files.push(PathBuf::from(other)),
        }
    }

    if let Err(e) = cote_lib::run(files, line, column) {
        eprintln!("cote 启动失败：{e}");
        std::process::exit(1);
    }
}
