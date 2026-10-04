//! 基于 Ropey 的文本缓冲区封装。
//!
//! 所有对外索引均为**字符索引**（`char` 计数，非字节），
//! 与 ropey 的单位一致，避免字节/字符换算错误。

use unicode_segmentation::UnicodeSegmentation;

/// grapheme 边界计算的取样窗口（字符数）。
/// 窗口边界可能切开多字符 grapheme（极端情况），对 MVP 可接受。
const GRAPHEME_WINDOW: usize = 64;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Buffer {
    rope: ropey::Rope,
}

impl Buffer {
    pub fn new() -> Self {
        Self { rope: ropey::Rope::new() }
    }

    pub fn from_text(text: &str) -> Self {
        Self { rope: ropey::Rope::from_str(text) }
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn is_empty(&self) -> bool {
        self.rope.len_chars() == 0
    }

    /// 完整文本拷贝（O(n)）。
    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    /// 区间文本拷贝 `[start, end)`（字符索引）。
    pub fn slice(&self, start: usize, end: usize) -> String {
        let end = end.min(self.rope.len_chars());
        let start = start.min(end);
        self.rope.slice(start..end).to_string()
    }

    pub fn char_at(&self, idx: usize) -> Option<char> {
        if idx < self.rope.len_chars() {
            Some(self.rope.char(idx))
        } else {
            None
        }
    }

    pub fn insert(&mut self, idx: usize, text: &str) {
        self.rope.insert(idx.min(self.rope.len_chars()), text);
    }

    /// 删除 `[start, end)` 并返回被删除文本。
    pub fn remove(&mut self, start: usize, end: usize) -> String {
        let end = end.min(self.rope.len_chars());
        let start = start.min(end);
        let removed = self.rope.slice(start..end).to_string();
        self.rope.remove(start..end);
        removed
    }

    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    /// 第 `idx` 行内容（不含行终止符）。
    pub fn line(&self, idx: usize) -> String {
        if idx >= self.rope.len_lines() {
            return String::new();
        }
        let s = self.rope.line(idx).to_string();
        // 行终止符只可能是 \n / \r\n / \r（Ropey 语义），内容本身不会以 \r 结尾
        s.trim_end_matches(['\r', '\n']).to_string()
    }

    pub fn char_to_line(&self, idx: usize) -> usize {
        self.rope.char_to_line(idx.min(self.rope.len_chars()))
    }

    pub fn line_to_char(&self, idx: usize) -> usize {
        self.rope.line_to_char(idx.min(self.rope.len_lines()))
    }

    pub fn char_to_byte(&self, idx: usize) -> usize {
        self.rope.char_to_byte(idx.min(self.rope.len_chars()))
    }

    /// `idx` 前一个 grapheme 簇的起始字符索引（用于光标左移不拆字符）。
    pub fn prev_grapheme_start(&self, idx: usize) -> Option<usize> {
        if idx == 0 || idx > self.len_chars() {
            return None;
        }
        let start = idx.saturating_sub(GRAPHEME_WINDOW);
        let window = self.slice(start, idx);
        let last = window.graphemes(true).next_back()?;
        Some(idx - last.chars().count())
    }

    /// `idx` 后一个 grapheme 簇的结束字符索引（光标右移）。
    pub fn next_grapheme_end(&self, idx: usize) -> Option<usize> {
        let len = self.len_chars();
        if idx >= len {
            return None;
        }
        let end = (idx + GRAPHEME_WINDOW).min(len);
        let window = self.slice(idx, end);
        let first = window.graphemes(true).next()?;
        Some(idx + first.chars().count())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_remove_roundtrip() {
        let mut b = Buffer::from_text("hello");
        b.insert(5, " world");
        assert_eq!(b.text(), "hello world");
        let removed = b.remove(0, 6);
        assert_eq!(removed, "hello ");
        assert_eq!(b.text(), "world");
    }

    #[test]
    fn char_vs_byte_indexing() {
        // "中文" 每个字 3 字节；字符索引 1 应命中 "文"
        let b = Buffer::from_text("中文abc");
        assert_eq!(b.len_chars(), 5);
        assert_eq!(b.char_at(1), Some('文'));
        assert_eq!(b.char_to_byte(1), 3);
        assert_eq!(b.slice(1, 3), "文a");
    }

    #[test]
    fn line_mapping() {
        let b = Buffer::from_text("a\nbb\r\nccc\r");
        assert_eq!(b.len_lines(), 4); // "a\n" "bb\r\n" "ccc\r" + 末尾空行
        assert_eq!(b.line(0), "a");
        assert_eq!(b.line(1), "bb");
        assert_eq!(b.line(2), "ccc");
        assert_eq!(b.char_to_line(4), 1); // \r\n 属于它所终止的行（Ropey 语义）
        assert_eq!(b.char_to_line(6), 2); // 第一个 c
        assert_eq!(b.line_to_char(1), 2);
    }

    #[test]
    fn grapheme_boundaries() {
        // 👨‍👩‍👧 家庭 emoji 为单个 grapheme（多码点）
        let b = Buffer::from_text("a👨‍👩‍👧b");
        assert_eq!(b.prev_grapheme_start(b.len_chars()), Some(1 + 5));
        assert_eq!(b.next_grapheme_end(1), Some(1 + 5));
        // 简单 ASCII
        assert_eq!(b.prev_grapheme_start(1), Some(0));
        assert_eq!(b.next_grapheme_end(0), Some(1));
        assert_eq!(b.next_grapheme_end(b.len_chars()), None);
        assert_eq!(b.prev_grapheme_start(0), None);
    }

    #[test]
    fn out_of_range_is_clamped() {
        let mut b = Buffer::new();
        b.insert(100, "hi"); // 越界插入被夹紧
        assert_eq!(b.text(), "hi");
        assert_eq!(b.line(99), "");
    }
}
