#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use quicknote_benchmark_support::{
    BenchmarkStatus, PipeRequest, SharedStatus, acquire_single_instance, start_global_hotkey,
    start_pipe_server,
};
use rusqlite::{Connection, params};
use slint::{ComponentHandle, SharedString, Timer, TimerMode, Weak};
use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tray_icon::{
    Icon, TrayIconBuilder,
    menu::{Menu, MenuEvent, MenuItem},
};

slint::include_modules!();

struct Store {
    connection: Mutex<Connection>,
}

impl Store {
    fn open() -> Result<Self, Box<dyn std::error::Error>> {
        let data_directory = std::env::var_os("QUICKNOTE_BENCH_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::temp_dir()
                    .join("QuickNoteStackBenchmark")
                    .join("slint")
            });
        fs::create_dir_all(&data_directory)?;
        let connection = Connection::open(data_directory.join("quicknote.db"))?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY CHECK (id = 1), body TEXT NOT NULL, updated_at TEXT NOT NULL);",
        )?;
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM notes WHERE id = 1", [], |row| {
                row.get(0)
            })?;
        if count == 0 {
            connection.execute(
                "INSERT INTO notes (id, body, updated_at) VALUES (1, ?1, strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
                params![build_seed(std::env::var_os("QUICKNOTE_BENCH_FIXTURE").as_deref())],
            )?;
        }
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn load(&self) -> Result<String, rusqlite::Error> {
        self.connection
            .lock()
            .expect("Slint SQLite mutex poisoned")
            .query_row("SELECT body FROM notes WHERE id = 1", [], |row| row.get(0))
    }

    fn save(&self, body: &str) -> Result<(), rusqlite::Error> {
        let mut connection = self.connection.lock().expect("Slint SQLite mutex poisoned");
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO notes (id, body, updated_at) VALUES (1, ?1, strftime('%Y-%m-%dT%H:%M:%fZ','now')) ON CONFLICT(id) DO UPDATE SET body=excluded.body, updated_at=excluded.updated_at",
            params![body],
        )?;
        transaction.commit()
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A second launch exits cleanly while the first instance retains the global hotkey.
    let _single_instance = match acquire_single_instance("slint") {
        Ok(guard) => guard,
        Err(_) => return Ok(()),
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
    start_global_hotkey(status.clone(), move || {
        schedule_show(weak.clone(), hotkey_status.clone())
    });

    let pipe_weak = window.as_weak();
    let pipe_status = status.clone();
    let pipe_store = store.clone();
    start_pipe_server("slint", move |request| {
        handle_pipe(
            pipe_weak.clone(),
            pipe_status.clone(),
            pipe_store.clone(),
            request,
        )
    });

    // Keep tray resources alive for the duration of the event loop.
    let menu = Menu::new();
    let show_item = MenuItem::new("显示", true, None);
    let exit_item = MenuItem::new("退出", true, None);
    let show_id = show_item.id().clone();
    let exit_id = exit_item.id().clone();
    menu.append_items(&[&show_item, &exit_item])?;
    let icon = benchmark_icon()?;
    let _tray = TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip("QuickNote Slint benchmark")
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

    window.show()?;
    status.mark_visible();
    mark_editor_ready(&window, &status);
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

fn handle_pipe(
    window: Weak<BenchmarkWindow>,
    status: SharedStatus,
    store: Arc<Store>,
    request: PipeRequest,
) -> BenchmarkStatus {
    let command = request.command.as_deref().unwrap_or("status");
    match command {
        "show" | "insert-sentinel" => schedule_show(window, status.clone()),
        "hide" => {
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(window) = window.upgrade() {
                    let _ = store.save(window.get_note_text().as_str());
                    let _ = window.hide();
                }
            });
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
        _ => {
            let mut response = status.snapshot(request.id, "error");
            response.ok = false;
            response.error = Some(format!("Unknown command: {command}"));
            return response;
        }
    }
    status.snapshot(
        request.id,
        if command == "status" {
            "status"
        } else {
            "editor-focused"
        },
    )
}

fn build_seed(fixture: Option<&std::ffi::OsStr>) -> String {
    let source = fixture
        .map(Path::new)
        .and_then(|path| fs::read_to_string(path).ok())
        .unwrap_or_else(|| "QuickNote benchmark fixture · 中文输入 · SQLite autosave\n".to_owned());
    let mut seed = source.clone();
    while seed.len() < 8 * 1024 {
        seed.push('\n');
        seed.push_str(&source);
    }
    seed
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
