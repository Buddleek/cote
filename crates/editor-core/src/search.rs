//! 查找 / 替换（FR-5）。
//!
//! 纯文本模式基于正则转义实现（复用 Unicode 大小写折叠），
//! 整词匹配通过词边界后验过滤实现，避免 lookbehind 依赖。
//! 正则模式使用 fancy-regex（支持前后顾与反向引用）。
//!
//! 所有 `Match` 偏移均为**字符索引**。

use fancy_regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SearchOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    /// 起始字符索引（含）
    pub start: usize,
    /// 结束字符索引（不含）
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchError {
    EmptyQuery,
    InvalidRegex(String),
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SearchError::EmptyQuery => write!(f, "查询为空"),
            SearchError::InvalidRegex(e) => write!(f, "正则表达式无效: {e}"),
        }
    }
}

impl std::error::Error for SearchError {}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn build_regex(query: &str, opts: &SearchOptions) -> Result<Regex, SearchError> {
    if query.is_empty() {
        return Err(SearchError::EmptyQuery);
    }
    let pattern = if opts.regex {
        query.to_string()
    } else {
        fancy_regex::escape(query).into_owned()
    };
    let pattern = if opts.case_sensitive { pattern } else { format!("(?i){pattern}") };
    Regex::new(&pattern).map_err(|e| SearchError::InvalidRegex(e.to_string()))
}

/// 整词边界校验：匹配首/尾是词字符时，其外侧必须不是词字符。
fn boundary_ok(text: &str, byte_start: usize, byte_end: usize) -> bool {
    let first = text[byte_start..].chars().next().map(is_word_char).unwrap_or(false);
    let last = text[..byte_end].chars().next_back().map(is_word_char).unwrap_or(false);
    let before = text[..byte_start].chars().next_back().map(is_word_char).unwrap_or(false);
    let after = text[byte_end..].chars().next().map(is_word_char).unwrap_or(false);
    (!first || !before) && (!last || !after)
}

/// 字节偏移 → 字符偏移表（单次遍历 + 二分查找，避免每匹配 O(n)）。
struct ByteCharTable {
    entries: Vec<(usize, usize)>, // (byte_idx, char_idx)，按 byte 升序
}

impl ByteCharTable {
    fn new(text: &str) -> Self {
        Self {
            entries: text
                .char_indices()
                .enumerate()
                .map(|(ci, (bi, _))| (bi, ci))
                .collect(),
        }
    }

    fn lookup(&self, byte: usize) -> usize {
        let i = self.entries.partition_point(|&(b, _)| b <= byte);
        if i == 0 {
            0
        } else {
            self.entries[i - 1].1
        }
    }
}

/// 全部匹配（字符索引）。
pub fn find_all(text: &str, query: &str, opts: &SearchOptions) -> Result<Vec<Match>, SearchError> {
    Ok(find_matches(text, query, opts)?.into_iter().map(|m| Match { start: m.char_start, end: m.char_end }).collect())
}

struct ByteMatch {
    byte_start: usize,
    byte_end: usize,
    char_start: usize,
    char_end: usize,
}

fn find_matches(text: &str, query: &str, opts: &SearchOptions) -> Result<Vec<ByteMatch>, SearchError> {
    let re = build_regex(query, opts)?;
    let table = ByteCharTable::new(text);
    let mut out = vec![];
    for m in re.find_iter(text) {
        let m = m.map_err(|e| SearchError::InvalidRegex(e.to_string()))?;
        // 跳过空匹配：编辑器场景下无意义，且会使下方 end-1 下标回退崩溃
        if m.end() == m.start() {
            continue;
        }
        if opts.whole_word && !boundary_ok(text, m.start(), m.end()) {
            continue;
        }
        out.push(ByteMatch {
            byte_start: m.start(),
            byte_end: m.end(),
            char_start: table.lookup(m.start()),
            char_end: table.lookup(m.end() - 1) + 1,
        });
    }
    Ok(out)
}

/// 从 `from`（字符索引）开始查找第一个匹配；找不到时按 `wrap` 决定是否回绕到文首。
pub fn find_next(text: &str, from: usize, query: &str, opts: &SearchOptions, wrap: bool) -> Result<Option<Match>, SearchError> {
    let all = find_all(text, query, opts)?;
    Ok(match all.iter().find(|m| m.start >= from) {
        Some(m) => Some(*m),
        None if wrap => all.first().copied(),
        None => None,
    })
}

/// 全部替换。返回（新文本, 替换次数）。
///
/// - 纯文本模式：replacement 为字面量；
/// - 正则模式（且非整词过滤）：replacement 支持 `$1` 捕获组展开；
/// - 正则 + 整词：逐匹配字面替换（不支持捕获组）。
pub fn replace_all(
    text: &str,
    query: &str,
    replacement: &str,
    opts: &SearchOptions,
) -> Result<(String, usize), SearchError> {
    if opts.regex && !opts.whole_word {
        let re = build_regex(query, opts)?;
        // 手工逐匹配替换：跳过空匹配（replace_all 会在每个位置插入替换串），
        // 同时经 Captures::expand 支持 $1 捕获组展开
        let mut out = String::with_capacity(text.len());
        let mut last = 0usize;
        let mut pos = 0usize;
        let mut count = 0usize;
        while pos <= text.len() {
            let caps = match re.captures_from_pos(text, pos) {
                Ok(Some(c)) => c,
                _ => break,
            };
            let m = caps.get(0).expect("capture group 0 always exists");
            if m.as_str().is_empty() {
                // 空匹配：前进一个字符避免死循环
                match text[m.start()..].chars().next() {
                    Some(c) => pos = m.start() + c.len_utf8(),
                    None => break,
                }
                continue;
            }
            out.push_str(&text[last..m.start()]);
            let mut expanded = String::with_capacity(replacement.len());
            caps.expand(replacement, &mut expanded);
            out.push_str(&expanded);
            last = m.end();
            pos = m.end();
            count += 1;
        }
        out.push_str(&text[last..]);
        return Ok((out, count));
    }

    let matches = find_matches(text, query, opts)?;
    let mut out = String::with_capacity(text.len());
    let mut last = 0usize;
    for m in &matches {
        out.push_str(&text[last..m.byte_start]);
        out.push_str(replacement);
        last = m.byte_end;
    }
    out.push_str(&text[last..]);
    Ok((out, matches.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "the cat sat on the category mat. Cat!";
    const CJK: &str = "中文ABC中文abc";

    fn find(text: &str, q: &str, opts: SearchOptions) -> Vec<Match> {
        find_all(text, q, &opts).unwrap()
    }

    #[test]
    fn plain_case_insensitive() {
        let m = find(TEXT, "cat", SearchOptions::default());
        assert_eq!(m.len(), 3); // cat、category 中的 cat、Cat
        assert_eq!(TEXT[m[0].start..m[0].end], *"cat");
    }

    #[test]
    fn plain_case_sensitive() {
        let m = find(TEXT, "Cat", SearchOptions { case_sensitive: true, ..Default::default() });
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].start, 33);
    }

    #[test]
    fn whole_word() {
        let m = find(TEXT, "cat", SearchOptions { whole_word: true, ..Default::default() });
        // 只命中独立的 "cat" 和 "Cat"，不命中 category
        assert_eq!(m.len(), 2);
        assert_eq!(m[1].start, 33);
    }

    #[test]
    fn cjk_char_offsets() {
        let m = find(CJK, "ABC", SearchOptions { case_sensitive: true, ..Default::default() });
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].start, 2);
        assert_eq!(m[0].end, 5);
        let m = find(CJK, "中文", SearchOptions { case_sensitive: true, ..Default::default() });
        assert_eq!(m.len(), 2);
        assert_eq!(m[1].start, 5);
    }

    #[test]
    fn regex_mode() {
        let opts = SearchOptions { regex: true, ..Default::default() };
        let m = find(TEXT, r"\bc\w+", opts);
        assert_eq!(m.len(), 3); // cat, category, Cat
        let m = find(TEXT, r"(?i)\bcat\b", opts);
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn regex_invalid() {
        let err = find_all(TEXT, "a(", &SearchOptions { regex: true, ..Default::default() });
        assert!(matches!(err, Err(SearchError::InvalidRegex(_))));
    }

    #[test]
    fn empty_query_error() {
        assert_eq!(find_all(TEXT, "", &SearchOptions::default()), Err(SearchError::EmptyQuery));
    }

    #[test]
    fn replace_plain_literal() {
        let (out, n) = replace_all(TEXT, "cat", "dog", &SearchOptions { whole_word: true, ..Default::default() }).unwrap();
        assert_eq!(n, 2);
        assert!(out.contains("the dog sat"));
        assert!(out.contains("dog!"));
        assert!(out.contains("category"));
    }

    #[test]
    fn replace_literal_keeps_dollar() {
        let (out, n) = replace_all("a b a", "a", "$1", &SearchOptions::default()).unwrap();
        assert_eq!(n, 2);
        assert_eq!(out, "$1 b $1");
    }

    #[test]
    fn replace_regex_capture_groups() {
        let opts = SearchOptions { regex: true, ..Default::default() };
        let (out, n) = replace_all("john smith, jane doe", r"(\w+) (\w+)", "$2 $1", &opts).unwrap();
        assert_eq!(n, 2);
        assert_eq!(out, "smith john, doe jane");
    }

    #[test]
    fn empty_match_regex_is_skipped_everywhere() {
        let opts = SearchOptions { regex: true, ..Default::default() };
        // x* 可匹配空串：查找不得 panic，且不应产生空匹配项
        let m = find("abc", "x*", opts);
        assert!(m.is_empty());
        // 替换同样跳过空匹配（replace_all 会变成每位置插入）
        let (out, n) = replace_all("abc", "x*", "-", &opts).unwrap();
        assert_eq!((out.as_str(), n), ("abc", 0));
        // 混合空/非空匹配：只替换非空
        let (out, n) = replace_all("abc", "a?b", "X", &opts).unwrap();
        assert_eq!((out.as_str(), n), ("Xc", 1));
        // find_next 同样安全
        assert_eq!(find_next("abc", 0, "x*", &opts, true).unwrap(), None);
    }

    #[test]
    fn find_next_with_wrap() {
        let opts = SearchOptions::default();
        let m = find_next(TEXT, 40, "cat", &opts, true).unwrap().unwrap();
        assert_eq!(m.start, 4); // 回绕到第一个
        let m = find_next(TEXT, 7, "cat", &opts, false).unwrap().unwrap();
        assert_eq!(m.start, 19);
        assert_eq!(find_next(TEXT, 0, "zzz", &opts, true).unwrap(), None);
    }

    #[test]
    fn case_folding_unicode() {
        // 简单大小写折叠：开尔文符号 K (U+212A) 与 ASCII k 互相匹配
        let m = find("1\u{212A}2", "k", SearchOptions::default());
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].start, 1);
        assert_eq!(m[0].end, 2);
    }
}
