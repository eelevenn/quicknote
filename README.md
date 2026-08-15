# QuickNote

QuickNote 是面向 Windows 11 的本地优先快速记录应用。生产实现采用 Rust、Slint 与 bundled SQLite；Windows 能力由窄 `PlatformServices` seam 隔离，共享应用模块不依赖 Win32。

## 工程结构

- `crates/quicknote-app`：平台中立的深应用模块，包含命令/查询接口、SQLite 单写者、schema 与迁移。
- `crates/quicknote-ui`：可由桌面与未来移动壳复用的 Slint 窗口骨架。
- `apps/quicknote-windows`：Windows adapter、单实例激活路由与生产入口。
- `benchmarks/windows-stack`：技术栈与 Windows 集成 spike 证据，不属于生产实现。

## 本地验证

```powershell
# 检查平台中立应用模块。
cargo check -p quicknote-app

# 运行应用模块与共享 UI 测试。
cargo test --workspace --exclude quicknote-windows

# 在 Windows x64 上生成生产 Release 构建。
.\scripts\build-windows.ps1
```

Windows 生产身份固定为产品名 `QuickNote`、AUMID `eelevenn.QuickNote`、协议 `quicknote`，数据位于 `%LOCALAPPDATA%\QuickNote\`。
