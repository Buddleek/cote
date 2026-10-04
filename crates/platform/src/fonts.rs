//! CJK 字体回退（FR-7.1）：egui 默认字体不含中日韩字形，
//! 打开中文文件会显示豆腐块。这里尽力加载各平台系统中文字体，
//! 追加到 Proportional 与 Monospace 两个 family（拉丁字符仍由默认字体渲染）。

use eframe::egui;

pub fn install_cjk_fallback(ctx: &egui::Context) {
    let candidates: &[&str] = if cfg!(target_os = "windows") {
        &[
            "C:\\Windows\\Fonts\\msyh.ttc",     // Microsoft YaHei
            "C:\\Windows\\Fonts\\simsun.ttc",   // SimSun
            "C:\\Windows\\Fonts\\simhei.ttf",   // SimHei
        ]
    } else if cfg!(target_os = "macos") {
        &[
            "/System/Library/Fonts/PingFang.ttc",
            "/System/Library/Fonts/STHeiti Light.ttc",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
        ]
    } else {
        &[
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJKsc-Regular.ttc",
            "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        ]
    };

    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            let mut defs = egui::FontDefinitions::default();
            // 已存在同名字体则不重复加载
            if defs.font_data.contains_key("cjk_fallback") {
                return;
            }
            defs.font_data.insert("cjk_fallback".to_owned(), egui::FontData::from_owned(bytes).into());
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                if let Some(list) = defs.families.get_mut(&family) {
                    list.push("cjk_fallback".to_owned());
                }
            }
            ctx.set_fonts(defs);
            return;
        }
    }
    // 找不到系统字体：保持默认（西文正常，CJK 显示豆腐块），不致命
}
