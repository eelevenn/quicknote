# Issue 22：生产安装器选型——WiX MSI 与签名 EXE

日期：2026-08-17
状态：选型已落地并完成 unsigned 结构验证，尚未完成正式签名与干净系统验收
目标平台：Windows 11 25H2 或更新版本，x64
对应 Issue：[验证生产安装器、签名与 QuickNote MVP 发布候选](https://github.com/eelevenn/quicknote/issues/22)

## 结论

**选择 WiX 生成的 x64、固定每用户范围 MSI；不使用 Burn 外层 EXE，也不把 spike 的 IExpress 或自研自解压 EXE 提升为生产安装器。** MSI 仍然安装的是未打包的 Win32 `QuickNote.exe`，不会给应用引入 MSIX 包身份。

理由不是 MSI 能做到而 EXE 做不到，而是 Issue 22 最难的门槛——同版本修复、覆盖升级、失败回滚、卸载登记和确定日志——已经由 Windows Installer 定义并实现。自研 EXE 要达到相同结果，必须自行实现并长期维护事务日志、旧版本备份、注册表/快捷方式补偿、恢复入口和卸载器；代码签名只能证明文件的发布者与完整性，并不会赋予安装事务语义。[Windows Installer 是 Windows 自带的安装与配置服务](https://learn.microsoft.com/en-us/windows/win32/msi/windows-installer-portal)，失败时默认生成并执行回滚脚本、保存被删除文件的副本并恢复原状态。[回滚说明](https://learn.microsoft.com/en-us/windows/win32/msi/rollback-installation)

这个选择仍有两个硬性条件：

1. 最终结论必须由**实际正式签名后的 MSI**证明安装器不超过 `18 MiB`，不能用未签名或仅估算的大小代替。
2. 必须按本文末尾矩阵在干净 Windows 11 25H2 x64 标准用户账户上完成故障注入。若 WiX MSI 未通过任一硬门槛，应记录独立 Issue 后重新比较，不能直接改用未经同等测试的 EXE。

## 比较范围

本文中的“签名 EXE”指：不依赖 MSI/Burn/其他安装引擎、由 QuickNote 自己维护安装状态的已签名 Win32 安装器。若候选实际是另一个成熟安装引擎生成的 EXE，应按那个引擎的一手文档重新评估，不能把其能力归因于 EXE 文件格式本身。

两种方案都安装相同的 Rust + Slint x64 产物到相同路径，并注册相同的产品身份。当前代码已经固定：

- 产品名 `QuickNote`、AUMID `eelevenn.QuickNote`、协议 `quicknote:`，见 [`PRODUCT_IDENTITY`](../../crates/quicknote-app/src/platform.rs)。
- 应用数据目录为 `%LOCALAPPDATA%\QuickNote`，与安装目录分离；通知 AUMID、COM 激活、协议、开始菜单快捷方式和登录启动均使用当前用户注册表或用户目录，见 [`WindowsPlatformServices`](../../apps/quicknote-windows/src/windows_platform.rs)。
- 快捷键和登录启动偏好保存在 SQLite；数据库迁移前创建一致备份，迁移失败回滚 schema 并保留备份，见 [ADR 0003](../adr/0003-sqlite-storage-and-versioned-exports.md)。
- [Issue 21 发布门槛](../release-gates/issue-21-local-transcription.md)记录的旧 Release 应用为 `13,033,984` 字节、转写 sidecar 为 `289,792` 字节。静态 CRT 与卸载清理落地后的当前应用为 `13,232,128` 字节，sidecar 仍为 `289,792` 字节；主 MSI 只包含应用本体，sidecar 与约 `177.3 MiB` 的本地转写包继续按需单独分发。

## 逐项比较

| 门槛 | WiX 每用户 MSI | 签名的自研 EXE | 判断 |
| --- | --- | --- | --- |
| 每用户、无提权 | `Package Scope="perUser"` 声明每用户安装并使用 limited privileges；Windows Installer 将目录、快捷方式和注册重定向到当前用户。[WiX scope](https://docs.firegiant.com/wix/schema/wxs/packagescopetype/)、[安装上下文](https://learn.microsoft.com/en-us/windows/win32/msi/installation-context) | 可只写用户可写目录和 HKCU 而不提权，但必须自行审计每个操作和依赖。 | MSI 有声明式约束和系统语义，胜。 |
| 稳定安装路径 | 在 x64 每用户 MSI 中，`ProgramFiles64Folder` 映射到 `%LOCALAPPDATA%\Programs`；固定子目录 `QuickNote`，版本号不得进入路径。[目录映射](https://learn.microsoft.com/en-us/windows/win32/msi/installation-context#folder-redirection) | 可选择相同路径；需自行防止路径漂移、并发安装和部分覆盖。 | 都可满足，MSI 状态更可审计。 |
| 开始菜单、AUMID、协议 | MSI 原生创建当前用户开始菜单快捷方式；`MsiShortcutProperty`/WiX `ShortcutProperty` 可写 AUMID 和 Toast activator CLSID。[AUMID 要求](https://learn.microsoft.com/en-us/windows/win32/shell/appids)、[WiX ShortcutProperty](https://docs.firegiant.com/wix/schema/wxs/shortcutproperty/)、[Microsoft 的 WiX 通知示例](https://learn.microsoft.com/zh-cn/windows/apps/design/shell/tiles-and-notifications/send-local-toast-desktop-cpp-wrl)；协议和 COM 键可由 WiX 写入 HKCU/HKCU\Software\Classes。[注册表重定向](https://learn.microsoft.com/en-us/windows/win32/msi/installation-context#registry-redirection) | 可以调用 Shell/注册表 API 创建完全相同资源，但创建、修复、升级和卸载的对称性全部由 QuickNote 负责。 | MSI 胜。 |
| 卸载入口 | Windows Installer 根据包属性自动登记当前用户的“添加/删除程序”信息，并在完整卸载时移除登记；仍保留命令行修复和卸载能力。[ARP 配置](https://learn.microsoft.com/en-us/windows/win32/msi/configuring-add-remove-programs-with-windows-installer) | 必须自行创建 HKCU 卸载元数据、持久化卸载器，并保证升级后 `UninstallString` 始终有效。 | MSI 胜。 |
| 同版本修复 | Windows Installer 原生支持 `/f` 修复，`REINSTALLMODE` 可重写文件、HKCU 注册和快捷方式。[命令行选项](https://learn.microsoft.com/en-us/windows/win32/msi/command-line-options)、[REINSTALLMODE](https://learn.microsoft.com/en-us/windows/win32/msi/reinstallmode) | 必须自行定义“同版本”、检查完整性并恢复每一种资源。 | MSI 明显胜。 |
| 覆盖升级 | 固定 `UpgradeCode`、每个公开版本新 `ProductCode`，由 Upgrade 表检测旧版本并执行 major upgrade。[Major upgrade](https://learn.microsoft.com/en-us/windows/win32/msi/major-upgrades) | 必须自行发现旧版本、互斥运行实例、替换二进制并迁移安装元数据。 | MSI 胜。 |
| 升级失败/中断 | Windows Installer 提供回滚，但 WiX 排程必须正确；详见下节。 | 必须自行实现 staging、旧版备份、跨文件/注册表补偿、崩溃恢复和幂等重入；单个文件替换不是完整安装事务。 | MSI 明显胜。 |
| 用户数据默认保留 | 只要 MSI 不声明拥有 `%LOCALAPPDATA%\QuickNote`，标准卸载只删除安装资源；文件夹删除动作也只删除空目录。[RemoveFiles/RemoveFolders](https://learn.microsoft.com/en-us/windows/win32/msi/standard-actions-reference)、[RemoveFolders](https://learn.microsoft.com/en-us/windows/win32/msi/removefolders-action) | 可保留，但删除范围完全取决于自研代码；路径错误的破坏面更大。 | 都可满足，MSI 更容易证明所有权边界。 |
| 快捷键/自启动/通知身份/提醒 | 固定路径和身份；用户偏好不归 MSI 所有，结构性 Shell 注册归 MSI 所有。升级只替换安装资源，不删除 SQLite 或应用管理的 HKCU Run 值。 | 可以保持，但每个升级分支都要显式保护这些状态。 | MSI 胜，前提是按下文划分所有权。 |
| 正式签名 | WiX 官方支持直接用 SignTool 签 MSI；内嵌 cabinet 由 MSI 签名覆盖。[WiX 签名](https://docs.firegiant.com/wix/tools/signing/) | SignTool 可签 EXE。 | 能力相同；两者都必须使用正式身份并实际验证。 |
| 安装器体积 | MSI 表和嵌入 cabinet 有少量开销，但可选择高压缩；不带 Burn 时无 bootstrapper 负担。[WiX 压缩选项](https://docs.firegiant.com/wix/tools/msbuild/) | 自定义 stub 可很小，也可使用更激进压缩；同时必须携带事务与卸载逻辑，最终大小只能实测。 | EXE 有潜在体积优势，但当前主 MSI 的原始应用载荷约 `12.62 MiB`，仍有充分空间，尚不足以推翻可靠性优势。 |
| 目标机运行时 | Windows Installer 5.0 自 Windows 7 起随 OS 提供，Windows 11 无需另装；WiX 的 .NET SDK/工具只用于构建。[Windows Installer runtime](https://learn.microsoft.com/en-us/windows/win32/msi/windows-installer-portal)、[WiX 构建工具要求](https://docs.firegiant.com/wix/using-wix/) | 可以构建为自包含原生 EXE，但所有安装引擎代码由产物携带；QuickNote 应用本身的系统依赖不因安装器格式改变。 | MSI 格式无额外目标机运行时；当前静态 CRT 构建已消除动态 VC Runtime 导入，仍须在干净系统启动复核。 |

## 推荐的 MSI 契约

### 1. 每用户范围与路径

- 构建 x64 MSI，范围固定为 `perUser`，不提供“为所有用户安装”选项，所有首次安装、修复、升级和卸载均在同一当前用户上下文完成。WiX 官方定义 `perUser` 为无需提升权限；Microsoft 也说明每用户应用的更新和修复继续在每用户上下文中发生。[WiX scope](https://docs.firegiant.com/wix/schema/wxs/packagescopetype/)、[`MSIINSTALLPERUSER`](https://learn.microsoft.com/en-us/windows/win32/msi/msiinstallperuser)
- 安装路径固定为 `%LOCALAPPDATA%\Programs\QuickNote`，不可选择版本化子目录，也不允许 UI 改到 Program Files、Windows 或其他需要提权的位置。
- 不添加 `requestedExecutionLevel=requireAdministrator` 的 bootstrapper，不使用 Burn。QuickNote 没有需要串联安装的外部前置包；Windows Installer 已随目标 OS 提供。

### 2. Shell 与通知身份的所有权

MSI 应拥有并在修复时重建以下**结构性资源**：

- 当前用户开始菜单 `QuickNote.lnk`；
- 快捷方式属性 `System.AppUserModel.ID=eelevenn.QuickNote` 和当前固定 Toast activator CLSID；
- HKCU 下的 AUMID、COM `LocalServer32` 与 `quicknote:` 协议注册；
- 当前用户的卸载入口。

应用仍应在进程创建 UI 前设置同一个显式 AUMID。Microsoft 要求显式 AUMID 在进程、窗口、快捷方式和关联中保持一致，并建议不需要并存版本时从 AUMID 中省略版本字段，以便升级沿用同一身份。[AUMID 指南](https://learn.microsoft.com/en-us/windows/win32/shell/appids) 未打包应用的计划通知依赖稳定 AUMID 和激活注册；Microsoft 的非 C#/C++ 指南也要求用同一 AUMID 注册应用并发送通知。[未打包应用通知](https://learn.microsoft.com/zh-cn/windows/apps/develop/notifications/app-notifications/send-local-toast-other-apps)

组件标识符必须进入版本控制并跨版本稳定，不能每次 harvest 随机生成。AUMID、协议、CLSID、`UpgradeCode`、安装目录名和快捷方式身份均为发布契约，不得由版本号或构建路径派生。

### 3. 修复与版本策略

- 一个发布版本只有一组不可变字节；不得在相同版本号下静默替换公开 MSI。
- 用户对同一 MSI 执行维护/修复时，Windows Installer 恢复安装文件、HKCU 结构性注册和快捷方式。强制验收可使用 `msiexec /faumsv <msi> /l*v <log>`；`a` 强制重装文件，`u/m/s` 重写注册和快捷方式，`v` 从给定 MSI 重新缓存。各标志语义见 [REINSTALLMODE](https://learn.microsoft.com/en-us/windows/win32/msi/reinstallmode)。
- 每个公开升级都递增 MSI `ProductVersion` 的前三段之一并使用新 `ProductCode`；`UpgradeCode` 永久稳定。Windows Installer 比较版本时只使用前三段，因此不能把可排序 build 号只放在第四段。[ProductVersion](https://learn.microsoft.com/en-us/windows/win32/msi/productversion)
- 不启用 `AllowSameVersionUpgrades`。WiX 明确警告 MSI 忽略第四段；允许“同版本升级”也可能把同前三段的较旧 build 当成升级，重新引入已修复问题。[WiX MajorUpgrade](https://docs.firegiant.com/wix/schema/wxs/majorupgrade/) 同版本行为只定义为对同一不可变产品执行 repair，不定义为发布热修复。
- 默认阻止降级，避免只前向 SQLite schema 被旧客户端打开。

### 4. 覆盖升级与安装回滚

WiX `MajorUpgrade` **不得使用默认 `afterInstallValidate` 排程**。WiX 官方说明默认排程先完整删除旧产品，若新安装失败，机器上可能两个版本都没有。[WiX MajorUpgrade schedule](https://docs.firegiant.com/wix/schema/wxs/majorupgrade/#schedule)

QuickNote 应先采用 `afterInstallInitialize`：旧产品的移除发生在安装事务开始之后；新版本安装失败时，Windows Installer 会回滚旧产品移除并重新安装旧版。与 `afterInstallExecute` 相比，这个选择对组件重组更宽容，不依赖同等严格的组件引用计数规则；代价是升级时会先移除旧版文件再安装新版，因此必须完成断电和强杀故障注入，不能只验证普通错误返回。

安装包应尽量只使用 MSI 标准表和标准动作，不在安装事务中启动 QuickNote、执行 SQLite 迁移或使用未配套 rollback action 的自定义动作。Microsoft 明确指出，自定义动作若要支持回滚需要额外创作。[Rollback custom actions](https://learn.microsoft.com/en-us/windows/win32/msi/rollback-installation)

### 5. 用户数据、偏好与提醒

安装器与应用状态的所有权必须分开：

| 资源 | 所有者 | 升级 | 显式卸载 |
| --- | --- | --- | --- |
| `%LOCALAPPDATA%\Programs\QuickNote` 中已发布二进制与 notices | MSI | 事务替换 | 删除 |
| `%LOCALAPPDATA%\QuickNote` 中 SQLite、备份、诊断、转写包 | 应用/用户 | 原样保留 | 默认保留 |
| 用户自行选择的导出文件 | 用户 | 不触碰 | 不触碰 |
| AUMID、协议、COM 激活与开始菜单快捷方式 | MSI | 以相同身份重建 | 删除 |
| SQLite 中的快捷键与登录启动偏好 | 应用/用户 | 原样保留 | 默认保留 |
| HKCU Run 中的 `QuickNote --startup` | 应用根据用户偏好管理 | 固定路径下原样保留 | 删除启动投影，但不改 SQLite 偏好 |
| Windows 中已计划的 QuickNote 通知 | 应用 | 不清空，以稳定 AUMID/协议继续工作 | 应清空系统投影，数据库事实仍保留 |

MSI 不得为 `%LOCALAPPDATA%\QuickNote` 创作 `File`、通配 `RemoveFile`、递归清理或卸载复选框。删除用户数据必须是应用内独立操作，显示确切范围并二次确认；不能塞进默认卸载流程。

数据库迁移不属于安装事务。安装成功后的首次应用启动才执行现有 Online Backup + SQLite 事务迁移；迁移失败由应用回滚 schema、保留诊断备份并拒绝继续写入。这样，MSI 升级失败时数据库尚未被新客户端触碰；MSI 成功而数据库迁移失败时，安装器状态与数据恢复职责也不会混在一个不可证明的自定义动作里。

显式卸载需要在干净系统上确认不会遗留可触发的计划通知，同时不能借此删除数据库。若标准 MSI 注册删除不足以停止计划通知，应增加一个最小、签名、仅在真正卸载而非 major upgrade 时运行的清理入口，并为其补齐失败与回滚测试；在获得证据前，这仍是 Issue 22 的未通过项。

## 正式签名方案

MSI 与 EXE 在签名能力上没有决定性差异。生产要求应统一为：

1. 使用经身份验证、链到受信任根的正式 Authenticode 代码签名身份：合资格时可用 Microsoft Artifact Signing，否则使用受信 CA 的 OV/EV 代码签名证书；自签名证书不合格。Microsoft 当前说明有效 OV/EV 会显示已验证发布者，但新文件或新发布者即使已签名仍可能在 SmartScreen 积累信誉前显示“无法识别”警告；EV 已不再自动绕过该警告。[SmartScreen 信誉](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation)
2. 先签 `QuickNote.exe`、sidecar 和所有随安装交付的可执行文件，再生成并签最终 MSI。WiX 官方说明：cab 内嵌时只需签 MSI 即可覆盖 cabinet 完整性，但这不替代对安装后独立 EXE 的签名。[WiX 签名流程](https://docs.firegiant.com/wix/tools/signing/)
3. 使用 SHA-256 文件摘要与 RFC 3161 SHA-256 时间戳，即 SignTool 的 `/fd SHA256 /tr <timestamp-url> /td SHA256`。Microsoft 说明时间戳用于在签名证书过期后维持长期有效性，并不应再用 SHA-1 作为新发布的唯一算法。[Authenticode 时间戳](https://learn.microsoft.com/en-us/windows/win32/seccrypto/time-stamping-authenticode-signatures)
4. 发布门禁对每个 EXE 和 MSI 执行 `signtool verify /pa /all /v <file>`，失败或警告均阻止发布；`/pa` 使用默认 Authenticode 验证策略。[SignTool](https://learn.microsoft.com/en-us/windows/win32/seccrypto/signtool) 同时在文件属性、SmartScreen 提示和“已安装的应用”中人工核对预期发布者文本。
5. 所有版本使用同一验证后的发布者身份，保护私钥或仅授予 CI 最小签名权限；签名后不再修改产物。若使用 Artifact Signing，官方说明它可签 SignTool 支持的文件，并给出 `/pa` 验证方式。[Artifact Signing FAQ](https://learn.microsoft.com/en-us/azure/artifact-signing/faq)

## 体积与运行时判断

当前主 MSI 只包含约 `12.62 MiB` 的应用 EXE，sidecar 和模型/runtime 下载包明确排除在外。纯 MSI 只增加安装数据库、嵌入 cabinet 和签名；WiX 支持 `high` 等 cabinet 压缩等级。[WiX MSBuild 属性](https://docs.firegiant.com/wix/tools/msbuild/) 当前 unsigned MSI 实测为 `5,431,296` 字节，但最终门槛仍必须由正式签名后的 MSI 证明。

应同时记录三组互不混淆的数字：

- 正式签名 MSI 的精确字节数及 MiB；
- 安装目录中应用本体、sidecar、notices 等的精确总字节数；
- Windows 11 自带的共享系统运行时清单，不把它们计入安装目录，也不宣称为 QuickNote 自带。

不要加 Burn EXE：当前没有需要链式安装的前置包，Burn 会增加新的可执行层、缓存、签名步骤和故障面。WiX 文档也说明 Burn bundle 需要分别签提取出的 engine 和最终 bundle。[Burn 签名](https://docs.firegiant.com/wix/tools/signing/#signing-bundles)

WiX 的 .NET SDK/命令行工具是**构建依赖**，不是用户机器依赖；目标机使用 Windows 自带的 Windows Installer 5.0。[WiX 使用说明](https://docs.firegiant.com/wix/using-wix/)、[Windows Installer runtime](https://learn.microsoft.com/en-us/windows/win32/msi/windows-installer-portal) 自研 Rust EXE 可以同样无额外共享运行时，但它携带的解压、事务、日志和卸载逻辑仍算安装器自身内容，最终体积必须用同一签名后口径测量。

初次检查曾发现 `quicknote-windows.exe` 导入 `VCRUNTIME140.dll`，因此不能把安装引擎无额外 runtime 误写成应用无前置依赖。Microsoft 说明使用 Visual Studio runtime DLL 的应用必须确保用户机器具有相应 DLL。[需要再分发的 DLL](https://learn.microsoft.com/en-us/cpp/windows/determining-which-dlls-to-redistribute?view=msvc-170)

中央部署 `vc_redist.x64.exe` 写入系统目录并要求管理员权限，与“每用户无提权”冲突；官方允许 app-local 或静态链接，但 QuickNote 将承担安全更新服务责任，Microsoft 也建议优先动态链接。[部署方式比较](https://learn.microsoft.com/en-us/cpp/windows/choosing-a-deployment-method?view=msvc-170)

最小实验现已落地：Rust + Slint 使用 `crt-static`，C++ sidecar 显式使用 `/MT`；自动化 `dumpbin /dependents` 门禁确认两者均不导入 `VCRUNTIME`、`MSVCP` 或 `CONCRT` DLL。20 次冷启动、50 次热快捷键和 10 次后台内存样本在当前开发机通过硬预算，但最终仍须在无 VC Redist 假设的干净 VM 上复核。**不得链式运行需提权的 VC Redist，也不得把开发机结果冒充干净系统结果。**

另有一项非技术门槛：WiX 当前官方仓库说明，产生收入的使用需要 Open Source Maintenance Fee。QuickNote 在采用前应由人工确认适用性并留存结论。[WiX 官方 README](https://github.com/wixtoolset/wix/blob/main/README.md#open-source-maintenance-fee)

## Issue 22 安装器验收矩阵

以下证据全部基于正式 Release x64 和候选签名身份；每一项保存 MSI verbose log、进程退出码、安装前后文件/注册表清单和复现步骤。

| 场景 | 操作 | 必须结果 |
| --- | --- | --- |
| 首次安装 | 标准用户运行签名 MSI | 无 UAC；固定路径；只有当前用户可见的开始菜单和卸载入口；AUMID、协议、COM 注册正确；签名发布者正确。 |
| 同版本修复 | 分别删除/损坏 EXE、开始菜单快捷方式、协议键后执行强制 repair | 所有 MSI-owned 资源恢复；SQLite、导出文件、快捷键偏好和启动偏好不重置；修复后仅一个产品登记。 |
| 覆盖升级 | 安装 N，创建真实便签、提醒、快捷键和开机启动状态，再安装 N+1 | 安装路径不变；只有 N+1 登记；快捷键可重新注册；Run 值仍指向固定路径；AUMID/协议/通知激活不变；待处理提醒仍存在。 |
| 升级失败 | 在复制、移除旧产品和提交阶段分别注入磁盘满、访问拒绝、进程强杀 | 安装返回失败；旧版本签名文件、快捷方式、协议与卸载入口恢复并可启动；数据库未被安装器改写；日志能定位阶段。 |
| 突然中断 | VM 快照上在上述阶段断电并重启 | Windows Installer 完成恢复后旧版本可运行，或给出可自动/明确执行的恢复路径；不得处于无版本可用状态。 |
| 迁移失败 | MSI 成功后在首次启动注入 SQLite migration 失败 | 旧 schema 回滚，诊断备份保留，应用拒绝危险写入；这项由应用证据而非 MSI rollback 冒充通过。 |
| 显式卸载 | 从 Windows“已安装的应用”卸载 | 安装文件、开始菜单、协议、COM、AUMID 投影、Run 投影、卸载登记和系统计划通知清理；`%LOCALAPPDATA%\QuickNote` 数据库/备份/转写包及用户导出保留。 |
| 卸载后重装 | 完成上一项后重装同版或新版 | 应用重新使用保留数据库并重新投影提醒；没有重复快捷方式、协议键、卸载登记或通知身份。 |
| 签名 | 对下载产物和安装后所有 PE/MSI 执行 SignTool 验证 | `/pa /all /v` 成功；发布者完全匹配批准名称；带 RFC 3161 时间戳；SmartScreen 实际表现单独记录，不把“已签名”等同于“必无警告”。 |
| 大小/依赖 | 对最终签名产物和实际安装树计数，并在未安装 VC Redist 的干净 VM 启动 | MSI `<= 18 MiB`，安装后应用本体 `<= 45 MiB`；Windows Installer/UCRT 等共享系统 runtime 另表报告；`VCRUNTIME140.dll` 已通过静态 CRT 或经许可的 app-local 部署解决；按需语音包不混入安装器。 |

## 最终建议

Issue 22 的生产候选应实现为**一个签名的 WiX x64 per-user MSI**，保持 `%LOCALAPPDATA%\Programs\QuickNote`、`eelevenn.QuickNote`、`quicknote:`、Toast CLSID 与 `UpgradeCode` 稳定，使用事务内的 `afterInstallInitialize` major upgrade，禁止同版本热修复和降级，且让 MSI 永不拥有 `%LOCALAPPDATA%\QuickNote` 用户数据；已落地的静态 CRT 策略必须继续通过导入表门禁，并在无 VC Redist 假设的干净 VM 上验收。

自研签名 EXE 只保留为有证据的后备路线：只有实际签名 MSI 超过体积硬门槛，或干净系统证明 MSI 存在无法满足的产品约束时才重开比较；届时 EXE 必须用相同矩阵证明事务恢复和卸载对称性，不能以“体积更小”替代数据与升级可靠性。
