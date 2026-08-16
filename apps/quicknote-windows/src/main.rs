//! QuickNote Windows 11 x64 生产入口。

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
mod windows_platform;

#[cfg(windows)]
use quicknote_app::platform::{
    ActivationRequest, InstanceRole, PRODUCT_IDENTITY, PlatformServices,
};
#[cfg(windows)]
use quicknote_app::{
    Application, ApplicationConfig, Command, EditingSnapshot, EditorIntent, NoteAction,
    NoteDocument, NoteLifecycle, SaveState,
};
#[cfg(windows)]
use quicknote_ui::{MainWindow, NoteListItem, PreviewLinkItem, QuickCaptureWindow, SearchListItem};
#[cfg(windows)]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
#[cfg(windows)]
use slint::{
    CloseRequestResponse, ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel,
};
#[cfg(windows)]
use std::cell::RefCell;
#[cfg(windows)]
use std::ffi::c_void;
#[cfg(windows)]
use std::rc::Rc;
#[cfg(windows)]
use windows::Win32::Foundation::HWND;
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, IsIconic, SetForegroundWindow};

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

    // 只有主实例会打开 SQLite、启动自动保存协调器和创建窗口。
    let application = Rc::new(Application::open(ApplicationConfig::new(
        platform.data_directory()?,
    ))?);
    let main_window = MainWindow::new()?;
    let quick_capture = QuickCaptureWindow::new()?;
    let initial_editor = application.editing_snapshot()?;
    set_editor_document(&main_window, &quick_capture, &initial_editor);
    sync_library(&main_window, &application)?;

    let initial_snapshot = application.snapshot()?;
    let shortcut = initial_snapshot.global_shortcut;
    main_window.set_shortcut_text(SharedString::from(shortcut.to_string()));
    main_window.set_startup_enabled(initial_snapshot.startup_enabled);
    main_window.set_snooze_minutes(i32::from(initial_snapshot.default_snooze_minutes));
    match application.install_global_shortcut(&platform) {
        Ok(shortcut) => main_window
            .set_shortcut_status(SharedString::from(format!("{} 已启用（防重复）", shortcut))),
        Err(error) => main_window.set_shortcut_status(SharedString::from(format!(
            "快捷键未启用，可修改后重试：{error}"
        ))),
    }

    wire_editor_callbacks(
        &main_window,
        &quick_capture,
        Rc::clone(&application),
        platform.clone(),
    );
    wire_window_lifecycle(&main_window, &quick_capture, Rc::clone(&application));
    route_activation(
        &main_window,
        &quick_capture,
        &application,
        initial_activation,
    );

    let activation_timer = Timer::default();
    let main_weak = main_window.as_weak();
    let capture_weak = quick_capture.as_weak();
    let timer_application = Rc::clone(&application);
    let last_timer_snapshot = Rc::new(RefCell::new(None::<EditingSnapshot>));
    let timer_snapshot_cache = Rc::clone(&last_timer_snapshot);
    activation_timer.start(TimerMode::Repeated, Duration::from_millis(50), move || {
        let (Some(main), Some(capture)) = (main_weak.upgrade(), capture_weak.upgrade()) else {
            return;
        };
        while let Ok(activation) = activation_receiver.try_recv() {
            route_activation(&main, &capture, &timer_application, activation);
        }
        // 只同步保存反馈和摘要，不重设可见 TextEdit 的正文或选择范围。
        if let Ok(snapshot) = timer_application.editing_snapshot() {
            let mut cached = timer_snapshot_cache.borrow_mut();
            if cached.as_ref() != Some(&snapshot) {
                sync_editor_status(&main, &capture, &snapshot);
                *cached = Some(snapshot);
            }
        }
    });

    slint::run_event_loop_until_quit()?;
    // 非按钮路径结束事件循环时仍做一次尽力刷新；交互路径会在退出前阻止失败。
    let _ = application.edit(EditorIntent::Flush);
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
fn wire_editor_callbacks(
    main: &MainWindow,
    capture: &QuickCaptureWindow,
    application: Rc<Application>,
    platform: windows_platform::WindowsPlatformServices,
) {
    let application_for_main = Rc::clone(&application);
    let main_weak = main.as_weak();
    let capture_weak = capture.as_weak();
    main.on_body_edited(move |body| {
        if let (Some(main), Some(capture)) = (main_weak.upgrade(), capture_weak.upgrade()) {
            match application_for_main.edit(EditorIntent::ReplaceBody(body.to_string())) {
                Ok(snapshot) => {
                    // 主页可见时只更新隐藏的快速记录正文，避免移动主页选择范围。
                    capture.set_editor_body(body);
                    sync_markdown_preview(&main, &snapshot.body);
                    sync_editor_status(&main, &capture, &snapshot);
                }
                Err(error) => show_editor_error(&main, &capture, &error),
            }
        }
    });

    let application_for_capture = Rc::clone(&application);
    let main_weak = main.as_weak();
    let capture_weak = capture.as_weak();
    capture.on_body_edited(move |body| {
        if let (Some(main), Some(capture)) = (main_weak.upgrade(), capture_weak.upgrade()) {
            match application_for_capture.edit(EditorIntent::ReplaceBody(body.to_string())) {
                Ok(snapshot) => {
                    // 快速记录可见时只更新隐藏的主页正文。
                    main.set_editor_body(body);
                    sync_markdown_preview(&main, &snapshot.body);
                    sync_editor_status(&main, &capture, &snapshot);
                }
                Err(error) => show_editor_error(&main, &capture, &error),
            }
        }
    });

    let application_for_switch = Rc::clone(&application);
    let main_weak = main.as_weak();
    let capture_weak = capture.as_weak();
    main.on_note_selected(move |id| {
        if let (Some(main), Some(capture)) = (main_weak.upgrade(), capture_weak.upgrade()) {
            let result = uuid::Uuid::parse_str(id.as_str())
                .map_err(|error| quicknote_app::ApplicationError::InvalidCommand {
                    message: format!("便签身份无效：{error}"),
                })
                .and_then(|id| application_for_switch.edit(EditorIntent::SwitchCurrent(id)));
            match result {
                Ok(snapshot) => {
                    set_editor_document(&main, &capture, &snapshot);
                    let _ = sync_library(&main, &application_for_switch);
                    if main.get_compact_mode() {
                        show_quick_capture_snapshot(&main, &capture, &snapshot);
                    } else {
                        main.invoke_focus_editor_at(body_offset(&snapshot.body));
                    }
                }
                Err(error) => show_editor_error(&main, &capture, &error),
            }
        }
    });

    let application_for_new = Rc::clone(&application);
    let main_weak = main.as_weak();
    let capture_weak = capture.as_weak();
    main.on_new_note(move || {
        if let (Some(main), Some(capture)) = (main_weak.upgrade(), capture_weak.upgrade()) {
            match application_for_new.edit(EditorIntent::NewBlankDraft) {
                Ok(snapshot) => {
                    set_editor_document(&main, &capture, &snapshot);
                    if main.get_compact_mode() {
                        show_quick_capture_snapshot(&main, &capture, &snapshot);
                    } else {
                        main.invoke_focus_editor_at(0);
                    }
                }
                Err(error) => show_editor_error(&main, &capture, &error),
            }
        }
    });

    wire_retry(main, capture, Rc::clone(&application));

    let application_for_shortcut = Rc::clone(&application);
    let platform_for_shortcut = platform.clone();
    let main_weak = main.as_weak();
    main.on_apply_shortcut(move |value| {
        let Some(main) = main_weak.upgrade() else {
            return;
        };
        match application_for_shortcut
            .configure_global_shortcut(&platform_for_shortcut, value.as_str())
        {
            Ok(shortcut) => {
                main.set_shortcut_text(SharedString::from(shortcut.to_string()));
                main.set_shortcut_status(SharedString::from(format!(
                    "{} 已启用（防重复）",
                    shortcut
                )));
            }
            Err(error) => {
                // 平台替换失败时恢复已持久化的旧有效组合。
                if let Ok(snapshot) = application_for_shortcut.snapshot() {
                    main.set_shortcut_text(SharedString::from(
                        snapshot.global_shortcut.to_string(),
                    ));
                }
                main.set_shortcut_status(SharedString::from(format!(
                    "修改失败，仍使用旧组合：{error}"
                )));
            }
        }
    });

    wire_product_callbacks(main, capture, application, platform);
}

#[cfg(windows)]
fn wire_product_callbacks(
    main: &MainWindow,
    capture: &QuickCaptureWindow,
    application: Rc<Application>,
    platform: windows_platform::WindowsPlatformServices,
) {
    let app = Rc::clone(&application);
    let main_weak = main.as_weak();
    let capture_weak = capture.as_weak();
    main.on_archive_note(move |id| {
        run_note_action(
            &app,
            main_weak.upgrade(),
            capture_weak.upgrade(),
            parse_note_action_id(id.as_str()).map(NoteAction::Archive),
            "便签已归档，提醒已永久清除",
        );
    });

    let app = Rc::clone(&application);
    let main_weak = main.as_weak();
    let capture_weak = capture.as_weak();
    main.on_unarchive_note(move |id| {
        run_note_action(
            &app,
            main_weak.upgrade(),
            capture_weak.upgrade(),
            parse_note_action_id(id.as_str()).map(NoteAction::Unarchive),
            "便签已取消归档",
        );
    });

    let app = Rc::clone(&application);
    let main_weak = main.as_weak();
    let capture_weak = capture.as_weak();
    main.on_move_to_trash(move |id| {
        run_note_action(
            &app,
            main_weak.upgrade(),
            capture_weak.upgrade(),
            parse_note_action_id(id.as_str()).map(NoteAction::MoveToTrash),
            "便签已移入回收站，将保留 30×24 小时",
        );
    });

    let app = Rc::clone(&application);
    let main_weak = main.as_weak();
    let capture_weak = capture.as_weak();
    main.on_restore_note(move |id| {
        run_note_action(
            &app,
            main_weak.upgrade(),
            capture_weak.upgrade(),
            parse_note_action_id(id.as_str()).map(NoteAction::RestoreFromTrash),
            "便签已恢复到归档",
        );
    });

    let app = Rc::clone(&application);
    let main_weak = main.as_weak();
    let capture_weak = capture.as_weak();
    main.on_permanently_delete(move |id| {
        run_note_action(
            &app,
            main_weak.upgrade(),
            capture_weak.upgrade(),
            parse_note_action_id(id.as_str()).map(NoteAction::PermanentlyDelete),
            "便签及其自动备份历史已永久清除",
        );
    });

    let app = Rc::clone(&application);
    let main_weak = main.as_weak();
    main.on_open_readonly(move |id| {
        let Some(main) = main_weak.upgrade() else {
            return;
        };
        match app
            .edit(EditorIntent::Flush)
            .and_then(|_| parse_note_action_id(id.as_str()).and_then(|id| app.note(id)))
        {
            Ok(note) => show_readonly_note(&main, &note),
            Err(error) => main.set_action_status(SharedString::from(format!("打开失败：{error}"))),
        }
    });

    let app = Rc::clone(&application);
    let main_weak = main.as_weak();
    let capture_weak = capture.as_weak();
    main.on_navigate(move |_| {
        let (Some(main), Some(capture)) = (main_weak.upgrade(), capture_weak.upgrade()) else {
            return false;
        };
        match app.edit(EditorIntent::Flush) {
            Ok(snapshot) => {
                sync_editor_status(&main, &capture, &snapshot);
                true
            }
            Err(error) => {
                show_editor_error(&main, &capture, &error);
                false
            }
        }
    });

    let app = Rc::clone(&application);
    let main_weak = main.as_weak();
    let capture_weak = capture.as_weak();
    main.on_search_requested(move |query| {
        let (Some(main), Some(capture)) = (main_weak.upgrade(), capture_weak.upgrade()) else {
            return false;
        };
        match app
            .edit(EditorIntent::Flush)
            .and_then(|_| app.search(query.as_str()))
        {
            Ok(results) => {
                set_search_results(&main, results);
                true
            }
            Err(error) => {
                show_editor_error(&main, &capture, &error);
                main.set_action_status(SharedString::from(format!("搜索失败：{error}")));
                false
            }
        }
    });

    let app = Rc::clone(&application);
    let main_weak = main.as_weak();
    let capture_weak = capture.as_weak();
    main.on_search_result_opened(move |id| {
        let (Some(main), Some(capture)) = (main_weak.upgrade(), capture_weak.upgrade()) else {
            return;
        };
        let result = parse_note_action_id(id.as_str()).and_then(|id| app.note(id));
        match result {
            Ok(note) if note.lifecycle == NoteLifecycle::Active => {
                match app.edit(EditorIntent::SwitchCurrent(note.id)) {
                    Ok(snapshot) => {
                        main.set_section(0);
                        set_editor_document(&main, &capture, &snapshot);
                        let _ = sync_library(&main, &app);
                        if main.get_compact_mode() {
                            show_quick_capture_snapshot(&main, &capture, &snapshot);
                        } else {
                            main.invoke_focus_editor_at(body_offset(&snapshot.body));
                        }
                    }
                    Err(error) => show_editor_error(&main, &capture, &error),
                }
            }
            Ok(note) => match app.edit(EditorIntent::Flush) {
                Ok(_) => show_readonly_note(&main, &note),
                Err(error) => show_editor_error(&main, &capture, &error),
            },
            Err(error) => main.set_action_status(SharedString::from(format!("打开失败：{error}"))),
        }
    });

    let app = Rc::clone(&application);
    let platform_for_link = platform.clone();
    let main_weak = main.as_weak();
    main.on_open_link(move |url| {
        let Some(main) = main_weak.upgrade() else {
            return;
        };
        match app.open_external_link(&platform_for_link, url.as_str()) {
            Ok(()) => main.set_action_status(SharedString::from("已交给默认浏览器打开")),
            Err(error) => {
                main.set_action_status(SharedString::from(format!("链接未打开：{error}")))
            }
        }
    });

    wire_settings_callbacks(main, application, platform);
}

#[cfg(windows)]
fn wire_settings_callbacks(
    main: &MainWindow,
    application: Rc<Application>,
    platform: windows_platform::WindowsPlatformServices,
) {
    let app = Rc::clone(&application);
    let startup_platform = platform.clone();
    let main_weak = main.as_weak();
    main.on_startup_changed(move |enabled| {
        let Some(main) = main_weak.upgrade() else {
            return;
        };
        match app.configure_startup(&startup_platform, enabled) {
            Ok(()) => main.set_action_status(SharedString::from(if enabled {
                "已启用登录后启动"
            } else {
                "已关闭登录后启动"
            })),
            Err(error) => {
                if let Ok(snapshot) = app.snapshot() {
                    main.set_startup_enabled(snapshot.startup_enabled);
                }
                main.set_action_status(SharedString::from(format!("自启动修改失败：{error}")));
            }
        }
    });

    let app = Rc::clone(&application);
    let main_weak = main.as_weak();
    main.on_snooze_changed(move |minutes| {
        let Some(main) = main_weak.upgrade() else {
            return;
        };
        let result = u16::try_from(minutes)
            .map_err(|error| quicknote_app::ApplicationError::InvalidCommand {
                message: format!("稍后提醒时长无效：{error}"),
            })
            .and_then(|minutes| app.execute(Command::SetDefaultSnoozeMinutes { minutes }));
        match result {
            Ok(_) => {
                main.set_snooze_minutes(minutes);
                main.set_action_status(SharedString::from("默认稍后提醒时长已保存"));
            }
            Err(error) => {
                main.set_action_status(SharedString::from(format!("设置保存失败：{error}")))
            }
        }
    });

    let app = Rc::clone(&application);
    let export_platform = platform.clone();
    let main_weak = main.as_weak();
    main.on_export_json(move || {
        let Some(main) = main_weak.upgrade() else {
            return;
        };
        let target = match export_platform.data_directory() {
            Ok(directory) => directory.join("exports").join("quicknote.json"),
            Err(error) => {
                main.set_action_status(SharedString::from(format!("导出目录不可用：{error}")));
                return;
            }
        };
        match app.export_json(&export_platform, &target) {
            Ok(()) => main.set_action_status(SharedString::from(format!(
                "完整 JSON 已原子导出到 {}",
                target.display()
            ))),
            Err(error) => {
                main.set_action_status(SharedString::from(format!("JSON 导出失败：{error}")))
            }
        }
    });

    let app = Rc::clone(&application);
    let export_platform = platform.clone();
    let main_weak = main.as_weak();
    main.on_export_markdown(move |id| {
        let Some(main) = main_weak.upgrade() else {
            return;
        };
        let result = parse_note_action_id(id.as_str()).and_then(|note_id| {
            let directory = export_platform.data_directory().map_err(|error| {
                quicknote_app::ApplicationError::Platform {
                    operation: error.operation,
                    message: error.message,
                }
            })?;
            let target = directory.join("exports").join(format!("{note_id}.md"));
            app.export_markdown(&export_platform, note_id, &target)?;
            Ok(target)
        });
        match result {
            Ok(target) => main.set_action_status(SharedString::from(format!(
                "Markdown 已原子导出到 {}",
                target.display()
            ))),
            Err(error) => {
                main.set_action_status(SharedString::from(format!("Markdown 导出失败：{error}")))
            }
        }
    });

    let main_weak = main.as_weak();
    main.on_open_transcription_packages(move || {
        if let Some(main) = main_weak.upgrade() {
            main.set_action_status(SharedString::from(
                "本地转写包管理入口已就绪；模型下载与校验由 Issue #21 接入",
            ));
        }
    });
}

#[cfg(windows)]
fn run_note_action(
    application: &Application,
    main: Option<MainWindow>,
    capture: Option<QuickCaptureWindow>,
    action: Result<NoteAction, quicknote_app::ApplicationError>,
    success_message: &str,
) {
    let (Some(main), Some(capture)) = (main, capture) else {
        return;
    };
    let result = action.and_then(|action| application.apply_note_action(action));
    match result {
        Ok(snapshot) => {
            set_editor_document(&main, &capture, &snapshot);
            match sync_library(&main, application) {
                Ok(()) => main.set_action_status(SharedString::from(success_message)),
                Err(error) => {
                    main.set_action_status(SharedString::from(format!("列表刷新失败：{error}")))
                }
            }
        }
        Err(error) => {
            main.set_action_status(SharedString::from(format!("操作未执行：{error}")));
            show_editor_error(&main, &capture, &error);
        }
    }
}

#[cfg(windows)]
fn parse_note_action_id(value: &str) -> Result<uuid::Uuid, quicknote_app::ApplicationError> {
    uuid::Uuid::parse_str(value).map_err(|error| quicknote_app::ApplicationError::InvalidCommand {
        message: format!("便签身份无效：{error}"),
    })
}

#[cfg(windows)]
fn wire_retry(main: &MainWindow, capture: &QuickCaptureWindow, application: Rc<Application>) {
    let application_for_main = Rc::clone(&application);
    let main_weak = main.as_weak();
    let capture_weak = capture.as_weak();
    main.on_retry_save(move || {
        retry_save(
            &application_for_main,
            main_weak.upgrade(),
            capture_weak.upgrade(),
        );
    });

    let main_weak = main.as_weak();
    let capture_weak = capture.as_weak();
    capture.on_retry_save(move || {
        retry_save(&application, main_weak.upgrade(), capture_weak.upgrade());
    });
}

#[cfg(windows)]
fn retry_save(
    application: &Application,
    main: Option<MainWindow>,
    capture: Option<QuickCaptureWindow>,
) {
    let (Some(main), Some(capture)) = (main, capture) else {
        return;
    };
    match application.edit(EditorIntent::RetrySave) {
        Ok(snapshot) => sync_editor_status(&main, &capture, &snapshot),
        Err(error) => show_editor_error(&main, &capture, &error),
    }
}

#[cfg(windows)]
fn wire_window_lifecycle(
    main: &MainWindow,
    capture: &QuickCaptureWindow,
    application: Rc<Application>,
) {
    let application_for_capture = Rc::clone(&application);
    let main_weak = main.as_weak();
    let capture_weak = capture.as_weak();
    main.on_open_quick_capture(move || {
        if let (Some(main), Some(capture)) = (main_weak.upgrade(), capture_weak.upgrade()) {
            open_quick_capture(&main, &capture, &application_for_capture);
        }
    });

    let application_for_dismiss = Rc::clone(&application);
    let main_weak = main.as_weak();
    let capture_weak = capture.as_weak();
    capture.on_dismissed(move || {
        if let (Some(main), Some(capture)) = (main_weak.upgrade(), capture_weak.upgrade()) {
            match application_for_dismiss.edit(EditorIntent::Flush) {
                Ok(snapshot) => {
                    sync_editor_status(&main, &capture, &snapshot);
                    let _ = capture.hide();
                }
                Err(error) => show_editor_error(&main, &capture, &error),
            }
        }
    });

    let application_for_main = Rc::clone(&application);
    let main_weak = main.as_weak();
    let capture_weak = capture.as_weak();
    capture.on_open_main(move || {
        if let (Some(main), Some(capture)) = (main_weak.upgrade(), capture_weak.upgrade()) {
            match application_for_main.edit(EditorIntent::Flush) {
                Ok(snapshot) => show_main_window(&main, &capture, &snapshot),
                Err(error) => show_editor_error(&main, &capture, &error),
            }
        }
    });

    let application_for_exit = Rc::clone(&application);
    let main_weak = main.as_weak();
    let capture_weak = capture.as_weak();
    main.on_exit_requested(move || {
        if let (Some(main), Some(capture)) = (main_weak.upgrade(), capture_weak.upgrade()) {
            request_exit(&main, &capture, &application_for_exit);
        }
    });

    let application_for_main_close = Rc::clone(&application);
    let main_weak = main.as_weak();
    let capture_weak = capture.as_weak();
    main.window().on_close_requested(move || {
        let (Some(main), Some(capture)) = (main_weak.upgrade(), capture_weak.upgrade()) else {
            return CloseRequestResponse::HideWindow;
        };
        match application_for_main_close.edit(EditorIntent::Flush) {
            Ok(_) => {
                let _ = slint::quit_event_loop();
                CloseRequestResponse::HideWindow
            }
            Err(error) => {
                show_editor_error(&main, &capture, &error);
                CloseRequestResponse::KeepWindowShown
            }
        }
    });

    let application_for_capture_close = Rc::clone(&application);
    let main_weak = main.as_weak();
    let capture_weak = capture.as_weak();
    capture.window().on_close_requested(move || {
        let (Some(main), Some(capture)) = (main_weak.upgrade(), capture_weak.upgrade()) else {
            return CloseRequestResponse::HideWindow;
        };
        match application_for_capture_close.edit(EditorIntent::Flush) {
            Ok(snapshot) => {
                sync_editor_status(&main, &capture, &snapshot);
                CloseRequestResponse::HideWindow
            }
            Err(error) => {
                show_editor_error(&main, &capture, &error);
                CloseRequestResponse::KeepWindowShown
            }
        }
    });
}

#[cfg(windows)]
fn route_activation(
    main: &MainWindow,
    capture: &QuickCaptureWindow,
    application: &Application,
    activation: ActivationRequest,
) {
    match activation {
        ActivationRequest::GlobalShortcutPressed => {
            route_global_shortcut(main, capture, application);
        }
        ActivationRequest::ShowQuickCapture => open_quick_capture(main, capture, application),
        ActivationRequest::ShowMain | ActivationRequest::ProtocolUri(_) => {
            match application.edit(EditorIntent::Flush) {
                Ok(snapshot) => show_main_window(main, capture, &snapshot),
                Err(error) => show_editor_error(main, capture, &error),
            }
        }
        ActivationRequest::Resumed | ActivationRequest::Reminder(_) => {
            if let Ok(snapshot) = application.editing_snapshot() {
                sync_editor_status(main, capture, &snapshot);
            }
        }
    }
}

#[cfg(windows)]
fn route_global_shortcut(
    main: &MainWindow,
    capture: &QuickCaptureWindow,
    application: &Application,
) {
    let main_hwnd = window_hwnd(main.window());
    let capture_hwnd = window_hwnd(capture.window());
    // SAFETY: GetForegroundWindow 不借用返回句柄，失败时返回空句柄。
    let foreground = unsafe { GetForegroundWindow() };
    if main_hwnd.is_some_and(|hwnd| hwnd == foreground)
        || capture_hwnd.is_some_and(|hwnd| hwnd == foreground)
    {
        return;
    }

    if capture.window().is_visible() {
        focus_window(capture.window());
        return;
    }
    if main.window().is_visible()
        && main_hwnd.is_some_and(|hwnd| {
            // SAFETY: hwnd 来自仍存活的 Slint 主窗口。
            !unsafe { IsIconic(hwnd) }.as_bool()
        })
    {
        focus_window(main.window());
        return;
    }

    // 没有可见窗口或主页最小化时，才真正打开快速记录。
    open_quick_capture(main, capture, application);
}

#[cfg(windows)]
fn open_quick_capture(main: &MainWindow, capture: &QuickCaptureWindow, application: &Application) {
    match application.edit(EditorIntent::OpenCurrent) {
        Ok(snapshot) => {
            show_quick_capture_snapshot(main, capture, &snapshot);
            let _ = sync_library(main, application);
        }
        Err(error) => show_editor_error(main, capture, &error),
    }
}

#[cfg(windows)]
fn show_quick_capture_snapshot(
    main: &MainWindow,
    capture: &QuickCaptureWindow,
    snapshot: &EditingSnapshot,
) {
    set_editor_document(main, capture, snapshot);
    let _ = main.hide();
    capture.window().set_minimized(false);
    let _ = capture.show();
    capture.invoke_focus_editor_at(body_offset(&snapshot.body));
    focus_window(capture.window());
}

#[cfg(windows)]
fn show_main_window(main: &MainWindow, capture: &QuickCaptureWindow, snapshot: &EditingSnapshot) {
    set_editor_document(main, capture, snapshot);
    let _ = capture.hide();
    main.window().set_minimized(false);
    let _ = main.show();
    main.invoke_focus_editor_at(body_offset(&snapshot.body));
    focus_window(main.window());
}

#[cfg(windows)]
fn request_exit(main: &MainWindow, capture: &QuickCaptureWindow, application: &Application) {
    match application.edit(EditorIntent::Flush) {
        Ok(_) => {
            let _ = slint::quit_event_loop();
        }
        Err(error) => show_editor_error(main, capture, &error),
    }
}

#[cfg(windows)]
fn set_editor_document(
    main: &MainWindow,
    capture: &QuickCaptureWindow,
    snapshot: &EditingSnapshot,
) {
    let body = SharedString::from(snapshot.body.clone());
    main.set_editor_body(body.clone());
    capture.set_editor_body(body);
    main.set_editor_note_id(SharedString::from(
        snapshot
            .note_id
            .map(|id| id.to_string())
            .unwrap_or_default(),
    ));
    sync_markdown_preview(main, &snapshot.body);
    sync_editor_status(main, capture, snapshot);
}

#[cfg(windows)]
fn sync_markdown_preview(main: &MainWindow, body: &str) {
    let preview = Application::preview_markdown(body);
    main.set_preview_text(SharedString::from(preview.text));
    let links = preview
        .links
        .into_iter()
        .map(|link| PreviewLinkItem {
            label: SharedString::from(link.label),
            url: SharedString::from(link.url),
        })
        .collect::<Vec<_>>();
    main.set_preview_links(ModelRc::new(VecModel::from(links)));
}

#[cfg(windows)]
fn sync_editor_status(main: &MainWindow, capture: &QuickCaptureWindow, snapshot: &EditingSnapshot) {
    // 首次自动保存会为草稿分配身份，必须同步到 UI 才能启用时间与生命周期操作。
    main.set_editor_note_id(SharedString::from(
        snapshot
            .note_id
            .map(|id| id.to_string())
            .unwrap_or_default(),
    ));
    let title = SharedString::from(snapshot.title.clone());
    main.set_editor_title(title.clone());
    capture.set_editor_title(title);
    let (status, failed) = save_status(&snapshot.save_state);
    let status = SharedString::from(status);
    main.set_save_status(status.clone());
    capture.set_save_status(status);
    main.set_save_failed(failed);
    capture.set_save_failed(failed);

    let items = snapshot
        .active_notes
        .iter()
        .map(|note| NoteListItem {
            id: SharedString::from(note.id.to_string()),
            title: SharedString::from(note.title.clone()),
            is_current: note.is_current,
            metadata: SharedString::from(note_metadata(note, "活跃")),
        })
        .collect::<Vec<_>>();
    main.set_active_items(ModelRc::new(VecModel::from(items)));
}

#[cfg(windows)]
fn sync_library(
    main: &MainWindow,
    application: &Application,
) -> Result<(), quicknote_app::ApplicationError> {
    let library = application.library_snapshot()?;
    main.set_active_items(note_item_model(&library.active, "活跃"));
    main.set_archived_items(note_item_model(&library.archived, "只读归档"));
    main.set_trash_items(note_item_model(&library.trashed, "待自动清理"));
    Ok(())
}

#[cfg(windows)]
fn note_item_model(notes: &[quicknote_app::NoteSummary], fallback: &str) -> ModelRc<NoteListItem> {
    let items = notes
        .iter()
        .map(|note| NoteListItem {
            id: SharedString::from(note.id.to_string()),
            title: SharedString::from(note.title.clone()),
            is_current: note.is_current,
            metadata: SharedString::from(note_metadata(note, fallback)),
        })
        .collect::<Vec<_>>();
    ModelRc::new(VecModel::from(items))
}

#[cfg(windows)]
fn note_metadata(note: &quicknote_app::NoteSummary, fallback: &str) -> String {
    if note.is_current {
        "当前便签".to_owned()
    } else {
        fallback.to_owned()
    }
}

#[cfg(windows)]
fn set_search_results(main: &MainWindow, results: Vec<quicknote_app::SearchResult>) {
    let items = results
        .into_iter()
        .map(|result| SearchListItem {
            id: SharedString::from(result.id.to_string()),
            title: SharedString::from(result.title),
            lifecycle: SharedString::from(match result.lifecycle {
                NoteLifecycle::Active => "活跃",
                NoteLifecycle::Archived => "归档",
                NoteLifecycle::Trashed => "回收站",
            }),
            detail: SharedString::from(if result.exceeds_performance_guarantee {
                "正文命中 · 超过 1 MiB 性能保证范围"
            } else if result.matched_in_body {
                "正文命中"
            } else {
                "标题命中"
            }),
        })
        .collect::<Vec<_>>();
    main.set_search_items(ModelRc::new(VecModel::from(items)));
}

#[cfg(windows)]
fn show_readonly_note(main: &MainWindow, note: &NoteDocument) {
    main.set_readonly_note_id(SharedString::from(note.id.to_string()));
    main.set_readonly_title(SharedString::from(note.title.clone()));
    main.set_readonly_body(SharedString::from(note.body.clone()));
    main.set_readonly_lifecycle(SharedString::from(match note.lifecycle {
        NoteLifecycle::Active => "活跃",
        NoteLifecycle::Archived => "归档",
        NoteLifecycle::Trashed => "回收站",
    }));
    main.set_section(4);
}

#[cfg(windows)]
fn save_status(state: &SaveState) -> (String, bool) {
    match state {
        SaveState::BlankDraft => ("空白草稿不会创建便签".to_owned(), false),
        SaveState::Scheduled => ("等待自动保存…".to_owned(), false),
        SaveState::Saving => ("正在自动保存…".to_owned(), false),
        SaveState::Saved => ("已自动保存".to_owned(), false),
        SaveState::Failed { message } => (format!("自动保存失败，可重试：{message}"), true),
    }
}

#[cfg(windows)]
fn show_editor_error(
    main: &MainWindow,
    capture: &QuickCaptureWindow,
    error: &quicknote_app::ApplicationError,
) {
    let message = SharedString::from(format!("操作已中止，正文仍在内存中：{error}"));
    main.set_save_status(message.clone());
    capture.set_save_status(message);
    main.set_save_failed(true);
    capture.set_save_failed(true);
}

#[cfg(windows)]
fn body_offset(body: &str) -> i32 {
    i32::try_from(body.len()).unwrap_or(i32::MAX)
}

#[cfg(windows)]
fn window_hwnd(window: &slint::Window) -> Option<HWND> {
    let provider = window.window_handle();
    let handle = provider.window_handle().ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(handle) => Some(HWND(handle.hwnd.get() as *mut c_void)),
        _ => None,
    }
}

#[cfg(windows)]
fn focus_window(window: &slint::Window) {
    if let Some(hwnd) = window_hwnd(window) {
        // SAFETY: hwnd 来自仍存活的 Slint 窗口；失败只表示前台策略拒绝聚焦。
        let _ = unsafe { SetForegroundWindow(hwnd) };
    }
}

#[cfg(not(windows))]
fn main() {
    // 该包只交付 Windows 壳；非 Windows 构建保留明确诊断。
    eprintln!("quicknote-windows 只能在 Windows 上运行");
}
