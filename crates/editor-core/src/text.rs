//! 文本变换（FR-3.4/3.5/3.7/3.8 与 FR-8.1 统计）。

use unicode_normalization::UnicodeNormalization;

// ---------- 大小写 ----------

pub fn to_upper(s: &str) -> String {
    s.to_uppercase()
}

pub fn to_lower(s: &str) -> String {
    s.to_lowercase()
}

/// 每个词首字母大写（其余小写）。
pub fn to_title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut at_word_start = true;
    for c in s.chars() {
        if c.is_alphanumeric() {
            if at_word_start {
                out.extend(c.to_uppercase());
            } else {
                out.extend(c.to_lowercase());
            }
            at_word_start = false;
        } else {
            out.push(c);
            at_word_start = true;
        }
    }
    out
}

/// 每句首字母大写（以 . ! ? 换行界定句）。
pub fn to_sentence_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut at_sentence_start = true;
    for c in s.chars() {
        if at_sentence_start && c.is_alphabetic() {
            out.extend(c.to_uppercase());
            at_sentence_start = false;
        } else {
            out.extend(c.to_lowercase());
            if matches!(c, '.' | '!' | '?' | '\n') {
                at_sentence_start = true;
            }
        }
    }
    out
}

// ---------- 全角 / 半角（FR-3.4） ----------

/// 全角 → 半角：U+3000 → 空格，FF01–FF5E → ASCII 21–7E。
/// 注：片假名全角→半角暂不支持（见 README 已知限制）。
pub fn full_to_half(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{3000}' => ' ',
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            _ => c,
        })
        .collect()
}

/// 半角 → 全角：ASCII 21–7E → FF01–FF5E（空格保持不变），半角片假名 → 全角。
/// 半角片假名按连续段做 NFKC，这样浊音组合（ｶ + ﾞ）能合成单个全角浊音（ガ）。
pub fn half_to_full(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    let mut kana_run = String::new();
    for c in s.chars() {
        if ('\u{FF61}'..='\u{FF9F}').contains(&c) {
            kana_run.push(c);
            continue;
        }
        if !kana_run.is_empty() {
            out.push_str(&kana_run.nfkc().collect::<String>());
            kana_run.clear();
        }
        match c {
            '\u{21}'..='\u{7E}' => out.push(char::from_u32(c as u32 + 0xFEE0).unwrap_or(c)),
            _ => out.push(c),
        }
    }
    if !kana_run.is_empty() {
        out.push_str(&kana_run.nfkc().collect::<String>());
    }
    out
}

// ---------- Unicode 规范化（FR-3.5） ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormalForm {
    Nfc,
    Nfd,
    Nfkc,
    Nfkd,
}

pub fn normalize(s: &str, form: NormalForm) -> String {
    match form {
        NormalForm::Nfc => s.nfc().collect(),
        NormalForm::Nfd => s.nfd().collect(),
        NormalForm::Nfkc => s.nfkc().collect(),
        NormalForm::Nfkd => s.nfkd().collect(),
    }
}

// ---------- Tab / 空格 ----------

/// Tab → 空格：按列对齐到 tab stop（列号从行首计）。
pub fn tabs_to_spaces(s: &str, tab_width: usize) -> String {
    let tab_width = tab_width.max(1);
    let mut out = String::with_capacity(s.len() + 16);
    let mut col = 0usize;
    for c in s.chars() {
        match c {
            '\n' => {
                out.push('\n');
                col = 0;
            }
            '\t' => {
                let n = tab_width - (col % tab_width);
                for _ in 0..n {
                    out.push(' ');
                }
                col += n;
            }
            other => {
                out.push(other);
                col += 1;
            }
        }
    }
    out
}

/// 行首空格 → Tab：行首前导空格按 tab stop 合并（不触碰行内缩进之后的空格）。
pub fn leading_spaces_to_tabs(s: &str, tab_width: usize) -> String {
    let tab_width = tab_width.max(1);
    let mut out = String::with_capacity(s.len());
    for line in split_lines_keep(s) {
        let content_end = line.trim_end_matches(['\r', '\n']).len();
        let (content, terminator) = line.split_at(content_end);
        let spaces = content.len() - content.trim_start_matches(' ').len();
        let indent = &content[..spaces];
        let rest = &content[spaces..];
        out.push_str(&collapse_indent(indent, tab_width));
        out.push_str(rest);
        out.push_str(terminator);
    }
    out
}

fn collapse_indent(indent: &str, tab_width: usize) -> String {
    let mut out = String::new();
    let mut run = 0usize;
    for c in indent.chars() {
        if c == ' ' {
            run += 1;
            if run == tab_width {
                out.push('\t');
                run = 0;
            }
        } else {
            // 遇到已有 Tab：凑满的先落盘
            if run > 0 {
                for _ in 0..run {
                    out.push(' ');
                }
                run = 0;
            }
            out.push('\t');
        }
    }
    for _ in 0..run {
        out.push(' ');
    }
    out
}

// ---------- 行级操作（FR-3.7） ----------

/// 按行切分并保留每行终止符；空文本返回空 Vec。
pub fn split_lines_keep(s: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut start = 0usize;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                out.push(&s[start..=i]);
                start = i + 1;
            }
            b'\r' if i + 1 >= bytes.len() || bytes[i + 1] != b'\n' => {
                out.push(&s[start..=i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

fn line_content(line: &str) -> &str {
    line.trim_end_matches(['\r', '\n'])
}

/// 重组行列表：无终止符的行若不在末尾，补一个 \n 防止相邻行粘连。
fn rejoin(lines: &[&str]) -> String {
    let mut out = String::new();
    let n = lines.len();
    for (i, l) in lines.iter().enumerate() {
        out.push_str(l);
        if i + 1 < n && !(l.ends_with('\n') || l.ends_with('\r')) {
            out.push('\n');
        }
    }
    out
}

pub fn sort_lines(s: &str, ascending: bool, numeric: bool, case_insensitive: bool) -> String {
    let mut lines = split_lines_keep(s);
    let key = |l: &&str| -> (bool, f64, String) {
        let c = line_content(l).trim();
        let folded = if case_insensitive {
            c.to_lowercase()
        } else {
            c.to_string()
        };
        if numeric {
            // 直接 parse（Rust 接受 "+3"/"-3"）；不能剥符号再解析，否则负数变正数
            let num = c.parse::<f64>().ok();
            match num {
                Some(n) => (false, n, folded),
                None => (true, 0.0, folded),
            }
        } else {
            (false, 0.0, folded)
        }
    };
    let cmp = |a: &&str, b: &&str| {
        key(a)
            .partial_cmp(&key(b))
            .unwrap_or(std::cmp::Ordering::Equal)
    };
    lines.sort_by(cmp);
    if !ascending {
        lines.reverse();
    }
    rejoin(&lines)
}

pub fn unique_lines(s: &str) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut out = String::with_capacity(s.len());
    for line in split_lines_keep(s) {
        if seen.insert(line_content(line).to_string()) {
            out.push_str(line);
        }
    }
    out
}

pub fn reverse_lines(s: &str) -> String {
    let mut lines = split_lines_keep(s);
    lines.reverse();
    rejoin(&lines)
}

/// 去除每行行尾空白（空格与 Tab）。
pub fn trim_trailing_whitespace(s: &str) -> String {
    split_lines_keep(s)
        .into_iter()
        .map(|line| {
            let content_end = line.trim_end_matches(['\r', '\n']).len();
            let (content, terminator) = line.split_at(content_end);
            format!("{}{}", content.trim_end_matches([' ', '\t']), terminator)
        })
        .collect()
}

fn line_char_count(line: &str) -> usize {
    line.chars().count()
}

pub fn delete_line(s: &str, line_idx: usize) -> String {
    let lines = split_lines_keep(s);
    if line_idx >= lines.len() {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    for (i, l) in lines.iter().enumerate() {
        if i != line_idx {
            out.push_str(l);
        }
    }
    // 删除的是最后一行且它没有终止符时，前一行残留的终止符会成为行尾
    if out.ends_with('\n') || out.ends_with('\r') {
        // 保持原样
    }
    out
}

pub fn duplicate_line(s: &str, line_idx: usize) -> String {
    let lines = split_lines_keep(s);
    if line_idx >= lines.len() {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + line_char_count(lines[line_idx]));
    for (i, l) in lines.iter().enumerate() {
        out.push_str(l);
        if i == line_idx {
            // 复制行内容 + 终止符（无终止符则补一个，保证成行）
            if l.ends_with('\n') || l.ends_with('\r') {
                out.push_str(l);
            } else {
                out.push_str(l);
                out.push('\n');
            }
        }
    }
    out
}

/// 上移/下移一行（delta: -1 上移，+1 下移）。
pub fn move_line(s: &str, line_idx: usize, delta: i32) -> String {
    let mut lines = split_lines_keep(s);
    if line_idx >= lines.len() {
        return s.to_string();
    }
    let target = line_idx as i64 + delta as i64;
    if target < 0 || target as usize >= lines.len() {
        return s.to_string();
    }
    let target = target as usize;
    lines.swap(line_idx, target);
    rejoin(&lines)
}

// ---------- 行注释切换（FR-3.8） ----------

/// 对每行切换行注释前缀：已注释则去掉，未注释则加上（加在最左侧，前缀后跟一个空格）。
pub fn toggle_line_comment(s: &str, prefix: &str) -> String {
    split_lines_keep(s)
        .into_iter()
        .map(|line| {
            let content_end = line.trim_end_matches(['\r', '\n']).len();
            let (content, terminator) = line.split_at(content_end);
            let trimmed = content.trim_start();
            if let Some(after) = trimmed.strip_prefix(prefix) {
                // 去掉注释前缀及其后跟的一个空格
                let rest = after.strip_prefix(' ').unwrap_or(after);
                let indent = &content[..content.len() - trimmed.len()];
                format!("{indent}{rest}{terminator}")
            } else if !content.trim().is_empty() {
                format!("{prefix} {content}{terminator}")
            } else {
                line.to_string()
            }
        })
        .collect()
}

// ---------- 单词补全（FR-6.1） ----------

pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// 文档内单词补全候选（FR-6.1）：收集全文的"词"（字母数字/下划线连续段，中日韩
/// 单字也视为词），取以 prefix 开头且不等于 prefix 的去重候选。
/// 排序：短优先，同长按字典序；最多返回 limit 个。
pub fn word_completions(text: &str, prefix: &str, limit: usize) -> Vec<String> {
    if prefix.is_empty() {
        return vec![];
    }
    let mut words = std::collections::HashSet::new();
    let mut cur = String::new();
    for c in text.chars() {
        if is_word_char(c) {
            cur.push(c);
        } else if !cur.is_empty() {
            words.insert(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        words.insert(cur);
    }
    let mut v: Vec<String> = words
        .into_iter()
        .filter(|w| w.starts_with(prefix) && w != prefix)
        .collect();
    v.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    v.truncate(limit);
    v
}

// ---------- 统计（FR-8.1） ----------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextStats {
    pub chars: usize,
    pub bytes: usize,
    pub lines: usize,
    pub words: usize,
}

fn is_cjk_word_char(c: char) -> bool {
    // CJK 统计策略：中日韩文字按"字"计（对齐 CotEditor）
    matches!(c,
        '\u{4E00}'..='\u{9FFF}'    // CJK 统一表意文字
        | '\u{3400}'..='\u{4DBF}'  // 扩展 A
        | '\u{3040}'..='\u{30FF}'  // 平假名 + 片假名
        | '\u{AC00}'..='\u{D7AF}'  // 谚文
        | '\u{F900}'..='\u{FAFF}'  // 兼容表意文字
    )
}

/// 统计：字符数（码点）、字节数、行数、词数。
/// 词数 = 连续字母数字串 + 每个中日韩字符单独计 1。
pub fn stats(s: &str) -> TextStats {
    let mut st = TextStats {
        bytes: s.len(),
        chars: s.chars().count(),
        ..Default::default()
    };
    if !s.is_empty() {
        st.lines = 1 + newline_count(s);
    }
    let mut in_word = false;
    for c in s.chars() {
        if is_cjk_word_char(c) {
            st.words += 1;
            in_word = false;
        } else if c.is_alphanumeric() || c == '_' {
            if !in_word {
                st.words += 1;
                in_word = true;
            }
        } else {
            in_word = false;
        }
    }
    st
}

fn newline_count(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut n = 0;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'\n' => n += 1,
            b'\r' if i + 1 < bytes.len() && bytes[i + 1] != b'\n' => n += 1,
            _ => {}
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_transforms() {
        assert_eq!(to_upper("abc déf"), "ABC DÉF");
        assert_eq!(to_title_case("hello world foo"), "Hello World Foo");
        assert_eq!(
            to_sentence_case("hello. world! again? yes"),
            "Hello. World! Again? Yes"
        );
    }

    #[test]
    fn width_conversions() {
        assert_eq!(full_to_half("ＡＢＣ１２３，：！　ｘ"), "ABC123,:! x");
        assert_eq!(half_to_full("ABC123,:!"), "ＡＢＣ１２３，：！");
        // 半角片假名 → 全角
        assert_eq!(half_to_full("ｱｲｳ"), "アイウ");
        assert_eq!(half_to_full("ｶﾞ"), "ガ"); // 半角浊音组合 → 单个全角浊音
                                             // 往返（ASCII 部分）
        assert_eq!(full_to_half(&half_to_full("abc")), "abc");
    }

    #[test]
    fn normalization_forms() {
        // é 的分解形式 (NFD) → 组合形式 (NFC)
        let decomposed = "e\u{301}";
        assert_eq!(normalize(decomposed, NormalForm::Nfc), "é");
        assert_eq!(normalize("é", NormalForm::Nfd), decomposed);
        // ㍿ → 株式会社（NFKC 兼容分解）
        assert_eq!(normalize("㍿", NormalForm::Nfkc), "株式会社");
    }

    #[test]
    fn tab_space_conversion() {
        assert_eq!(tabs_to_spaces("a\tb", 4), "a   b");
        assert_eq!(tabs_to_spaces("ab\tb", 4), "ab  b");
        assert_eq!(tabs_to_spaces("\t\tb", 4), "        b");
        assert_eq!(leading_spaces_to_tabs("    x\n  y", 4), "\tx\n  y");
        assert_eq!(leading_spaces_to_tabs("        z", 4), "\t\tz");
    }

    #[test]
    fn line_splitting_keeps_terminators() {
        let lines = split_lines_keep("a\nb\r\nc\r");
        assert_eq!(lines, vec!["a\n", "b\r\n", "c\r"]);
        assert_eq!(split_lines_keep(""), Vec::<&str>::new());
        assert_eq!(split_lines_keep("abc"), vec!["abc"]);
    }

    #[test]
    fn line_operations() {
        let s = "3\n1\n2\n10\nb\na";
        // 末行无终止符，排序移动后自动补 \n 防粘连
        assert_eq!(sort_lines(s, true, false, false), "1\n10\n2\n3\na\nb\n");
        assert_eq!(sort_lines(s, true, true, false), "1\n2\n3\n10\na\nb\n");
        // 负数按真实值排序（不能把 -2 当 2）；未终止行移到中间会补换行
        assert_eq!(sort_lines("1\n-2\n3", true, true, false), "-2\n1\n3");
        assert_eq!(sort_lines("+3\n-1\n2", true, true, false), "-1\n2\n+3\n");
        assert_eq!(sort_lines("B\na\nC", true, false, true), "a\nB\nC");
        assert_eq!(sort_lines(s, false, false, false), "b\na\n3\n2\n10\n1\n");
        // 去重保持原顺序与各行终止符
        assert_eq!(unique_lines("x\ny\nx\nz\ny"), "x\ny\nz\n");
        assert_eq!(reverse_lines("1\n2\n3"), "3\n2\n1\n");
        // 只去行尾空白，保留行首缩进
        assert_eq!(trim_trailing_whitespace("a  \n\tb\t\n c "), "a\n\tb\n c");
        assert_eq!(delete_line("1\n2\n3", 1), "1\n3");
        assert_eq!(duplicate_line("1\n2", 0), "1\n1\n2");
        assert_eq!(move_line("1\n2\n3", 0, 1), "2\n1\n3");
        assert_eq!(move_line("1\n2\n3", 0, -1), "1\n2\n3");
        // 中文行同样工作
        assert_eq!(delete_line("一\n二", 0), "二");
    }

    #[test]
    fn line_comment_toggle() {
        let s = "fn a() {}\n\nfn b() {}";
        assert_eq!(toggle_line_comment(s, "//"), "// fn a() {}\n\n// fn b() {}");
        let commented = toggle_line_comment(s, "//");
        assert_eq!(toggle_line_comment(&commented, "//"), s);
    }

    #[test]
    fn word_completions_basic() {
        let text = "hello world helvetica 中文 中文站 hello";
        assert_eq!(
            word_completions(text, "hel", 10),
            vec!["hello".to_string(), "helvetica".to_string()]
        );
        // 前缀本身被排除；中日韩单字为候选
        assert_eq!(
            word_completions(text, "中文", 10),
            vec!["中文站".to_string()]
        );
        assert!(word_completions(text, "", 10).is_empty());
        assert!(word_completions(text, "zzz", 10).is_empty());
        // 限制数量：短优先
        assert_eq!(word_completions(text, "hel", 1), vec!["hello".to_string()]);
    }

    #[test]
    fn stats_counts() {
        let st = stats("hello world 中文\nsecond line");
        assert_eq!(st.lines, 2);
        assert_eq!(st.words, 2 + 2 + 2); // hello world + 中/文 + second line
        assert_eq!(st.chars, 26);
        assert_eq!(st.bytes, "hello world 中文\nsecond line".len());
        assert_eq!(stats("").lines, 0);
        let st = stats("a\r\nb\rc\rd");
        assert_eq!(st.lines, 4);
    }
}
