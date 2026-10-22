//! 用软件窗口派发真实键盘事件，验证快捷键不会吞掉正文或破坏选择。

use quicknote_ui::{MainWindow, QuickCaptureWindow};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Key, Platform, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalSize, SharedString};
use std::cell::Cell;
use std::rc::Rc;

/// 测试只需要声明式控件和输入分发，不打开操作系统窗口。
struct TestPlatform;

impl Platform for TestPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer))
    }
}

/// 按下与松开成对派发，覆盖 Slint 的修饰键状态机。
fn press(window: &slint::Window, text: impl Into<SharedString>) {
    let text = text.into();
    window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    window.dispatch_event(WindowEvent::KeyReleased { text });
    slint::platform::update_timers_and_animations();
}

#[test]
fn keyboard_capture_search_new_note_and_dismiss_preserve_text() {
    slint::platform::set_platform(Box::new(TestPlatform)).unwrap();
    let capture = QuickCaptureWindow::new().unwrap();
    capture.window().set_size(LogicalSize::new(540., 320.));
    capture.show().unwrap();
    capture.set_editor_body("中文正文".into());
    capture.invoke_focus_editor_at("中文正文".len() as i32);
    let dismissed = Rc::new(Cell::new(false));
    let signal = Rc::clone(&dismissed);
    capture.on_dismissed(move || signal.set(true));
    press(capture.window(), "追加");
    assert_eq!(capture.get_editor_body(), "中文正文追加");
    press(capture.window(), Key::Escape);
    assert!(dismissed.get());
    assert_eq!(capture.get_editor_body(), "中文正文追加");
    capture.hide().unwrap();

    let main = MainWindow::new().unwrap();
    main.window().set_size(LogicalSize::new(940., 650.));
    main.show().unwrap();
    main.invoke_focus_editor_at(0);
    slint::platform::update_timers_and_animations();
    let created = Rc::new(Cell::new(0));
    let count = Rc::clone(&created);
    main.on_new_note(move || count.set(count.get() + 1));
    main.window().dispatch_event(WindowEvent::KeyPressed {
        text: Key::Control.into(),
    });
    press(main.window(), "n");
    main.window().dispatch_event(WindowEvent::KeyReleased {
        text: Key::Control.into(),
    });
    assert_eq!(created.get(), 1);
    assert!(main.get_editor_body().is_empty());
    main.window().dispatch_event(WindowEvent::KeyPressed {
        text: Key::Control.into(),
    });
    press(main.window(), "f");
    main.window().dispatch_event(WindowEvent::KeyReleased {
        text: Key::Control.into(),
    });
    press(main.window(), "关键词");
    assert_eq!(main.get_search_text(), "关键词");
    assert!(main.get_editor_body().is_empty());
    main.set_confirm_action(2);
    press(main.window(), Key::Escape);
    assert_eq!(main.get_confirm_action(), 0);

    // 可选渲染证据直接来自 Slint，避免桌面锁定或遮挡影响截图。
    if let Ok(directory) = std::env::var("QUICKNOTE_UI_SNAPSHOTS") {
        use std::io::Write;
        std::fs::create_dir_all(&directory).unwrap();
        main.set_section(0);
        main.set_editor_body("MVP 验收便签\n\n中文记录、自动保存、归档与恢复。\n\n- 整理想法\n- 安排提醒\n- 导出便签".into());
        main.set_editor_title("MVP 验收便签".into());
        main.set_editor_note_id("preview".into());
        main.set_save_status("已自动保存".into());
        main.set_active_items(slint::ModelRc::new(slint::VecModel::from(vec![
            quicknote_ui::NoteListItem {
                id: "preview".into(),
                title: "MVP 验收便签".into(),
                is_current: true,
                metadata: "当前便签".into(),
            },
        ])));
        for (name, width, height, section) in [
            ("main", 940., 650., 0),
            ("compact", 420., 650., 0),
            ("settings", 940., 650., 3),
        ] {
            main.set_section(section);
            main.window().set_size(LogicalSize::new(width, height));
            slint::platform::update_timers_and_animations();
            let snapshot = main.window().take_snapshot().unwrap();
            let mut output =
                std::fs::File::create(std::path::Path::new(&directory).join(format!("{name}.ppm")))
                    .unwrap();
            write!(
                output,
                "P6\n{} {}\n255\n",
                snapshot.width(),
                snapshot.height()
            )
            .unwrap();
            let pixels = snapshot
                .as_slice()
                .iter()
                .flat_map(|pixel| [pixel.r, pixel.g, pixel.b])
                .collect::<Vec<_>>();
            output.write_all(&pixels).unwrap();
        }
    }
}
