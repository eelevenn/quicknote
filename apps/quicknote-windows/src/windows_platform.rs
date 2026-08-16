//! `PlatformServices` 的 Windows 11 adapter。

use quicknote_app::platform::{
    ActivationHandler, ActivationRequest, GlobalShortcut, InstanceLease, InstanceRole,
    NotificationProjectionKey, PRODUCT_IDENTITY, PlatformCommand, PlatformError, PlatformServices,
    ShortcutKey, reminder_protocol_uri,
};
use quicknote_app::{ReminderActivation, ReminderActivationAction};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::DateTime;
use windows::UI::Notifications::{ScheduledToastNotification, ToastNotificationManager};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE,
    GetLastError, HANDLE, INVALID_HANDLE_VALUE, LPARAM, PROPERTYKEY, WPARAM,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_MODE, MOVEFILE_REPLACE_EXISTING,
    MOVEFILE_WRITE_THROUGH, MoveFileExW, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_MESSAGE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    WaitNamedPipeW,
};
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentThreadId};
use windows::Win32::System::WinRT::{RO_INIT_SINGLETHREADED, RoInitialize, RoUninitialize};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey,
    UnregisterHotKey,
};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{
    IShellLinkW, SetCurrentProcessExplicitAppUserModelID, ShellExecuteW, ShellLink,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SW_SHOWNORMAL, WM_APP,
    WM_HOTKEY,
};
use windows::core::{GUID, HSTRING, Interface, PCWSTR};
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};

const MUTEX_NAME: &str = "Local\\eelevenn.QuickNote.SingleInstance.v1";
const PIPE_NAME: &str = r"\\.\pipe\eelevenn.quicknote.activation.v1";
const SHUTDOWN_MESSAGE: &[u8] = b"__quicknote_shutdown__";
const MAX_ACTIVATION_BYTES: usize = 64 * 1024;
const HOTKEY_CONTROL_MESSAGE: u32 = WM_APP + 17;
const HOTKEY_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const WINDOWS_EPOCH_OFFSET_100NS: i64 = 116_444_736_000_000_000;
const APP_USER_MODEL_PROPERTY_SET: GUID = GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3);
const APP_USER_MODEL_ID_KEY: PROPERTYKEY = PROPERTYKEY {
    fmtid: APP_USER_MODEL_PROPERTY_SET,
    pid: 5,
};
const TOAST_ACTIVATOR_CLSID_KEY: PROPERTYKEY = PROPERTYKEY {
    fmtid: APP_USER_MODEL_PROPERTY_SET,
    pid: 26,
};
// 快捷方式和当前用户 COM 注册共用稳定 CLSID；通知动作仍统一走 quicknote 协议。
const TOAST_ACTIVATOR_CLSID: &str = "{7E5ACBFA-9501-4F8A-9C85-60C1AE5D17C4}";

/// UI 线程持有的 WinRT apartment 租约。
pub struct WinRtApartment;

impl Drop for WinRtApartment {
    fn drop(&mut self) {
        // SAFETY: 与同一线程成功的 RoInitialize 成对调用。
        unsafe { RoUninitialize() };
    }
}

/// 在创建 Slint 窗口前初始化通知 API 使用的 STA apartment。
pub fn initialize_winrt_apartment() -> Result<WinRtApartment, PlatformError> {
    // SAFETY: 主 UI 线程仅在启动时调用一次，并由返回租约负责反初始化。
    unsafe { RoInitialize(RO_INIT_SINGLETHREADED) }
        .map_err(|error| PlatformError::new("initialize_winrt", error.to_string()))?;
    Ok(WinRtApartment)
}

/// 使用稳定 AUMID、协议和本地数据目录的生产 Windows adapter。
#[derive(Clone)]
pub struct WindowsPlatformServices {
    state: Arc<WindowsPlatformState>,
}

#[derive(Default)]
struct WindowsPlatformState {
    activation_handler: Mutex<Option<ActivationHandler>>,
    hotkey_thread: Mutex<Option<HotkeyThread>>,
}

impl WindowsPlatformServices {
    /// 创建无进程内状态的 adapter。
    pub fn new() -> Self {
        Self {
            state: Arc::new(WindowsPlatformState::default()),
        }
    }

    /// 在创建窗口前设置通知与任务栏共同使用的稳定 AUMID。
    pub fn configure_process_identity(&self) -> Result<(), PlatformError> {
        let aumid = HSTRING::from(PRODUCT_IDENTITY.aumid);
        // SAFETY: HSTRING 在同步 Shell 调用期间保持有效。
        unsafe { SetCurrentProcessExplicitAppUserModelID(PCWSTR(aumid.as_ptr())) }
            .map_err(|error| PlatformError::new("set_process_aumid", error.to_string()))?;
        self.register_notification_identity()
    }

    fn register_notification_identity(&self) -> Result<(), PlatformError> {
        let executable = std::env::current_exe().map_err(|error| {
            PlatformError::new("resolve_notification_executable", error.to_string())
        })?;
        let current_user = RegKey::predef(HKEY_CURRENT_USER);
        let (identity, _) = current_user
            .create_subkey(format!(
                r"Software\Classes\AppUserModelId\{}",
                PRODUCT_IDENTITY.aumid
            ))
            .map_err(|error| {
                PlatformError::new("register_notification_identity", error.to_string())
            })?;
        identity
            .set_value("DisplayName", &PRODUCT_IDENTITY.product_name)
            .map_err(|error| {
                PlatformError::new("register_notification_identity", error.to_string())
            })?;

        // Compat 桌面通知身份要求 CLSID 指向当前 EXE；协议动作不会调用这个入口。
        let (activator, _) = current_user
            .create_subkey(format!(
                r"Software\Classes\CLSID\{TOAST_ACTIVATOR_CLSID}\LocalServer32"
            ))
            .map_err(|error| {
                PlatformError::new("register_notification_activator", error.to_string())
            })?;
        activator
            .set_value(
                "",
                &format!("\"{}\" --toast-activated", executable.display()),
            )
            .map_err(|error| {
                PlatformError::new("register_notification_activator", error.to_string())
            })?;

        let protocol_path = format!(r"Software\Classes\{}", PRODUCT_IDENTITY.protocol);
        let (protocol, _) = current_user
            .create_subkey(&protocol_path)
            .map_err(|error| {
                PlatformError::new("register_notification_protocol", error.to_string())
            })?;
        protocol
            .set_value(
                "",
                &format!("URL:{} Protocol", PRODUCT_IDENTITY.product_name),
            )
            .and_then(|_| protocol.set_value("URL Protocol", &""))
            .map_err(|error| {
                PlatformError::new("register_notification_protocol", error.to_string())
            })?;
        let (command, _) = current_user
            .create_subkey(format!(r"{protocol_path}\shell\open\command"))
            .map_err(|error| {
                PlatformError::new("register_notification_protocol", error.to_string())
            })?;
        command
            .set_value("", &format!("\"{}\" \"%1\"", executable.display()))
            .map_err(|error| {
                PlatformError::new("register_notification_protocol", error.to_string())
            })?;
        let executable_text = executable.to_string_lossy().into_owned();
        let shortcut = Self::notification_shortcut_path()?;
        let registered_executable = identity.get_value::<String, _>("ExecutablePath").ok();
        if registered_executable.as_deref() != Some(executable_text.as_str()) || !shortcut.exists()
        {
            Self::ensure_notification_shortcut(&executable)?;
            identity
                .set_value("ExecutablePath", &executable_text)
                .map_err(|error| {
                    PlatformError::new("register_notification_identity", error.to_string())
                })?;
        }
        Ok(())
    }

    fn notification_shortcut_path() -> Result<PathBuf, PlatformError> {
        let app_data = std::env::var_os("APPDATA")
            .ok_or_else(|| PlatformError::new("resolve_notification_shortcut", "APPDATA 未配置"))?;
        Ok(PathBuf::from(app_data)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join(format!("{}.lnk", PRODUCT_IDENTITY.product_name)))
    }

    fn ensure_notification_shortcut(executable: &Path) -> Result<(), PlatformError> {
        let shortcut = Self::notification_shortcut_path()?;
        let parent = shortcut.parent().ok_or_else(|| {
            PlatformError::new("resolve_notification_shortcut", "开始菜单路径无父目录")
        })?;
        fs::create_dir_all(parent).map_err(|error| {
            PlatformError::new("create_notification_shortcut_directory", error.to_string())
        })?;

        let executable_wide = wide_path(executable)?;
        let shortcut_wide = wide_path(&shortcut)?;
        let description = wide(&format!("{} 桌面便签", PRODUCT_IDENTITY.product_name));
        let working_directory = executable.parent().map(wide_path).transpose()?;

        // SAFETY: WinRT STA 已在调用前初始化；所有 PCWSTR 缓冲区在同步 COM 调用期间保持有效。
        unsafe {
            let shell_link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
                .map_err(|error| {
                    PlatformError::new("create_notification_shortcut", error.to_string())
                })?;
            shell_link
                .SetPath(PCWSTR(executable_wide.as_ptr()))
                .and_then(|_| shell_link.SetDescription(PCWSTR(description.as_ptr())))
                .map_err(|error| {
                    PlatformError::new("configure_notification_shortcut", error.to_string())
                })?;
            if let Some(working_directory) = working_directory.as_ref() {
                shell_link
                    .SetWorkingDirectory(PCWSTR(working_directory.as_ptr()))
                    .map_err(|error| {
                        PlatformError::new("configure_notification_shortcut", error.to_string())
                    })?;
            }

            let property_store: IPropertyStore = shell_link.cast().map_err(|error| {
                PlatformError::new("open_notification_shortcut_properties", error.to_string())
            })?;
            let aumid = PROPVARIANT::from(PRODUCT_IDENTITY.aumid);
            let activator = PROPVARIANT::from(TOAST_ACTIVATOR_CLSID);
            property_store
                .SetValue(&APP_USER_MODEL_ID_KEY, &aumid)
                .and_then(|_| property_store.SetValue(&TOAST_ACTIVATOR_CLSID_KEY, &activator))
                .and_then(|_| property_store.Commit())
                .map_err(|error| {
                    PlatformError::new("write_notification_shortcut_identity", error.to_string())
                })?;

            let persist_file: IPersistFile = shell_link.cast().map_err(|error| {
                PlatformError::new("persist_notification_shortcut", error.to_string())
            })?;
            persist_file
                .Save(PCWSTR(shortcut_wide.as_ptr()), true)
                .map_err(|error| {
                    PlatformError::new("persist_notification_shortcut", error.to_string())
                })?;
        }
        Ok(())
    }

    fn notification_notifier() -> Result<windows::UI::Notifications::ToastNotifier, PlatformError> {
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(PRODUCT_IDENTITY.aumid))
            .map_err(|error| PlatformError::new("create_notification_notifier", error.to_string()))
    }

    #[allow(clippy::too_many_arguments)]
    fn upsert_notification(
        reminder_id: uuid::Uuid,
        trigger_version: u64,
        scheduled_at_ms: i64,
        title: &str,
        body: &str,
        snooze_minutes: u16,
        open_activation_key: &str,
        snooze_activation_key: &str,
    ) -> Result<(), PlatformError> {
        let notifier = Self::notification_notifier()?;
        let group = notification_group(reminder_id);
        let tag = encode_trigger_tag(trigger_version);
        let scheduled = notifier.GetScheduledToastNotifications().map_err(|error| {
            PlatformError::new("list_scheduled_notifications", error.to_string())
        })?;
        let mut already_present = false;
        let size = scheduled.Size().map_err(|error| {
            PlatformError::new("list_scheduled_notifications", error.to_string())
        })?;
        for index in 0..size {
            let item = scheduled.GetAt(index).map_err(|error| {
                PlatformError::new("read_scheduled_notification", error.to_string())
            })?;
            if item
                .Group()
                .map(|value| value.to_string())
                .unwrap_or_default()
                == group
                && item
                    .Tag()
                    .map(|value| value.to_string())
                    .unwrap_or_default()
                    == tag
            {
                already_present = true;
            }
        }
        if already_present {
            return Ok(());
        }

        let open_activation = ReminderActivation {
            activation_key: open_activation_key.to_owned(),
            reminder_id,
            trigger_version,
            action: ReminderActivationAction::Open,
            snooze_minutes: None,
        };
        let snooze_activation = ReminderActivation {
            activation_key: snooze_activation_key.to_owned(),
            reminder_id,
            trigger_version,
            action: ReminderActivationAction::Snooze,
            snooze_minutes: Some(snooze_minutes),
        };
        let open_uri = xml_escape(&reminder_protocol_uri(&open_activation));
        let snooze_uri = xml_escape(&reminder_protocol_uri(&snooze_activation));
        let xml = format!(
            "<toast activationType=\"protocol\" launch=\"{open_uri}\">\
             <visual><binding template=\"ToastGeneric\">\
             <text>{}</text><text>{}</text>\
             </binding></visual><actions>\
             <action content=\"稍后提醒（{snooze_minutes} 分钟）\" activationType=\"protocol\" arguments=\"{snooze_uri}\"/>\
             <action content=\"打开\" activationType=\"protocol\" arguments=\"{open_uri}\"/>\
             </actions></toast>",
            xml_escape(title),
            xml_escape(body)
        );
        let document = XmlDocument::new()
            .map_err(|error| PlatformError::new("create_notification_xml", error.to_string()))?;
        document
            .LoadXml(&HSTRING::from(xml))
            .map_err(|error| PlatformError::new("load_notification_xml", error.to_string()))?;
        let delivery_time = unix_ms_to_windows_datetime(scheduled_at_ms)?;
        let notification =
            ScheduledToastNotification::CreateScheduledToastNotification(&document, delivery_time)
                .map_err(|error| {
                    PlatformError::new("create_scheduled_notification", error.to_string())
                })?;
        // 官方取消示例只依赖 Tag + Group；不设置额外的 16 字符 Id 可避免旧通知平台限制。
        notification
            .SetGroup(&HSTRING::from(group))
            .map_err(|error| {
                PlatformError::new("group_scheduled_notification", error.to_string())
            })?;
        notification
            .SetTag(&HSTRING::from(tag))
            .map_err(|error| PlatformError::new("tag_scheduled_notification", error.to_string()))?;
        notifier
            .AddToSchedule(&notification)
            .map_err(|error| PlatformError::new("schedule_notification", error.to_string()))
    }

    fn cancel_notification(
        reminder_id: uuid::Uuid,
        trigger_version: u64,
    ) -> Result<(), PlatformError> {
        let notifier = Self::notification_notifier()?;
        let group = notification_group(reminder_id);
        let tag = encode_trigger_tag(trigger_version);
        let scheduled = notifier.GetScheduledToastNotifications().map_err(|error| {
            PlatformError::new("list_scheduled_notifications", error.to_string())
        })?;
        let size = scheduled.Size().map_err(|error| {
            PlatformError::new("list_scheduled_notifications", error.to_string())
        })?;
        let mut matches = Vec::new();
        for index in 0..size {
            let item = scheduled.GetAt(index).map_err(|error| {
                PlatformError::new("read_scheduled_notification", error.to_string())
            })?;
            if item
                .Group()
                .map(|value| value.to_string())
                .unwrap_or_default()
                == group
                && item
                    .Tag()
                    .map(|value| value.to_string())
                    .unwrap_or_default()
                    == tag
            {
                matches.push(item);
            }
        }
        for item in matches {
            notifier
                .RemoveFromSchedule(&item)
                .map_err(|error| PlatformError::new("cancel_notification", error.to_string()))?;
        }
        Ok(())
    }

    fn replace_global_shortcut(&self, shortcut: GlobalShortcut) -> Result<(), PlatformError> {
        let handler = self
            .state
            .activation_handler
            .lock()
            .map_err(|error| PlatformError::new("read_activation_handler", error.to_string()))?
            .clone()
            .ok_or_else(|| {
                PlatformError::new("register_global_shortcut", "主实例激活处理器尚未就绪")
            })?;
        let mut hotkey = self
            .state
            .hotkey_thread
            .lock()
            .map_err(|error| PlatformError::new("lock_global_shortcut", error.to_string()))?;
        if hotkey.is_none() {
            *hotkey = Some(HotkeyThread::start(handler)?);
        }
        hotkey.as_ref().expect("快捷键线程已创建").replace(shortcut)
    }

    fn clear_global_shortcut(&self) -> Result<(), PlatformError> {
        let hotkey = self
            .state
            .hotkey_thread
            .lock()
            .map_err(|error| PlatformError::new("lock_global_shortcut", error.to_string()))?;
        if let Some(hotkey) = hotkey.as_ref() {
            hotkey.clear()?;
        }
        Ok(())
    }

    fn set_startup_enabled(&self, enabled: bool) -> Result<(), PlatformError> {
        let current_user = RegKey::predef(HKEY_CURRENT_USER);
        let run_key_path = r"Software\Microsoft\Windows\CurrentVersion\Run";
        if enabled {
            let (run_key, _) = current_user
                .create_subkey(run_key_path)
                .map_err(|error| PlatformError::new("enable_startup", error.to_string()))?;
            let executable = std::env::current_exe().map_err(|error| {
                PlatformError::new("resolve_startup_executable", error.to_string())
            })?;
            let command = format!("\"{}\" --startup", executable.display());
            run_key
                .set_value(PRODUCT_IDENTITY.product_name, &command)
                .map_err(|error| PlatformError::new("enable_startup", error.to_string()))
        } else {
            let run_key = current_user
                .open_subkey_with_flags(run_key_path, KEY_SET_VALUE)
                .map_err(|error| PlatformError::new("disable_startup", error.to_string()))?;
            match run_key.delete_value(PRODUCT_IDENTITY.product_name) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(PlatformError::new("disable_startup", error.to_string())),
            }
        }
    }

    fn open_external_link(url: &str) -> Result<(), PlatformError> {
        let operation = wide("open");
        let target = wide(url);
        // SAFETY: 所有字符串在同步 ShellExecuteW 调用期间保持 NUL 结尾且有效。
        let result = unsafe {
            ShellExecuteW(
                None,
                PCWSTR(operation.as_ptr()),
                PCWSTR(target.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        if result.0 as isize <= 32 {
            return Err(PlatformError::new(
                "open_external_link",
                format!("ShellExecuteW 返回错误代码 {}", result.0 as isize),
            ));
        }
        Ok(())
    }
}

impl Default for WindowsPlatformServices {
    fn default() -> Self {
        Self::new()
    }
}

impl PlatformServices for WindowsPlatformServices {
    fn data_directory(&self) -> Result<PathBuf, PlatformError> {
        let local_app_data = std::env::var_os("LOCALAPPDATA")
            .ok_or_else(|| PlatformError::new("resolve_data_directory", "LOCALAPPDATA 不可用"))?;
        Ok(PathBuf::from(local_app_data).join(PRODUCT_IDENTITY.product_name))
    }

    fn acquire_single_instance(
        &self,
        initial_activation: ActivationRequest,
        on_activation: ActivationHandler,
    ) -> Result<InstanceRole, PlatformError> {
        let mutex_name = wide(MUTEX_NAME);
        // SAFETY: 名称以 NUL 结尾，安全属性为空，句柄由租约或当前分支关闭。
        let mutex =
            unsafe { CreateMutexW(None, false, PCWSTR(mutex_name.as_ptr())) }.map_err(|error| {
                PlatformError::new("create_single_instance_mutex", error.to_string())
            })?;
        // SAFETY: CreateMutexW 按约定用 last-error 报告对象已经存在。
        let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        if already_exists {
            // SAFETY: 当前分支不再使用该有效句柄。
            let _ = unsafe { CloseHandle(mutex) };
            send_activation(&initial_activation)?;
            return Ok(InstanceRole::SecondaryForwarded);
        }

        *self
            .state
            .activation_handler
            .lock()
            .map_err(|error| PlatformError::new("store_activation_handler", error.to_string()))? =
            Some(on_activation.clone());

        let (ready_sender, ready_receiver) = std::sync::mpsc::sync_channel(1);
        let listener = thread::Builder::new()
            .name("quicknote-activation-pipe".to_owned())
            .spawn(move || listen_for_activations(on_activation, ready_sender))
            .map_err(|error| {
                // SAFETY: 线程未启动成功，当前分支仍拥有句柄。
                let _ = unsafe { CloseHandle(mutex) };
                PlatformError::new("start_activation_listener", error.to_string())
            })?;
        ready_receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .map_err(|error| {
                PlatformError::new("start_activation_listener", error.to_string())
            })??;

        Ok(InstanceRole::Primary(Box::new(WindowsInstanceLease {
            mutex,
            listener: Some(listener),
        })))
    }

    fn apply(&self, command: PlatformCommand) -> Result<(), PlatformError> {
        match command {
            PlatformCommand::SetGlobalShortcut { shortcut } => {
                self.replace_global_shortcut(shortcut)
            }
            PlatformCommand::ClearGlobalShortcut => self.clear_global_shortcut(),
            PlatformCommand::SetStartupEnabled { enabled } => self.set_startup_enabled(enabled),
            PlatformCommand::OpenExternalLink { url } => Self::open_external_link(&url),
            PlatformCommand::UpsertNotification {
                reminder_id,
                trigger_version,
                note_id: _,
                scheduled_at_ms,
                title,
                body,
                snooze_minutes,
                open_activation_key,
                snooze_activation_key,
            } => Self::upsert_notification(
                reminder_id,
                trigger_version,
                scheduled_at_ms,
                &title,
                &body,
                snooze_minutes,
                &open_activation_key,
                &snooze_activation_key,
            ),
            PlatformCommand::CancelNotification {
                reminder_id,
                trigger_version,
            } => Self::cancel_notification(reminder_id, trigger_version),
            // 托盘由后续纵向切片实现。
            other => Err(PlatformError::new(
                "apply_platform_command",
                format!("平台命令尚未接入生产实现：{other:?}"),
            )),
        }
    }

    fn scheduled_notifications(&self) -> Result<Vec<NotificationProjectionKey>, PlatformError> {
        let notifier = Self::notification_notifier()?;
        let scheduled = notifier.GetScheduledToastNotifications().map_err(|error| {
            PlatformError::new("list_scheduled_notifications", error.to_string())
        })?;
        let size = scheduled.Size().map_err(|error| {
            PlatformError::new("list_scheduled_notifications", error.to_string())
        })?;
        let mut result = Vec::new();
        for index in 0..size {
            let item = scheduled.GetAt(index).map_err(|error| {
                PlatformError::new("read_scheduled_notification", error.to_string())
            })?;
            let group = item
                .Group()
                .map(|value| value.to_string())
                .unwrap_or_default();
            let Some(reminder_id) = uuid::Uuid::parse_str(&group).ok() else {
                continue;
            };
            let Some(trigger_version) = item
                .Tag()
                .ok()
                .and_then(|value| decode_trigger_tag(&value.to_string()))
            else {
                continue;
            };
            result.push(NotificationProjectionKey {
                reminder_id,
                trigger_version,
            });
        }
        Ok(result)
    }

    fn write_file_atomically(&self, target: &Path, contents: &[u8]) -> Result<(), PlatformError> {
        let parent = target
            .parent()
            .ok_or_else(|| PlatformError::new("prepare_atomic_export", "导出目标缺少父目录"))?;
        fs::create_dir_all(parent)
            .map_err(|error| PlatformError::new("create_export_directory", error.to_string()))?;
        let file_name = target
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| PlatformError::new("prepare_atomic_export", "导出文件名无效"))?;
        let temporary = parent.join(format!(
            ".{file_name}.{}.{}.tmp",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        let result = (|| -> Result<(), PlatformError> {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .map_err(|error| {
                    PlatformError::new("create_export_temporary", error.to_string())
                })?;
            file.write_all(contents)
                .and_then(|_| file.sync_all())
                .map_err(|error| PlatformError::new("write_export_temporary", error.to_string()))?;
            drop(file);

            let source = wide_path(&temporary)?;
            let destination = wide_path(target)?;
            // SAFETY: 路径在同步调用期间保持 NUL 结尾；标志提供替换与落盘语义。
            unsafe {
                MoveFileExW(
                    PCWSTR(source.as_ptr()),
                    PCWSTR(destination.as_ptr()),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            }
            .map_err(|error| PlatformError::new("replace_export_target", error.to_string()))
        })();
        if result.is_err() {
            // 临时路径由本次调用唯一创建，失败清理不会修改既有目标。
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

struct HotkeyThread {
    commands: mpsc::Sender<HotkeyCommand>,
    thread_id: u32,
    worker: Option<JoinHandle<()>>,
}

enum HotkeyCommand {
    Replace {
        shortcut: GlobalShortcut,
        respond_to: mpsc::Sender<Result<(), PlatformError>>,
    },
    Clear {
        respond_to: mpsc::Sender<Result<(), PlatformError>>,
    },
    Shutdown,
}

#[derive(Clone, Copy)]
struct RegisteredHotkey {
    id: i32,
    shortcut: GlobalShortcut,
}

impl HotkeyThread {
    fn start(handler: ActivationHandler) -> Result<Self, PlatformError> {
        let (commands, receiver) = mpsc::channel();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("quicknote-global-hotkey".to_owned())
            .spawn(move || {
                // PeekMessageW 强制创建线程消息队列，之后 PostThreadMessageW 才可靠。
                let thread_id = unsafe { GetCurrentThreadId() };
                let mut message = MSG::default();
                // SAFETY: message 在同步调用期间有效，空 HWND 表示当前线程队列。
                let _ = unsafe { PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE) };
                let _ = ready_sender.send(thread_id);
                run_hotkey_loop(receiver, handler);
            })
            .map_err(|error| PlatformError::new("start_global_hotkey", error.to_string()))?;
        let thread_id = ready_receiver
            .recv_timeout(HOTKEY_RESPONSE_TIMEOUT)
            .map_err(|error| PlatformError::new("start_global_hotkey", error.to_string()))?;
        Ok(Self {
            commands,
            thread_id,
            worker: Some(worker),
        })
    }

    fn replace(&self, shortcut: GlobalShortcut) -> Result<(), PlatformError> {
        let (respond_to, response) = mpsc::channel();
        self.send(HotkeyCommand::Replace {
            shortcut,
            respond_to,
        })?;
        response
            .recv_timeout(HOTKEY_RESPONSE_TIMEOUT)
            .map_err(|error| PlatformError::new("register_global_shortcut", error.to_string()))?
    }

    fn clear(&self) -> Result<(), PlatformError> {
        let (respond_to, response) = mpsc::channel();
        self.send(HotkeyCommand::Clear { respond_to })?;
        response
            .recv_timeout(HOTKEY_RESPONSE_TIMEOUT)
            .map_err(|error| PlatformError::new("clear_global_shortcut", error.to_string()))?
    }

    fn send(&self, command: HotkeyCommand) -> Result<(), PlatformError> {
        self.commands
            .send(command)
            .map_err(|error| PlatformError::new("send_global_hotkey_command", error.to_string()))?;
        // SAFETY: thread_id 来自仍由 self.worker 保活的消息循环线程。
        unsafe { PostThreadMessageW(self.thread_id, HOTKEY_CONTROL_MESSAGE, WPARAM(0), LPARAM(0)) }
            .map_err(|error| PlatformError::new("wake_global_hotkey_thread", error.to_string()))
    }
}

impl Drop for HotkeyThread {
    fn drop(&mut self) {
        let _ = self.commands.send(HotkeyCommand::Shutdown);
        // SAFETY: worker 尚未回收，线程消息队列仍然存在。
        let _ = unsafe {
            PostThreadMessageW(self.thread_id, HOTKEY_CONTROL_MESSAGE, WPARAM(0), LPARAM(0))
        };
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run_hotkey_loop(receiver: mpsc::Receiver<HotkeyCommand>, handler: ActivationHandler) {
    let mut registered: Option<RegisteredHotkey> = None;
    loop {
        let mut message = MSG::default();
        // SAFETY: message 指向当前栈上的有效 MSG，空 HWND 读取线程消息队列。
        let result = unsafe { GetMessageW(&mut message, None, 0, 0) }.0;
        if result <= 0 {
            break;
        }
        if message.message == HOTKEY_CONTROL_MESSAGE {
            while let Ok(command) = receiver.try_recv() {
                match command {
                    HotkeyCommand::Replace {
                        shortcut,
                        respond_to,
                    } => {
                        let result = replace_registered_hotkey(&mut registered, shortcut);
                        let _ = respond_to.send(result);
                    }
                    HotkeyCommand::Clear { respond_to } => {
                        let result = clear_registered_hotkey(&mut registered);
                        let _ = respond_to.send(result);
                    }
                    HotkeyCommand::Shutdown => {
                        let _ = clear_registered_hotkey(&mut registered);
                        return;
                    }
                }
            }
        } else if message.message == WM_HOTKEY
            && registered.is_some_and(|hotkey| message.wParam.0 == hotkey.id as usize)
        {
            handler(ActivationRequest::GlobalShortcutPressed);
        }
    }
    let _ = clear_registered_hotkey(&mut registered);
}

fn replace_registered_hotkey(
    registered: &mut Option<RegisteredHotkey>,
    shortcut: GlobalShortcut,
) -> Result<(), PlatformError> {
    if registered.is_some_and(|current| current.shortcut == shortcut) {
        return Ok(());
    }
    let new_id = match registered {
        Some(current) if current.id == 0x5101 => 0x5102,
        _ => 0x5101,
    };
    // SAFETY: 线程消息队列已经创建；ID 只在该线程内使用。
    unsafe {
        RegisterHotKey(
            None,
            new_id,
            hotkey_modifiers(shortcut),
            shortcut_virtual_key(shortcut.key()),
        )
    }
    .map_err(|error| {
        PlatformError::new(
            "register_global_shortcut",
            format!("{} 注册失败，可能已被其他应用占用：{error}", shortcut),
        )
    })?;

    if let Some(old) = registered {
        // 先注册新组合再移除旧组合，冲突时旧快捷键始终保留。
        if let Err(error) = unsafe { UnregisterHotKey(None, old.id) } {
            let _ = unsafe { UnregisterHotKey(None, new_id) };
            return Err(PlatformError::new(
                "replace_global_shortcut",
                format!("旧快捷键未能安全移除：{error}"),
            ));
        }
    }
    *registered = Some(RegisteredHotkey {
        id: new_id,
        shortcut,
    });
    Ok(())
}

fn clear_registered_hotkey(registered: &mut Option<RegisteredHotkey>) -> Result<(), PlatformError> {
    if let Some(current) = *registered {
        // SAFETY: current.id 由当前线程成功注册且尚未移除。
        unsafe { UnregisterHotKey(None, current.id) }
            .map_err(|error| PlatformError::new("clear_global_shortcut", error.to_string()))?;
        *registered = None;
    }
    Ok(())
}

fn hotkey_modifiers(shortcut: GlobalShortcut) -> HOT_KEY_MODIFIERS {
    let modifiers = shortcut.modifiers();
    let mut bits = MOD_NOREPEAT.0;
    if modifiers.ctrl() {
        bits |= MOD_CONTROL.0;
    }
    if modifiers.alt() {
        bits |= MOD_ALT.0;
    }
    if modifiers.shift() {
        bits |= MOD_SHIFT.0;
    }
    HOT_KEY_MODIFIERS(bits)
}

fn shortcut_virtual_key(key: ShortcutKey) -> u32 {
    match key {
        ShortcutKey::Letter(value) => value as u32,
        ShortcutKey::Digit(value) => u32::from(b'0' + value),
        ShortcutKey::Function(value) => 0x70 + u32::from(value - 1),
        ShortcutKey::Space => 0x20,
        ShortcutKey::Tab => 0x09,
        ShortcutKey::Enter => 0x0D,
        ShortcutKey::Escape => 0x1B,
        ShortcutKey::Backspace => 0x08,
        ShortcutKey::Insert => 0x2D,
        ShortcutKey::Delete => 0x2E,
        ShortcutKey::Home => 0x24,
        ShortcutKey::End => 0x23,
        ShortcutKey::PageUp => 0x21,
        ShortcutKey::PageDown => 0x22,
        ShortcutKey::Left => 0x25,
        ShortcutKey::Right => 0x27,
        ShortcutKey::Up => 0x26,
        ShortcutKey::Down => 0x28,
        ShortcutKey::Semicolon => 0xBA,
        ShortcutKey::Equals => 0xBB,
        ShortcutKey::Comma => 0xBC,
        ShortcutKey::Minus => 0xBD,
        ShortcutKey::Period => 0xBE,
        ShortcutKey::Slash => 0xBF,
        ShortcutKey::Backtick => 0xC0,
        ShortcutKey::LeftBracket => 0xDB,
        ShortcutKey::Backslash => 0xDC,
        ShortcutKey::RightBracket => 0xDD,
        ShortcutKey::Quote => 0xDE,
    }
}

struct WindowsInstanceLease {
    mutex: HANDLE,
    listener: Option<JoinHandle<()>>,
}

// Windows 内核句柄可跨线程关闭；租约自身不在监听线程中使用该句柄。
unsafe impl Send for WindowsInstanceLease {}
impl InstanceLease for WindowsInstanceLease {}

impl Drop for WindowsInstanceLease {
    fn drop(&mut self) {
        let _ = send_pipe_message(SHUTDOWN_MESSAGE);
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
        // SAFETY: 租约唯一拥有该互斥量句柄。
        let _ = unsafe { CloseHandle(self.mutex) };
    }
}

fn listen_for_activations(
    on_activation: ActivationHandler,
    ready: std::sync::mpsc::SyncSender<Result<(), PlatformError>>,
) {
    let mut ready = Some(ready);
    loop {
        let pipe_name = wide(PIPE_NAME);
        // SAFETY: 名称以 NUL 结尾；返回句柄在每次循环末尾关闭。
        let pipe = unsafe {
            CreateNamedPipeW(
                PCWSTR(pipe_name.as_ptr()),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                MAX_ACTIVATION_BYTES as u32,
                MAX_ACTIVATION_BYTES as u32,
                0,
                None,
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            if let Some(ready) = ready.take() {
                // SAFETY: 读取紧邻失败 Win32 调用的错误码。
                let error = unsafe { GetLastError() };
                let _ = ready.send(Err(PlatformError::new(
                    "create_activation_pipe",
                    format!("Win32 error {}", error.0),
                )));
            }
            return;
        }
        if let Some(ready) = ready.take() {
            let _ = ready.send(Ok(()));
        }

        // SAFETY: pipe 是当前线程拥有的有效命名管道句柄。
        let connected = match unsafe { ConnectNamedPipe(pipe, None) } {
            Ok(()) => true,
            Err(_) => {
                // SAFETY: 读取紧邻 ConnectNamedPipe 的错误码。
                (unsafe { GetLastError() }) == ERROR_PIPE_CONNECTED
            }
        };
        if connected {
            let mut buffer = vec![0_u8; MAX_ACTIVATION_BYTES];
            let mut bytes_read = 0_u32;
            // SAFETY: 缓冲区在同步读取期间保持有效。
            if unsafe { ReadFile(pipe, Some(&mut buffer), Some(&mut bytes_read), None) }.is_ok() {
                buffer.truncate(bytes_read as usize);
                if buffer == SHUTDOWN_MESSAGE {
                    // SAFETY: 在关闭前先结束当前客户端连接。
                    let _ = unsafe { DisconnectNamedPipe(pipe) };
                    let _ = unsafe { CloseHandle(pipe) };
                    return;
                }
                if let Ok(activation) = serde_json::from_slice::<ActivationRequest>(&buffer) {
                    on_activation(activation);
                }
            }
            // SAFETY: 同步客户端处理完成，允许下一实例连接。
            let _ = unsafe { DisconnectNamedPipe(pipe) };
        }
        // SAFETY: 当前循环唯一拥有 pipe。
        let _ = unsafe { CloseHandle(pipe) };
    }
}

fn send_activation(activation: &ActivationRequest) -> Result<(), PlatformError> {
    let payload = serde_json::to_vec(activation)
        .map_err(|error| PlatformError::new("encode_activation", error.to_string()))?;
    send_pipe_message(&payload)
}

fn send_pipe_message(payload: &[u8]) -> Result<(), PlatformError> {
    let pipe_name = wide(PIPE_NAME);
    // SAFETY: 名称以 NUL 结尾；最多等待主实例五秒创建管道。
    if !unsafe { WaitNamedPipeW(PCWSTR(pipe_name.as_ptr()), 5_000) }.as_bool() {
        return Err(PlatformError::new(
            "wait_for_primary_instance",
            "主实例激活管道未就绪",
        ));
    }
    // SAFETY: 参数遵循命名管道客户端的同步打开约定。
    let pipe = unsafe {
        CreateFileW(
            PCWSTR(pipe_name.as_ptr()),
            GENERIC_READ.0 | GENERIC_WRITE.0,
            FILE_SHARE_MODE(0),
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(|error| PlatformError::new("open_activation_pipe", error.to_string()))?;
    let mut bytes_written = 0_u32;
    // SAFETY: payload 在同步写入期间保持有效。
    let result = unsafe { WriteFile(pipe, Some(payload), Some(&mut bytes_written), None) }
        .map_err(|error| PlatformError::new("forward_activation", error.to_string()));
    // SAFETY: 客户端当前分支唯一拥有 pipe。
    let _ = unsafe { CloseHandle(pipe) };
    result?;
    if bytes_written as usize != payload.len() {
        return Err(PlatformError::new(
            "forward_activation",
            "激活消息未完整写入",
        ));
    }
    Ok(())
}

fn notification_group(reminder_id: uuid::Uuid) -> String {
    reminder_id.simple().to_string()
}

fn encode_trigger_tag(mut trigger_version: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if trigger_version == 0 {
        return "0".to_owned();
    }
    let mut encoded = Vec::new();
    while trigger_version > 0 {
        encoded.push(char::from(DIGITS[(trigger_version % 36) as usize]));
        trigger_version /= 36;
    }
    encoded.into_iter().rev().collect()
}

fn decode_trigger_tag(value: &str) -> Option<u64> {
    value.chars().try_fold(0_u64, |result, character| {
        character
            .to_digit(36)
            .and_then(|digit| result.checked_mul(36)?.checked_add(u64::from(digit)))
    })
}

fn unix_ms_to_windows_datetime(timestamp_ms: i64) -> Result<DateTime, PlatformError> {
    let ticks = timestamp_ms
        .checked_mul(10_000)
        .and_then(|value| value.checked_add(WINDOWS_EPOCH_OFFSET_100NS))
        .ok_or_else(|| {
            PlatformError::new("encode_notification_time", "提醒时间超出 Windows 范围")
        })?;
    Ok(DateTime {
        UniversalTime: ticks,
    })
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide_path(path: &Path) -> Result<Vec<u16>, PlatformError> {
    let value = path
        .to_str()
        .ok_or_else(|| PlatformError::new("encode_windows_path", "路径不是有效 Unicode"))?;
    Ok(wide(value))
}

#[cfg(test)]
mod tests {
    use super::{
        WINDOWS_EPOCH_OFFSET_100NS, WindowsPlatformServices, decode_trigger_tag,
        encode_trigger_tag, hotkey_modifiers, initialize_winrt_apartment, notification_group,
        shortcut_virtual_key, unix_ms_to_windows_datetime, xml_escape,
    };
    use quicknote_app::platform::{GlobalShortcut, PRODUCT_IDENTITY, ShortcutKey};
    use std::time::Duration;
    use windows::UI::Notifications::ToastNotificationManager;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT,
    };

    #[test]
    fn windows_registration_always_adds_no_repeat_and_maps_keys() {
        let default = GlobalShortcut::default();
        let modifiers = hotkey_modifiers(default).0;
        assert_ne!(modifiers & MOD_NOREPEAT.0, 0);
        assert_ne!(modifiers & MOD_CONTROL.0, 0);
        assert_ne!(modifiers & MOD_ALT.0, 0);
        assert_eq!(modifiers & MOD_SHIFT.0, 0);
        assert_eq!(shortcut_virtual_key(default.key()), u32::from(b'Q'));

        let function = GlobalShortcut::parse("Shift+F24").expect("解析功能键");
        assert_eq!(function.key(), ShortcutKey::Function(24));
        assert_eq!(shortcut_virtual_key(function.key()), 0x87);
        assert_ne!(hotkey_modifiers(function).0 & MOD_NOREPEAT.0, 0);
    }

    #[test]
    fn notification_payload_uses_windows_time_and_safe_xml() {
        assert_eq!(
            unix_ms_to_windows_datetime(0)
                .expect("编码 Unix epoch")
                .UniversalTime,
            WINDOWS_EPOCH_OFFSET_100NS
        );
        assert_eq!(xml_escape("<&\"'>"), "&lt;&amp;&quot;&apos;&gt;");
        let reminder_id =
            uuid::Uuid::parse_str("019c1234-5678-7abc-8def-0123456789ab").expect("解析提醒 UUID");
        assert_eq!(
            notification_group(reminder_id),
            "019c123456787abc8def0123456789ab"
        );
        assert_eq!(
            decode_trigger_tag(&encode_trigger_tag(u64::MAX)),
            Some(u64::MAX)
        );
        assert!(encode_trigger_tag(u64::MAX).len() <= 16);
    }

    #[test]
    #[ignore = "会向当前 Windows 用户发送一条真实系统通知"]
    fn scheduled_notification_reaches_windows_history() {
        let _apartment = initialize_winrt_apartment().expect("初始化 WinRT apartment");
        let platform = WindowsPlatformServices::new();
        platform
            .configure_process_identity()
            .expect("注册桌面通知身份");
        let reminder_id =
            uuid::Uuid::parse_str("019c20aa-bbcc-7def-8123-456789abcdef").expect("解析验收 UUID");
        let trigger_version = 20_026_u64;
        let group = notification_group(reminder_id);
        let tag = encode_trigger_tag(trigger_version);

        // 按 Windows 官方示例至少提前十秒安排，并留出五秒完成历史持久化。
        WindowsPlatformServices::upsert_notification(
            reminder_id,
            trigger_version,
            chrono::Utc::now().timestamp_millis() + 10_000,
            "QuickNote notification acceptance",
            "Windows history must retain this reminder",
            10,
            "acceptance-open",
            "acceptance-snooze",
        )
        .expect("安排真实通知");
        std::thread::sleep(Duration::from_secs(15));

        let history = ToastNotificationManager::History().expect("读取通知历史服务");
        let items = history
            .GetHistoryWithId(&windows::core::HSTRING::from(PRODUCT_IDENTITY.aumid))
            .expect("读取 QuickNote 通知历史");
        let found = (0..items.Size().expect("读取通知历史数量")).any(|index| {
            let Ok(item) = items.GetAt(index) else {
                return false;
            };
            item.Group()
                .map(|value| value.to_string())
                .unwrap_or_default()
                == group
                && item
                    .Tag()
                    .map(|value| value.to_string())
                    .unwrap_or_default()
                    == tag
        });

        // 无论断言结果如何，均尽力移除验收产生的通知或残留计划。
        let _ = history.RemoveGroupWithId(
            &windows::core::HSTRING::from(&group),
            &windows::core::HSTRING::from(PRODUCT_IDENTITY.aumid),
        );
        let _ = WindowsPlatformServices::cancel_notification(reminder_id, trigger_version);
        assert!(found, "到点通知没有进入 QuickNote 的 Windows 通知历史");
    }
}
