# QuickNote

QuickNote 是面向 Windows 11 的本地优先快速记录应用。生产实现采用 Rust、Slint 与 bundled SQLite；Windows 能力由窄 `PlatformServices` seam 隔离，共享应用模块不依赖 Win32。

## 工程结构

- `crates/quicknote-app`：平台中立的深应用模块，包含命令/查询接口、SQLite 单写者、schema 与迁移。
- `crates/quicknote-ui`：可由桌面与未来移动壳复用的 Slint 主页与快速记录窗口。
- `apps/quicknote-windows`：Windows adapter、全局快捷键、单实例激活路由与生产入口。
- `benchmarks/windows-stack`：技术栈与 Windows 集成 spike 证据，不属于生产实现。

当前纵向切片支持默认 `Ctrl+Alt+Q` 快捷呼出、当前便签与空白草稿、250 ms 尾随且最长 1 秒等待的真实自动保存，以及主页切换当前便签。首次非空保存会在一个事务中创建便签、正文和当前指针；关闭、切换和退出前刷新失败时，操作会被中止且正文保留在内存中。

## 本地验证

```powershell
# 检查平台中立应用模块。
cargo check -p quicknote-app

# 运行应用模块、共享 UI 与 Windows 壳测试。
cargo test --workspace

# 在 Windows x64 上生成生产 Release 构建。
.\scripts\build-windows.ps1
```

Windows 生产身份固定为产品名 `QuickNote`、AUMID `eelevenn.QuickNote`、协议 `quicknote`，数据位于 `%LOCALAPPDATA%\QuickNote\`。
