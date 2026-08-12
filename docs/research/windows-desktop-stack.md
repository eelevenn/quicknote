# QuickNote 轻量 Windows 桌面技术栈研究

> 对应决策票据：[评估轻量 Windows 桌面技术栈](https://github.com/eelevenn/quicknote/issues/2)  
> 研究日期：2026-08-12

## 结论摘要

目前没有可信的一手资料能直接证明任一框架可在 QuickNote 的真实功能负载下达到热呼出 P95 ≤ 80 ms、冷启动 P95 ≤ 800 ms、后台空闲内存 ≤ 50 MB、安装包 ≤ 15 MB。官方资料能回答的是运行时架构、分发边界和功能能力；最终数值必须由同一台 Windows 11 设备上的等价原型实测。

建议让以下四项进入同一轮基准，而不是现在选定最终架构：

1. **Tauri 2 + 精简 Web 前端 + Rust 核心**：综合开发效率、现代界面和应用本体体积的首要候选；最大不确定性是常驻 WebView2 进程组的 P95 内存。
2. **WPF + .NET Framework 4.8.1**：Windows 11 自带运行时条件下的轻量原生对照组；系统集成和文本编辑成熟，但微软明确建议新开发使用现代 .NET，因此它是有意接受旧平台边界的候选。
3. **Rust + Slint**：无浏览器、无 .NET 应用运行时的轻量候选；已有 Fluent 风格、文本编辑、托盘和无障碍基础，但 Windows 通知、全局快捷键等仍需直接做 Win32 集成，生态风险高于前两项。
4. **WinUI 3 + C#/.NET（framework-dependent）**：Windows 11 视觉、控件和系统 API 的第一方基准；必须实测 Windows App SDK 与 .NET 两层运行时启动、内存和首次安装成本。自包含发布不是 15 MB 安装包目标下的合理默认方案。

纯 Win32/C++ 能提供最低运行时开销的性能下界，但会把现代布局、主题、无障碍和编辑体验的大量工作变成自研，因此不建议作为 MVP 产品短名单；可以做一个只含窗口和 `RegisterHotKey` 的微基准用于校准测量工具。

## 评价口径

应同时记录三类体积，不能只报一个“安装包大小”：

- **下载物**：用户首次下载的 QuickNote 安装器，以及安装器联网拉取的先决条件。
- **应用本体**：只归属于 QuickNote 的安装目录/包占用。
- **共享运行时**：WebView2、Windows App SDK、.NET Desktop Runtime 等机器级共享组件，单独列出且不计入应用本体；但必须计入“全新 Windows 11 机器首次安装额外下载量”。

内存统一测整个进程树的 private working set 与 commit，而不是只看主进程。Tauri 官方说明一个 Core 进程会管理一个或多个系统 WebView 进程，所以只测 Rust 主进程会系统性低估占用。[Tauri 进程模型](https://v2.tauri.app/concept/process-model/)

## 候选比较

| 候选 | 分发与运行时 | 对目标的结构性判断 | QuickNote 能力与主要风险 | 实测地位 |
| --- | --- | --- | --- | --- |
| Tauri 2 | Rust 核心编入应用；Windows UI 使用系统 WebView2。Windows 11 预装 Evergreen WebView2，运行时可由多应用共享；若捆绑 Fixed Runtime，Tauri 官方称安装器约增加 180 MB。[WebView2 Evergreen/Fixed](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/evergreen-vs-fixed-version)；[Tauri Windows installer](https://v2.tauri.app/distribute/windows-installer/) | 安装包 ≤ 15 MB、应用本体 ≤ 40 MB 有现实可能；后台 ≤ 50 MB 不能由架构说明保证，必须统计 WebView2 整个进程树。热呼出应测“WebView 常驻后 show/focus”，冷启动应包含首次创建 WebView。 | 官方功能覆盖全局快捷键、通知、单实例、自动启动、SQL、窗口状态；托盘是内建 API。[官方插件清单](https://v2.tauri.app/plugin/)；[系统托盘](https://v2.tauri.app/learn/system-tray/)；[SQLite 插件](https://v2.tauri.app/plugin/sql/)。编辑器可先使用原生 `textarea`，W3C 已定义 IME composition 与 `beforeinput`/`input` 顺序；风险在于焦点、中文 IME、WebView 恢复和 IPC 自动保存。[UI Events](https://www.w3.org/TR/uievents/)；[Input Events Level 2](https://www.w3.org/TR/input-events-2/) | **主候选** |
| WPF + .NET Framework 4.8.1 | Windows 11 22H2 起包含 .NET Framework 4.8.1；.NET Framework 4.x 是 Windows 组件，生命周期随操作系统。[Windows 版本与 .NET Framework](https://learn.microsoft.com/en-us/dotnet/framework/install/on-server-2019)；[生命周期 FAQ](https://learn.microsoft.com/en-au/lifecycle/faq/dotnet-framework) | 可利用 OS 自带运行时，最有希望把独立安装包压低；启动和常驻内存仍须测。安装器不应重复捆绑 .NET Framework。 | WPF 有成熟的多行文本、数据绑定和 Win32 互操作；WinForms `NotifyIcon` 是成熟的后台托盘组件。[WPF 官方仓库](https://github.com/dotnet/wpf)；[NotifyIcon](https://learn.microsoft.com/en-us/dotnet/desktop/winforms/controls/notifyicon-component-overview-windows-forms)。主要代价是微软明确说明新开发应使用现代 .NET，而不是 .NET Framework；现代 Windows 11 外观需要自定义样式，且未来平台能力不如 WinUI 直接。[.NET Framework 安装指南](https://learn.microsoft.com/en-us/dotnet/framework/install/) | **轻量原生对照候选** |
| Rust + Slint | Rust 原生二进制，无 WebView2 或 .NET 应用运行时；Slint 支持 Windows 10 x64、Windows 11 x64/ARM64。[Slint 桌面支持](https://docs.slint.dev/latest/docs/slint/guide/platforms/desktop/) | 架构上最有希望同时压低冷启动、后台内存和应用本体，但官方没有给出与本功能切片等价的 P95/RSS 数据，不能直接宣称达标。渲染器选择会显著影响体积与内存，基准需固定并记录。[后端与渲染器](https://docs.slint.dev/latest/docs/slint/guide/backends-and-renderers/backends_and_renderers/) | 官方已有 Fluent 明暗风格、`TextEdit`、无障碍属性和 `SystemTrayIcon`；托盘在 Windows 下直接使用 `Shell_NotifyIcon`。[Widget styles](https://docs.slint.dev/latest/docs/slint/reference/std-widgets/style/)；[TextEdit](https://docs.slint.dev/latest/docs/slint/reference/std-widgets/views/textedit/)；[无障碍属性](https://docs.slint.dev/latest/docs/slint/reference/common/)；[SystemTrayIcon](https://docs.slint.dev/latest/docs/slint/reference/window/systemtrayicon/)。全局快捷键、通知激活、单实例、启动项和 SQLite 需自行整合 Rust/Win32 crates 并做端到端验证。免费 proprietary desktop 使用要求遵守其 Royalty-free License 的披露条件，发布前必须复核。[Slint License](https://github.com/slint-ui/slint/blob/master/LICENSE.md) | **高性能探索候选** |
| WinUI 3 + C#/.NET | WinUI 3 属于 Windows App SDK，后者独立于 OS；framework-dependent 应用依赖机器上安装 Windows App SDK runtime，未打包应用还需 bootstrapper，C# 另需 .NET 6+。[WinUI 3](https://learn.microsoft.com/en-us/windows/apps/winui/winui3/)；[framework-dependent 部署](https://learn.microsoft.com/en-us/windows/apps/windows-app-sdk/deploy-unpackaged-apps) | framework-dependent 可缩小应用本体，但目标机器要有两层共享运行时；self-contained 会把 Windows App SDK 内容复制进输出，.NET 也必须 self-contained，因此与 15 MB 目标冲突。[Windows App SDK self-contained](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/self-contained-deploy/deploy-self-contained-apps)。单文件发布也会在首次启动解压，不代表安装后只有一个小文件。[WinUI 单文件发布](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/unpackage-winui-app) | Microsoft 推荐 WinUI 3 作为新原生 Windows 应用框架；系统通知可带按钮和后台动作，`TextBox` 提供 `TextChanging`/`TextChanged`，适合防抖自动保存。[Windows 开发平台选择](https://learn.microsoft.com/en-us/windows/apps/)；[App notifications](https://learn.microsoft.com/en-us/windows/apps/develop/notifications/app-notifications/)；[TextBox](https://learn.microsoft.com/en-us/windows/apps/develop/ui/controls/text-box)。风险集中于依赖部署、启动初始化、托盘仍要 Win32/WinForms 互操作，以及 C# 自包含体积。 | **第一方体验基准候选** |

### 没有进入短名单的方案

- **Wails** 与 Tauri 同样依赖 WebView2；官方文档提供下载、嵌入约 150 KB bootstrapper、浏览器跳转或报错四种缺失运行时处理方式，但它没有在 QuickNote 的 Windows 能力或资源模型上形成相对 Tauri 的明确优势。[Wails Windows guide](https://wails.io/docs/next/guides/windows/)
- **现代 .NET WPF/WinForms self-contained** 会捆绑 .NET Desktop Runtime；trimming 只适用于 self-contained 且存在反射兼容风险。它可以后续补测，但对 15 MB 安装包没有 WPF/.NET Framework 4.8.1 的结构性优势。[.NET trimming](https://learn.microsoft.com/en-us/dotnet/core/deploying/trimming/trim-self-contained)
- **原生 Win32/C++** 的系统 API 无额外 UI 运行时，`RegisterHotKey` 由 User32 直接提供，系统托盘由 Shell notification area 提供；但它更适合作为性能测量下界，而非快速交付现代双栏编辑器的产品候选。[RegisterHotKey](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registerhotkey)；[Notification Area](https://learn.microsoft.com/en-us/windows/win32/shell/notification-area)

## 自动保存编辑器的共同设计约束

技术栈不同，自动保存语义应保持一致：

1. UI 线程立即更新内存草稿；数据库写入采用 150–300 ms trailing debounce。
2. 失焦、隐藏窗口、切换便签、归档和进程正常退出前强制 flush；每次写入使用事务。
3. Web 前端在 `compositionstart` 到 `compositionend` 期间不得用后端回写重建编辑节点，否则可能破坏中文 IME 和光标；原生控件也要覆盖中文拼音组合态。
4. 基准原型只做纯文本/轻量 Markdown 源文本，不引入富文本编辑器。Tauri 用原生 `textarea`，WPF/WinUI 用原生多行 `TextBox`，Slint 用 `TextEdit`，避免候选之间因编辑器库产生无关差异。
5. SQLite 使用 WAL，并验证断电/强杀后的最后已确认输入；测量时分别记录“仅键入内存更新”和“提交 SQLite”延迟。SQLite 官方说明 WAL 的 commit 记录追加机制、`synchronous` 级别与掉电耐久性存在明确取舍，因此原型必须固定并报告这些设置。[SQLite WAL](https://sqlite.org/wal.html)；[SQLite PRAGMA](https://sqlite.org/pragma.html)

## 建议的等价基准切片

四个候选都实现同一份最小功能，不先做完整产品：

- 单实例后台进程、托盘、可隐藏的快速记录窗；
- 注册同一全局快捷键，触发 show → activate → focus 到正文输入框；
- 一张当前便签，启动时从 SQLite 读取，输入后防抖自动保存；
- 中文 IME、粘贴、撤销/重做、失焦隐藏；
- 一条本地通知，点击后唤起同一便签；
- 跟随系统明暗色；不包含语音模型、Markdown 渲染器、完整主页和安装器自动更新。

每个候选在 Release、x64、同一签名/安装模式下至少测 50 次冷启动、200 次热呼出；冷启动前结束所有候选进程，热呼出时保留后台常驻状态。记录：

- 冷启动：进程创建到输入框收到焦点且能接受键盘事件的 P50/P95；
- 热呼出：收到全局快捷键到输入框获得焦点的 P50/P95；
- 后台内存：隐藏窗口稳定 60 秒后，整个进程树的 private working set、commit 和进程数；
- 体积：安装器文件、安装目录应用本体、首次安装拉取的共享运行时分别记录；
- 正确性：100 次快速键入后强杀恢复、中文 IME、快捷键冲突、通知激活、Explorer 重启后托盘恢复。

### 进入下一轮的门槛

- 硬门槛全部满足：热呼出 P95 ≤ 80 ms、冷启动 P95 ≤ 800 ms、后台进程树空闲内存 ≤ 50 MB、安装包 ≤ 15 MB、应用本体 ≤ 40 MB。
- 任何共享运行时都必须单列；若全新 Windows 11 首次安装需额外下载，报告其真实下载量和失败体验。
- 正确性失败优先于平均性能：丢字、IME 光标错乱、快捷键后未聚焦、强杀丢失已过 debounce 的文本，均直接淘汰或要求修复后复测。
- 达到硬门槛后，再以挑战目标（热呼出 ≤ 50 ms、冷启动 ≤ 500 ms、后台 ≤ 35 MB、安装包 ≤ 10 MB）和实现复杂度排序。

## 对性能目标的判断

- **热呼出还可以更低**：应用保持后台、窗口与编辑器实例预创建时，≤ 50 ms 是合理的挑战目标；但必须从系统快捷键事件开始计时到真实输入焦点，不能只量 `show()` 调用。
- **后台内存有机会低于 35 MB，但不能预设**：Rust + Slint 与 WPF/.NET Framework 最值得验证；Tauri 必须统计 WebView2 子进程，WinUI 3/C# 必须统计 .NET 和 Windows App SDK 初始化后的稳定值。
- **安装包有机会低于 10 MB，但依赖口径**：Tauri 使用 Windows 11 预装的 WebView2、WPF 使用系统自带 .NET Framework、Slint 选择精简 renderer 时最有希望；WinUI 3/C# 只有 framework-dependent 且共享运行时已安装时才可能呈现小应用包。任何 Fixed WebView2 或 WinUI/.NET 全自包含发布都不应以 ≤ 15 MB 为预期。

因此，本票据只确定“进入实测的短名单与公平口径”，不替用户选定最终技术栈。最终架构应由上述等价基准和后续 Windows 系统集成研究共同决定。
