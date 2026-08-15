//! QuickNote Windows 11 x64 生产入口。

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
mod windows_platform;

#[cfg(windows)]
use quicknote_app::platform::{
    ActivationRequest, InstanceRole, PRODUCT_IDENTITY, PlatformServices,
};
#[cfg(windows)]
use quicknote_app::{Application, ApplicationConfig};
#[cfg(windows)]
use quicknote_ui::{MainWindow, QuickCaptureWindow};
#[cfg(windows)]
use slint::{ComponentHandle, SharedString, Timer, TimerMode};

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::{Arc, mpsc};
    use std::time::Duration;
    use windows_platform::WindowsPlatformServices;

    let platform = WindowsPlatformServices::new();
    platform.configure_process_identity()?;
    let initial_activation = activation_from_arguments();
    let (activation_sender, activation_receiver) = mpsc::channel();
    let instance_role = platform.acquire_single_instance(
        initial_activation.clone(),
        Arc::new(move |activation| {
            let _ = activation_sender.send(activation);
        }),
    )?;
    let _instance_lease = match instance_role {
        InstanceRole::Primary(lease) => lease,
        InstanceRole::SecondaryForwarded => return Ok(()),
    };

    // 只有主实例会打开 SQLite 和创建窗口。
    let application = Application::open(ApplicationConfig::new(platform.data_directory()?))?;
    let snapshot = application.snapshot()?;
    let main_window = MainWindow::new()?;
    let quick_capture = QuickCaptureWindow::new()?;
    main_window.set_status_text(SharedString::from(format!(
        "{} 应用模块已就绪",
        PRODUCT_IDENTITY.product_name
    )));
    main_window.set_schema_version(snapshot.schema.version);
    main_window.set_active_note_count(snapshot.active_note_count.try_into().unwrap_or(i32::MAX));

    wire_window_lifecycle(&main_window, &quick_capture);
    route_activation(&main_window, &quick_capture, initial_activation);

    let activation_timer = Timer::default();
    let main_weak = main_window.as_weak();
    let capture_weak = quick_capture.as_weak();
    activation_timer.start(TimerMode::Repeated, Duration::from_millis(50), move || {
        while let Ok(activation) = activation_receiver.try_recv() {
            if let (Some(main), Some(capture)) = (main_weak.upgrade(), capture_weak.upgrade()) {
                route_activation(&main, &capture, activation);
            }
        }
    });

    slint::run_event_loop_until_quit()?;
    drop(application);
    Ok(())
}

#[cfg(windows)]
fn activation_from_arguments() -> ActivationRequest {
    let mut arguments = std::env::args().skip(1);
    let protocol_prefix = format!("{}:", PRODUCT_IDENTITY.protocol);
    if let Some(argument) = arguments
        .find(|argument| argument == "--quick-capture" || argument.starts_with(&protocol_prefix))
    {
        if argument == "--quick-capture" {
            return ActivationRequest::ShowQuickCapture;
        }
        return ActivationRequest::ProtocolUri(argument);
    }
    ActivationRequest::ShowMain
}

#[cfg(windows)]
fn wire_window_lifecycle(main: &MainWindow, capture: &QuickCaptureWindow) {
    let main_weak = main.as_weak();
    let capture_weak = capture.as_weak();
    main.on_open_quick_capture(move || {
        if let (Some(main), Some(capture)) = (main_weak.upgrade(), capture_weak.upgrade()) {
            let _ = main.hide();
            let _ = capture.show();
            capture.invoke_focus_editor();
        }
    });

    let capture_weak = capture.as_weak();
    capture.on_dismissed(move || {
        if let Some(capture) = capture_weak.upgrade() {
            let _ = capture.hide();
        }
    });

    let main_weak = main.as_weak();
    let capture_weak = capture.as_weak();
    capture.on_open_main(move || {
        if let (Some(main), Some(capture)) = (main_weak.upgrade(), capture_weak.upgrade()) {
            let _ = capture.hide();
            let _ = main.show();
        }
    });

    main.on_exit_requested(|| {
        let _ = slint::quit_event_loop();
    });
}

#[cfg(windows)]
fn route_activation(
    main: &MainWindow,
    capture: &QuickCaptureWindow,
    activation: ActivationRequest,
) {
    match activation {
        ActivationRequest::ShowQuickCapture => {
            let _ = main.hide();
            let _ = capture.show();
            capture.invoke_focus_editor();
        }
        ActivationRequest::ShowMain | ActivationRequest::ProtocolUri(_) => {
            let _ = capture.hide();
            let _ = main.show();
        }
        ActivationRequest::Resumed => {
            main.set_status_text(SharedString::from("系统已恢复，等待后续提醒协调切片"));
        }
    }
}

#[cfg(not(windows))]
fn main() {
    // 该包只交付 Windows 壳；非 Windows 构建保留明确诊断。
    eprintln!("quicknote-windows 只能在 Windows 上运行");
}
