//! platform：平台适配层（FR-9.5 / FR-10 / NFR-8）。
//!
//! GUI 框架之外的平台差异都收敛在这里：
//! - [`fs`]：原子写入（临时文件 + rename，FR-1.5 防 half-write）
//! - [`session`]：会话持久化
//!
//! 文件对话框由 Tauri dialog plugin 提供（原 rfd 封装与 egui 字体回退
//! 随 UI 迁移废弃）。
//!
//! TODO(M1+)：打印（FR-10）、文件关联注册（FR-9.5）、单实例 IPC（FR-9.4）、
//! macOS 全局菜单栏、任务栏进度。

pub mod fs;
pub mod session;
