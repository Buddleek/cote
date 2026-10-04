//! 换行符识别与转换（FR-2.5）。

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineEnding {
    /// 平台默认（Windows 为 CRLF，其余 LF）
    #[default]
    Lf,
    Crlf,
    Cr,
}

impl LineEnding {
    pub fn as_str(&self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::Crlf => "\r\n",
            LineEnding::Cr => "\r",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            LineEnding::Lf => "LF (Unix)",
            LineEnding::Crlf => "CRLF (Windows)",
            LineEnding::Cr => "CR (Classic Mac)",
        }
    }

    /// 无 BOM/编码等线索时的平台默认值。
    pub fn platform_default() -> Self {
        if cfg!(windows) {
            LineEnding::Crlf
        } else {
            LineEnding::Lf
        }
    }
}

/// 三种换行符的计数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NewlineInfo {
    pub lf: usize,
    pub crlf: usize,
    pub cr: usize,
}

impl NewlineInfo {
    pub fn total(&self) -> usize {
        self.lf + self.crlf + self.cr
    }

    /// 是否混合换行（状态栏提示，FR-2.5）。
    pub fn mixed(&self) -> bool {
        [self.lf, self.crlf, self.cr].iter().filter(|&&c| c > 0).count() > 1
    }

    /// 数量最多的换行符；无换行时返回 None。
    pub fn dominant(&self) -> Option<LineEnding> {
        let m = self.lf.max(self.crlf).max(self.cr);
        if m == 0 {
            return None;
        }
        if self.crlf == m {
            Some(LineEnding::Crlf)
        } else if self.cr == m {
            Some(LineEnding::Cr)
        } else {
            Some(LineEnding::Lf)
        }
    }
}

/// 统计文本中的换行符分布。
pub fn analyze(text: &str) -> NewlineInfo {
    let mut info = NewlineInfo::default();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\r' if i + 1 < bytes.len() && bytes[i + 1] == b'\n' => {
                info.crlf += 1;
                i += 2;
            }
            b'\r' => {
                info.cr += 1;
                i += 1;
            }
            b'\n' => {
                info.lf += 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    info
}

/// 把所有换行符（含混合）统一转换为 target。
pub fn convert_all(text: &str, target: LineEnding) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str(target.as_str());
            }
            '\n' => out.push_str(target.as_str()),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyze_counts_and_mixed() {
        let info = analyze("a\r\nb\nc\rd\r\n");
        assert_eq!(info.crlf, 2);
        assert_eq!(info.lf, 1);
        assert_eq!(info.cr, 1);
        assert!(info.mixed());
        assert_eq!(info.dominant(), Some(LineEnding::Crlf));

        let info = analyze("no newline");
        assert_eq!(info.total(), 0);
        assert!(!info.mixed());
        assert_eq!(info.dominant(), None);
    }

    #[test]
    fn convert_all_forms() {
        assert_eq!(super::convert_all("a\r\nb\rc\nd", LineEnding::Lf), "a\nb\nc\nd");
        assert_eq!(super::convert_all("a\nb\nc\nd", LineEnding::Crlf), "a\r\nb\r\nc\r\nd");
        assert_eq!(super::convert_all("a\r\nb", LineEnding::Cr), "a\rb");
        assert_eq!(super::convert_all("", LineEnding::Lf), "");
    }
}
