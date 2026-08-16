---
status: accepted
---

# 采用 Rust + Slint，并隔离平台能力

QuickNote 的桌面实现采用 Rust + Slint。领域核心、SQLite schema/迁移、自动保存状态机、激活命令和主要声明式 UI 保持平台中立；通知、全局快捷键、托盘、自启动、单实例和生命周期由窄 `PlatformServices` adapter 实现。Windows 壳不得反向渗入共享核心。

这个选择建立在完整 Windows spike 上：冷启动、热呼出、后台内存、安装器和应用本体均通过 #7 调整后的硬门槛。Spike 只保留为验证证据；其临时存储、界面和安装脚本不进入生产工程。

未来 Android 与 iOS 复用核心、数据模型和主要 UI；移动端不是当前交付物。Slint 采用 Royalty-free License，并在产品 About/归属区域保留可见说明；正式发布前再次复核许可条款。
