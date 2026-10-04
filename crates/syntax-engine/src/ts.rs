//! tree-sitter 语法树高亮（FR-4 质量升级）。
//!
//! 对内置主力语言（Rust / Python / JavaScript / C / JSON）使用 tree-sitter
//! 解析语法树并按标准 capture 名着色，捕获范围远超扫描式分词
//! （函数名、类型、属性、运算符等）。
//!
//! 回退策略：`HighlightConfiguration` 构建失败（查询与 grammar 版本不匹配等）
//! 或语言无 tree-sitter 支持时返回 `None`，UI 层回退到扫描式 [`crate::highlight`]，
//! 功能永不劣化。TOML / Markdown 因 grammar 版本或注入解析复杂度暂不接入。
//!
//! 性能：带文本级缓存（文本未变直接返回缓存 spans）；文本变化时整体重解析
//! （≤1MB 一次解析约 1–30ms，仅发生在编辑帧），布局层由 UI 的 Arc 缓存兜底。

use crate::{LanguageDef, Span, TokenKind};
use tree_sitter_highlight::{HighlightConfiguration, Highlighter};

/// 支持语法树高亮的内置语言。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TsLang {
    Rust,
    Python,
    JavaScript,
    C,
    Json,
    Java,
    Cpp,
    Html,
}

/// 识别为 tree-sitter 支持的语言（按内置语言名匹配）。
/// XML/HTML 共用 HTML grammar（标签/属性/注释结构一致）。
pub fn ts_lang_for(def: &LanguageDef) -> Option<TsLang> {
    match def.name.as_str() {
        "Rust" => Some(TsLang::Rust),
        "Python" => Some(TsLang::Python),
        "JavaScript" => Some(TsLang::JavaScript),
        "C" => Some(TsLang::C),
        "Json" | "JSON" => Some(TsLang::Json),
        "Java" => Some(TsLang::Java),
        "C++" => Some(TsLang::Cpp),
        "HTML" | "XML" => Some(TsLang::Html),
        _ => None,
    }
}

/// 识别的 capture 名 → TokenKind（未列出的捕获按 Plain 处理）。
fn kind_of_capture(name: &str) -> TokenKind {
    match name {
        "comment" => TokenKind::Comment,
        "string" | "string.escape" | "string.special" => TokenKind::String,
        "keyword" | "keyword.function" | "keyword.operator" | "keyword.return" | "include"
        | "label" | "tag" => TokenKind::Keyword,
        "number" => TokenKind::Number,
        "constant" | "constant.builtin" | "boolean" => TokenKind::Constant,
        "function" | "function.builtin" | "function.call" | "function.macro" | "method" => {
            TokenKind::Function
        }
        "type" | "type.builtin" | "constructor" | "class" | "struct" | "enum" | "interface"
        | "namespace" => TokenKind::Type,
        "property" | "field" | "attribute" | "string.key" | "string.special.key" => {
            TokenKind::Property
        }
        "operator" => TokenKind::Operator,
        "punctuation" | "punctuation.bracket" | "punctuation.delimiter"
        | "punctuation.special" => TokenKind::Punct,
        _ => TokenKind::Plain,
    }
}

/// 全部可识别的 capture 名（传给 configure 以固定 Highlight 索引）。
const RECOGNIZED: &[&str] = &[
    "attribute",
    "boolean",
    "comment",
    "constant",
    "constant.builtin",
    "constructor",
    "embedded",
    "escape",
    "field",
    "function",
    "function.builtin",
    "function.call",
    "function.macro",
    "include",
    "interface",
    "keyword",
    "keyword.function",
    "keyword.operator",
    "keyword.return",
    "label",
    "method",
    "namespace",
    "number",
    "operator",
    "property",
    "punctuation",
    "punctuation.bracket",
    "punctuation.delimiter",
    "punctuation.special",
    "string",
    "string.escape",
    "string.key",
    "string.special",
    "string.special.key",
    "struct",
    "tag",
    "type",
    "type.builtin",
    "variable",
    "variable.builtin",
    "variable.parameter",
    "enum",
    "class",
];

/// 语法树高亮器（每语言实例一个，带文本级缓存）。
pub struct TsHighlighter {
    highlighter: Highlighter,
    config: HighlightConfiguration,
    kind_map: Vec<TokenKind>,
    last_text: String,
    last_spans: Vec<Span>,
}

impl TsHighlighter {
    /// 为内置语言构建高亮器；构建失败（查询不匹配等）返回 None → UI 回退扫描器。
    pub fn for_language(def: &LanguageDef) -> Option<Self> {
        let ts_lang = ts_lang_for(def)?;
        let language = match ts_lang {
            TsLang::Rust => tree_sitter_rust::LANGUAGE.into(),
            TsLang::Python => tree_sitter_python::LANGUAGE.into(),
            TsLang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            TsLang::C => tree_sitter_c::LANGUAGE.into(),
            TsLang::Json => tree_sitter_json::LANGUAGE.into(),
            TsLang::Java => tree_sitter_java::LANGUAGE.into(),
            TsLang::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            TsLang::Html => tree_sitter_html::LANGUAGE.into(),
        };
        let (name, highlights, injections): (&str, String, String) = match ts_lang {
            // 各 grammar crate 的常量命名不统一（HIGHLIGHTS_QUERY / HIGHLIGHT_QUERY）
            TsLang::Rust => (
                "rust",
                tree_sitter_rust::HIGHLIGHTS_QUERY.to_string(),
                tree_sitter_rust::INJECTIONS_QUERY.to_string(),
            ),
            TsLang::Python => (
                "python",
                tree_sitter_python::HIGHLIGHTS_QUERY.to_string(),
                String::new(),
            ),
            TsLang::JavaScript => (
                "javascript",
                tree_sitter_javascript::HIGHLIGHT_QUERY.to_string(),
                tree_sitter_javascript::INJECTIONS_QUERY.to_string(),
            ),
            TsLang::C => ("c", tree_sitter_c::HIGHLIGHT_QUERY.to_string(), String::new()),
            // 官方查询把 key 捕获为 string.special.key，但被其后的 @string 覆盖
            // （同范围多捕获时后写优先）；追加末尾规则让 JSON 键显示为 Property 色
            TsLang::Json => (
                "json",
                format!(
                    "{}\n(pair key: (_) @property)\n",
                    tree_sitter_json::HIGHLIGHTS_QUERY
                ),
                String::new(),
            ),
            TsLang::Java => (
                "java",
                tree_sitter_java::HIGHLIGHTS_QUERY.to_string(),
                String::new(),
            ),
            TsLang::Cpp => (
                "cpp",
                tree_sitter_cpp::HIGHLIGHT_QUERY.to_string(),
                String::new(),
            ),
            TsLang::Html => (
                "html",
                tree_sitter_html::HIGHLIGHTS_QUERY.to_string(),
                tree_sitter_html::INJECTIONS_QUERY.to_string(),
            ),
        };
        let mut config = match HighlightConfiguration::new(language, name, &highlights, &injections, "")
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[ts] {name} 高亮查询构建失败: {e}");
                return None;
            }
        };
        config.configure(RECOGNIZED);
        // configure 后，Highlight(i).0 即 RECOGNIZED[i] 的索引
        let kind_map = RECOGNIZED.iter().map(|n| kind_of_capture(n)).collect();
        Some(Self {
            highlighter: Highlighter::new(),
            config,
            kind_map,
            last_text: String::new(),
            last_spans: vec![],
        })
    }

    /// 返回文本的高亮 spans（文本未变时命中缓存）。
    pub fn spans_for(&mut self, text: &str) -> Vec<Span> {
        if text == self.last_text {
            return self.last_spans.clone();
        }
        let spans = self.highlight_internal(text);
        self.last_text = text.to_string();
        self.last_spans = spans.clone();
        spans
    }

    fn highlight_internal(&mut self, text: &str) -> Vec<Span> {
        let Self { highlighter, config, kind_map, .. } = self;
        let iter = match highlighter.highlight(config, text.as_bytes(), None, |_| None) {
            Ok(it) => it,
            Err(_) => return degraded(text),
        };
        let mut spans: Vec<Span> = vec![];
        let mut stack: Vec<usize> = vec![];
        for ev in iter {
            let ev = match ev {
                Ok(e) => e,
                Err(_) => return degraded(text),
            };
            match ev {
                tree_sitter_highlight::HighlightEvent::HighlightStart(i) => {
                    stack.push(i.0);
                }
                tree_sitter_highlight::HighlightEvent::HighlightEnd => {
                    stack.pop();
                }
                tree_sitter_highlight::HighlightEvent::Source { start, end } => {
                    if end <= start {
                        continue;
                    }
                    let kind = stack
                        .last()
                        .and_then(|idx| kind_map.get(*idx).copied())
                        .unwrap_or(TokenKind::Plain);
                    if let Some(last) = spans.last_mut() {
                        if last.kind == kind && last.end == start {
                            last.end = end;
                            continue;
                        }
                    }
                    spans.push(Span { start, end, kind });
                }
            }
        }
        // 覆盖全文本（供 UI 直接消费）
        let mut merged: Vec<Span> = vec![];
        let mut pos = 0usize;
        for s in spans {
            if s.start > pos {
                merged.push(Span { start: pos, end: s.start, kind: TokenKind::Plain });
            }
            merged.push(s);
            pos = merged.last().unwrap().end;
        }
        if pos < text.len() {
            merged.push(Span { start: pos, end: text.len(), kind: TokenKind::Plain });
        }
        merged
    }
}

/// 解析失败时的降级输出：整篇 Plain。
fn degraded(text: &str) -> Vec<Span> {
    if text.is_empty() {
        vec![]
    } else {
        vec![Span { start: 0, end: text.len(), kind: TokenKind::Plain }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{builtin, find_by_name};

    fn spans_kinds(spans: &[Span], start: usize, end: usize) -> Vec<TokenKind> {
        spans
            .iter()
            .filter(|s| s.start < end && s.end > start)
            .map(|s| s.kind)
            .collect()
    }

    #[test]
    fn rust_ts_highlights_functions_and_keywords() {
        let rust = find_by_name(builtin(), "Rust").unwrap();
        let mut hl = TsHighlighter::for_language(rust).expect("rust ts highlighter");
        let code = "fn main() { let s = \"hi\"; }";
        let spans = hl.spans_for(code);
        // fn 关键字
        assert!(spans_kinds(&spans, 0, 2).contains(&TokenKind::Keyword));
        // main → Function
        assert!(spans_kinds(&spans, 3, 7).contains(&TokenKind::Function));
        // 字符串
        assert!(spans_kinds(&spans, 17, 21).contains(&TokenKind::String));
        // 全覆盖且相邻衔接
        assert_eq!(spans.first().unwrap().start, 0);
        assert_eq!(spans.last().unwrap().end, code.len());
        for w in spans.windows(2) {
            assert_eq!(w[0].end, w[1].start);
        }
    }

    #[test]
    fn ts_spans_cached_by_text() {
        let rust = find_by_name(builtin(), "Rust").unwrap();
        let mut hl = TsHighlighter::for_language(rust).unwrap();
        let a = hl.spans_for("let x = 1;");
        let b = hl.spans_for("let x = 1;");
        assert_eq!(a, b);
        // 文本结构变化 → spans 数量变化（Span 不含文本内容，需结构性差异）
        let c = hl.spans_for("let x = 1;\nlet y = 2;");
        assert_ne!(a.len(), c.len());
    }

    #[test]
    fn json_ts_highlights_keys_and_constants() {
        let json = find_by_name(builtin(), "JSON").unwrap();
        let mut hl = TsHighlighter::for_language(json).expect("json ts highlighter");
        let code = "{\"k\": 1, \"b\": true}";
        let spans = hl.spans_for(code);
        // 数字
        assert!(spans.iter().any(|s| s.kind == TokenKind::Number));
        // true → Constant
        assert!(spans.iter().any(|s| s.kind == TokenKind::Constant));
        // 键 → Property
        assert!(spans.iter().any(|s| s.kind == TokenKind::Property));
    }

    #[test]
    fn unsupported_language_returns_none() {
        // Markdown / TOML 等未接入 tree-sitter，应回退扫描器
        assert!(TsHighlighter::for_language(find_by_name(builtin(), "Markdown").unwrap()).is_none());
        assert!(TsHighlighter::for_language(find_by_name(builtin(), "TOML").unwrap()).is_none());
        assert!(TsHighlighter::for_language(find_by_name(builtin(), "Ruby").unwrap()).is_none());
    }

    #[test]
    fn java_ts_highlights_class_and_keywords() {
        let java = find_by_name(builtin(), "Java").unwrap();
        let mut hl = TsHighlighter::for_language(java).expect("java ts highlighter");
        let code = "public class App { void run() {} }";
        let spans = hl.spans_for(code);
        assert!(spans.iter().any(|s| s.kind == TokenKind::Keyword)); // public / class
        assert!(spans.iter().any(|s| s.kind == TokenKind::Type)); // App
    }

    #[test]
    fn html_ts_highlights_tags_and_attributes() {
        let html = find_by_name(builtin(), "HTML").unwrap();
        let mut hl = TsHighlighter::for_language(html).expect("html ts highlighter");
        let code = "<div class=\"box\">文本</div>";
        let spans = hl.spans_for(code);
        // div → tag（映射为 Keyword）
        assert!(spans.iter().any(|s| s.kind == TokenKind::Keyword
            && &code[s.start..s.end] == "div"));
        // class → attribute（映射为 Property）
        assert!(spans.iter().any(|s| s.kind == TokenKind::Property
            && &code[s.start..s.end] == "class"));
        assert!(spans.iter().any(|s| s.kind == TokenKind::String));
        // 注释
        let code = "<!-- hi --><p>x</p>";
        let spans = hl.spans_for(code);
        assert!(spans.iter().any(|s| s.kind == TokenKind::Comment));
    }

    #[test]
    fn c_ts_highlights_comments() {
        let c = find_by_name(builtin(), "C").unwrap();
        let mut hl = TsHighlighter::for_language(c).expect("c ts highlighter");
        let code = "/* note */ int x;";
        let spans = hl.spans_for(code);
        assert!(spans.iter().any(|s| s.kind == TokenKind::Comment));
        // int 是基本类型：官方查询捕获为 type.builtin
        assert!(spans.iter().any(|s| s.kind == TokenKind::Type));
    }

    #[test]
    fn python_ts_highlights_defs() {
        let py = find_by_name(builtin(), "Python").unwrap();
        let mut hl = TsHighlighter::for_language(py).expect("python ts highlighter");
        let code = "def greet():\n    return 'hi'\n";
        let spans = hl.spans_for(code);
        assert!(spans.iter().any(|s| s.kind == TokenKind::Keyword)); // def / return
        assert!(spans.iter().any(|s| s.kind == TokenKind::String)); // 'hi'
    }
}
