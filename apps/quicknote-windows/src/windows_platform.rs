//! `PlatformServices` 的 Windows 11 adapter。

use quicknote_app::platform::{
    ActivationHandler, ActivationRequest, InstanceLease, InstanceRole, PRODUCT_IDENTITY,
    PlatformCommand, PlatformError, PlatformServices,
};
use std::path::PathBuf;
use std::thread::{self, JoinHandle};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE,
    GetLastError, HANDLE, INVALID_HANDLE_VALUE,
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
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
use windows::core::{HSTRING, PCWSTR};

const MUTEX_NAME: &str = "Local\\eelevenn.QuickNote.SingleInstance.v1";
const PIPE_NAME: &str = r"\\.\pipe\eelevenn.quicknote.activation.v1";
const SHUTDOWN_MESSAGE: &[u8] = b"__quicknote_shutdown__";
const MAX_ACTIVATION_BYTES: usize = 64 * 1024;

/// 使用稳定 AUMID、协议和本地数据目录的生产 Windows adapter。
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowsPlatformServices;

impl WindowsPlatformServices {
    /// 创建无进程内状态的 adapter。
    pub fn new() -> Self {
        Self
    }

    /// 在创建窗口前设置通知与任务栏共同使用的稳定 AUMID。
    pub fn configure_process_identity(&self) -> Result<(), PlatformError> {
        let aumid = HSTRING::from(PRODUCT_IDENTITY.aumid);
        // SAFETY: HSTRING 在同步 Shell 调用期间保持有效。
        unsafe { SetCurrentProcessExplicitAppUserModelID(PCWSTR(aumid.as_ptr())) }
            .map_err(|error| PlatformError::new("set_process_aumid", error.to_string()))
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
        // #17 只建立 seam；具体快捷键、通知、托盘和自启动在对应纵向切片实现。
        Err(PlatformError::new(
            "apply_platform_command",
            format!("平台命令尚未接入生产实现：{command:?}"),
        ))
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
