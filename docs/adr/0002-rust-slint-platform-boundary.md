---
status: accepted
---

# 采用 Rust + Slint，并隔离平台能力

QuickNote 的桌面实现采用 Rust + Slint。领域核心、SQLite schema/迁移、自动保存状态机、激活命令和主要声明式 UI 保持平台中立；通知、全局快捷键、托盘、自启动、单实例和生命周期由窄 `PlatformServices` 适配层实现。Windows 壳不得反向渗入共享核心。未来 Android 与 iOS 复用核心、数据模型和主要 UI；两端都要兼容，但 iOS 可以晚于 Android，不要求当前提供 iOS CI 或产物。

这个选择建立在带 Windows 集成功能的同构 spike 上：20 次冷启动 P95 为 167.07 ms，50 次热呼出 P95 为 43.60 ms，10 次空闲私有工作集 P95 为 6.86 MiB；每用户安装器 4.77 MiB，安装后文件 10.65 MiB。应用退出后的计划通知进入 Windows 通知历史、安装后冷启动激活、第二实例路由、动作幂等、热键冲突恢复、恢复扫描、托盘重建消息、中文编辑与 Windows UI Automation 名称均通过。自动化没有让工作站实际睡眠或终止 Explorer，而是向生产处理路径发送对应的真实 Windows 消息；发布前仍需在干净 Windows 环境执行一次人工端到端验收。

WPF 与 WinUI 3 的 UI 仅面向 Windows，无法满足主要 UI 复用；当前 Tauri 原型的完整进程树超过放宽后的热呼出和空闲内存预算。Slint 需要维护较窄的 Windows 原生适配层，UI 组件生态也更小，但这些成本低于未来重写移动 UI 或持续超出常驻资源预算。

Slint 采用 Royalty-free License，并在产品 About/归属区域保留可见的 Slint 说明；发布前必须复核当时许可证文本。若后续 Android 主路径或低权重 iOS 编译路径出现框架级阻塞，重新打开技术栈决策，不自动回退到 Tauri。
