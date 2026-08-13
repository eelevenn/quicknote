#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod platform;

use quicknote_benchmark_support::{
    BenchmarkStatus, HotkeyController, PipeRequest, PowerResumeController, SharedStatus,
    acquire_single_instance, send_pipe_request, start_pipe_server, start_power_resume_listener,
    start_rebindable_global_hotkey,
};
use quicknote_stack_slint::{BenchmarkWindow, core::ActivationCommand, store::Store};
use slint::{ComponentHandle, SharedString, Timer, TimerMode, Weak};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use tray_icon::{
    Icon, TrayIconBuilder,
    menu::{Menu, MenuEvent, MenuItem},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.iter().any(|argument| argument == "--register") {
        platform::register_current_executable()?;
        return Ok(());
    }
    if arguments.iter().any(|argument| argument == "--unregister") {
        platform::unregister()?;
        return Ok(());
    }
    platform::set_current_process_identity()?;

    let activation = arguments
        .iter()
        .find(|argument| argument.starts_with("quicknote-spike://"))
        .map(|argument| ActivationCommand::parse(argument))
        .transpose()?;

    // A second launch forwards its activation to the existing instance.
    let _single_instance = match acquire_single_instance("slint") {
        Ok(guard) => guard,
        Err(_) => {
            if let Some(command) = activation {
                let _ = send_pipe_request(
                    "slint",
                    &PipeRequest {
                        id: Some("activation-forward".to_owned()),
                        command: Some("activate".to_owned()),
                        value: Some(command.as_uri()),
                    },
                );
            }
            return Ok(());
        }
    };

    let store = Arc::new(Store::open()?);
    let status = SharedStatus::new("slint");
    let window = BenchmarkWindow::new()?;
    window.set_note_text(SharedString::from(store.load()?));

    let save_timer = Rc::new(Timer::default());
    let pending_text = Rc::new(RefCell::new(String::new()));
    {
        let store = store.clone();
        let save_timer = save_timer.clone();
        let pending_text = pending_text.clone();
        window.on_note_edited(move |body| {
            *pending_text.borrow_mut() = body.to_string();
            let store = store.clone();
            let pending_text = pending_text.clone();
            save_timer.start(
                TimerMode::SingleShot,
                Duration::from_millis(250),
                move || {
                    let _ = store.save(&pending_text.borrow());
                },
            );
        });
    }

    let weak = window.as_weak();
    let hotkey_status = status.clone();
    let hotkey = Arc::new(start_rebindable_global_hotkey(
        status.clone(),
        "Ctrl+Alt+Q",
        move || schedule_show(weak.clone(), hotkey_status.clone()),
    ));
    hotkey.wait_until_ready();

    let power_store = store.clone();
    let power_status = status.clone();
    let power = start_power_resume_listener(move || scan_overdue(&power_store, &power_status))?;

    {
        let store = store.clone();
        let weak = window.as_weak();
        window.on_schedule_reminder(move || {
            let message = match store.schedule_test_reminder(10) {
                Ok(reminder) => match platform::schedule_reminder(&reminder) {
                    Ok(()) => format!("测试提醒已安排：10 秒后，ID {}", reminder.id),
                    Err(error) => format!("计划通知失败：{error}"),
                },
                Err(error) => format!("保存提醒失败：{error}"),
            };
            if let Some(window) = weak.upgrade() {
                window.set_status_text(message.into());
            }
        });
    }

    {
        let hotkey = hotkey.clone();
        let weak = window.as_weak();
        window.on_rebind_hotkey(move || {
            let message = match hotkey.rebind("Ctrl+Shift+Q") {
                Ok(()) => "正在切换为 Ctrl+Shift+Q".to_owned(),
                Err(error) => format!("快捷键切换失败：{error}"),
            };
            if let Some(window) = weak.upgrade() {
                window.set_status_text(message.into());
            }
        });
    }

    let pipe_weak = window.as_weak();
    let pipe_status = status.clone();
    let pipe_store = store.clone();
    let pipe_hotkey = hotkey.clone();
    let pipe_power = power.clone();
    start_pipe_server("slint", move |request| {
        handle_pipe(
            pipe_weak.clone(),
            pipe_status.clone(),
            pipe_store.clone(),
            pipe_hotkey.clone(),
            pipe_power.clone(),
            request,
        )
    });

    // Keep tray resources alive for the duration of the event loop. tray-icon handles
    // TaskbarCreated and re-registers after Explorer restarts.
    let menu = Menu::new();
    let show_item = MenuItem::new("显示", true, None);
    let exit_item = MenuItem::new("退出", true, None);
    let show_id = show_item.id().clone();
    let exit_id = exit_item.id().clone();
    menu.append_items(&[&show_item, &exit_item])?;
    let icon = benchmark_icon()?;
    let _tray = TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip("QuickNote Slint integration spike")
        .with_menu(Box::new(menu))
        .build()?;
    let tray_window = window.as_weak();
    let tray_status = status.clone();
    std::thread::spawn(move || {
        while let Ok(event) = MenuEvent::receiver().recv() {
            if event.id == show_id {
                schedule_show(tray_window.clone(), tray_status.clone());
            } else if event.id == exit_id {
                let _ = slint::invoke_from_event_loop(|| {
                    slint::quit_event_loop().ok();
                });
                break;
            }
        }
    });

    // Startup only initializes state; resume owns missed-reminder detection so a delivered
    // toast remains present in Windows notification history while the app is exited.

    window.show()?;
    status.mark_visible();
    mark_editor_ready(&window, &status);
    if let Some(command) = activation {
        apply_activation(&store, &status, &window.as_weak(), &command);
    }
    slint::run_event_loop_until_quit()?;

    save_timer.stop();
    store.save(window.get_note_text().as_str())?;
    Ok(())
}

fn schedule_show(window: Weak<BenchmarkWindow>, status: SharedStatus) {
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(window) = window.upgrade() {
            let _ = window.show();
            status.mark_visible();
            mark_editor_ready(&window, &status);
        }
    });
}

fn mark_editor_ready(window: &BenchmarkWindow, status: &SharedStatus) {
    window.invoke_focus_editor();
    // Mutate and restore the bound editor text so readiness proves editability.
    let original = window.get_note_text();
    let mut sentinel = original.to_string();
    sentinel.push('§');
    window.set_note_text(SharedString::from(sentinel));
    window.set_note_text(original);
    status.mark_ready();
}

fn apply_activation(
    store: &Arc<Store>,
    status: &SharedStatus,
    window: &Weak<BenchmarkWindow>,
    command: &ActivationCommand,
) {
    let result = store.apply_activation(command);
    status.mark_activation(&command.as_uri());
    let message = match result {
        Ok(true) => {
            // Keep the OS scheduler a projection of the SQLite reminder fact source.
            let platform_result: Result<(), String> = match command {
                ActivationCommand::Snooze { reminder_id, .. } => {
                    match store.reminder(*reminder_id) {
                        Ok(Some(reminder)) => platform::schedule_reminder(&reminder)
                            .map_err(|error| error.to_string()),
                        Ok(None) => Err("稍后提醒对应的领域记录不存在。".to_owned()),
                        Err(error) => Err(error.to_string()),
                    }
                }
                ActivationCommand::Archive { reminder_id, .. } => {
                    platform::cancel_reminder(*reminder_id).map_err(|error| error.to_string())
                }
                ActivationCommand::Open { .. } => Ok(()),
            };
            match platform_result {
                Ok(()) => format!("已应用通知动作：{}", command.as_uri()),
                Err(error) => format!("通知动作已保存，但平台同步失败：{error}"),
            }
        }
        Ok(false) => format!("重复通知动作已忽略：{}", command.as_uri()),
        Err(error) => format!("通知动作失败：{error}"),
    };
    if let Some(window) = window.upgrade() {
        window.set_status_text(message.into());
        if matches!(command, ActivationCommand::Open { .. }) {
            let _ = window.show();
            mark_editor_ready(&window, status);
        }
    }
}

fn scan_overdue(store: &Arc<Store>, status: &SharedStatus) {
    status.mark_resume_scan();
    // ADR-0001 treats a missed reminder as local state and forbids toast catch-up.
    let _ = store.scan_overdue_once();
}

fn handle_pipe(
    window: Weak<BenchmarkWindow>,
    status: SharedStatus,
    store: Arc<Store>,
    hotkey: Arc<HotkeyController>,
    power: PowerResumeController,
    request: PipeRequest,
) -> BenchmarkStatus {
    let command = request.command.as_deref().unwrap_or("status");
    let mut error = None;
    match command {
        "show" | "insert-sentinel" => schedule_show(window, status.clone()),
        "hide" => {
            let hide_store = store.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(window) = window.upgrade() {
                    let _ = hide_store.save(window.get_note_text().as_str());
                    let _ = window.hide();
                }
            });
        }
        "activate" => match request.value.as_deref().map(ActivationCommand::parse) {
            Some(Ok(activation)) => {
                let window_clone = window.clone();
                let status_clone = status.clone();
                let store_clone = store.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    apply_activation(&store_clone, &status_clone, &window_clone, &activation)
                });
            }
            Some(Err(message)) => error = Some(message),
            None => error = Some("Activation URI is missing.".to_owned()),
        },
        "schedule-reminder" => {
            let delay = request
                .value
                .as_deref()
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or(10);
            match store.schedule_test_reminder(delay) {
                Ok(reminder) => {
                    if let Err(platform_error) = platform::schedule_reminder(&reminder) {
                        error = Some(platform_error.to_string());
                    }
                }
                Err(store_error) => error = Some(store_error.to_string()),
            }
        }
        "scheduled-count" => {
            if let Err(platform_error) = platform::scheduled_count() {
                error = Some(platform_error.to_string());
            }
        }
        "history-count" => {
            if let Err(platform_error) = platform::history_count() {
                error = Some(platform_error.to_string());
            }
        }
        "clear-history" => {
            if let Err(platform_error) = platform::clear_history() {
                error = Some(platform_error.to_string());
            }
        }
        "simulate-resume" => {
            if let Err(power_error) = power.simulate_resume() {
                error = Some(power_error);
            }
        }
        "rebind-hotkey" => {
            let spec = request.value.as_deref().unwrap_or("Ctrl+Shift+Q");
            if let Err(hotkey_error) = hotkey.rebind(spec) {
                error = Some(hotkey_error);
            }
        }
        "shutdown" => {
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(window) = window.upgrade() {
                    let _ = window.hide();
                }
                slint::quit_event_loop().ok();
            });
        }
        "status" => {}
        _ => error = Some(format!("Unknown command: {command}")),
    }
    let mut response = status.snapshot(
        request.id,
        if command == "status" {
            "status"
        } else {
            "editor-focused"
        },
    );
    if let Some(message) = error {
        response.ok = false;
        response.error = Some(message);
    }
    if let Ok(count) = platform::scheduled_count() {
        response.scheduled_notification_count = Some(count);
    }
    if let Ok(count) = platform::history_count() {
        response.notification_history_count = Some(count);
    }
    if let Ok(Some(reminder)) = store.reminder(1) {
        response.reminder_status = Some(reminder.status);
        response.reminder_due_at = Some(reminder.due_at);
        response.reminder_catch_up_at = reminder.catch_up_at;
        response.reminder_last_action = reminder.last_action;
    }
    response
}

fn benchmark_icon() -> Result<Icon, tray_icon::BadIcon> {
    const SIZE: u32 = 32;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let paper = (5..27).contains(&x) && (3..29).contains(&y);
            let line = paper && (8..24).contains(&x) && matches!(y, 10 | 15 | 20);
            let color = if line {
                [102, 93, 84, 255]
            } else if paper {
                [255, 250, 243, 255]
            } else {
                [233, 223, 210, 255]
            };
            rgba.extend_from_slice(&color);
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE)
}
