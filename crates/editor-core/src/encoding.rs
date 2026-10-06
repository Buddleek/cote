//! 编码检测、解码与编码（FR-2）。
//!
//! 检测顺序（对齐 CotEditor 策略）：
//! 1. BOM（UTF-8 / UTF-16LE/BE / UTF-32LE/BE）——确定性最高；
//! 2. 合法 UTF-8 → 按 UTF-8（编辑器场景下正确优先于浏览器式猜测）；
//! 3. chardetng 统计检测（Firefox 同源检测器，已排除 UTF-8 时禁用其 UTF-8 候选）。
//!
//! UTF-32 不在 WHATWG/encoding_rs 范围内，此处按字节序手动转换；
//! 其余编码由 encoding_rs 承担。有损编码的不可表示字符通过
//! `encode_from_utf8_without_replacement` 的 `Unmappable` 逐个上报。

use encoding_rs::{EncoderResult, Encoding};

/// UI 直接展示的常用编码（FR-2.7：全集来自 encoding_rs + 手写 UTF-32，常用优先）。
/// 注意不收录 ISO-8859-1：WHATWG 将该标签映射到 windows-1252，单独列出只会误导。
pub const COMMON_ENCODINGS: &[&str] = &[
    "UTF-8",
    "UTF-16LE",
    "UTF-16BE",
    "UTF-32LE",
    "UTF-32BE",
    "GBK",
    "GB18030",
    "Big5",
    "Shift_JIS",
    "EUC-JP",
    "ISO-2022-JP",
    "EUC-KR",
    "windows-1252",
    "windows-1251",
    "windows-1250",
    "windows-1254",
    "windows-874",
    "ISO-8859-2",
    "ISO-8859-15",
    "KOI8-R",
];

/// UTF 家族（含 encoding_rs 不覆盖的 UTF-32）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UtfKind {
    Utf8,
    Utf16Le,
    Utf16Be,
    Utf32Le,
    Utf32Be,
}

impl UtfKind {
    pub fn name(&self) -> &'static str {
        match self {
            UtfKind::Utf8 => "UTF-8",
            UtfKind::Utf16Le => "UTF-16LE",
            UtfKind::Utf16Be => "UTF-16BE",
            UtfKind::Utf32Le => "UTF-32LE",
            UtfKind::Utf32Be => "UTF-32BE",
        }
    }

    pub fn bom_bytes(&self) -> &'static [u8] {
        match self {
            UtfKind::Utf8 => &[0xEF, 0xBB, 0xBF],
            UtfKind::Utf16Le => &[0xFF, 0xFE],
            UtfKind::Utf16Be => &[0xFE, 0xFF],
            UtfKind::Utf32Le => &[0xFF, 0xFE, 0x00, 0x00],
            UtfKind::Utf32Be => &[0x00, 0x00, 0xFE, 0xFF],
        }
    }

    pub fn from_name(name: &str) -> Option<UtfKind> {
        match name.to_ascii_uppercase().as_str() {
            "UTF-8" => Some(UtfKind::Utf8),
            "UTF-16LE" => Some(UtfKind::Utf16Le),
            "UTF-16BE" => Some(UtfKind::Utf16Be),
            "UTF-32LE" => Some(UtfKind::Utf32Le),
            "UTF-32BE" => Some(UtfKind::Utf32Be),
            _ => None,
        }
    }
}

/// BOM 检测结果。
pub struct Bom {
    pub kind: UtfKind,
    /// BOM 字节数
    pub len: usize,
}

/// 识别 BOM。注意 UTF-32LE 的 BOM 是 UTF-16LE BOM 的前缀，必须先判 32 位。
pub fn detect_bom(bytes: &[u8]) -> Option<Bom> {
    if bytes.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) {
        Some(Bom {
            kind: UtfKind::Utf32Le,
            len: 4,
        })
    } else if bytes.starts_with(&[0x00, 0x00, 0xFE, 0xFF]) {
        Some(Bom {
            kind: UtfKind::Utf32Be,
            len: 4,
        })
    } else if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        Some(Bom {
            kind: UtfKind::Utf8,
            len: 3,
        })
    } else if bytes.starts_with(&[0xFF, 0xFE]) {
        Some(Bom {
            kind: UtfKind::Utf16Le,
            len: 2,
        })
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        Some(Bom {
            kind: UtfKind::Utf16Be,
            len: 2,
        })
    } else {
        None
    }
}

/// UTF-32 手动解码。非法码点 → U+FFFD 并标记。
fn utf32_decode(bytes: &[u8], le: bool) -> (String, bool) {
    let mut out = String::with_capacity(bytes.len() / 4);
    let mut had_errors = false;
    let mut i = 0;
    while i + 4 <= bytes.len() {
        let unit = if le {
            u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]])
        } else {
            u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]])
        };
        match char::from_u32(unit) {
            Some(c) => out.push(c),
            None => {
                had_errors = true;
                out.push('\u{FFFD}');
            }
        }
        i += 4;
    }
    if i < bytes.len() {
        had_errors = true; // 残余不足一个码元
    }
    (out, had_errors)
}

fn utf32_encode(text: &str, le: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() * 4);
    for c in text.chars() {
        let unit = c as u32;
        let bytes = if le {
            unit.to_le_bytes()
        } else {
            unit.to_be_bytes()
        };
        out.extend_from_slice(&bytes);
    }
    out
}

/// UTF-16 手动解码（代理对组合；孤立代理 → U+FFFD 并标记）。
/// encoding_rs 的 UTF-16 编码行为受 WHATWG 兼容规则影响（encode 方向不可靠），故手写。
fn utf16_decode(bytes: &[u8], le: bool) -> (String, bool) {
    let mut units = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i + 2 <= bytes.len() {
        let unit = if le {
            u16::from_le_bytes([bytes[i], bytes[i + 1]])
        } else {
            u16::from_be_bytes([bytes[i], bytes[i + 1]])
        };
        units.push(unit);
        i += 2;
    }
    let mut out = String::with_capacity(units.len());
    let mut had_errors = !bytes.len().is_multiple_of(2);
    for r in char::decode_utf16(units) {
        match r {
            Ok(c) => out.push(c),
            Err(_) => {
                had_errors = true;
                out.push('\u{FFFD}');
            }
        }
    }
    (out, had_errors)
}

fn utf16_encode(text: &str, le: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() * 2 + 2);
    for unit in text.encode_utf16() {
        let bytes = if le {
            unit.to_le_bytes()
        } else {
            unit.to_be_bytes()
        };
        out.extend_from_slice(&bytes);
    }
    out
}

fn decode_utf_kind(bytes: &[u8], kind: UtfKind) -> (String, bool) {
    match kind {
        UtfKind::Utf8 => match std::str::from_utf8(bytes) {
            Ok(s) => (s.to_string(), false),
            Err(_) => (String::from_utf8_lossy(bytes).into_owned(), true),
        },
        UtfKind::Utf16Le => utf16_decode(bytes, true),
        UtfKind::Utf16Be => utf16_decode(bytes, false),
        UtfKind::Utf32Le => utf32_decode(bytes, true),
        UtfKind::Utf32Be => utf32_decode(bytes, false),
    }
}

/// 解码结果。
#[derive(Debug, Clone)]
pub struct DecodeResult {
    pub text: String,
    /// 编码名（encoding_rs 规范名或 UTF-32LE/UTF-32BE）
    pub encoding: String,
    pub had_bom: bool,
    /// 解码过程中是否发生错误替换（部分字节无法表示）
    pub had_errors: bool,
}

/// 自动检测并解码。
pub fn decode_auto(bytes: &[u8]) -> DecodeResult {
    if let Some(bom) = detect_bom(bytes) {
        let (text, had_errors) = decode_utf_kind(&bytes[bom.len..], bom.kind);
        return DecodeResult {
            text,
            encoding: bom.kind.name().to_string(),
            had_bom: true,
            had_errors,
        };
    }

    if std::str::from_utf8(bytes).is_ok() {
        return DecodeResult {
            text: String::from_utf8_lossy(bytes).into_owned(),
            encoding: "UTF-8".to_string(),
            had_bom: false,
            had_errors: false,
        };
    }

    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(bytes, true);
    // 走到这里说明不是合法 UTF-8，禁用检测器的 UTF-8 候选
    let enc = detector.guess(None, false);
    let (text, had_errors) = enc.decode_without_bom_handling(bytes);
    DecodeResult {
        text: text.into_owned(),
        encoding: enc.name().to_string(),
        had_bom: false,
        had_errors,
    }
}

/// 用指定编码解码（用户"以指定编码重新打开"，FR-2.2）。
pub fn decode_with(bytes: &[u8], encoding: &str) -> DecodeResult {
    if let Some(kind) = UtfKind::from_name(encoding) {
        let (mut text, had_errors) = decode_utf_kind(bytes, kind);
        let mut had_bom = false;
        if text.starts_with('\u{FEFF}') {
            text.replace_range(..'\u{FEFF}'.len_utf8(), "");
            had_bom = true;
        }
        return DecodeResult {
            text,
            encoding: kind.name().to_string(),
            had_bom,
            had_errors,
        };
    }

    let enc = Encoding::for_label(encoding.as_bytes()).unwrap_or(encoding_rs::UTF_8);
    let (text, had_errors) = enc.decode_without_bom_handling(bytes);
    let mut text = text.into_owned();
    let mut had_bom = false;
    if text.starts_with('\u{FEFF}') {
        text.replace_range(..'\u{FEFF}'.len_utf8(), "");
        had_bom = true;
    }
    DecodeResult {
        text,
        encoding: enc.name().to_string(),
        had_bom,
        had_errors,
    }
}

pub fn is_utf_family(encoding: &str) -> bool {
    UtfKind::from_name(encoding).is_some()
}

/// 编码结果：字节 + 有损点列表（FR-2.3：目标编码无法表示的字符逐项上报）。
/// 损失字符以"丢弃"处理——调用方应先用 [`unmappable_chars`] 弹确认。
#[derive(Debug, Clone, Default)]
pub struct EncodeResult {
    pub bytes: Vec<u8>,
    /// (字节偏移于原文, 被丢弃的字符)
    pub losses: Vec<(usize, char)>,
}

fn encode_utf_kind(text: &str, kind: UtfKind) -> Vec<u8> {
    match kind {
        UtfKind::Utf8 => text.as_bytes().to_vec(),
        UtfKind::Utf16Le => utf16_encode(text, true),
        UtfKind::Utf16Be => utf16_encode(text, false),
        UtfKind::Utf32Le => utf32_encode(text, true),
        UtfKind::Utf32Be => utf32_encode(text, false),
    }
}

/// 编码为字节序列。BOM 由调用方依据策略决定（FR-2.4）。
/// 有损编码中不可表示的字符被丢弃并记录在 `losses`。
pub fn encode_text(text: &str, encoding: &str, write_bom: bool) -> EncodeResult {
    if let Some(kind) = UtfKind::from_name(encoding) {
        let body = encode_utf_kind(text, kind);
        let mut bytes = Vec::with_capacity(body.len() + 4);
        if write_bom {
            bytes.extend_from_slice(kind.bom_bytes());
        }
        bytes.extend_from_slice(&body);
        return EncodeResult {
            bytes,
            losses: vec![],
        };
    }

    let Some(enc) = Encoding::for_label(encoding.as_bytes()) else {
        return encode_text(text, "UTF-8", write_bom);
    };
    let mut encoder = enc.new_encoder();
    let mut out = Vec::with_capacity(text.len());
    let mut losses = vec![];
    let mut consumed = 0usize; // 已消费字节数
    let mut src = text;
    loop {
        let mut dest = [0u8; 4096];
        let (result, read, written) =
            encoder.encode_from_utf8_without_replacement(src, &mut dest, false);
        out.extend_from_slice(&dest[..written]);
        match result {
            EncoderResult::InputEmpty => break,
            EncoderResult::OutputFull => {
                consumed += read;
                src = &src[read..];
                continue;
            }
            EncoderResult::Unmappable(c) => {
                // read 已包含该不可映射字符本身，其字节偏移 = consumed + read - len_utf8
                losses.push((consumed + read - c.len_utf8(), c));
                consumed += read;
                src = &src[read..];
            }
        }
    }
    // 冲刷
    let mut dest = [0u8; 4096];
    let _ = encoder.encode_from_utf8_without_replacement("", &mut dest, true);

    let mut bytes = Vec::with_capacity(out.len() + 4);
    if write_bom {
        // 有损编码无 BOM 概念；此处仅当名字映射到 UTF 家族时才写（防御式，通常不触发）
        if let Some(kind) = UtfKind::from_name(encoding) {
            bytes.extend_from_slice(kind.bom_bytes());
        }
    }
    bytes.extend_from_slice(&out);
    EncodeResult { bytes, losses }
}

/// 找出目标编码无法表示的字符（FR-2.3 确认对话框数据源）。
/// 返回 (字符在原文中的字节偏移, 字符)。
pub fn unmappable_chars(text: &str, encoding: &str) -> Vec<(usize, char)> {
    if UtfKind::from_name(encoding).is_some() {
        return vec![]; // UTF 家族全 Unicode 可表示
    }
    encode_text(text, encoding, false).losses
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 菜单全集必须可用：每个编码都能用 ASCII 往返（编码→字节→解码还原）。
    #[test]
    fn common_encodings_all_roundtrip() {
        for name in COMMON_ENCODINGS {
            let bytes = encode_text("abc", name, false).bytes;
            let r = decode_with(&bytes, name);
            assert_eq!(r.text, "abc", "{name} 往返失败");
            // encoding_rs 规范名可能与菜单标签大小写不同（如 gb18030）
            assert!(
                r.encoding.eq_ignore_ascii_case(name),
                "{name} 解码后报告为 {}",
                r.encoding
            );
            assert!(!r.had_errors);
        }
        // 多字节编码额外保证 CJK 无损；单字节编码丢弃 CJK 属预期（有损编码）
        for name in [
            "UTF-8",
            "UTF-16LE",
            "UTF-16BE",
            "UTF-32LE",
            "UTF-32BE",
            "GBK",
            "GB18030",
            "Big5",
            "Shift_JIS",
            "EUC-JP",
            "EUC-KR",
        ] {
            let bytes = encode_text("中文", name, false).bytes;
            assert_eq!(decode_with(&bytes, name).text, "中文", "{name} 丢失 CJK");
        }
    }

    #[test]
    fn bom_detection_all_variants() {
        assert_eq!(detect_bom(&[0xEF, 0xBB, 0xBF, b'a']).unwrap().len, 3);
        assert_eq!(detect_bom(&[0xFF, 0xFE, b'a', 0]).unwrap().len, 2);
        assert_eq!(detect_bom(&[0xFE, 0xFF, 0, b'a']).unwrap().len, 2);
        assert_eq!(detect_bom(&[0xFF, 0xFE, 0x00, 0x00]).unwrap().len, 4); // 32LE 优先
        assert_eq!(detect_bom(&[0x00, 0x00, 0xFE, 0xFF]).unwrap().len, 4);
        assert!(detect_bom(b"hello").is_none());
    }

    #[test]
    fn utf32_roundtrip() {
        let text = "中a\u{1F600}"; // BMP + 星形 plane
        let bytes = encode_text(text, "UTF-32LE", false).bytes;
        assert_eq!(bytes.len(), 4 * text.chars().count());
        // 无 BOM 的 UTF-32 无法自动检测（chardetng 不支持），需指定编码解码
        let r = decode_with(&bytes, "UTF-32LE");
        assert_eq!(r.text, text);
        assert_eq!(r.encoding, "UTF-32LE");
        assert!(!r.had_errors);

        let bytes = encode_text(text, "UTF-32BE", true).bytes;
        assert_eq!(&bytes[..4], &[0x00, 0x00, 0xFE, 0xFF]);
        let r = decode_auto(&bytes);
        assert_eq!(r.text, text);
        assert_eq!(r.encoding, "UTF-32BE");
    }

    #[test]
    fn utf16_roundtrip_all_planes() {
        let text = "中a\u{1F600}\u{FEFF}尾";
        for (name, _le) in [("UTF-16LE", true), ("UTF-16BE", false)] {
            let bytes = encode_text(text, name, false).bytes;
            assert_eq!(bytes.len(), 2 * text.encode_utf16().count());
            let r = decode_with(&bytes, name);
            assert_eq!(r.text, text);
            assert!(!r.had_errors);
        }
        // 孤立代理 → 替换字符并标记
        let (s, had) = utf16_decode(&[0x00, 0xD8, b'a', 0], true);
        assert!(had);
        assert_eq!(s, "\u{FFFD}a");
    }

    #[test]
    fn decode_gbk() {
        // "中文" 的 GBK 字节
        let bytes = [0xD6, 0xD0, 0xCE, 0xC4];
        let r = decode_with(&bytes, "GBK");
        assert_eq!(r.text, "中文");
        assert!(!r.had_errors);
    }

    #[test]
    fn decode_auto_utf8_with_and_without_bom() {
        let with_bom = [0xEF, 0xBB, 0xBF, b'a', b'b'];
        let r = decode_auto(&with_bom);
        assert_eq!(r.text, "ab");
        assert_eq!(r.encoding, "UTF-8");
        assert!(r.had_bom);

        let r = decode_auto("中文abc".as_bytes());
        assert_eq!(r.text, "中文abc");
        assert_eq!(r.encoding, "UTF-8");
        assert!(!r.had_bom);
    }

    #[test]
    fn decode_auto_guesses_non_utf8() {
        // 一段较长的 GBK 中文，UTF-8 不合法，应统计检出中文编码并正确解码
        let sample = "你好，世界。这是一段用于编码自动检测的中文文本，长度足够让统计检测生效。";
        let gbk_bytes = encode_text(sample, "GBK", false).bytes;
        let r = decode_auto(&gbk_bytes);
        assert_eq!(r.text, sample);
        assert!(
            r.encoding.eq_ignore_ascii_case("GBK") || r.encoding.eq_ignore_ascii_case("GB18030")
        );
    }

    #[test]
    fn encode_utf8_bom_policies() {
        let out = encode_text("中", "UTF-8", false);
        assert_eq!(out.bytes, "中".as_bytes());
        assert!(out.losses.is_empty());

        let out = encode_text("中", "UTF-8", true);
        assert_eq!(out.bytes, [0xEF, 0xBB, 0xBF, 0xE4, 0xB8, 0xAD]);
    }

    #[test]
    fn encode_utf16_with_bom() {
        let out = encode_text("a", "UTF-16LE", true);
        assert_eq!(out.bytes, [0xFF, 0xFE, b'a', 0x00]);
        // 解码回去
        let r = decode_auto(&out.bytes);
        assert_eq!(r.text, "a");
        assert_eq!(r.encoding, "UTF-16LE");
    }

    #[test]
    fn unmappable_and_lossy_encode() {
        // 🙂 (U+1F642) 不在 GBK 中（注意 GBK 可编码 ←↑→↓ 等常用箭头）
        let text = "中文🙂abc";
        let losses = unmappable_chars(text, "GBK");
        assert_eq!(losses.len(), 1);
        assert_eq!(losses[0].1, '🙂');
        assert_eq!(losses[0].0, 6); // 两个中文字 6 字节之后

        let out = encode_text(text, "GBK", false);
        assert_eq!(out.bytes, [0xD6, 0xD0, 0xCE, 0xC4, b'a', b'b', b'c']);
        assert_eq!(out.losses.len(), 1);
    }

    #[test]
    fn utf_family_probe() {
        assert!(is_utf_family("UTF-8"));
        assert!(is_utf_family("UTF-16LE"));
        assert!(is_utf_family("utf-32le"));
        assert!(!is_utf_family("GBK"));
        assert!(!is_utf_family("no-such-encoding"));
    }

    #[test]
    fn gbk_roundtrip_via_document_flow() {
        let original = "双字节编码往返测试 123";
        let bytes = encode_text(original, "GBK", false).bytes;
        let decoded = decode_auto(&bytes);
        assert_eq!(decoded.text, original);
        let reencoded = encode_text(&decoded.text, &decoded.encoding, decoded.had_bom);
        assert_eq!(reencoded.bytes, bytes);
    }
}
