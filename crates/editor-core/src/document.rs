//! 文档状态机：缓冲区 + 编码/换行元数据 + 撤销栈（FR-1/2/3 的核心）。
//!
//! 撤销分组策略（FR-3.1）：
//! - 连续追加输入合并为一组（Typing run）；
//! - 连续退格合并为一组（Backspacing run）；
//! - 显式 `begin_group()` / `end_group()` 之间的所有编辑整组撤销；
//! - 其余编辑各自成组；
//! - 撤销后新编辑会清空重做栈。

use std::path::PathBuf;

use crate::buffer::Buffer;
use crate::encoding::{self, EncodeResult};
use crate::newline::{self, LineEnding, NewlineInfo};
use crate::text::TextStats;
use crate::undo::{Edit, EditGroup};

#[derive(Debug)]
enum OpenGroup {
    Idle,
    /// 连续追加输入：`text` 是本次 run 累计插入的内容
    Typing {
        start: usize,
        text: String,
        cursor_before: usize,
    },
    /// 连续退格：`removed` 是累计删除的内容（按时间顺序，最早删的在前）
    Backspacing {
        start: usize,
        removed: String,
        cursor_before: usize,
    },
    /// 显式分组
    Explicit {
        edits: Vec<Edit>,
        cursor_before: usize,
    },
}

#[derive(Debug)]
pub struct Document {
    buffer: Buffer,
    path: Option<PathBuf>,
    encoding: String,
    had_bom: bool,
    newline: LineEnding,
    dirty: bool,
    undo_stack: Vec<EditGroup>,
    redo_stack: Vec<EditGroup>,
    open: OpenGroup,
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Document {
    pub fn new() -> Self {
        Self {
            buffer: Buffer::new(),
            path: None,
            encoding: "UTF-8".to_string(),
            had_bom: false,
            newline: LineEnding::platform_default(),
            dirty: false,
            undo_stack: vec![],
            redo_stack: vec![],
            open: OpenGroup::Idle,
        }
    }

    /// 从已有文本构建文档（不走撤销栈，测试与程序化构造用）。
    pub fn from_text(text: &str) -> Self {
        let mut d = Self::new();
        d.buffer = Buffer::from_text(text);
        d
    }

    /// 从字节构建文档（自动检测编码与换行）。
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut doc = Self::new();
        doc.load_bytes(bytes);
        doc
    }

    /// 用字节内容整体替换文档（打开/重新加载）。保留 path，清空撤销历史。
    pub fn load_bytes(&mut self, bytes: &[u8]) {
        let decoded = encoding::decode_auto(bytes);
        self.load_decoded(&decoded.text, &decoded.encoding, decoded.had_bom);
    }

    /// 用显式解码结果加载（用户"以指定编码重新打开"）。
    /// 与 [`Self::load_bytes`] 的区别：内容必须来自调用方按该编码的解码结果，
    /// 不能先自动检测再改标签——否则显示内容与保存编码不一致（曾因此出错）。
    pub fn load_decoded(&mut self, text: &str, encoding: &str, had_bom: bool) {
        self.buffer = Buffer::from_text(text);
        self.encoding = encoding.to_string();
        self.had_bom = had_bom;
        self.newline = newline::analyze(text)
            .dominant()
            .unwrap_or_else(LineEnding::platform_default);
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.open = OpenGroup::Idle;
        self.dirty = false;
    }

    // ---------- 只读访问 ----------

    pub fn text(&self) -> String {
        self.buffer.text()
    }

    pub fn len_chars(&self) -> usize {
        self.buffer.len_chars()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    pub fn path(&self) -> Option<&PathBuf> {
        self.path.as_ref()
    }

    pub fn set_path(&mut self, path: Option<PathBuf>) {
        self.path = path;
    }

    pub fn encoding_name(&self) -> &str {
        &self.encoding
    }

    pub fn had_bom(&self) -> bool {
        self.had_bom
    }

    pub fn set_had_bom(&mut self, v: bool) {
        self.had_bom = v;
    }

    pub fn newline(&self) -> LineEnding {
        self.newline
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn set_dirty(&mut self, v: bool) {
        self.dirty = v;
    }

    pub fn undo_depth(&self) -> usize {
        self.undo_stack.len()
    }

    pub fn redo_depth(&self) -> usize {
        self.redo_stack.len()
    }

    /// 换行符分布（O(n) 扫描，调用方可缓存）。
    pub fn newline_info(&self) -> NewlineInfo {
        newline::analyze(&self.buffer.text())
    }

    pub fn stats(&self) -> TextStats {
        crate::text::stats(&self.buffer.text())
    }

    pub fn line_col(&self, char_idx: usize) -> (usize, usize) {
        let line = self.buffer.char_to_line(char_idx);
        let line_start = self.buffer.line_to_char(line);
        (line + 1, char_idx - line_start + 1)
    }

    /// 行号（0 基）→ 该行首字符索引（越界自动夹紧）。跳转到行用。
    pub fn line_to_char(&self, line: usize) -> usize {
        self.buffer.line_to_char(line)
    }

    /// 行数（Ropey 语义：末尾换行后存在一个空行）。
    pub fn line_count(&self) -> usize {
        self.buffer.len_lines()
    }

    // ---------- 编辑（FR-3） ----------

    /// 插入文本。连续在同一位置的追加输入自动并入同一撤销组。
    pub fn insert_text(&mut self, pos: usize, s: &str) {
        if s.is_empty() {
            return;
        }
        let pos = pos.min(self.buffer.len_chars());
        let inserted_chars = s.chars().count();
        match &mut self.open {
            OpenGroup::Explicit { edits, .. } => {
                edits.push(Edit {
                    start: pos,
                    removed: String::new(),
                    inserted: s.to_string(),
                    cursor_before: pos,
                    cursor_after: pos + inserted_chars,
                });
            }
            OpenGroup::Typing { start, text, .. } if *start + text.chars().count() == pos => {
                text.push_str(s);
            }
            _ => {
                self.finalize();
                self.redo_stack.clear();
                self.open = OpenGroup::Typing {
                    start: pos,
                    text: s.to_string(),
                    cursor_before: pos,
                };
            }
        }
        self.buffer.insert(pos, s);
        self.dirty = true;
    }

    /// 删除 `[start, end)`。连续退格（end 落在上一组删除起点上）自动并入同一撤销组。
    pub fn delete_range(&mut self, start: usize, end: usize) {
        let end = end.min(self.buffer.len_chars());
        let start = start.min(end);
        if start == end {
            return;
        }
        let removed = self.buffer.slice(start, end);
        match &mut self.open {
            OpenGroup::Explicit { edits, .. } => {
                edits.push(Edit {
                    start,
                    removed: removed.clone(),
                    inserted: String::new(),
                    cursor_before: end,
                    cursor_after: start,
                });
            }
            OpenGroup::Backspacing {
                start: s0,
                removed: r,
                ..
            } if end == *s0 => {
                *s0 = start;
                r.insert_str(0, &removed);
            }
            _ => {
                self.finalize();
                self.redo_stack.clear();
                self.open = OpenGroup::Backspacing {
                    start,
                    removed,
                    cursor_before: end,
                };
            }
        }
        self.buffer.remove(start, end);
        self.dirty = true;
    }

    /// 显式撤销分组开始（多点编辑、批量操作等）。
    pub fn begin_group(&mut self) {
        self.finalize();
        self.redo_stack.clear();
        self.open = OpenGroup::Explicit {
            edits: vec![],
            cursor_before: 0,
        };
    }

    /// 显式撤销分组结束。
    pub fn end_group(&mut self) {
        self.finalize();
    }

    /// 整体替换文本（换行转换、外部内容替换等）。作为一条可撤销编辑。
    pub fn replace_all_text(&mut self, new_text: &str) {
        self.finalize();
        let old = self.buffer.text();
        if old == new_text {
            return;
        }
        let edit = Edit {
            start: 0,
            removed: old,
            inserted: new_text.to_string(),
            cursor_before: 0,
            cursor_after: new_text.chars().count(),
        };
        self.buffer = Buffer::from_text(new_text);
        self.push_group(EditGroup::One(edit));
        self.dirty = true;
    }

    // ---------- 撤销 / 重做 ----------

    /// 撤销一步。返回撤销后的光标位置（字符索引）。
    pub fn undo(&mut self) -> Option<usize> {
        self.finalize();
        let group = self.undo_stack.pop()?;
        let cursor = group.cursor_before().unwrap_or(0);
        for e in group.edits().iter().rev() {
            self.transform(e, false);
        }
        self.redo_stack.push(group);
        Some(cursor)
    }

    /// 重做一步。返回重做后的光标位置（字符索引）。
    pub fn redo(&mut self) -> Option<usize> {
        self.finalize();
        let group = self.redo_stack.pop()?;
        let cursor = group.cursor_after().unwrap_or(0);
        for e in group.edits() {
            self.transform(e, true);
        }
        self.undo_stack.push(group);
        Some(cursor)
    }

    /// 撤销一步，并返回本组编辑与撤销后光标位置（字符索引）。
    /// 编辑按**撤销应用顺序**（逆时间序）返回；每条 `Edit` 的 `start`
    /// 是「该条编辑作用时刻」的坐标——从撤销前的文本出发逆序逐条把
    /// `[start, start+插入数)` 替换为 `removed` 即可逐步回退。
    /// 增量 UI 据此把撤销同步给编辑器视图，无需整篇传输。
    pub fn undo_with_ops(&mut self) -> Option<(Vec<Edit>, usize)> {
        self.finalize();
        let group = self.undo_stack.pop()?;
        let cursor = group.cursor_before().unwrap_or(0);
        let ops: Vec<Edit> = group.edits().iter().rev().cloned().collect();
        for e in &ops {
            self.transform(e, false);
        }
        self.redo_stack.push(group);
        Some((ops, cursor))
    }

    /// 重做一步，并返回本组编辑（正时间序）与重做后光标位置（字符索引）。
    /// 从重做前的文本出发正序逐条把 `[start, start+删除数)` 替换为 `inserted`。
    pub fn redo_with_ops(&mut self) -> Option<(Vec<Edit>, usize)> {
        self.finalize();
        let group = self.redo_stack.pop()?;
        let cursor = group.cursor_after().unwrap_or(0);
        let ops: Vec<Edit> = group.edits().to_vec();
        for e in &ops {
            self.transform(e, true);
        }
        self.undo_stack.push(group);
        Some((ops, cursor))
    }

    /// 把编辑作用到缓冲。redo=true 顺向应用，false 逆向回退。
    fn transform(&mut self, e: &Edit, redo: bool) {
        let (take, put) = if redo {
            (&e.removed, &e.inserted)
        } else {
            (&e.inserted, &e.removed)
        };
        let take_len = take.chars().count();
        if take_len > 0 {
            self.buffer.remove(e.start, e.start + take_len);
        }
        if !put.is_empty() {
            self.buffer.insert(e.start, put);
        }
    }

    fn push_group(&mut self, g: EditGroup) {
        if !g.is_empty() {
            self.undo_stack.push(g);
            self.redo_stack.clear();
        }
    }

    /// 关闭当前打开的撤销组，落入撤销栈。
    fn finalize(&mut self) {
        let taken = std::mem::replace(&mut self.open, OpenGroup::Idle);
        match taken {
            OpenGroup::Typing {
                start,
                text,
                cursor_before,
            } => {
                if !text.is_empty() {
                    let after = start + text.chars().count();
                    self.push_group(EditGroup::One(Edit {
                        start,
                        removed: String::new(),
                        inserted: text,
                        cursor_before,
                        cursor_after: after,
                    }));
                }
            }
            OpenGroup::Backspacing {
                start,
                removed,
                cursor_before,
            } => {
                if !removed.is_empty() {
                    self.push_group(EditGroup::One(Edit {
                        start,
                        removed,
                        inserted: String::new(),
                        cursor_before,
                        cursor_after: start,
                    }));
                }
            }
            OpenGroup::Explicit {
                edits,
                cursor_before,
            } => {
                if !edits.is_empty() {
                    let _ = cursor_before;
                    self.push_group(EditGroup::Many(edits));
                }
            }
            OpenGroup::Idle => {}
        }
    }

    // ---------- 编码 / 换行 / 保存（FR-2） ----------

    /// 设置保存用编码（元数据变更，本身不改内容）。
    pub fn set_encoding(&mut self, name: &str) -> bool {
        if encoding_rs::Encoding::for_label(name.as_bytes()).is_some() {
            self.encoding = name.to_string();
            self.dirty = true;
            true
        } else {
            false
        }
    }

    /// 设置换行符：转换缓冲区内所有换行（可撤销），并记录保存风格。
    pub fn set_newline(&mut self, target: LineEnding) {
        self.newline = target;
        let converted = newline::convert_all(&self.buffer.text(), target);
        self.replace_all_text(&converted);
    }

    /// 编码为字节序列（保存用）。BOM 策略由调用方决定（FR-2.4）。
    pub fn to_bytes(&self, write_bom: bool) -> EncodeResult {
        encoding::encode_text(&self.buffer.text(), &self.encoding, write_bom)
    }

    /// 目标编码无法表示的字符（FR-2.3 确认对话框数据源）。
    pub fn unmappable_chars(&self) -> Vec<(usize, char)> {
        encoding::unmappable_chars(&self.buffer.text(), &self.encoding)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(s: &str) -> Document {
        Document::from_text(s)
    }

    #[test]
    fn typing_run_is_single_undo() {
        let mut d = doc("");
        d.insert_text(0, "h");
        d.insert_text(1, "e");
        d.insert_text(2, "l");
        d.insert_text(3, "l");
        d.insert_text(4, "o");
        assert_eq!(d.undo_depth(), 0); // 还在打开的 run 里
        assert_eq!(d.text(), "hello");
        assert_eq!(d.undo(), Some(0));
        assert_eq!(d.text(), "");
        assert_eq!(d.redo(), Some(5));
        assert_eq!(d.text(), "hello");
    }

    #[test]
    fn cursor_jump_breaks_typing_run() {
        let mut d = doc("");
        d.insert_text(0, "ab");
        d.insert_text(0, "x"); // 位置跳到别处 → 结束上一组
        assert_eq!(d.undo_depth(), 1);
        assert_eq!(d.text(), "xab");
        assert_eq!(d.undo(), Some(0));
        assert_eq!(d.text(), "ab");
    }

    #[test]
    fn backspace_run_is_single_undo() {
        let mut d = doc("hello");
        d.delete_range(4, 5); // 退格删 o
        d.delete_range(3, 4); // 退格删 l
        assert_eq!(d.text(), "hel");
        assert_eq!(d.undo_depth(), 0);
        assert_eq!(d.undo(), Some(5)); // 撤销到删除前光标
        assert_eq!(d.text(), "hello");
    }

    #[test]
    fn type_after_backspace_groups_separately() {
        let mut d = doc("");
        d.insert_text(0, "abc");
        d.delete_range(2, 3); // 删 c
        d.insert_text(2, "X");
        assert_eq!(d.text(), "abX");
        assert_eq!(d.undo(), Some(2)); // 撤销插入 X
        assert_eq!(d.text(), "ab");
        assert_eq!(d.undo(), Some(3)); // 撤销删除 c
        assert_eq!(d.text(), "abc");
        assert_eq!(d.redo(), Some(2));
        assert_eq!(d.text(), "ab");
    }

    #[test]
    fn explicit_group_undoes_together() {
        let mut d = doc("hello");
        d.begin_group();
        d.insert_text(5, " world");
        d.delete_range(0, 1); // 删 h
        d.end_group();
        assert_eq!(d.text(), "ello world");
        assert_eq!(d.undo_depth(), 1); // 整组只占一个撤销单元
        assert_eq!(d.undo(), Some(5)); // 撤销到组开始前的光标
        assert_eq!(d.text(), "hello");
        assert_eq!(d.redo(), Some(0));
        assert_eq!(d.text(), "ello world");
    }

    #[test]
    fn new_edit_clears_redo() {
        let mut d = doc("a");
        d.insert_text(1, "b");
        d.undo();
        d.insert_text(0, "c");
        assert_eq!(d.redo_depth(), 0);
        assert_eq!(d.text(), "ca");
    }

    #[test]
    fn undo_redo_with_multibyte() {
        let mut d = doc("");
        d.insert_text(0, "中文");
        d.insert_text(2, "abc"); // 相邻位置 → 合并进同一撤销组
        assert_eq!(d.text(), "中文abc");
        d.undo();
        assert_eq!(d.text(), "");
        assert_eq!(d.redo(), Some(5));
        assert_eq!(d.text(), "中文abc");

        // 跳回行首再输入 → 独立撤销组
        let mut d = doc("");
        d.insert_text(0, "中文");
        d.insert_text(0, "ab");
        assert_eq!(d.text(), "ab中文");
        d.undo();
        assert_eq!(d.text(), "中文");
        d.undo();
        assert_eq!(d.text(), "");
    }

    #[test]
    fn undo_ops_replay_reconstructs_text() {
        // 多条编辑的显式分组：逆序回放 ops 应从撤销后文本重建撤销前文本
        let mut d = doc("hello");
        d.begin_group();
        d.insert_text(5, " world");
        d.delete_range(0, 1); // 删 h
        d.end_group();
        let before = d.text();
        let (ops, _cursor) = d.undo_with_ops().unwrap();
        let mut replayed = before.clone();
        for e in &ops {
            let start_byte = byte_of(&replayed, e.start);
            let end_byte = byte_of(&replayed, e.start + e.chars_inserted());
            replayed.replace_range(start_byte..end_byte, &e.removed);
        }
        assert_eq!(replayed, "hello");
        assert_eq!(d.text(), "hello");

        // 重做同理：正序回放
        let before_redo = d.text();
        let (ops, _cursor) = d.redo_with_ops().unwrap();
        let mut replayed = before_redo;
        for e in &ops {
            let start_byte = byte_of(&replayed, e.start);
            let end_byte = byte_of(&replayed, e.start + e.chars_removed());
            replayed.replace_range(start_byte..end_byte, &e.inserted);
        }
        assert_eq!(replayed, "ello world");
        assert_eq!(d.text(), "ello world");
    }

    fn byte_of(s: &str, char_idx: usize) -> usize {
        s.char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(s.len())
    }

    #[test]
    fn line_col() {
        let d = doc("ab\n中文\ncd");
        assert_eq!(d.line_col(0), (1, 1));
        assert_eq!(d.line_col(2), (1, 3)); // 换行符位置算第 3 列
        assert_eq!(d.line_col(3), (2, 1));
        assert_eq!(d.line_col(4), (2, 2));
        assert_eq!(d.line_col(8), (3, 3)); // 文本末尾 = 第 3 行第 3 列
    }

    #[test]
    fn newline_conversion_is_undoable() {
        let mut d = doc("a\r\nb\nc");
        d.set_newline(LineEnding::Lf);
        assert_eq!(d.text(), "a\nb\nc");
        assert_eq!(d.newline(), LineEnding::Lf);
        d.undo();
        assert_eq!(d.text(), "a\r\nb\nc");
        // 内容不变时只改元数据，不产生编辑
        let mut d = doc("a\nb");
        d.set_newline(LineEnding::Lf);
        assert_eq!(d.undo_depth(), 0);
    }

    #[test]
    fn encode_decode_metadata() {
        let mut d = doc("中文");
        d.set_encoding("GBK");
        assert!(d.is_dirty());
        let out = d.to_bytes(false);
        assert_eq!(out.bytes, [0xD6, 0xD0, 0xCE, 0xC4]);
        let d2 = Document::from_bytes(&out.bytes);
        assert_eq!(d2.text(), "中文");
        assert_eq!(d2.encoding_name(), "GBK");
        // 不可表示字符（GBK 无法编码 emoji）
        d.insert_text(2, "🙂");
        assert_eq!(d.unmappable_chars().len(), 1);
    }

    #[test]
    fn load_bytes_resets_state() {
        let mut d = doc("old content");
        d.insert_text(11, "x");
        assert!(d.is_dirty());
        d.load_bytes("fresh".as_bytes());
        assert_eq!(d.text(), "fresh");
        assert!(!d.is_dirty());
        assert_eq!(d.undo_depth(), 0);
    }

    #[test]
    fn load_decoded_keeps_explicit_encoding() {
        // GBK 字节 + 用户显式选择 windows-1252：内容必须按 1252 解码，
        // 不能自动检测成 GBK 再贴 1252 标签
        let gbk_bytes = encoding::encode_text("中文", "GBK", false).bytes;
        let d1252 = encoding::decode_with(&gbk_bytes, "windows-1252");
        let mut d = Document::new();
        d.load_decoded(&d1252.text, &d1252.encoding, d1252.had_bom);
        assert_eq!(d.encoding_name(), "windows-1252");
        assert_eq!(d.text(), d1252.text);
        assert_ne!(d.text(), "中文"); // 不能仍按 GBK 的结果显示
        assert!(!d.is_dirty());
        assert_eq!(d.undo_depth(), 0);
        // 按显式编码写回应得到原始字节（往返一致）
        let out = d.to_bytes(false);
        assert_eq!(out.bytes, gbk_bytes);
    }

    #[test]
    fn stats_and_info() {
        let d = doc("hello world\n中文测试");
        let st = d.stats();
        assert_eq!(st.lines, 2);
        assert_eq!(st.words, 2 + 4);
        let info = d.newline_info();
        assert_eq!(info.lf, 1);
        assert!(!info.mixed());
    }
}
