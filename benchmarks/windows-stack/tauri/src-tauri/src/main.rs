#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use quicknote_benchmark_support::{
    BenchmarkStatus, PipeRequest, SharedStatus, acquire_single_instance, start_global_hotkey,
    start_pipe_server,
};
use rusqlite::{Connection, params};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, State, WebviewWindow};

struct AppState {
    connection: Mutex<Connection>,
    status: SharedStatus,
}

#[tauri::command]
fn load_note(state: State<'_, AppState>) -> Result<String, String> {
    state
        .connection
        .lock()
        .map_err(|error| error.to_string())?
        .query_row("SELECT body FROM notes WHERE id = 1", [], |row| row.get(0))
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn save_note(body: String, state: State<'_, AppState>) -> Result<(), String> {
    let mut connection = state.connection.lock().map_err(|error| error.to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "INSERT INTO notes (id, body, updated_at) VALUES (1, ?1, strftime('%Y-%m-%dT%H:%M:%fZ','now')) ON CONFLICT(id) DO UPDATE SET body=excluded.body, updated_at=excluded.updated_at",
            params![body],
        )
        .map_err(|error| error.to_string())?;
    transaction.commit().map_err(|error| error.to_string())
}

#[tauri::command]
fn editor_ready(state: State<'_, AppState>) {
    state.status.mark_ready();
}

#[tauri::command]
fn exit_app(app: AppHandle) {
    app.exit(0);
}

fn main() {
    // A second launch exits cleanly while the first instance retains the global hotkey.
    let _single_instance = match acquire_single_instance("tauri") {
        Ok(guard) => guard,
        Err(_) => return,
    };
    let status = SharedStatus::new("tauri");
    let connection = open_store().expect("initialize Tauri benchmark SQLite store");
    let shared_state = AppState {
        connection: Mutex::new(connection),
        status: status.clone(),
    };

    tauri::Builder::default()
        .manage(shared_state)
        .invoke_handler(tauri::generate_handler![
            load_note,
            save_note,
            editor_ready,
            exit_app
        ])
        .setup(move |app| {
            let show_item = MenuItem::with_id(app, "show", "显示", true, None::<&str>)?;
            let exit_item = MenuItem::with_id(app, "exit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_item, &exit_item])?;
            let _tray = TrayIconBuilder::new()
                .icon(
                    app.default_window_icon()
                        .expect("Tauri default icon")
                        .clone(),
                )
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_window(app.get_webview_window("main")),
                    "exit" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.eval("window.benchmarkExit && window.benchmarkExit()");
                        }
                    }
                    _ => {}
                })
                .build(app)?;

            let app_handle = app.handle().clone();
            let hotkey_status = status.clone();
            start_global_hotkey(status.clone(), move || {
                show_window(app_handle.get_webview_window("main"));
                hotkey_status.mark_visible();
            });

            let pipe_handle = app.handle().clone();
            let pipe_status = status.clone();
            start_pipe_server("tauri", move |request| {
                handle_pipe(&pipe_handle, &pipe_status, request)
            });
            if let Some(window) = app.get_webview_window("main") {
                status.mark_visible();
                let _ = window.set_focus();
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("run Tauri benchmark prototype");
}

fn handle_pipe(app: &AppHandle, status: &SharedStatus, request: PipeRequest) -> BenchmarkStatus {
    let command = request.command.as_deref().unwrap_or("status");
    match command {
        "show" => {
            show_window(app.get_webview_window("main"));
            status.mark_visible();
        }
        "hide" => {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.eval("window.benchmarkHide && window.benchmarkHide()");
            }
        }
        "insert-sentinel" => show_window(app.get_webview_window("main")),
        "shutdown" => {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.eval("window.benchmarkExit && window.benchmarkExit()");
            } else {
                app.exit(0);
            }
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

fn show_window(window: Option<WebviewWindow>) {
    if let Some(window) = window {
        let _ = window.show();
        let _ = window.set_focus();
        let _ = window.eval("window.benchmarkReady && window.benchmarkReady()");
    }
}

fn open_store() -> Result<Connection, Box<dyn std::error::Error>> {
    let data_directory = std::env::var_os("QUICKNOTE_BENCH_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir()
                .join("QuickNoteStackBenchmark")
                .join("tauri")
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
    Ok(connection)
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
