# Issue 22：QuickNote v0.1.0 MVP 发布候选门槛

本文是 [Issue 22](https://github.com/eelevenn/quicknote/issues/22) 的执行清单。当前实现已经进入发布候选阶段，但**没有正式签名、干净虚拟机和人工验收证据时不得发布，也不得关闭 Issue**。

安装器选型与一手资料见 [生产安装器调研](../research/issue-22-installer-selection.md)。结论固定为 WiX x64、per-user MSI，不使用 Burn、IExpress 或自研自解压 EXE。

## SignPath Foundation 申请预览例外

SignPath Foundation 要求申请项目已经公开发布需要签名的二进制形式。为取得免费开源签名资格，允许发布一个严格隔离的未签名 GitHub **Pre-release**，但它不是 MVP 发布候选，也不得推广给普通用户：

- 标签必须使用 `v<version>-signpath-preview.<n>`，并由 `.github/workflows/unsigned-signpath-preview.yml` 从干净提交构建。
- Release 标题和正文必须显著写明 `UNSIGNED`、`SignPath application preview` 和 `Do not install for normal use`。
- 只附加 MSI、来源清单与 SHA-256 清单；不得附加本地工作树构建物、PDB、模型或上游 runtime。
- Release 必须勾选 Pre-release，不得设置为 Latest，不得出现在 README 的普通下载入口。
- 预览 MSI 的 `NotSigned` 状态是预期证据；它不能用于关闭本 Issue，也不能替代任何签名、SmartScreen 或干净系统门槛。
- SignPath 审核完成后保留该 Release 作为申请证据；正式签名版本使用新的不可变版本和产物。

创建公开 Pre-release 是外部发布动作，必须在执行前取得仓库所有者明确确认。

## v0.1.0 范围

依据 [ADR-0004](../adr/0004-mvp-excludes-voice-input.md)，v0.1.0 不包含录音入口、转写预览、模型下载、推理 sidecar、云端设置、凭据或音频网络调用。Issue 21 的语音许可、性能和故障注入证据保留为历史记录，不再阻断本版本；未来恢复语音必须走新的决策和发布门槛。

## 固定发布契约

| 项目 | 固定值 |
| --- | --- |
| 支持系统 | Windows 11 25H2 或更新版本，x64 |
| 权限与范围 | 当前用户、标准用户可安装、不得触发 UAC |
| 安装目录 | `%LOCALAPPDATA%\Programs\QuickNote` |
| 用户数据 | `%LOCALAPPDATA%\QuickNote`，MSI 永不拥有，默认卸载保留 |
| AUMID | `eelevenn.QuickNote` |
| 协议 | `quicknote:` |
| Toast CLSID | `{7E5ACBFA-9501-4F8A-9C85-60C1AE5D17C4}` |
| MSI UpgradeCode | `{22D55004-7823-4192-A3D2-046F408953B1}` |
| 大小门槛 | 正式签名 MSI `<= 18 MiB`；安装目录应用本体 `<= 45 MiB` |
| 性能门槛 | 冷启动 P95 `<= 900 ms`；热快捷键 P95 `<= 90 ms`；稳定后台完整进程树 P95 `<= 60 MiB` |

每个公开版本必须更新 workspace 的三段版本号并生成新 ProductCode；UpgradeCode、组件 GUID、安装路径和 Shell 身份保持稳定。同一版本的字节不可变，只允许维修，不允许同版本热修复或降级。

## 当前开发机证据

2026-08-23 在 Windows build 26200、12 个逻辑处理器的当前开发机，对 ADR-0004 范围收缩后的产物完成以下自动验证：

- 静态 CRT Release 应用为 `12,773,888` 字节，SHA-256 为 `37ec9540ec1ba51c0bc568efec9559a0ac1f7764f00d43fe3db1a9bca1861f01`；`dumpbin /dependents` 未发现 `VCRUNTIME`、`MSVCP` 或 `CONCRT` 动态导入。
- unsigned MSI 为 `5,275,648` 字节，SHA-256 为 `280498ceb2b6680535a5f7a2eef06a7b0eda001d7194f5ba929eb2e029b81c99`，ProductCode `{D3B322C4-511A-5FE7-A5EE-C653F511F6DC}`；per-user、稳定身份和 18 MiB 静态门禁通过。
- 20 次冷启动 P95 为 `403.113 ms`，50 次真实全局快捷键热启动 P95 为 `85.547 ms`，10 次稳定后台完整进程树私有工作集 P95 为 `3,022,848` 字节。
- Release profile 的 10,000 张、100 MiB 英文正文子串搜索运行 20 次，P95 为 `1.426 ms`；英文大小写、中文、文字 `%`/`_`/反斜杠/引号、活跃与归档范围、排除回收站和无截断合同均通过。优化与原始样本由 [Issue 33](https://github.com/eelevenn/quicknote/issues/33) 跟踪。首页、切换、保存和导出仍需目标机端到端证据。

这些数字只证明实现和测量链路可运行。MSI 尚未正式签名，开发机也不是规定的 4 逻辑核心、8 GiB 干净系统，因此发布矩阵仍保持未通过。

## 自动化入口

开发和 CI 可生成未签名 MSI，但它不是发布候选：

```powershell
# 构建静态 CRT 的 Release 应用和未签名 MSI。
.\scripts\build-windows.ps1
.\scripts\Test-WindowsRuntimeDependencies.ps1 `
  -Path '.\target\x86_64-pc-windows-msvc\release\quicknote-windows.exe'
.\scripts\Build-QuickNoteInstaller.ps1 -SkipApplicationBuild
.\scripts\Test-QuickNoteInstaller.ps1 `
  -InstallerPath .\artifacts\release-candidate\QuickNote-0.1.0-x64.msi `
  -AllowUnsigned
```

正式候选只有一个入口，顺序固定为构建、签 EXE、检查动态依赖、构建 MSI、签 MSI、验证签名与 MSI 表：

```powershell
.\scripts\New-QuickNoteReleaseCandidate.ps1 `
  -Version 0.1.0 `
  -CertificateThumbprint '<正式代码签名证书 SHA-1 thumbprint>'
```

签名脚本使用 SHA-256 文件摘要和 RFC 3161 SHA-256 时间戳，并对每个产物执行 SignTool `/pa /all /v`。v0.1.0 不生成或分发语音模型、runtime 或 sidecar。

## 干净系统矩阵

在每个干净 Windows 11 25H2 x64 标准用户快照中先生成环境和空白矩阵：

```powershell
.\scripts\New-QuickNoteReleaseEvidence.ps1 `
  -Version 0.1.0 `
  -ArtifactPath .\artifacts\release-candidate\QuickNote-0.1.0-x64.msi
```

然后运行安装生命周期。必须使用不会触碰真实用户数据的临时账户；脚本发现 `%LOCALAPPDATA%\QuickNote` 已存在时会拒绝继续。

```powershell
.\scripts\Test-QuickNoteInstaller.ps1 `
  -PreviousInstallerPath '<已正式签名的 N 版本 MSI>' `
  -InstallerPath .\artifacts\release-candidate\QuickNote-0.1.0-x64.msi `
  -InstallLifecycle
```

以下任一场景缺少 MSI verbose log、前后文件/注册表快照和明确结果时，状态保持 `not-run` 或 `failed`：

| 场景 | 必须证明 |
| --- | --- |
| 首装、重复安装、修复 | 无 UAC；固定路径；快捷方式、协议、AUMID、CLSID 和 ARP 唯一；损坏资源可恢复 |
| N → N+1 覆盖升级 | 数据、快捷键、自启动、提醒和稳定身份保留；只剩 N+1 登记 |
| 失败与突然中断 | 在复制、移除旧版和提交阶段分别注入磁盘满、访问拒绝、强杀与断电；旧版可恢复启动 |
| 迁移失败 | MSI 不碰数据库；首次启动迁移失败时 schema 回滚、诊断备份保留并拒绝危险写入 |
| 显式卸载 | 安装文件、Shell 注册、Run 投影、计划通知与通知历史清理；数据库、备份和导出保留 |
| 卸载后重装 | 使用保留数据库恢复，提醒重新投影，无重复身份或快捷方式 |
| 签名与 SmartScreen | 下载 MSI 和安装后 EXE 的正式发布者、时间戳与 `/pa /all /v` 结果一致；记录 SmartScreen 实际提示 |

## 性能、规模与可访问性

性能脚本会保留每次冷/热样本和每次完整后代进程树内存样本；热启动必须由真实 `Ctrl+Alt+Q` 全局快捷键触发：

```powershell
.\scripts\Test-WindowsShell.ps1 `
  -ExecutablePath .\target\x86_64-pc-windows-msvc\release\quicknote-windows.exe `
  -ColdRuns 20 -HotRuns 50 -MemorySamples 10 -IdleSeconds 30

cargo test --locked --release -p quicknote-app --test search_scale -- --ignored --nocapture
```

上述命令不能替代以下人工工作：

- 在 4 个可用逻辑核心、8 GiB、CPU-only 目标机上记录系统、硬件、活动电源计划和后台负载。
- 使用 10,000 张、正文合计至少 100 MiB 的真实 SQLite 数据，覆盖首页、搜索、切换、保存和导出，不只运行合成搜索测试。
- 仅用键盘完整操作快速记录、主页、搜索、生命周期、提醒、设置和导出；使用 Narrator 核对名称、角色、状态和焦点。
- 在浅色、深色、高对比度以及 100%、150%、200% 缩放下核对布局、焦点可见性和状态不只依赖颜色。

## 发布前人工阻断项

- [ ] 使用正式 OV/EV 或 Microsoft Artifact Signing 身份生成最终 MSI；未签名或测试证书不合格。
- [ ] 在无 VC Redist 假设的干净系统运行；PE 导入表门禁通过且应用可启动。
- [ ] 完成首次安装、维修、覆盖升级、失败回滚、断电恢复、卸载和重装矩阵。
- [ ] 完成冷/热/内存硬预算，以及 10,000 张、100 MiB 数据集的端到端性能验收。
- [ ] 完成键盘、Narrator、高对比度、浅/深主题和三档缩放验收。
- [ ] 人工复核 Rust、Slint、SQLite、WiX 及全部传递依赖的许可证和 notices。
- [ ] 核对正式签名后的 MSI 与安装目录精确字节数；保存 SHA-256、签名验证和发布清单。

## 发布结论

只有所有自动与人工矩阵项均为 `passed`、每项关联原始证据且不存在开放硬门槛时，Issue 22 才可转为完成。任何失败都应记录独立 Issue 并保留原始证据，不能通过移除样本或改用未验证安装器绕过门槛。
