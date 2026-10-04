//! editor-core：纯 Rust 编辑内核，无任何 GUI 依赖。
//!
//! 设计目标（见需求文档 NFR-9）：
//! - 可独立单测 / fuzz；
//! - 编辑全程 Unicode / grapheme 安全；
//! - 除显式转换外，未编辑内容字节级不变；
//! - 撤销历史分组正确（连续输入合并、整组回退）。
//!
//! 模块划分：
//! - [`buffer`]：rope 文本缓冲（Ropey 封装）
//! - [`undo`]：编辑记录与撤销分组数据结构
//! - [`document`]：文档状态机（缓冲 + 编码 + 换行 + 撤销栈）
//! - [`encoding`]：BOM / 编码检测、解码、有损编码与字节编码
//! - [`newline`]：换行符识别与转换
//! - [`search`]：查找 / 替换（纯文本与正则）
//! - [`text`]：文本变换（大小写、全半角、规范化、行操作、统计）

pub mod buffer;
pub mod document;
pub mod encoding;
pub mod newline;
pub mod search;
pub mod text;
pub mod undo;
