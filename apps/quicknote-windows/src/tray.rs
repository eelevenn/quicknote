//! 独立消息线程拥有托盘图标，所有用户动作仍交给主实例统一处理。

use quicknote_app::platform::{ActivationHandler, ActivationRequest, PlatformError};
use std::cell::RefCell;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

const TRAY_MESSAGE: u32 = WM_APP + 24;

// 窗口过程只在所属线程访问处理器，不向 Win32 借用 Rust 栈指针。
thread_local! {
    static HANDLER: RefCell<Option<ActivationHandler>> = RefCell::new(None);
    static TASKBAR_CREATED: RefCell<u32> = const { RefCell::new(0) };
}

/// 保活托盘消息循环；释放时在原线程移除图标和窗口。
pub struct TrayThread {
    thread_id: u32,
    worker: Option<JoinHandle<()>>,
}

impl TrayThread {
    /// 等待首次图标注册结果，失败时允许主页显示可恢复错误。
    pub fn start(handler: ActivationHandler) -> Result<Self, PlatformError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("quicknote-tray".into())
            .spawn(move || {
                HANDLER.with(|slot| *slot.borrow_mut() = Some(handler));
                // SAFETY: 静态宽字符串和当前线程的窗口生命周期覆盖所有调用。
                let result = unsafe { create_tray_window() };
                match result {
                    Ok(hwnd) => {
                        let _ = sender.send(Ok(unsafe { GetCurrentThreadId() }));
                        let mut message = MSG::default();
                        // SAFETY: 消息在当前线程分发；退出后才销毁窗口。
                        unsafe {
                            while GetMessageW(&mut message, None, 0, 0).0 > 0 {
                                let _ = TranslateMessage(&message);
                                DispatchMessageW(&message);
                            }
                            let _ = Shell_NotifyIconW(NIM_DELETE, &icon_data(hwnd));
                            let _ = DestroyWindow(hwnd);
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(Err(error));
                    }
                }
                HANDLER.with(|slot| *slot.borrow_mut() = None);
            })
            .map_err(|error| PlatformError::new("start_tray", error.to_string()))?;
        match receiver.recv() {
            Ok(Ok(thread_id)) => Ok(Self {
                thread_id,
                worker: Some(worker),
            }),
            result => {
                let _ = worker.join();
                Err(match result {
                    Ok(Err(error)) => error,
                    _ => PlatformError::new("start_tray", "托盘线程提前退出"),
                })
            }
        }
    }
}

impl Drop for TrayThread {
    fn drop(&mut self) {
        // SAFETY: 线程由 worker 保活；WM_QUIT 使它自行释放 Win32 资源。
        let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// 使用隐藏顶层窗口接收 Explorer 重建广播；message-only 窗口收不到广播。
unsafe fn create_tray_window() -> Result<HWND, PlatformError> {
    let create = || -> windows::core::Result<HWND> {
        // SAFETY: 模块句柄及类名存活到线程退出，窗口没有外借数据。
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance.into(),
                lpszClassName: w!("QuickNote.Tray"),
                ..Default::default()
            };
            RegisterClassW(&class);
            TASKBAR_CREATED
                .with(|slot| *slot.borrow_mut() = RegisterWindowMessageW(w!("TaskbarCreated")));
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class.lpszClassName,
                w!("QuickNote"),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance.into()),
                None,
            )
        }
    };
    let hwnd =
        create().map_err(|error| PlatformError::new("create_tray_window", error.to_string()))?;
    // Explorer 在登录启动时可能尚未就绪；保留窗口并定时重试直至成功。
    unsafe {
        if !Shell_NotifyIconW(NIM_ADD, &icon_data(hwnd)).as_bool() {
            SetTimer(Some(hwnd), 1, 2000, None);
        }
    }
    Ok(hwnd)
}

/// 图标数据每次按同一窗口和 ID 重建，Explorer 重启后不会产生重复入口。
fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: TRAY_MESSAGE,
        // SAFETY: 系统共享图标无需 DestroyIcon，句柄在进程生命周期内有效。
        hIcon: unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap_or_default(),
        ..Default::default()
    };
    for (slot, character) in data
        .szTip
        .iter_mut()
        .zip("QuickNote · 快速记录".encode_utf16())
    {
        *slot = character;
    }
    data
}

/// 把托盘命令转成与快捷键、协议相同的激活事件。
fn activate(request: ActivationRequest) {
    HANDLER.with(|slot| {
        if let Some(handler) = slot.borrow().as_ref() {
            handler(request);
        }
    });
}

/// 菜单只收集选择，保存失败等领域处理全部在 UI 线程执行。
unsafe fn show_menu(hwnd: HWND) {
    // SAFETY: 菜单在同步 TrackPopupMenu 返回后释放；所有字符串是静态值。
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let _ = AppendMenuW(menu, MF_STRING, 1, w!("快速记录"));
        let _ = AppendMenuW(menu, MF_STRING, 2, w!("打开主页"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, 3, w!("退出 QuickNote"));
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let _ = SetForegroundWindow(hwnd);
        let selection = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            None,
            hwnd,
            None,
        )
        .0;
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        match selection {
            1 => activate(ActivationRequest::ShowQuickCapture),
            2 => activate(ActivationRequest::ShowMain),
            3 => activate(ActivationRequest::ExitRequested),
            _ => {}
        }
    }
}

/// Win32 回调禁止展开异常；这里只转发动作及维护图标。
unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: 回调中的 hwnd 由系统提供，仅在所属线程同步使用。
    unsafe {
        if message == TRAY_MESSAGE {
            match lparam.0 as u32 {
                WM_LBUTTONUP => activate(ActivationRequest::ShowQuickCapture),
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                _ => {}
            }
            return LRESULT(0);
        }
        if message == WM_TIMER
            || TASKBAR_CREATED.with(|slot| message != 0 && message == *slot.borrow())
        {
            if Shell_NotifyIconW(NIM_ADD, &icon_data(hwnd)).as_bool() {
                let _ = KillTimer(Some(hwnd), 1);
            } else {
                SetTimer(Some(hwnd), 1, 2000, None);
            }
            return LRESULT(0);
        }
        if message == WM_POWERBROADCAST && matches!(wparam.0, 7 | 18) {
            activate(ActivationRequest::Resumed);
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}
