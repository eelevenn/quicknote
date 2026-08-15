//! `PlatformServices` 的 Windows 11 adapter。

use quicknote_app::platform::{
    ActivationHandler, ActivationRequest, GlobalShortcut, InstanceLease, InstanceRole,
    PRODUCT_IDENTITY, PlatformCommand, PlatformError, PlatformServices, ShortcutKey,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE,
    GetLastError, HANDLE, INVALID_HANDLE_VALUE, LPARAM, WPARAM,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_MODE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
    ReadFile, WriteFile,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_MESSAGE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    WaitNamedPipeW,
};
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey,
    UnregisterHotKey,
};
use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
use windows::Win32::UI::WindowsAndMessaging::{
    GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, WM_APP, WM_HOTKEY,
};
use windows::core::{HSTRING, PCWSTR};

const MUTEX_NAME: &str = "Local\\eelevenn.QuickNote.SingleInstance.v1";
const PIPE_NAME: &str = r"\\.\pipe\eelevenn.quicknote.activation.v1";
const SHUTDOWN_MESSAGE: &[u8] = b"__quicknote_shutdown__";
const MAX_ACTIVATION_BYTES: usize = 64 * 1024;
const HOTKEY_CONTROL_MESSAGE: u32 = WM_APP + 17;
const HOTKEY_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

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
            .map_err(|error| PlatformError::new("set_process_aumid", error.to_string()))
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
            // 提醒、托盘和自启动仍由各自后续纵向切片实现。
            other => Err(PlatformError::new(
                "apply_platform_command",
                format!("平台命令尚未接入生产实现：{other:?}"),
            )),
        }
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

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::{hotkey_modifiers, shortcut_virtual_key};
    use quicknote_app::platform::{GlobalShortcut, ShortcutKey};
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
}
