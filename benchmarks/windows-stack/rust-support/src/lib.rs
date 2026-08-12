use serde::{Deserialize, Serialize};
use std::ffi::c_void;
use std::sync::{Arc, Mutex};
use std::thread;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_PIPE_CONNECTED, GetLastError, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    FlushFileBuffers, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, RegisterHotKey, UnregisterHotKey, VK_Q,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};

const HOTKEY_ID: i32 = 0x514E;

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

#[derive(Clone, Default, Deserialize)]
pub struct PipeRequest {
    pub id: Option<String>,
    pub command: Option<String>,
    pub value: Option<String>,
}

#[derive(Clone, Serialize)]
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
    thread::spawn(move || {
        // SAFETY: A null HWND registers a thread hotkey; this thread owns its message loop.
        let registered = unsafe {
            RegisterHotKey(
                std::ptr::null_mut(),
                HOTKEY_ID,
                MOD_CONTROL | MOD_ALT,
                VK_Q as u32,
            )
        } != 0;
        if !registered {
            status.set_hotkey_registered(
                false,
                Some(format!("RegisterHotKey failed: {}", unsafe {
                    GetLastError()
                })),
            );
            return;
        }
        status.set_hotkey_registered(true, None);
        let mut message = MSG::default();
        // SAFETY: message points to writable memory for the lifetime of the loop.
        while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {
            if message.message == WM_HOTKEY && message.wParam == HOTKEY_ID as usize {
                status.mark_hotkey();
                callback();
            }
        }
        // SAFETY: This thread registered HOTKEY_ID and unregisters it during teardown.
        unsafe { UnregisterHotKey(std::ptr::null_mut(), HOTKEY_ID) };
    });
}

pub fn qpc() -> i64 {
    let mut value = 0;
    // SAFETY: Windows writes one i64 to the supplied valid pointer.
    unsafe { QueryPerformanceCounter(&mut value) };
    value
}
