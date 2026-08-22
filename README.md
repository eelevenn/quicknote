# QuickNote

QuickNote 是面向 Windows 11 的本地优先快速记录应用。生产实现采用 Rust、Slint 与 bundled SQLite；Windows 能力由窄 `PlatformServices` seam 隔离，共享应用模块不依赖 Win32。

QuickNote 自有源代码采用 [MIT License](LICENSE)。发布和使用前请同时阅读[隐私说明](PRIVACY.md)、[第三方组件说明](THIRD_PARTY_NOTICES.md)与[代码签名政策](CODE_SIGNING_POLICY.md)。项目正在准备申请 SignPath Foundation 免费开源代码签名；获批前的任何构建都不代表已经由 SignPath Foundation 签名。

## 工程结构

- `crates/quicknote-app`：平台中立的深应用模块，包含命令/查询接口、SQLite 单写者、schema 与迁移。
- `crates/quicknote-ui`：可由桌面与未来移动壳复用的 Slint 主页与快速记录窗口。
- `apps/quicknote-windows`：Windows adapter、全局快捷键、单实例激活路由与生产入口。
- `benchmarks/windows-stack`：技术栈与 Windows 集成 spike 证据，不属于生产实现。

当前纵向切片支持默认 `Ctrl+Alt+Q` 快捷呼出、当前便签与空白草稿、250 ms 尾随且最长 1 秒等待的真实自动保存，以及宽窗口双栏/窄窗口单栏主页。首次非空保存会在一个事务中创建便签、正文和当前指针；关闭、切换、归档、导出和退出前刷新失败时，操作会被中止且正文保留在内存中。

主页实现严格的 `活跃 ⇄ 归档 → 回收站 → 永久清除` 生命周期、30 天回收站维护、活跃/归档完整正文子串搜索、安全轻量 Markdown 预览、版本化 JSON 与单张 Markdown 原子导出。设置包含全局快捷键、登录后启动、默认稍后提醒时长及 About/第三方许可证，并通过 Slint 系统调色板跟随 Windows 主题。

截止时间与单一提醒相互独立；未来截止时间在提醒为空时默认安排同刻通知。提醒事实、版本化动作与持久 outbox 在同一 SQLite 事务提交，Windows 计划通知仅在提交后投影。通知提供固化时长的“稍后提醒”和“打开”，支持退出后的通知历史、单实例冷/热激活、错过提醒、休眠恢复收敛、退避重试及 Explorer 计划丢失后的无重复重建。

v0.1.0 MVP 不包含语音输入、模型下载或音频网络调用；范围决定见 [ADR-0004](docs/adr/0004-mvp-excludes-voice-input.md)。

## 本地验证

```powershell
# 检查平台中立应用模块。
cargo check -p quicknote-app

# 运行应用模块、共享 UI 与 Windows 壳测试。
cargo test --workspace

# 显式运行 10,000 张 / 100 MiB 搜索规模验收。
cargo test -p quicknote-app --test search_scale -- --ignored --nocapture

# 在 Windows x64 上生成生产 Release 构建。
# 此命令构建 v0.1.0 的文字-only Windows 应用。
.\scripts\build-windows.ps1

# 构建并静态检查未签名的每用户 MSI；只用于开发和 CI。
.\scripts\Build-QuickNoteInstaller.ps1 -SkipApplicationBuild
.\scripts\Test-QuickNoteInstaller.ps1 `
  -InstallerPath .\artifacts\release-candidate\QuickNote-0.1.0-x64.msi `
  -AllowUnsigned
```

Windows 生产身份固定为产品名 `QuickNote`、AUMID `eelevenn.QuickNote`、协议 `quicknote`，数据位于 `%LOCALAPPDATA%\QuickNote\`。

正式候选必须先签所有 PE，再构建并签 MSI；入口为 `scripts/New-QuickNoteReleaseCandidate.ps1`。该命令只接受当前 workspace 版本和有效的当前用户代码签名证书，不提供跳过签名的发布模式。安装器路线、干净系统矩阵及仍需人工完成的门槛见 [Issue 22 发布候选门槛](docs/release-gates/issue-22-release-candidate.md)。
