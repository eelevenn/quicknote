use serde::{Deserialize, Serialize};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_PIPE_CONNECTED, GetLastError, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FlushFileBuffers, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey, UnregisterHotKey,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, HWND_MESSAGE, MSG,
    PostMessageW, PostThreadMessageW, RegisterClassW, WM_APP, WM_HOTKEY, WM_POWERBROADCAST,
    WNDCLASSW,
};

const HOTKEY_ID: i32 = 0x514E;
const WM_REBIND_HOTKEY: u32 = WM_APP + 0x514;
const PBT_APMRESUMEAUTOMATIC: usize = 0x0012;

pub struct SingleInstanceGuard(*mut c_void);

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        // SAFETY: The guard exclusively owns this mutex handle.
        unsafe { CloseHandle(self.0) };
    }
}

pub fn acquire_single_instance(candidate: &str) -> Result<SingleInstanceGuard, String> {
    let name = format!("Local\\QuickNote.StackBenchmark.{candidate}");
    let wide_name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: Security attributes are null and the name is NUL-terminated.
    let handle = unsafe { CreateMutexW(std::ptr::null(), 1, wide_name.as_ptr()) };
    if handle.is_null() {
        return Err(format!("CreateMutexW failed: {}", unsafe {
            GetLastError()
        }));
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe { CloseHandle(handle) };
        return Err("Another benchmark instance is already running.".to_owned());
    }
    Ok(SingleInstanceGuard(handle))
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct PipeRequest {
    pub id: Option<String>,
    pub command: Option<String>,
    pub value: Option<String>,
}

/// Sends one request to a running candidate through the shared benchmark pipe.
pub fn send_pipe_request(
    candidate: &str,
    request: &PipeRequest,
) -> Result<BenchmarkStatus, String> {
    let pipe_name = format!(r"\\.\pipe\quicknote-stack-{candidate}");
    let wide_name: Vec<u16> = pipe_name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: The pipe path is NUL-terminated and no optional security pointer is used.
    let pipe = unsafe {
        CreateFileW(
            wide_name.as_ptr(),
            FILE_GENERIC_READ | FILE_GENERIC_WRITE,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if pipe == INVALID_HANDLE_VALUE {
        return Err(format!("Open named pipe failed: {}", unsafe {
            GetLastError()
        }));
    }

    let result = (|| {
        let mut payload = serde_json::to_vec(request).map_err(|error| error.to_string())?;
        payload.push(b'\n');
        let mut written = 0_u32;
        // SAFETY: The buffer remains valid during this synchronous write.
        if unsafe {
            WriteFile(
                pipe,
                payload.as_ptr(),
                payload.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(format!("Write named pipe failed: {}", unsafe {
                GetLastError()
            }));
        }

        let mut buffer = [0_u8; 8192];
        let mut read = 0_u32;
        // SAFETY: The buffer and byte count point to valid writable memory.
        if unsafe {
            ReadFile(
                pipe,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                &mut read,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(format!("Read named pipe failed: {}", unsafe {
                GetLastError()
            }));
        }
        serde_json::from_slice::<BenchmarkStatus>(&buffer[..read as usize])
            .map_err(|error| error.to_string())
    })();
    // SAFETY: This call releases the handle created above exactly once.
    unsafe { CloseHandle(pipe) };
    result
}

#[derive(Clone, Serialize, Deserialize)]
pub struct BenchmarkStatus {
    pub id: Option<String>,
    pub ok: bool,
    pub candidate: String,
    pub pid: u32,
    #[serde(rename = "event")]
    pub event_name: String,
    pub frequency: i64,
    #[serde(rename = "processStartTicks")]
    pub process_start_ticks: i64,
    #[serde(rename = "hotkeyReceivedTicks")]
    pub hotkey_received_ticks: i64,
    #[serde(rename = "windowVisibleTicks")]
    pub window_visible_ticks: i64,
    #[serde(rename = "editorFocusedTicks")]
    pub editor_focused_ticks: i64,
    #[serde(rename = "sentinelAcceptedTicks")]
    pub sentinel_accepted_ticks: i64,
    #[serde(rename = "showSequence")]
    pub show_sequence: u64,
    #[serde(rename = "hotkeyRegistered")]
    pub hotkey_registered: bool,
    #[serde(rename = "hotkeySpec")]
    pub hotkey_spec: String,
    #[serde(rename = "activationCount")]
    pub activation_count: u64,
    #[serde(rename = "lastActivation")]
    pub last_activation: Option<String>,
    #[serde(rename = "resumeScanCount")]
    pub resume_scan_count: u64,
    #[serde(rename = "scheduledNotificationCount")]
    pub scheduled_notification_count: Option<u32>,
    #[serde(rename = "notificationHistoryCount")]
    pub notification_history_count: Option<u32>,
    #[serde(rename = "reminderStatus")]
    pub reminder_status: Option<String>,
    #[serde(rename = "reminderDueAt")]
    pub reminder_due_at: Option<i64>,
    #[serde(rename = "reminderCatchUpAt")]
    pub reminder_catch_up_at: Option<i64>,
    #[serde(rename = "reminderLastAction")]
    pub reminder_last_action: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct SharedStatus(Arc<Mutex<BenchmarkStatus>>);

impl SharedStatus {
    pub fn new(candidate: &str) -> Self {
        let mut frequency = 0;
        // SAFETY: Windows writes one i64 to the supplied valid pointer.
        unsafe { QueryPerformanceFrequency(&mut frequency) };
        Self(Arc::new(Mutex::new(BenchmarkStatus {
            id: None,
            ok: true,
            candidate: candidate.to_owned(),
            pid: std::process::id(),
            event_name: "status".to_owned(),
            frequency,
            process_start_ticks: qpc(),
            hotkey_received_ticks: 0,
            window_visible_ticks: 0,
            editor_focused_ticks: 0,
            sentinel_accepted_ticks: 0,
            show_sequence: 0,
            hotkey_registered: false,
            hotkey_spec: "Ctrl+Alt+Q".to_owned(),
            activation_count: 0,
            last_activation: None,
            resume_scan_count: 0,
            scheduled_notification_count: None,
            notification_history_count: None,
            reminder_status: None,
            reminder_due_at: None,
            reminder_catch_up_at: None,
            reminder_last_action: None,
            error: None,
        })))
    }

    pub fn snapshot(&self, id: Option<String>, event_name: &str) -> BenchmarkStatus {
        let mut status = self
            .0
            .lock()
            .expect("benchmark status mutex poisoned")
            .clone();
        status.id = id;
        status.event_name = event_name.to_owned();
        status
    }

    pub fn mark_hotkey(&self) {
        self.0
            .lock()
            .expect("benchmark status mutex poisoned")
            .hotkey_received_ticks = qpc();
    }

    pub fn mark_visible(&self) {
        self.0
            .lock()
            .expect("benchmark status mutex poisoned")
            .window_visible_ticks = qpc();
    }

    pub fn mark_ready(&self) {
        let mut status = self.0.lock().expect("benchmark status mutex poisoned");
        let now = qpc();
        status.editor_focused_ticks = now;
        status.sentinel_accepted_ticks = now;
        status.show_sequence += 1;
    }

    pub fn set_hotkey_registered(&self, registered: bool, error: Option<String>) {
        let mut status = self.0.lock().expect("benchmark status mutex poisoned");
        status.hotkey_registered = registered;
        status.ok = registered;
        status.error = error;
    }

    pub fn set_hotkey_state(&self, spec: &str, registered: bool, error: Option<String>) {
        let mut status = self.0.lock().expect("benchmark status mutex poisoned");
        status.hotkey_spec = spec.to_owned();
        status.hotkey_registered = registered;
        status.ok = registered;
        status.error = error;
    }

    pub fn mark_activation(&self, activation: &str) {
        let mut status = self.0.lock().expect("benchmark status mutex poisoned");
        status.activation_count += 1;
        status.last_activation = Some(activation.to_owned());
    }

    pub fn mark_resume_scan(&self) {
        self.0
            .lock()
            .expect("benchmark status mutex poisoned")
            .resume_scan_count += 1;
    }
}

pub fn start_pipe_server<F>(candidate: &str, handler: F)
where
    F: Fn(PipeRequest) -> BenchmarkStatus + Send + Sync + 'static,
{
    let pipe_name = format!(r"\\.\pipe\quicknote-stack-{candidate}");
    let handler = Arc::new(handler);
    thread::spawn(move || {
        loop {
            let wide_name: Vec<u16> = pipe_name.encode_utf16().chain(Some(0)).collect();
            // SAFETY: The UTF-16 name is NUL-terminated and all buffer sizes are valid.
            let pipe = unsafe {
                CreateNamedPipeW(
                    wide_name.as_ptr(),
                    PIPE_ACCESS_DUPLEX,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                    1,
                    8192,
                    8192,
                    0,
                    std::ptr::null(),
                )
            };
            if pipe == INVALID_HANDLE_VALUE {
                thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }

            // SAFETY: pipe is a valid named-pipe handle owned by this thread.
            let connected = unsafe { ConnectNamedPipe(pipe, std::ptr::null_mut()) } != 0
                || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
            if connected {
                let mut buffer = [0_u8; 8192];
                let mut bytes_read = 0_u32;
                // SAFETY: buffer and output count point to valid writable memory.
                let read = unsafe {
                    ReadFile(
                        pipe,
                        buffer.as_mut_ptr(),
                        buffer.len() as u32,
                        &mut bytes_read,
                        std::ptr::null_mut(),
                    )
                };
                if read != 0 {
                    let line = String::from_utf8_lossy(&buffer[..bytes_read as usize]);
                    let request =
                        serde_json::from_str::<PipeRequest>(line.trim()).unwrap_or_default();
                    let response = handler(request);
                    if let Ok(mut json) = serde_json::to_vec(&response) {
                        json.push(b'\n');
                        let mut bytes_written = 0_u32;
                        // SAFETY: json remains alive for the duration of this synchronous write.
                        unsafe {
                            WriteFile(
                                pipe,
                                json.as_ptr(),
                                json.len() as u32,
                                &mut bytes_written,
                                std::ptr::null_mut(),
                            )
                        };
                        // Ensure the client receives the response before the server disconnects.
                        unsafe { FlushFileBuffers(pipe) };
                    }
                }
            }
            // SAFETY: pipe is no longer used after disconnect and close.
            unsafe {
                DisconnectNamedPipe(pipe);
                CloseHandle(pipe);
            }
        }
    });
}

pub fn start_global_hotkey<F>(status: SharedStatus, callback: F)
where
    F: Fn() + Send + 'static,
{
    let _ = start_rebindable_global_hotkey(status, "Ctrl+Alt+Q", callback);
}

#[derive(Clone)]
pub struct HotkeyController {
    thread_id: Arc<AtomicU32>,
    desired: Arc<Mutex<String>>,
    ready: Arc<Mutex<Option<mpsc::Receiver<()>>>>,
}

impl HotkeyController {
    /// Waits for the worker message queue before a caller attempts a runtime rebind.
    pub fn wait_until_ready(&self) {
        if let Some(receiver) = self
            .ready
            .lock()
            .expect("hotkey ready mutex poisoned")
            .take()
        {
            let _ = receiver.recv_timeout(std::time::Duration::from_secs(2));
        }
    }

    /// Requests a new chord; the worker records registration success in SharedStatus.
    pub fn rebind(&self, spec: &str) -> Result<(), String> {
        *self.desired.lock().expect("hotkey desired mutex poisoned") = spec.to_owned();
        let thread_id = self.thread_id.load(Ordering::Acquire);
        if thread_id == 0 {
            return Err("Hotkey thread is not ready.".to_owned());
        }
        // SAFETY: The target thread owns a GetMessageW loop and accepts this private message.
        if unsafe { PostThreadMessageW(thread_id, WM_REBIND_HOTKEY, 0, 0) } == 0 {
            return Err(format!("PostThreadMessageW failed: {}", unsafe {
                GetLastError()
            }));
        }
        Ok(())
    }
}

pub fn start_rebindable_global_hotkey<F>(
    status: SharedStatus,
    initial_spec: &str,
    callback: F,
) -> HotkeyController
where
    F: Fn() + Send + 'static,
{
    let thread_id = Arc::new(AtomicU32::new(0));
    let desired = Arc::new(Mutex::new(initial_spec.to_owned()));
    let (ready_tx, ready_rx) = mpsc::channel();
    let thread_id_worker = thread_id.clone();
    let desired_worker = desired.clone();
    thread::spawn(move || {
        thread_id_worker.store(
            unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() },
            Ordering::Release,
        );
        let bind = |spec: &str, status: &SharedStatus| -> bool {
            let Some((modifiers, virtual_key)) = parse_hotkey_spec(spec) else {
                status.set_hotkey_state(
                    spec,
                    false,
                    Some("Unsupported hotkey specification.".to_owned()),
                );
                return false;
            };
            // SAFETY: A null HWND registers a hotkey owned by this message-loop thread.
            let registered =
                unsafe { RegisterHotKey(std::ptr::null_mut(), HOTKEY_ID, modifiers, virtual_key) }
                    != 0;
            status.set_hotkey_state(
                spec,
                registered,
                (!registered)
                    .then(|| format!("RegisterHotKey failed: {}", unsafe { GetLastError() })),
            );
            registered
        };
        let first_spec = desired_worker
            .lock()
            .expect("hotkey desired mutex poisoned")
            .clone();
        let mut active = bind(&first_spec, &status);
        let _ = ready_tx.send(());
        let mut message = MSG::default();
        // SAFETY: message points to writable memory for the lifetime of the loop.
        while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {
            if message.message == WM_HOTKEY && message.wParam == HOTKEY_ID as usize {
                status.mark_hotkey();
                callback();
            } else if message.message == WM_REBIND_HOTKEY {
                if active {
                    // SAFETY: This thread owns the current HOTKEY_ID registration.
                    unsafe { UnregisterHotKey(std::ptr::null_mut(), HOTKEY_ID) };
                }
                let spec = desired_worker
                    .lock()
                    .expect("hotkey desired mutex poisoned")
                    .clone();
                active = bind(&spec, &status);
            }
        }
        if active {
            // SAFETY: This thread owns the current HOTKEY_ID registration.
            unsafe { UnregisterHotKey(std::ptr::null_mut(), HOTKEY_ID) };
        }
    });
    HotkeyController {
        thread_id,
        desired,
        ready: Arc::new(Mutex::new(Some(ready_rx))),
    }
}

fn parse_hotkey_spec(spec: &str) -> Option<(u32, u32)> {
    let mut modifiers = MOD_NOREPEAT;
    let mut key = None;
    for part in spec.split('+').map(str::trim) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= MOD_CONTROL,
            "alt" => modifiers |= MOD_ALT,
            "shift" => modifiers |= MOD_SHIFT,
            value if value.len() == 1 => {
                let byte = value.as_bytes()[0].to_ascii_uppercase();
                if byte.is_ascii_alphanumeric() {
                    key = Some(byte as u32);
                } else {
                    return None;
                }
            }
            _ => return None,
        }
    }
    key.map(|key| (modifiers, key))
}

static POWER_RESUME_CALLBACK: std::sync::OnceLock<Arc<dyn Fn() + Send + Sync>> =
    std::sync::OnceLock::new();

/// Keeps the hidden native window alive and lets the harness exercise the same resume path.
#[derive(Clone)]
pub struct PowerResumeController {
    window: Arc<Mutex<isize>>,
}

impl PowerResumeController {
    /// Sends the real WM_POWERBROADCAST resume message without suspending the workstation.
    pub fn simulate_resume(&self) -> Result<(), String> {
        let raw = *self.window.lock().expect("power window mutex poisoned");
        if raw == 0 {
            return Err("Power listener is not ready.".to_owned());
        }
        // SAFETY: The stored HWND belongs to the listener thread for this process.
        if unsafe {
            PostMessageW(
                raw as *mut c_void,
                WM_POWERBROADCAST,
                PBT_APMRESUMEAUTOMATIC,
                0,
            )
        } == 0
        {
            return Err(format!("PostMessageW failed: {}", unsafe {
                GetLastError()
            }));
        }
        Ok(())
    }
}

/// Starts a message-only window that observes actual Windows automatic-resume broadcasts.
pub fn start_power_resume_listener<F>(callback: F) -> Result<PowerResumeController, String>
where
    F: Fn() + Send + Sync + 'static,
{
    POWER_RESUME_CALLBACK
        .set(Arc::new(callback))
        .map_err(|_| "Power listener can only be started once.".to_owned())?;
    let window = Arc::new(Mutex::new(0_isize));
    let worker_window = window.clone();
    let (ready_tx, ready_rx) = mpsc::channel();
    thread::spawn(move || {
        let class_name: Vec<u16> = "QuickNoteSpikePowerWindow"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        // SAFETY: Null requests the module for the current process.
        let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
        let class = WNDCLASSW {
            lpfnWndProc: Some(power_window_proc),
            hInstance: instance,
            lpszClassName: class_name.as_ptr(),
            ..Default::default()
        };
        // SAFETY: The class fields remain valid for registration and this process owns the class.
        unsafe { RegisterClassW(&class) };
        // SAFETY: This creates a message-only window with no visible surface or user data.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class_name.as_ptr(),
                class_name.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            )
        };
        *worker_window.lock().expect("power window mutex poisoned") = hwnd as isize;
        let _ = ready_tx.send(!hwnd.is_null());
        if hwnd.is_null() {
            return;
        }
        let mut message = MSG::default();
        // SAFETY: message points to writable memory for the lifetime of the listener.
        while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {
            // SAFETY: The message was initialized by GetMessageW for this listener thread.
            unsafe { DispatchMessageW(&message) };
        }
    });
    match ready_rx.recv_timeout(std::time::Duration::from_secs(2)) {
        Ok(true) => Ok(PowerResumeController { window }),
        _ => Err("Could not create the power-resume listener window.".to_owned()),
    }
}

unsafe extern "system" fn power_window_proc(
    hwnd: *mut c_void,
    message: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    if message == WM_POWERBROADCAST && wparam == PBT_APMRESUMEAUTOMATIC {
        if let Some(callback) = POWER_RESUME_CALLBACK.get() {
            callback();
        }
        return 1;
    }
    // SAFETY: All messages not consumed above are forwarded to the default procedure.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

pub fn qpc() -> i64 {
    let mut value = 0;
    // SAFETY: Windows writes one i64 to the supplied valid pointer.
    unsafe { QueryPerformanceCounter(&mut value) };
    value
}
