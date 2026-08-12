# QuickNote Windows 系统集成研究

> 研究票据：[评估 Windows 快捷键、托盘、通知与自启动集成](https://github.com/eelevenn/quicknote/issues/3)  
> 研究日期：2026-08-12  
> 范围：Windows 11 桌面 MVP；只采用 Microsoft、Tauri 及其直接依赖项目的一手文档或源代码。

## 结论摘要

QuickNote 所需的全局快捷键、单实例、系统托盘、失焦隐藏和登录启动都不构成技术栈阻断项；Tauri 2 与原生 WinUI 3/.NET 均能实现。真正影响短名单的是**交互式提醒**与其部署方式：

1. 全局快捷键应使用 Win32 `RegisterHotKey` 语义，注册失败必须成为可见、可恢复的设置状态；默认组合应可改，不能使用 `F12` 或依赖 `Win` 键组合。
2. 应保持单一后台进程，关闭窗口只隐藏；快捷键、托盘、第二次启动、通知点击、通知按钮统一路由到同一实例。Tauri 可用官方 single-instance 插件；WinUI 3 可用 `AppInstance.FindOrRegisterForKey` / `RedirectActivationToAsync`。
3. 托盘可由框架封装；Windows 原生契约是 `Shell_NotifyIcon`。实现或所选库必须处理 Explorer 重启后的 `TaskbarCreated`，并在取消托盘菜单后归还焦点。
4. “点击窗口外隐藏”应由窗口失焦/停用事件触发，但录音、日期选择器、上下文菜单、通知激活和开发者工具等内部弹层必须设抑制条件，不能把每次失焦都无条件视为关闭意图。
5. 登录启动默认关闭。无包身份版本可写当前用户 `Run` 键；有包身份版本应使用 `windows.startupTask` / `StartupTask`。用户在任务管理器禁用后，应用不能越权重新开启。
6. 提醒的可靠实现应把数据库中的提醒记录作为事实源，同时向 Windows 注册系统计划通知。计划通知在应用不运行时仍可显示，但只有 **5 分钟交付窗口**；电脑关闭超过该窗口会丢弃。因此 QuickNote 必须在启动、登录启动和系统恢复时扫描逾期提醒并补发一次，且持久化去重标记。
7. Tauri 官方 notification 插件在 Windows 只提供基础通知；其 Actions API 明确仅支持移动端。若 Tauri 进入最终短名单，Windows 的“打开 / 稍后提醒 / 归档”必须通过 Rust 侧直接调用 Windows App SDK `AppNotificationManager` / `AppNotificationBuilder`（或等价的一方 Windows API），不能把该插件当完整实现。
8. 本地 App Notification 可以用于已打包和未打包应用，但生产分发仍应是“已安装应用”。无包身份 .NET 应让 `AppNotificationManager.Register()` 建立 COM 激活；MSIX 则需声明 toast COM 激活扩展。自包含部署还要处理 Windows App SDK Singleton 包依赖，并以 `AppNotificationManager.IsSupported()` 探测。

据此，Windows 集成不会淘汰 Tauri 2 或 WinUI 3/.NET，但会给架构票据增加一项硬门槛：**候选栈必须证明交互式计划通知在已安装、应用未运行、第二实例重定向和无/有包身份目标下的完整闭环**。

## 能力与建议

### 1. 全局快捷键

Windows 原生入口是 `RegisterHotKey`。成功后系统向指定窗口或调用线程投递 `WM_HOTKEY`；`MOD_NOREPEAT` 可防止按住按键产生重复消息。注册会在组合已被其他应用占用时失败；`F12` 永久保留给调试器，含 Windows 徽标键的组合保留给操作系统。[Microsoft：RegisterHotKey](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registerhotkey)

产品与实现约束：

- 默认快捷键必须可配置，并在启动及修改时检查真实注册结果；失败时保留旧的有效组合或关闭该能力，设置页显示冲突并允许重试。
- 注册在常驻的原生后台层完成，不依赖可见 WebView/页面生命周期；进程退出时注销。
- 默认候选宜从 `Ctrl+Alt+Space`、`Ctrl+Shift+Space` 等不含 `Win` 的组合中实测选择；需覆盖中文输入法、PowerToys、显卡覆盖层、远程桌面和常见启动器冲突。
- 使用 `MOD_NOREPEAT` 或框架等价能力，热键只处理按下一次，避免重复显隐。

Tauri 官方 global-shortcut 插件支持 Windows，提供注册、注销和冲突结果；其文档也明确指出被其他程序占用时处理器不会触发。[Tauri：global-shortcut API](https://v2.tauri.app/reference/javascript/global-shortcut/) 插件本身包装 `global-hotkey`，可在 Rust 后端注册，因此窗口隐藏时仍可工作。[Tauri 插件源代码](https://github.com/tauri-apps/plugins-workspace/blob/9c9343772b8feb8d63c84cd6e0d36164b33415c3/plugins/global-shortcut/src/lib.rs)

### 2. 单实例与激活路由

单实例不是只做“互斥锁”；第二次启动、通知点击、通知按钮和启动任务都带有激活意图，必须转发给既有实例，然后由 UI 线程决定显示主页、显示快速记录窗、切换便签、稍后提醒或归档。

WinUI 3 / Windows App SDK 默认允许多实例。官方建议尽量早在 `Main` / `wWinMain` 调用 `AppInstance.FindOrRegisterForKey`，非当前实例使用 `RedirectActivationToAsync` 后退出，避免初始化一套马上丢弃的 UI。[Microsoft：Application lifecycle migration](https://learn.microsoft.com/en-us/windows/apps/windows-app-sdk/migrate-to-windows-app-sdk/guides/applifecycle) 激活参数可覆盖普通启动、通知和登录启动等入口。[Microsoft：App activation](https://learn.microsoft.com/en-us/windows/apps/develop/launch/activate-an-app)

Tauri 官方 single-instance 插件支持 Windows，第二实例回调可收到参数和工作目录。[Tauri：single-instance](https://v2.tauri.app/plugin/single-instance/) 当前 Windows 实现使用命名 mutex 判断既有实例，以隐藏窗口接收 `WM_COPYDATA` 并转发参数。[Tauri 插件 Windows 源代码](https://github.com/tauri-apps/plugins-workspace/blob/9c9343772b8feb8d63c84cd6e0d36164b33415c3/plugins/single-instance/src/platform_impl/windows.rs)

建议定义单一激活协议，例如内部命令 `show-quick-capture`、`show-home`、`open-note:<id>`、`snooze:<reminder-id>:<duration>`、`archive:<note-id>`。所有入口只产生该协议，不直接操作数据库或窗口；主实例串行消费，保证自动保存与归档状态不竞态。

### 3. 系统托盘

Windows 原生 API 是 `Shell_NotifyIcon`：`NIM_ADD/MODIFY/DELETE` 管理图标，添加后应设置版本；托盘菜单取消后应使用 `NIM_SETFOCUS` 把焦点还给通知区域。[Microsoft：Shell_NotifyIcon](https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shell_notifyicona)

Explorer/任务栏重启会丢弃已添加图标。Shell 会广播注册名为 `TaskbarCreated` 的消息，应用必须收到后重新添加；DPI 变化也可能触发相关路径。[Microsoft：The Taskbar](https://learn.microsoft.com/en-us/windows/win32/shell/taskbar)

Tauri 内置托盘能力支持原生菜单与鼠标事件。[Tauri：系统托盘](https://v2.tauri.app/zh-cn/learn/system-tray/) 其直接依赖 `tray-icon` 的 Windows 实现已经监听 `TaskbarCreated` 并重新注册图标，因此可接受，但仍应加入 Explorer 重启验收用例。[tray-icon Windows 源代码](https://github.com/tauri-apps/tray-icon/blob/1c23131c96ebce1703d5dee17c483cfdc892999b/src/platform_impl/windows/mod.rs)

MVP 托盘菜单固定为“主页 / 新便签 / 退出”。普通关闭拦截为隐藏；只有“退出”真正结束后台进程。托盘图标可能被 Windows 收进溢出区，不能把它当唯一入口。

### 4. 快速记录窗：显示、焦点和点击外部隐藏

失焦隐藏的原生信号是 `WM_ACTIVATE` 的 `WA_INACTIVE`；WinUI 可监听 `Window.Activated` 的 Deactivated，Tauri 提供 `WindowEvent::Focused(false)`，并直接给出失焦隐藏示例。[Microsoft：WM_ACTIVATE](https://learn.microsoft.com/en-us/windows/win32/inputdev/wm-activate) [Tauri：WindowEvent](https://docs.rs/tauri/latest/tauri/enum.WindowEvent.html) [Tauri：Builder::on_window_event](https://docs.rs/tauri/latest/tauri/struct.Builder.html#method.on_window_event)

但“失焦”不等同于“点击应用外”：系统通知、日期/时间弹层、原生文件或上下文菜单、语音权限 UI、IME 候选窗、调试器和另一个 QuickNote 窗口都可能改变焦点。建议：

- 快速记录窗失焦后投递一次异步关闭判断；若应用正在展示受管理弹层、正在处理通知/托盘激活、仍有 QuickNote 拥有窗口激活，或录音流程要求确认，则不隐藏。
- 先提交当前自动保存事务，再隐藏；隐藏窗口而非销毁 WebView/控件树，以保护 P95 热呼出目标。
- 快捷键触发来自用户输入，通常可 `show + set_focus`；第二实例或后台逻辑并不总能抢到前台。Windows 明确限制 `SetForegroundWindow`，失败时只能闪烁任务栏等请求注意，不能保证强制置前。[Microsoft：SetForegroundWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setforegroundwindow)
- 主页从列表打开便签并设为“当前”后，快速窗下次读取当前 ID；两个窗口不得各自缓存一份当前便签。

### 5. 多显示器与 DPI

快捷窗应出现在**触发快捷键时鼠标指针所在显示器**的工作区内；若产品测试表明键盘用户更期望前台窗口所在屏幕，可调整为“前台窗口显示器，无法判定时鼠标屏幕”。不要使用只代表主屏的 `SM_CXSCREEN/SM_CYSCREEN`，应使用 `MonitorFromPoint` / `MonitorFromWindow` 与 `GetMonitorInfo` 的工作区坐标，并允许负坐标。[Microsoft：Positioning Objects on Multiple Display Monitors](https://learn.microsoft.com/en-us/windows/win32/gdi/positioning-objects-on-multiple-display-monitors)

恢复历史窗口位置前必须验证显示器仍存在并把矩形夹在工作区内；需处理显示器拔插、任务栏在任意边、不同缩放比例及 `ScaleFactorChanged`。Tauri 窗口 API提供 `monitor_from_point`、`current_monitor` 和 `ScaleFactorChanged`，但这些只是机制，定位策略仍需自行实现。[Tauri：Window](https://docs.rs/tauri/latest/x86_64-pc-windows-msvc/tauri/window/struct.Window.html)

### 6. 登录后启动

产品决定是默认关闭、设置中开启。实现也应坚持用户可控：

- **未打包/传统安装器**：使用当前用户 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`；Windows 允许延迟启动，不能把登录时刻当精确调度点。[Microsoft：Run and RunOnce Registry Keys](https://learn.microsoft.com/en-us/windows/win32/setupapi/run-and-runonce-registry-keys)
- **MSIX 或带外部位置身份包**：在 manifest 声明 `windows.startupTask`，应用用 `StartupTask` 查询和切换。用户若在任务管理器禁用，状态为 `DisabledByUser`，只能由用户重新启用，应用不能覆盖。[Microsoft：StartupTask](https://learn.microsoft.com/en-us/uwp/api/windows.applicationmodel.startuptask) [Microsoft：StartupTaskState](https://learn.microsoft.com/en-us/uwp/api/windows.applicationmodel.startuptaskstate)

Tauri 官方 autostart 插件支持 Windows。[Tauri：autostart](https://v2.tauri.app/plugin/autostart/) 当前插件通过 `auto-launch` 实现；其 crate 0.5.0 的 Windows 源代码使用 HKCU `Run` 并检查 Task Manager 的 StartupApproved 状态，符合未打包路径，但不等同于有包身份的 `StartupTask`。[auto-launch 0.5.0 发布源码](https://crates.io/api/v1/crates/auto-launch/0.5.0/download)

启动参数应带 `--background --from-startup`，只创建托盘和提醒服务，不显示主页或快速窗。若用户通过任务管理器关闭启动项，设置页必须反映实际状态并指引到 Windows 设置，而不是偷偷复写。

## 通知与提醒可靠性

### 7. 原生通知和动作

新 Windows 桌面应用的推荐 API 是 Windows App SDK `Microsoft.Windows.AppNotifications.AppNotificationManager`；本地通知对 WinUI、WPF、WinForms 和未打包 Win32 均适用。[Microsoft：Windows notifications overview](https://learn.microsoft.com/en-us/windows/apps/develop/notifications/)

按钮可传参数激活应用；Windows App SDK 桌面应用的应用定义按钮会启动前台进程，但进程可以读取激活参数、仅执行动作后退出。系统自带 snooze/dismiss 动作也可用；最多 5 个按钮。[Microsoft：App notification content](https://learn.microsoft.com/en-us/windows/apps/develop/notifications/app-notifications/app-notifications-content)

QuickNote MVP 建议通知内容：

- 点击正文：`open-note`，打开对应便签并设为当前。
- 按钮：“稍后提醒”使用系统 snooze 选择或应用定义动作；“归档”使用应用定义动作。
- `noteId` / `reminderId` 放入激活参数；数据库事务必须幂等。归档动作即使从冷启动进入，也先落库、取消同一便签未来计划项，再决定是否退出后台动作进程。
- 使用 tag/group 将系统通知映射到便签/提醒，修改、归档、删除时同步移除或替换。

Tauri 的官方 notification 插件不能满足这一闭环：Windows 说明仅“安装后工作”，而 Actions API 明确为 Mobile Only。[Tauri：Notifications](https://v2.tauri.app/plugin/notification/) 当前桌面源代码只把标题、正文、图标、声音交给 `notify-rust` 显示，没有 Windows 动作或激活路由。[Tauri notification 源代码](https://github.com/tauri-apps/plugins-workspace/blob/9c9343772b8feb8d63c84cd6e0d36164b33415c3/plugins/notification/src/desktop.rs)

因此：

- WinUI 3/.NET 直接使用 AppNotification API。
- Tauri 2 必须写一个窄的 Windows Rust 插件/命令层调用 Windows App SDK，并把激活参数转成上述内部激活协议；不建议把通知业务留在 JavaScript 层。

### 8. 打包身份、安装和激活

本地 App Notification 不强制 package identity，但应用定义动作要可靠唤起未运行进程，仍需正确的应用身份/COM 注册：

- 未打包 .NET：在注册 `NotificationInvoked` 后调用 `AppNotificationManager.Register()`；官方说明该调用会自动建立通知点击启动应用所需的 COM server registration。[Microsoft：.NET app notifications](https://learn.microsoft.com/en-us/windows/apps/develop/notifications/app-notifications/app-notifications-dotnet)
- MSIX：manifest 声明 `windows.toastNotificationActivation` 与 COM server CLSID；通知、启动任务、卸载清理的身份更一致。
- 传统 Win32 老 API依赖 Start Menu shortcut + AUMID；这也解释了 Tauri 插件为何强调 Windows 通知只对已安装应用工作。[Microsoft：desktop toast and AUMID](https://learn.microsoft.com/en-us/windows/win32/shell/quickstart-sending-desktop-toast)

Windows App SDK `AppNotificationManager` 依赖 Singleton MSIX 包。框架依赖部署由运行时提供；自包含应用应调用 `IsSupported()`，或在安装器部署额外包，否则“单 EXE 自包含”不能自然等同于“通知能力自包含”。[Microsoft：self-contained deployment](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/self-contained-deploy/deploy-self-contained-apps)

QuickNote 不应提升为管理员运行；Windows App SDK 官方说明 elevated app 不支持 App Notifications。[Microsoft：app notifications quickstart](https://learn.microsoft.com/en-us/windows/apps/develop/notifications/app-notifications/app-notifications-quickstart)

**对分发决策的影响**：MVP 即使不进商店，也应使用每用户安装器并创建可靠应用身份。是否使用完整 MSIX、带外部位置的身份包或未打包安装器，应在架构/性能基准中实测；不能为了几 MB 包体直接选择 zip 绿色版，因为这会把通知激活、自启动可见性和卸载清理风险推给应用。

### 9. 应用未运行、休眠和关机时的提醒

Windows 支持提前注册 `ScheduledToastNotification`，到时即使应用不运行也可显示；Windows App SDK builder 生成 payload，再由 `Windows.UI.Notifications` 的计划 API注册和取消。[Microsoft：Schedule an app notification](https://learn.microsoft.com/en-us/windows/apps/develop/notifications/app-notifications/app-notifications-scheduled)

但该机制不是可靠闹钟：计划通知只有 **5 分钟交付窗口**；设备在计划时间关机并超过窗口后，该通知被丢弃。桌面应用在 Modern Standby 时用户态进程也可能暂停，不能依赖进程内 timer。[Microsoft：Schedule an app notification](https://learn.microsoft.com/en-us/windows/apps/develop/notifications/app-notifications/app-notifications-scheduled) [Microsoft：desktop app lifecycle](https://learn.microsoft.com/en-us/windows/apps/develop/launch/app-lifecycle)

建议的提醒算法：

1. 数据库提醒记录是事实源，至少保存 `dueAt`、`status`、`scheduledVersion`、`lastDeliveredAt`、`snoozedFrom`。
2. 创建/修改提醒时，事务提交后注册或替换系统计划通知；归档/删除时取消计划项。
3. 主实例启动、登录启动、从休眠恢复时扫描 `dueAt <= now` 且尚未交付/补发的记录，补发一次并持久化去重；`WM_POWERBROADCAST` 的 `PBT_APMRESUMEAUTOMATIC` 每次恢复都会发送，可作为原生恢复信号。[Microsoft：WM_POWERBROADCAST](https://learn.microsoft.com/en-us/windows/win32/power/wm-powerbroadcast)
4. 不承诺关机期间准点提醒；产品文案应是“设备可用时提醒，错过后在 QuickNote 下次运行时补发”。默认不开机启动意味着用户未再次启动应用前不能执行应用侧补发，这是产品已接受的边界。
5. 若应用保持运行，进程内 timer 只作为低延迟优化，不能替代系统计划项和启动扫描。
6. 系统通知可被用户、全局设置或组策略禁用；应用应读取 `AppNotificationManager.Setting`，在设置页明确显示，不宣称必达。[Microsoft：AppNotificationSetting](https://learn.microsoft.com/en-us/windows/windows-app-sdk/api/winrt/microsoft.windows.appnotifications.appnotificationsetting)

“Guaranteed delivery”若被提升为硬需求，将要求背景任务等更重的 package-identity 路线，超出当前本地优先轻量 MVP；现在无需承担。

## 对候选技术栈的门槛

| 能力 | Tauri 2 | WinUI 3/.NET | 架构门槛 |
| --- | --- | --- | --- |
| 全局快捷键 | 官方插件可用 | P/Invoke `RegisterHotKey` | 可配置、冲突可见、后台原生注册 |
| 单实例 | 官方插件可用，命名 mutex + `WM_COPYDATA` | `AppInstance` 激活重定向 | 所有入口统一成激活协议 |
| 托盘 | 内置，依赖库处理 `TaskbarCreated` | `Shell_NotifyIcon` 或成熟封装 | Explorer 重启、键盘和焦点验收 |
| 失焦隐藏 | `WindowEvent::Focused(false)` | `Window.Activated` / `WM_ACTIVATE` | 必须有受管理弹层抑制状态 |
| 登录启动 | 官方插件适合未打包 HKCU Run | `StartupTask` 或 HKCU Run | 默认关闭、尊重系统禁用 |
| 基础本地通知 | 官方插件可显示 | 原生支持 | 都应作为已安装应用测试 |
| Windows 通知动作 | 官方插件不支持 | 原生支持 | Tauri 必须加 Windows 原生桥 |
| 应用未运行时计划提醒 | 需原生桥 | WinRT 计划通知 | 5 分钟窗口 + 启动/恢复补发 |

进入最终技术栈前，候选实现必须在真实 Windows 11 机器通过以下 spike；否则不算“Windows 集成已支持”：

1. 进程后台、窗口隐藏时全局快捷键热呼出并聚焦输入框。
2. 快捷键冲突有明确错误，修改组合后无需重启即可恢复。
3. 第二次启动、通知正文、“稍后提醒”、“归档”都路由到同一进程且动作幂等。
4. 应用完全退出后，计划通知能出现；点击/按钮能冷启动并正确处理。
5. Explorer 重启后托盘恢复；系统休眠越过提醒时间后恢复扫描只补发一次。
6. 默认关闭登录启动，开启后静默驻留；在任务管理器禁用后应用尊重系统状态。
7. 双屏不同 DPI、负坐标、拔插显示器、任务栏不同位置时快捷窗不越界。
8. 通知被用户/组策略关闭、Windows App SDK Singleton 不可用时，设置页有可理解的降级状态。

## 建议写入后续规格的验收语义

- “快捷键可用”指注册成功且后台隐藏状态可触发；冲突不是静默失败。
- “关闭窗口”指隐藏并继续保持快捷键、托盘和提醒服务；“退出”才结束进程。
- “提醒”不承诺设备关机期间准时；系统计划通知失败或错过后，在下次启动/恢复补发一次。
- “归档”必须在数据库提交后取消未来系统计划项；重复通知动作不改变最终结果。
- 通知点击打开对应便签并设为当前；通知按钮动作不要求显示主页，除非动作本身是打开。
- “开机启动”用户可选且默认关闭；Windows 设置或任务管理器的禁用决定优先于应用设置。

## 参考资料

### Microsoft

- [RegisterHotKey function](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registerhotkey)
- [Application lifecycle functionality migration](https://learn.microsoft.com/en-us/windows/apps/windows-app-sdk/migrate-to-windows-app-sdk/guides/applifecycle)
- [App activation for Windows App SDK desktop apps](https://learn.microsoft.com/en-us/windows/apps/develop/launch/activate-an-app)
- [Shell_NotifyIcon function](https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shell_notifyicona)
- [The Taskbar](https://learn.microsoft.com/en-us/windows/win32/shell/taskbar)
- [WM_ACTIVATE message](https://learn.microsoft.com/en-us/windows/win32/inputdev/wm-activate)
- [SetForegroundWindow function](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setforegroundwindow)
- [Positioning objects on multiple display monitors](https://learn.microsoft.com/en-us/windows/win32/gdi/positioning-objects-on-multiple-display-monitors)
- [Run and RunOnce registry keys](https://learn.microsoft.com/en-us/windows/win32/setupapi/run-and-runonce-registry-keys)
- [StartupTask class](https://learn.microsoft.com/en-us/uwp/api/windows.applicationmodel.startuptask)
- [Windows notifications overview](https://learn.microsoft.com/en-us/windows/apps/develop/notifications/)
- [App notification content](https://learn.microsoft.com/en-us/windows/apps/develop/notifications/app-notifications/app-notifications-content)
- [Use app notifications with a .NET app](https://learn.microsoft.com/en-us/windows/apps/develop/notifications/app-notifications/app-notifications-dotnet)
- [Schedule an app notification](https://learn.microsoft.com/en-us/windows/apps/develop/notifications/app-notifications/app-notifications-scheduled)
- [Windows App SDK self-contained deployment](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/self-contained-deploy/deploy-self-contained-apps)
- [WM_POWERBROADCAST message](https://learn.microsoft.com/en-us/windows/win32/power/wm-powerbroadcast)

### Tauri 及其一方源代码

- [Global Shortcut API](https://v2.tauri.app/reference/javascript/global-shortcut/)
- [Single Instance plugin](https://v2.tauri.app/plugin/single-instance/)
- [System Tray](https://v2.tauri.app/learn/system-tray/)
- [Autostart plugin](https://v2.tauri.app/plugin/autostart/)
- [Notifications plugin](https://v2.tauri.app/plugin/notification/)
- [plugins-workspace 固定提交](https://github.com/tauri-apps/plugins-workspace/tree/9c9343772b8feb8d63c84cd6e0d36164b33415c3)
- [tray-icon Windows 固定提交](https://github.com/tauri-apps/tray-icon/blob/1c23131c96ebce1703d5dee17c483cfdc892999b/src/platform_impl/windows/mod.rs)
