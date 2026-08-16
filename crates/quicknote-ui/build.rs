//! 编译平台中立的 Slint 窗口定义。

fn main() {
    // 构建时只编译平台中立的声明式窗口定义。
    slint_build::compile("ui/app.slint").expect("编译 QuickNote Slint UI");
}
