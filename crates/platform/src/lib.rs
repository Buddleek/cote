//! platform：平台适配层（FR-9.5 / FR-10 / NFR-8）。
//!
//! GUI 框架之外的平台差异都收敛在这里：
//! - [`dialogs`]：原生文件对话框
//! - [`fs`]：原子写入（临时文件 + rename，FR-1.5 防 half-write）
//! - [`fonts`]：CJK 字体回退（中英混排正确，FR-7.1）
//!
//! TODO(M1+)：打印（FR-10）、文件关联注册（FR-9.5）、单实例 IPC（FR-9.4）、
//! macOS 全局菜单栏、任务栏进度、深浅色系统事件。

pub mod dialogs;
pub mod fonts;
pub mod fs;
pub mod session;
