//! syntax-engine：语法定义 + 轻量分词高亮。
//!
//! MVP 采用内置正则无关的扫描式分词（关键词/字符串/注释/数字），
//! 输出字节区间的 [`Span`] 列表，UI 层负责映射为颜色。
//! 升级到 tree-sitter 增量解析是后续计划（见需求文档 FR-4），
//! 届时 [`TokenKind`] 与 [`Span`] 接口保持不变，仅替换实现。
//!
//! 自定义语法（FR-4.3）：用户在语法目录放置 JSON 文件即可扩展语言，
//! 字段与 [`LanguageDef] 一一对应：
//!
//! ```json
//! {
//!   "name": "MyLang",
//!   "extensions": ["ml"],
//!   "keywords": ["let", "in"],
//!   "lineComment": "#",
//!   "blockComment": ["/*", "*/"]
//! }
//! ```

use std::path::Path;
use std::sync::OnceLock;

pub mod ts;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Plain,
    Keyword,
    String,
    Comment,
    Number,
    /// 函数 / 方法名（tree-sitter 提供）
    Function,
    /// 类型 / 类 / 结构体
    Type,
    /// 对象属性 / 字段
    Property,
    /// 常量（true/false/null 等）
    Constant,
    /// 运算符
    Operator,
    /// 标点符号
    Punct,
}

/// 一个高亮片段（字节偏移，保证落在字符边界上）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub kind: TokenKind,
}

/// 大纲提取规则（FR-4.4）：正则按行匹配，第一个捕获组作为条目标签
/// （无捕获组时取整行匹配）。每行只应用第一条命中的规则。
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct OutlineRule {
    pub kind: String,
    pub regex: String,
}

/// 语言定义（内置 + 用户 JSON 加载，均为持有所有权的值）。
#[derive(Debug, Clone, Default)]
pub struct LanguageDef {
    pub name: String,
    pub extensions: Vec<String>,
    pub keywords: Vec<String>,
    pub line_comment: Option<String>,
    pub block_comment: Option<(String, String)>,
    pub outline: Vec<OutlineRule>,
    /// 标记语言模式（XML/HTML）：高亮 <标签>、属性、<!-- 注释 -->
    pub markup: bool,
    /// 关键字大小写不敏感（SQL）
    pub ci_keywords: bool,
    /// YAML 模式：行首 `key:` → 属性色，`---`/`...` 文档标记 → 关键字色
    pub yaml: bool,
}

macro_rules! lang {
    ($name:literal, $ext:expr, $kw:expr, $lc:expr, $bc:expr, $outline:expr, $markup:expr, $ci:expr, $yaml:expr) => {
        LanguageDef {
            name: $name.to_string(),
            extensions: $ext.iter().map(|s: &&str| s.to_string()).collect(),
            keywords: $kw.iter().map(|s: &&str| s.to_string()).collect(),
            line_comment: $lc.map(|s: &str| s.to_string()),
            block_comment: $bc.map(|(a, b): (&str, &str)| (a.to_string(), b.to_string())),
            outline: $outline
                .iter()
                .map(|(k, r): &(&str, &str)| OutlineRule {
                    kind: k.to_string(),
                    regex: r.to_string(),
                })
                .collect(),
            markup: $markup,
            ci_keywords: $ci,
            yaml: $yaml,
        }
    };
}

/// 内置语言表（FR-4.1）。
pub fn builtin() -> &'static [LanguageDef] {
    static BUILTIN: OnceLock<Vec<LanguageDef>> = OnceLock::new();
    BUILTIN.get_or_init(|| {
        vec![
            lang!("Rust", &["rs"], &[
                "as","async","await","break","const","continue","crate","dyn","else","enum","extern",
                "false","fn","for","if","impl","in","let","loop","match","mod","move","mut","pub",
                "ref","return","self","Self","static","struct","super","trait","true","type","unsafe",
                "use","where","while",
            ], Some("//"), Some(("/*", "*/")), &[
                ("fn", r"^\s*(?:pub\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+(\w+)"),
                ("struct", r"^\s*(?:pub\s+)?struct\s+(\w+)"),
                ("enum", r"^\s*(?:pub\s+)?enum\s+(\w+)"),
                ("trait", r"^\s*(?:pub\s+)?trait\s+(\w+)"),
                ("impl", r"^\s*impl\b.*"),
                ("mod", r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)"),
            ], false, false, false),
            lang!("Python", &["py", "pyw"], &[
                "and","as","assert","async","await","break","class","continue","def","del","elif",
                "else","except","False","finally","for","from","global","if","import","in","is",
                "lambda","None","nonlocal","not","or","pass","raise","return","True","try","while",
                "with","yield",
            ], Some("#"), None, &[
                ("class", r"^\s*class\s+(\w+)"),
                ("def", r"^\s*(?:async\s+)?def\s+(\w+)"),
            ], false, false, false),
            lang!("JavaScript", &["js", "mjs", "cjs", "ts"], &[
                "async","await","break","case","catch","class","const","continue","default","delete",
                "do","else","export","extends","false","finally","for","from","function","if",
                "import","in","instanceof","let","new","null","of","return","static","super","switch",
                "this","throw","true","try","typeof","undefined","var","void","while","yield",
            ], Some("//"), Some(("/*", "*/")), &[
                ("class", r"^\s*(?:export\s+)?(?:default\s+)?class\s+(\w+)"),
                ("function", r"^\s*(?:export\s+)?(?:async\s+)?function\s*(\w+)"),
                ("const", r"^\s*(?:export\s+)?const\s+(\w+)\s*="),
            ], false, false, false),
            lang!("Java", &["java"], &[
                "abstract","assert","boolean","break","byte","case","catch","char","class","const",
                "continue","default","do","double","else","enum","extends","final","finally","float",
                "for","goto","if","implements","import","instanceof","int","interface","long","native",
                "new","package","private","protected","public","record","return","sealed","short",
                "static","strictfp","super","switch","synchronized","this","throw","throws","transient",
                "try","var","void","volatile","while","true","false","null","yield",
            ], Some("//"), Some(("/*", "*/")), &[
                ("class", r"^\s*(?:public\s+|private\s+|protected\s+)?(?:abstract\s+|final\s+|static\s+|sealed\s+|non-sealed\s+)*class\s+(\w+)"),
                ("interface", r"^\s*(?:public\s+|private\s+)?(?:sealed\s+)?interface\s+(\w+)"),
                ("enum", r"^\s*(?:public\s+|private\s+)?enum\s+(\w+)"),
                ("record", r"^\s*(?:public\s+|private\s+)?record\s+(\w+)"),
                ("fn", r"^\s*(?:@[\w\.]+\s+)*(?:public|private|protected|static|final|abstract|synchronized|native|default)[\w\s<>\[\],\.\?]*\s+(\w+)\s*\("),
            ], false, false, false),
            lang!("C", &["c", "h"], &[
                "auto","break","case","char","const","continue","default","do","double","else","enum",
                "extern","float","for","goto","if","int","long","register","return","short","signed",
                "sizeof","static","struct","switch","typedef","union","unsigned","void","volatile",
                "while",
            ], Some("//"), Some(("/*", "*/")), &[
                ("fn", r"^\s*[A-Za-z_][\w\s\*]*\b(\w+)\s*\([^;]*\)\s*\{?\s*$"),
                ("struct", r"^\s*(?:typedef\s+)?struct\s+(\w+)"),
            ], false, false, false),
            lang!("C++", &["cpp", "cc", "cxx", "hpp", "hh", "hxx"], &[
                "alignas","alignof","and","auto","bool","break","case","catch","char","class","const",
                "constexpr","const_cast","continue","decltype","default","delete","do","double",
                "dynamic_cast","else","enum","explicit","export","extern","false","float","for","friend",
                "goto","if","inline","int","long","mutable","namespace","new","noexcept","not","nullptr",
                "operator","or","override","private","protected","public","register","reinterpret_cast",
                "return","short","signed","sizeof","static","static_assert","static_cast","struct",
                "switch","template","this","throw","true","try","typedef","typeid","typename","union",
                "unsigned","using","virtual","void","volatile","wchar_t","while",
            ], Some("//"), Some(("/*", "*/")), &[
                ("class", r"^\s*(?:template\s*<[^>]*>\s*)?(?:class|struct)\s+(\w+)"),
                ("namespace", r"^\s*namespace\s+(\w+)"),
                ("fn", r"^\s*[A-Za-z_][\w:<>,\s\*&~]*\b(\w+)\s*\([^;]*\)\s*(?:const)?\s*\{?\s*$"),
            ], false, false, false),
            lang!("C#", &["cs"], &[
                "abstract","as","base","bool","break","byte","case","catch","char","checked","class",
                "const","continue","decimal","default","delegate","do","double","else","enum","event",
                "explicit","extern","false","finally","fixed","float","for","foreach","goto","if",
                "implicit","in","int","interface","internal","is","lock","long","namespace","new","null",
                "object","operator","out","override","params","private","protected","public","readonly",
                "record","ref","return","sbyte","sealed","short","sizeof","stackalloc","static","string",
                "struct","switch","this","throw","true","try","typeof","uint","ulong","unchecked",
                "unsafe","ushort","using","var","virtual","void","volatile","while",
            ], Some("//"), Some(("/*", "*/")), &[
                ("namespace", r"^\s*namespace\s+([\w\.]+)"),
                ("class", r"^\s*(?:public\s+|private\s+|internal\s+|protected\s+)?(?:abstract\s+|sealed\s+|static\s+|partial\s+)*class\s+(\w+)"),
                ("interface", r"^\s*(?:public\s+|internal\s+)?interface\s+(\w+)"),
                ("record", r"^\s*(?:public\s+|internal\s+)?(?:sealed\s+)?record\s+(\w+)"),
                ("fn", r"^\s*(?:public|private|protected|internal)[\w\s<>\[\],\.\?]*\s+(\w+)\s*\("),
            ], false, false, false),
            lang!("Go", &["go"], &[
                "break","case","chan","const","continue","default","defer","else","fallthrough","for",
                "func","go","goto","if","import","interface","map","package","range","return","select",
                "struct","switch","type","var","nil","true","false","iota",
            ], Some("//"), Some(("/*", "*/")), &[
                ("func", r"^\s*func\s+(?:\([^)]*\)\s*)?(\w+)"),
                ("type", r"^\s*type\s+(\w+)\s"),
            ], false, false, false),
            lang!("Kotlin", &["kt", "kts"], &[
                "as","break","class","continue","do","else","false","for","fun","if","in","interface",
                "is","null","object","package","return","super","this","throw","true","try","typealias",
                "typeof","val","var","when","while","by","catch","constructor","delegate","dynamic",
                "field","file","finally","get","import","init","param","property","receiver","set",
                "setparam","where","abstract","actual","annotation","companion","const","crossinline",
                "data","enum","expect","external","final","infix","inline","inner","internal","lateinit",
                "noinline","open","operator","out","override","private","protected","public","reified",
                "sealed","suspend","tailrec","vararg",
            ], Some("//"), Some(("/*", "*/")), &[
                ("class", r"^\s*(?:public\s+|private\s+|internal\s+|protected\s+)?(?:data\s+|sealed\s+|open\s+|abstract\s+|enum\s+|inner\s+)*class\s+(\w+)"),
                ("object", r"^\s*object\s+(\w+)"),
                ("interface", r"^\s*(?:public\s+|internal\s+)?interface\s+(\w+)"),
                ("fun", r"^\s*(?:private\s+|public\s+|internal\s+|protected\s+)?(?:suspend\s+)?fun\s+(?:<[^>]*>\s+)?(?:[\w\.]+\.)?(\w+)\s*\("),
            ], false, false, false),
            lang!("Ruby", &["rb"], &[
                "def","end","class","module","if","elsif","else","unless","while","until","for","in",
                "do","then","begin","rescue","ensure","raise","yield","return","break","next","redo",
                "retry","case","when","super","self","nil","true","false","and","or","not","alias",
                "defined?","__method__","require","require_relative","include","extend","attr_accessor",
                "attr_reader","attr_writer","lambda","proc","loop","puts","p",
            ], Some("#"), None, &[
                ("class", r"^\s*class\s+(\w+)"),
                ("module", r"^\s*module\s+(\w+)"),
                ("def", r"^\s*def\s+(?:self\.)?(\w+[?!]?)"),
            ], false, false, false),
            lang!("Shell", &["sh", "bash", "zsh"], &[
                "if","then","else","elif","fi","for","while","until","do","done","case","esac",
                "function","in","select","time","coproc","return","break","continue","exit","local",
                "export","readonly","declare","typeset","unset","shift","eval","exec","source","set",
                "trap","alias","bind","builtin","caller","command","echo","help","let","logout",
                "printf","read","shopt","test","type","wait",
            ], Some("#"), None, &[
                ("function", r"^\s*(?:function\s+)?(\w+)\s*\(\)\s*\{?"),
            ], false, false, false),
            lang!("SQL", &["sql"], &[
                "SELECT","FROM","WHERE","INSERT","INTO","VALUES","UPDATE","SET","DELETE","CREATE",
                "TABLE","DROP","ALTER","ADD","INDEX","VIEW","JOIN","LEFT","RIGHT","INNER","OUTER",
                "FULL","ON","GROUP","BY","ORDER","HAVING","LIMIT","OFFSET","UNION","ALL","DISTINCT",
                "AS","AND","OR","NOT","NULL","IS","IN","LIKE","BETWEEN","EXISTS","CASE","WHEN","THEN",
                "ELSE","END","PRIMARY","KEY","FOREIGN","REFERENCES","DEFAULT","UNIQUE","CHECK",
                "CONSTRAINT","IF","ASC","DESC","COUNT","SUM","AVG","MIN","MAX","WITH","RECURSIVE",
                "BEGIN","COMMIT","ROLLBACK","TRANSACTION","GRANT","REVOKE","TRUNCATE","REPLACE",
            ], Some("--"), Some(("/*", "*/")), &[
                ("table", r#"(?i)^\s*CREATE\s+TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?[`"\[]?([\w\.]+)"#),
                ("view", r"(?i)^\s*CREATE\s+(?:OR\s+REPLACE\s+)?VIEW\s+([\w\.]+)"),
            ], false, true, false),
            lang!("XML", &["xml", "svg", "xsl", "xslt", "plist"], &[], None, None, &[],
                true, false, false),
            lang!("HTML", &["html", "htm"], &[], None, None, &[],
                true, false, false),
            lang!("JSON", &["json"], &["false", "null", "true"], None, None, &[], false, false, false),
            lang!("TOML", &["toml"], &["false", "true"], Some("#"), None, &[
                ("section", r"^\s*\[([^\]]+)\]"),
            ], false, false, false),
            lang!("YAML", &["yaml", "yml"], &[
                "true","false","null","yes","no","on","off",
            ], Some("#"), None, &[
                ("key", r"^([\w][\w .\-]*):(?:\s|$)"),
            ], false, false, true),
            lang!("Markdown", &["md", "markdown"], &[], None, None, &[
                ("H1", r"^#\s+(.+)$"),
                ("H2", r"^##\s+(.+)$"),
                ("H3", r"^###\s+(.+)$"),
                ("H4", r"^####\s+(.+)$"),
                ("H5", r"^#####\s+(.+)$"),
                ("H6", r"^######\s+(.+)$"),
            ], false, false, false),
        ]
    })
}

/// 按扩展名在语言表中探测。
pub fn detect_for<'a>(defs: &'a [LanguageDef], path: &Path) -> Option<&'a LanguageDef> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    defs.iter().find(|l| l.extensions.iter().any(|e| e.eq_ignore_ascii_case(&ext)))
}

/// 按名称查找（大小写不敏感）。
pub fn find_by_name<'a>(defs: &'a [LanguageDef], name: &str) -> Option<&'a LanguageDef> {
    defs.iter().find(|l| l.name.eq_ignore_ascii_case(name))
}

// ---------- 用户自定义语法（FR-4.3） ----------

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct SyntaxFile {
    name: String,
    extensions: Vec<String>,
    keywords: Vec<String>,
    line_comment: Option<String>,
    block_comment: Option<(String, String)>,
    outline: Vec<OutlineRule>,
    markup: bool,
    ci_keywords: bool,
    yaml: bool,
}

/// 从单个 JSON 文件加载语法定义。
pub fn load_user_syntax(path: &Path) -> Option<LanguageDef> {
    let content = std::fs::read_to_string(path).ok()?;
    let sf: SyntaxFile = serde_json::from_str(&content).ok()?;
    if sf.name.trim().is_empty() {
        return None;
    }
    Some(LanguageDef {
        name: sf.name.trim().to_string(),
        extensions: sf.extensions,
        keywords: sf.keywords,
        line_comment: sf.line_comment,
        block_comment: sf.block_comment,
        outline: sf.outline,
        markup: sf.markup,
        ci_keywords: sf.ci_keywords,
        yaml: sf.yaml,
    })
}

/// 加载目录下全部 `*.json` 语法（按文件名排序，保证菜单顺序稳定）。
pub fn load_user_syntaxes(dir: &Path) -> Vec<LanguageDef> {
    let mut out = vec![];
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    let mut paths: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if p.extension().map(|e| e == "json").unwrap_or(false) {
            if let Some(def) = load_user_syntax(&p) {
                out.push(def);
            }
        }
    }
    out
}

// ---------- 大纲（FR-4.4） ----------

/// 大纲条目：行号（0 基）+ 标签 + 类别（fn/class/H1…）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineItem {
    pub line: usize,
    pub label: String,
    pub kind: String,
}

/// 单个大纲标签上限（过长截断）。
const OUTLINE_LABEL_MAX: usize = 60;

/// 按语言的大纲规则逐行提取条目。无语言或无规则时返回空。
pub fn outline(text: &str, lang: Option<&LanguageDef>) -> Vec<OutlineItem> {
    let Some(def) = lang else { return vec![] };
    if def.outline.is_empty() {
        return vec![];
    }
    let compiled: Vec<(&str, regex::Regex)> = def
        .outline
        .iter()
        .filter_map(|r| regex::Regex::new(&r.regex).ok().map(|re| (r.kind.as_str(), re)))
        .collect();
    if compiled.is_empty() {
        return vec![];
    }

    let mut out = vec![];
    for (i, raw) in text.split('\n').enumerate() {
        let line = raw.trim_end_matches('\r');
        for (kind, re) in &compiled {
            if let Some(caps) = re.captures(line) {
                // 第一个非空捕获组作标签，否则整行匹配
                let label = (1..caps.len())
                    .find_map(|g| caps.get(g).map(|m| m.as_str().trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| caps.get(0).unwrap().as_str().trim().to_string());
                if !label.is_empty() {
                    let mut label = label;
                    if label.chars().count() > OUTLINE_LABEL_MAX {
                        label = label.chars().take(OUTLINE_LABEL_MAX).collect::<String>() + "…";
                    }
                    out.push(OutlineItem { line: i, label, kind: (*kind).to_string() });
                }
                break; // 每行只取第一条命中的规则
            }
        }
    }
    out
}

// ---------- 高亮 ----------

/// 高亮：扫描全文产出 Span 列表（覆盖全部文本，相邻同种合并）。
/// 纯扫描分词，O(n)；对 10MB 文本一次全量扫描 < 100ms（可接受，后续换 tree-sitter 增量）。
pub fn highlight(text: &str, def: Option<&LanguageDef>) -> Vec<Span> {
    let Some(def) = def else {
        return vec![Span { start: 0, end: text.len(), kind: TokenKind::Plain }];
    };
    if text.is_empty() {
        return vec![];
    }

    let mut tokens: Vec<Span> = vec![];
    let bytes = text.as_bytes();
    let mut i = 0usize;

    while i < text.len() {
        // YAML：行首的 `---`/`...` 文档标记、`- ` 列表标记、`key:` 键
        if def.yaml && (i == 0 || bytes[i - 1] == b'\n') {
            let mut j = i;
            while j < bytes.len() && bytes[j] == b' ' {
                j += 1;
            }
            let indent = j - i;
            if indent == 0 && (text[j..].starts_with("---") || text[j..].starts_with("...")) {
                tokens.push(Span { start: i, end: j + 3, kind: TokenKind::Keyword });
                i = j + 3;
                continue;
            }
            if j < bytes.len()
                && bytes[j] == b'-'
                && j + 1 < bytes.len()
                && (bytes[j + 1] == b' ' || bytes[j + 1] == b'\n' || bytes[j + 1] == b'\r')
            {
                tokens.push(Span { start: j, end: j + 1, kind: TokenKind::Punct });
                i = j + 1;
                continue;
            }
            if j < bytes.len() && (bytes[j].is_ascii_alphabetic() || bytes[j] == b'_') {
                // 键扫描：扫到 ':' 且其后为空格/行尾才认定为键（YAML 映射语法要求）
                let mut k = j;
                while k < bytes.len() && bytes[k] != b':' && bytes[k] != b'\n' && bytes[k] != b'\r' {
                    k += 1;
                }
                if k < bytes.len()
                    && bytes[k] == b':'
                    && (k + 1 == bytes.len()
                        || matches!(bytes[k + 1], b' ' | b'\t' | b'\n' | b'\r'))
                {
                    tokens.push(Span { start: i, end: k, kind: TokenKind::Property });
                    i = k;
                    continue;
                }
            }
        }
        // 标记语言（XML/HTML）：<!-- 注释 --> 与 <标签 属性="值">
        if def.markup {
            if text[i..].starts_with("<!--") {
                let end = text[i + 4..]
                    .find("-->")
                    .map(|p| i + 4 + p + 3)
                    .unwrap_or(text.len());
                tokens.push(Span { start: i, end, kind: TokenKind::Comment });
                i = end;
                continue;
            }
            if bytes[i] == b'<' {
                i = scan_markup_tag(text, i, &mut tokens);
                continue;
            }
        }
        // 行注释
        if let Some(lc) = &def.line_comment {
            if text[i..].starts_with(lc.as_str()) {
                let end = text[i..].find('\n').map(|p| i + p).unwrap_or(text.len());
                tokens.push(Span { start: i, end, kind: TokenKind::Comment });
                i = end;
                continue;
            }
        }
        // 块注释
        if let Some((bs, be)) = &def.block_comment {
            if text[i..].starts_with(bs.as_str()) {
                let end = match text[i + bs.len()..].find(be.as_str()) {
                    Some(p) => i + bs.len() + p + be.len(),
                    None => text.len(),
                };
                tokens.push(Span { start: i, end, kind: TokenKind::Comment });
                i = end;
                continue;
            }
        }
        // 字符串
        if bytes[i] == b'"' || bytes[i] == b'\'' {
            let quote = bytes[i];
            let mut j = i + 1;
            while j < bytes.len() {
                if bytes[j] == b'\\' && j + 1 < bytes.len() {
                    j += 2;
                } else if bytes[j] == quote {
                    j += 1;
                    break;
                } else {
                    j += 1;
                }
            }
            tokens.push(Span { start: i, end: j.min(text.len()), kind: TokenKind::String });
            i = j;
            continue;
        }
        // 数字
        if bytes[i].is_ascii_digit() && !prev_is_word(bytes, i) {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'.' || bytes[j] == b'_') {
                j += 1;
            }
            tokens.push(Span { start: i, end: j, kind: TokenKind::Number });
            i = j;
            continue;
        }
        // 标识符 / 关键词
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            let word = &text[i..j];
            let is_kw = if def.ci_keywords {
                def.keywords.iter().any(|k| k.eq_ignore_ascii_case(word))
            } else {
                def.keywords.iter().any(|k| k == word)
            };
            let kind = if is_kw { TokenKind::Keyword } else { TokenKind::Plain };
            tokens.push(Span { start: i, end: j, kind });
            i = j;
            continue;
        }
        // 其余单字符（Plain）
        let c = text[i..].chars().next().unwrap();
        tokens.push(Span { start: i, end: i + c.len_utf8(), kind: TokenKind::Plain });
        i += c.len_utf8();
    }

    // 相邻同种合并 + 空隙补 Plain（保证覆盖全文本，供 UI 直接消费）
    let mut merged: Vec<Span> = vec![];
    let mut pos = 0usize;
    for t in tokens {
        if t.start > pos {
            merged.push(Span { start: pos, end: t.start, kind: TokenKind::Plain });
        }
        if let Some(last) = merged.last_mut() {
            if last.kind == t.kind && last.end == t.start {
                last.end = t.end;
                pos = t.end;
                continue;
            }
        }
        merged.push(t);
        pos = t.end;
    }
    if pos < text.len() {
        merged.push(Span { start: pos, end: text.len(), kind: TokenKind::Plain });
    }
    merged
}

fn prev_is_word(bytes: &[u8], i: usize) -> bool {
    i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_')
}

/// 扫描一个完整的标记语言标签（从 `<` 到 `>`）：
/// `<`/`>`/`/>` → Punct，标签名 → Keyword，后跟 `=` 的属性名 → Property，属性值 → String。
/// 返回新的扫描位置（至少前进 1 字节，保证外层循环推进）。
fn scan_markup_tag(text: &str, start: usize, tokens: &mut Vec<Span>) -> usize {
    let bytes = text.as_bytes();
    let len = text.len();
    tokens.push(Span { start, end: start + 1, kind: TokenKind::Punct });
    let mut j = start + 1;
    if j < len && (bytes[j] == b'/' || bytes[j] == b'?' || bytes[j] == b'!') {
        tokens.push(Span { start: j, end: j + 1, kind: TokenKind::Punct });
        j += 1;
    }
    // 标签名
    let name_start = j;
    while j < len && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || bytes[j] == b':' || bytes[j] == b'-') {
        j += 1;
    }
    if j > name_start {
        tokens.push(Span { start: name_start, end: j, kind: TokenKind::Keyword });
    }
    // 属性区：直到 '>'
    while j < len && bytes[j] != b'>' {
        match bytes[j] {
            b'"' | b'\'' => {
                let quote = bytes[j];
                let mut k = j + 1;
                while k < len {
                    if bytes[k] == quote {
                        k += 1;
                        break;
                    } else {
                        k += 1;
                    }
                }
                tokens.push(Span { start: j, end: k.min(len), kind: TokenKind::String });
                j = k;
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let a_start = j;
                while j < len
                    && (bytes[j].is_ascii_alphanumeric()
                        || bytes[j] == b'_'
                        || bytes[j] == b':'
                        || bytes[j] == b'-'
                        || bytes[j] == b'.')
                {
                    j += 1;
                }
                // 前瞻 '=' 判断是否属性名
                let mut k = j;
                while k < len && (bytes[k] == b' ' || bytes[k] == b'\t') {
                    k += 1;
                }
                let kind = if k < len && bytes[k] == b'=' {
                    TokenKind::Property
                } else {
                    TokenKind::Plain
                };
                tokens.push(Span { start: a_start, end: j, kind });
            }
            _ => {
                tokens.push(Span { start: j, end: j + 1, kind: TokenKind::Punct });
                j += 1;
            }
        }
    }
    if j < len {
        // 吃掉 '>'
        tokens.push(Span { start: j, end: j + 1, kind: TokenKind::Punct });
        j += 1;
    }
    j
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_by_extension() {
        let defs = builtin();
        assert_eq!(detect_for(defs, Path::new("main.rs")).unwrap().name, "Rust");
        assert_eq!(detect_for(defs, Path::new("app.TS")).unwrap().name, "JavaScript");
        assert!(detect_for(defs, Path::new("file.xyz")).is_none());
        assert!(detect_for(defs, Path::new("noext")).is_none());
    }

    #[test]
    fn rust_highlight_spans() {
        let code = "let s = \"hi\"; // note\n";
        let rust = find_by_name(builtin(), "Rust");
        let spans = highlight(code, rust);
        assert_eq!(spans[0], Span { start: 0, end: 3, kind: TokenKind::Keyword }); // let
        assert!(spans.iter().any(|s| s.kind == TokenKind::String && s.start == 8 && s.end == 12));
        assert!(spans.iter().any(|s| s.kind == TokenKind::Comment && s.start == 14));
        // 全覆盖
        assert_eq!(spans.first().unwrap().start, 0);
        assert_eq!(spans.last().unwrap().end, code.len());
        for w in spans.windows(2) {
            assert_eq!(w[0].end, w[1].start);
        }
    }

    #[test]
    fn block_comment_and_numbers() {
        let code = "/* c */ 0x1F 42";
        let c = find_by_name(builtin(), "C");
        let spans = highlight(code, c);
        assert!(spans.iter().any(|s| s.kind == TokenKind::Comment && s.start == 0 && s.end == 7));
        assert!(spans.iter().any(|s| s.kind == TokenKind::Number && &code[s.start..s.end] == "0x1F"));
        assert!(spans.iter().any(|s| s.kind == TokenKind::Number && &code[s.start..s.end] == "42"));
    }

    #[test]
    fn unterminated_string_and_comment() {
        let rust = find_by_name(builtin(), "Rust");
        let spans = highlight("let x = \"abc", rust);
        assert!(spans.iter().any(|s| s.kind == TokenKind::String && s.end == 12));
        let spans = highlight("/* abc", rust);
        assert!(spans.iter().any(|s| s.kind == TokenKind::Comment && s.end == 6));
    }

    #[test]
    fn no_language_is_all_plain() {
        let spans = highlight("anything 中文", None);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].kind, TokenKind::Plain);
    }

    #[test]
    fn cjk_text_never_splits_chars() {
        // 中文注释 + 中文串：span 边界不得落在多字节字符内部
        let code = "中文/*注释*/中文";
        let c = find_by_name(builtin(), "C");
        let spans = highlight(code, c);
        for s in &spans {
            assert!(code.is_char_boundary(s.start));
            assert!(code.is_char_boundary(s.end));
        }
    }

    #[test]
    fn user_syntax_json_roundtrip() {
        let dir = std::env::temp_dir().join("cote-syntax-test");
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("testlang.json");
        std::fs::write(&p, r##"{
            "name": "TestLang",
            "extensions": ["tst"],
            "keywords": ["foo", "bar"],
            "lineComment": "//",
            "blockComment": ["<#", "#>"]
        }"##).unwrap();
        let defs = load_user_syntaxes(&dir);
        assert_eq!(defs.len(), 1);
        let d = &defs[0];
        assert_eq!(d.name, "TestLang");
        assert_eq!(d.extensions, vec!["tst".to_string()]);
        assert_eq!(d.line_comment.as_deref(), Some("//"));
        assert_eq!(d.block_comment.as_ref().unwrap().0, "<#");

        let combined = [defs.as_slice(), builtin()].concat();
        assert!(detect_for(&combined, Path::new("a.tst")).is_some());
        let code = "foo 1 <# c #> \"s\"";
        let spans = highlight(code, Some(d));
        assert!(spans.iter().any(|s| s.kind == TokenKind::Keyword && &code[s.start..s.end] == "foo"));
        assert!(spans.iter().any(|s| s.kind == TokenKind::Comment && s.start == 6 && s.end == 13));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn detect_new_languages() {
        let defs = builtin();
        assert_eq!(detect_for(defs, Path::new("App.java")).unwrap().name, "Java");
        assert_eq!(detect_for(defs, Path::new("config.XML")).unwrap().name, "XML");
        assert_eq!(detect_for(defs, Path::new("index.html")).unwrap().name, "HTML");
        assert_eq!(detect_for(defs, Path::new("Main.cs")).unwrap().name, "C#");
        assert_eq!(detect_for(defs, Path::new("main.cpp")).unwrap().name, "C++");
        assert_eq!(detect_for(defs, Path::new("main.go")).unwrap().name, "Go");
        assert_eq!(detect_for(defs, Path::new("Main.kt")).unwrap().name, "Kotlin");
        assert_eq!(detect_for(defs, Path::new("app.rb")).unwrap().name, "Ruby");
        assert_eq!(detect_for(defs, Path::new("run.sh")).unwrap().name, "Shell");
        assert_eq!(detect_for(defs, Path::new("query.sql")).unwrap().name, "SQL");
    }

    #[test]
    fn java_highlight_and_outline() {
        let java = find_by_name(builtin(), "Java").unwrap();
        let code = "public class App {\n    public int add(int a, int b) {\n        return a + b;\n    }\n}\n";
        let spans = highlight(code, Some(java));
        assert!(spans.iter().any(|s| s.kind == TokenKind::Keyword));
        let items = outline(code, Some(java));
        assert!(items.iter().any(|i| i.kind == "class" && i.label == "App"));
        assert!(items.iter().any(|i| i.kind == "fn" && i.label == "add"));
    }

    #[test]
    fn xml_markup_highlight() {
        let xml = find_by_name(builtin(), "XML").unwrap();
        let code = "<?xml version=\"1.0\"?><!-- note --><root id=\"a1\"><name>值</name></root>";
        let spans = highlight(code, Some(xml));
        assert!(spans.iter().any(|s| s.kind == TokenKind::Comment)); // <!-- note -->
        assert!(spans.iter().any(|s| s.kind == TokenKind::Property)); // version / id
        assert!(spans.iter().any(|s| s.kind == TokenKind::String)); // "1.0" / "a1"
        // 标签名 root → Keyword：两个 root 标签名位置各一次
        let root_kws: Vec<_> = spans
            .iter()
            .filter(|s| s.kind == TokenKind::Keyword && &code[s.start..s.end] == "root")
            .collect();
        assert_eq!(root_kws.len(), 2);
        // 中文文本内容为 Plain 且不被拆碎
        assert!(spans
            .iter()
            .any(|s| s.kind == TokenKind::Plain && &code[s.start..s.end] == "值"));
        // 全覆盖衔接
        assert_eq!(spans.first().unwrap().start, 0);
        assert_eq!(spans.last().unwrap().end, code.len());
        for w in spans.windows(2) {
            assert_eq!(w[0].end, w[1].start);
        }
    }

    #[test]
    fn sql_case_insensitive_keywords() {
        let sql = find_by_name(builtin(), "SQL").unwrap();
        let spans = highlight("select name from users where id = 1;", Some(sql));
        assert!(spans.iter().any(|s| s.kind == TokenKind::Keyword
            && &"select name from users where id = 1;"[s.start..s.end] == "select"));
        assert!(spans.iter().any(|s| s.kind == TokenKind::Keyword
            && &"select name from users where id = 1;"[s.start..s.end] == "WHERE"
                || s.kind == TokenKind::Keyword
                    && &"select name from users where id = 1;"[s.start..s.end] == "where"));
        // -- 行注释（SQL 的行注释符）
        let spans = highlight("-- comment\nSELECT 1;", Some(sql));
        assert!(spans.iter().any(|s| s.kind == TokenKind::Comment));
    }

    #[test]
    fn yaml_highlight_and_outline() {
        let yaml = find_by_name(builtin(), "YAML").unwrap();
        let code = "---\nname: cote\nversion: 1.0\nitems:\n  - one\n  - two\n# 注释\nenabled: true\n";
        let spans = highlight(code, Some(yaml));
        // 顶层与嵌套键 → Property
        assert!(spans.iter().any(|s| s.kind == TokenKind::Property && &code[s.start..s.end] == "name"));
        assert!(spans.iter().any(|s| s.kind == TokenKind::Property && &code[s.start..s.end] == "items"));
        // `version: 1.0` 的键是 Property，1.0 是 Number
        assert!(spans.iter().any(|s| s.kind == TokenKind::Number && &code[s.start..s.end] == "1.0"));
        // 文档标记 → Keyword
        assert!(spans.iter().any(|s| s.kind == TokenKind::Keyword && &code[s.start..s.end] == "---"));
        // 注释
        assert!(spans.iter().any(|s| s.kind == TokenKind::Comment));
        // 列表标记 → Punct（两个 '-'）
        let dashes = spans.iter().filter(|s| s.kind == TokenKind::Punct && &code[s.start..s.end] == "-").count();
        assert_eq!(dashes, 2);
        // 布尔字面量 → Keyword
        assert!(spans.iter().any(|s| s.kind == TokenKind::Keyword && &code[s.start..s.end] == "true"));
        // `http://` 不应被误判为键（无冒号+空格结尾的行首词保持 Plain）
        let code2 = "homepage: http://example.com\n";
        let spans2 = highlight(code2, Some(yaml));
        assert!(spans2.iter().any(|s| s.kind == TokenKind::Property && &code2[s.start..s.end] == "homepage"));
        // URL 不应整体为 Property（只有 homepage 是键）
        assert!(!spans2
            .iter()
            .any(|s| s.kind == TokenKind::Property && code2[s.start..s.end].starts_with("http")));
        // 大纲：仅顶层键
        let items = outline(code, Some(yaml));
        assert!(items.iter().any(|i| i.label == "name"));
        assert!(items.iter().any(|i| i.label == "items"));
        assert!(!items.iter().any(|i| i.label.contains("one")));
    }

    #[test]
    fn go_and_cpp_outline() {
        let go = find_by_name(builtin(), "Go").unwrap();
        let items = outline("func main() {\n}\n\nfunc (s *S) Read() {\n}\n", Some(go));
        assert!(items.iter().any(|i| i.label == "main"));
        assert!(items.iter().any(|i| i.label == "Read"));

        let cpp = find_by_name(builtin(), "C++").unwrap();
        let code = "namespace app {\nclass Widget {\npublic:\n    void draw();\n};\n}\n";
        let items = outline(code, Some(cpp));
        assert!(items.iter().any(|i| i.kind == "namespace" && i.label == "app"));
        assert!(items.iter().any(|i| i.kind == "class" && i.label == "Widget"));
    }

    #[test]
    fn outline_rust_and_markdown() {
        let code = "mod app;\n\npub struct Config {\n}\n\nimpl Config {\n    pub fn new() -> Self {\n        0\n    }\n}\n";
        let items = outline(code, find_by_name(builtin(), "Rust"));
        assert_eq!(items[0].kind, "mod");
        assert_eq!(items[0].label, "app");
        assert_eq!(items[0].line, 0);
        assert!(items.iter().any(|i| i.kind == "struct" && i.label == "Config" && i.line == 2));
        assert!(items.iter().any(|i| i.kind == "impl" && i.line == 5));
        assert!(items.iter().any(|i| i.kind == "fn" && i.label == "new" && i.line == 6));

        let md = "# 标题一\n正文\n## 子标题\n";
        let items = outline(md, find_by_name(builtin(), "Markdown"));
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].kind, "H1");
        assert_eq!(items[0].label, "标题一");
        assert_eq!(items[1].kind, "H2");
        assert_eq!(items[1].line, 2);
    }

    #[test]
    fn outline_no_language_or_rules() {
        assert!(outline("anything", None).is_empty());
        assert!(outline("anything", find_by_name(builtin(), "JSON")).is_empty());
    }

    #[test]
    fn outline_long_label_truncated() {
        let long = "f".repeat(200);
        let code = format!("pub fn {long}() {{}}\n");
        let items = outline(&code, find_by_name(builtin(), "Rust"));
        assert_eq!(items.len(), 1);
        assert!(items[0].label.chars().count() <= 61); // 60 + 省略号
        assert!(items[0].label.ends_with('…'));
    }
}
