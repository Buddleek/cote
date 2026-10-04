//! 构建脚本：Windows 下把图标等资源嵌入可执行文件。
//! （Explorer 文件图标 + winit 窗口/任务栏图标都来自这里的 RT_GROUP_ICON 资源）

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // 路径相对 crate 根（crates/cli）；图标集中放在 workspace 的 assets/
        println!("cargo:rerun-if-changed=../../assets/app.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/app.ico");
        res.set("FileDescription", "Cote — 轻量跨平台纯文本编辑器");
        res.set("ProductName", "Cote");
        res.compile().expect("failed to compile windows resources");
    }
}
