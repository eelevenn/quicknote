use crate::core::ActivationCommand;
use rusqlite::{Connection, OptionalExtension, params};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Store {
    connection: Mutex<Connection>,
}

/// Observable reminder state used by the native adapter and verification harness.
#[derive(Clone, Debug)]
pub struct ReminderRecord {
    pub id: i64,
    pub note_id: i64,
    pub due_at: i64,
    pub status: String,
    pub catch_up_at: Option<i64>,
    pub last_action: Option<String>,
}

impl Store {
    /// Opens the isolated spike database and applies its minimal schema.
    pub fn open() -> Result<Self, Box<dyn std::error::Error>> {
        let data_directory = std::env::var_os("QUICKNOTE_BENCH_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("LOCALAPPDATA")
                    .map(PathBuf::from)
                    .unwrap_or_else(std::env::temp_dir)
                    .join("QuickNote")
                    .join("SlintIntegrationSpike")
            });
        fs::create_dir_all(&data_directory)?;
        let connection = Connection::open(data_directory.join("quicknote.db"))?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS notes (
                 id INTEGER PRIMARY KEY CHECK (id = 1),
                 body TEXT NOT NULL,
                 archived_at INTEGER,
                 updated_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS reminders (
                 id INTEGER PRIMARY KEY,
                 note_id INTEGER NOT NULL,
                 due_at INTEGER NOT NULL,
                 status TEXT NOT NULL,
                 catch_up_at INTEGER,
                 last_action TEXT,
                 last_action_at INTEGER
             );
             CREATE TABLE IF NOT EXISTS activation_receipts (
                 activation_key TEXT PRIMARY KEY,
                 applied_at INTEGER NOT NULL
             );",
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

    pub fn load(&self) -> Result<String, rusqlite::Error> {
        self.connection
            .lock()
            .expect("Slint SQLite mutex poisoned")
            .query_row("SELECT body FROM notes WHERE id = 1", [], |row| row.get(0))
    }

    pub fn save(&self, body: &str) -> Result<(), rusqlite::Error> {
        let mut connection = self.connection.lock().expect("Slint SQLite mutex poisoned");
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE notes SET body = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = 1",
            params![body],
        )?;
        transaction.commit()
    }

    pub fn schedule_test_reminder(
        &self,
        delay_seconds: i64,
    ) -> Result<ReminderRecord, rusqlite::Error> {
        let due_at = now_unix() + delay_seconds.max(1);
        let mut connection = self.connection.lock().expect("Slint SQLite mutex poisoned");
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO reminders (id, note_id, due_at, status, catch_up_at, last_action, last_action_at)
             VALUES (1, 1, ?1, 'scheduled', NULL, NULL, NULL)
             ON CONFLICT(id) DO UPDATE SET due_at=excluded.due_at, status='scheduled', catch_up_at=NULL, last_action=NULL, last_action_at=NULL",
            params![due_at],
        )?;
        transaction.commit()?;
        Ok(ReminderRecord {
            id: 1,
            note_id: 1,
            due_at,
            status: "scheduled".to_owned(),
            catch_up_at: None,
            last_action: None,
        })
    }

    /// Applies an activation once per delivery URI, making duplicate launches harmless.
    pub fn apply_activation(&self, command: &ActivationCommand) -> Result<bool, rusqlite::Error> {
        let now = now_unix();
        let activation_key = command.as_uri();
        let mut connection = self.connection.lock().expect("Slint SQLite mutex poisoned");
        let transaction = connection.transaction()?;
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO activation_receipts (activation_key, applied_at) VALUES (?1, ?2)",
            params![activation_key, now],
        )?;
        if inserted == 0 {
            transaction.commit()?;
            return Ok(false);
        }
        match command {
            ActivationCommand::Open { reminder_id, .. } => {
                transaction.execute(
                    "UPDATE reminders SET status='responded', last_action='open', last_action_at=?1 WHERE id=?2",
                    params![now, reminder_id],
                )?;
            }
            ActivationCommand::Snooze { reminder_id, .. } => {
                transaction.execute(
                    "UPDATE reminders SET due_at=?1, status='scheduled', catch_up_at=NULL, last_action='snooze', last_action_at=?2 WHERE id=?3",
                    params![now + 300, now, reminder_id],
                )?;
            }
            ActivationCommand::Archive { reminder_id, .. } => {
                transaction.execute("UPDATE notes SET archived_at=?1 WHERE id=1", params![now])?;
                transaction.execute(
                    "UPDATE reminders SET status='cancelled', last_action='archive', last_action_at=?1 WHERE id=?2",
                    params![now, reminder_id],
                )?;
            }
        }
        transaction.commit()?;
        Ok(true)
    }

    pub fn scan_overdue_once(&self) -> Result<Vec<ReminderRecord>, rusqlite::Error> {
        let now = now_unix();
        let mut connection = self.connection.lock().expect("Slint SQLite mutex poisoned");
        let transaction = connection.transaction()?;
        let rows = {
            let mut statement = transaction.prepare(
                "SELECT id, note_id, due_at, status, catch_up_at, last_action
                 FROM reminders WHERE due_at <= ?1 AND status='scheduled' AND catch_up_at IS NULL",
            )?;
            statement
                .query_map(params![now], |row| {
                    Ok(ReminderRecord {
                        id: row.get(0)?,
                        note_id: row.get(1)?,
                        due_at: row.get(2)?,
                        status: row.get(3)?,
                        catch_up_at: row.get(4)?,
                        last_action: row.get(5)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        for reminder in &rows {
            transaction.execute(
                "UPDATE reminders SET catch_up_at=?1 WHERE id=?2 AND catch_up_at IS NULL",
                params![now, reminder.id],
            )?;
        }
        transaction.commit()?;
        Ok(rows)
    }

    pub fn reminder(&self, id: i64) -> Result<Option<ReminderRecord>, rusqlite::Error> {
        self.connection
            .lock()
            .expect("Slint SQLite mutex poisoned")
            .query_row(
                "SELECT id, note_id, due_at, status, catch_up_at, last_action FROM reminders WHERE id=?1",
                params![id],
                |row| {
                    Ok(ReminderRecord {
                        id: row.get(0)?,
                        note_id: row.get(1)?,
                        due_at: row.get(2)?,
                        status: row.get(3)?,
                        catch_up_at: row.get(4)?,
                        last_action: row.get(5)?,
                    })
                },
            )
            .optional()
    }
}

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn build_seed(fixture: Option<&std::ffi::OsStr>) -> String {
    let source = fixture
        .map(Path::new)
        .and_then(|path| fs::read_to_string(path).ok())
        .unwrap_or_else(|| {
            "QuickNote Slint integration spike · 中文输入 · SQLite autosave\n".to_owned()
        });
    let mut seed = source.clone();
    while seed.len() < 8 * 1024 {
        seed.push('\n');
        seed.push_str(&source);
    }
    seed
}
