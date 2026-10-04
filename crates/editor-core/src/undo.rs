//! 撤销/重做数据结构。
//!
//! 关键设计：撤销组内每条原子编辑按**逆序取逆**回退——
//! 因为第 i 条编辑的逆恰好作用于"第 i 条之后的所有编辑都已回退"的状态，
//! 因此无需记录绝对快照即可正确撤销任意分组。

/// 一次原子文本变更：把 `[start, start + removed字符数)` 处的文本替换为 `inserted`。
/// `start` 为字符索引；`cursor_*` 为编辑前后的光标位置（字符索引）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub start: usize,
    pub removed: String,
    pub inserted: String,
    pub cursor_before: usize,
    pub cursor_after: usize,
}

impl Edit {
    pub fn chars_removed(&self) -> usize {
        self.removed.chars().count()
    }

    pub fn chars_inserted(&self) -> usize {
        self.inserted.chars().count()
    }
}

/// 一个可撤销单元：单条编辑，或显式分组（begin_group/end_group）的多条编辑。
#[derive(Debug, Clone)]
pub enum EditGroup {
    One(Edit),
    Many(Vec<Edit>),
}

impl EditGroup {
    pub fn edits(&self) -> &[Edit] {
        match self {
            EditGroup::One(e) => std::slice::from_ref(e),
            EditGroup::Many(es) => es,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.edits().is_empty()
    }

    pub fn cursor_before(&self) -> Option<usize> {
        self.edits().first().map(|e| e.cursor_before)
    }

    pub fn cursor_after(&self) -> Option<usize> {
        self.edits().last().map(|e| e.cursor_after)
    }
}
