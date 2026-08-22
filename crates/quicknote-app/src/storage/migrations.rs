use crate::ApplicationError;
use rusqlite::{Connection, MAIN_DB, TransactionBehavior};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// ASCII `QN01`，作为所有生产数据库的固定 SQLite 身份。
pub(crate) const APPLICATION_ID: i32 = 0x514E_3031;
/// 当前客户端能够完整读写的最高 schema 版本。
pub(crate) const SUPPORTED_SCHEMA_VERSION: i32 = 4;

/// 仅由模块内测试使用的确定性迁移故障点。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MigrationFault {
    None,
    AfterSchema,
}

pub(crate) fn open_database(
    data_directory: &Path,
    fault: MigrationFault,
) -> Result<Connection, ApplicationError> {
    fs::create_dir_all(data_directory)
        .map_err(|error| ApplicationError::storage("create_data_directory", error))?;
    let database_path = data_directory.join("quicknote.db");
    let had_database = fs::metadata(&database_path)
        .map(|metadata| metadata.len() > 0)
        .unwrap_or(false);
    let mut connection = Connection::open(&database_path)
        .map_err(|error| ApplicationError::storage("open_database", error))?;

    let found_id: i32 = connection
        .pragma_query_value(None, "application_id", |row| row.get(0))
        .map_err(|error| ApplicationError::storage("read_application_id", error))?;
    let found_version: i32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| ApplicationError::storage("read_schema_version", error))?;
    let has_user_schema: bool = connection
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_schema
                WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
            )",
            [],
            |row| row.get(0),
        )
        .map_err(|error| ApplicationError::storage("inspect_database_identity", error))?;

    if found_id != 0 && found_id != APPLICATION_ID {
        return Err(ApplicationError::DatabaseIdentity {
            expected: APPLICATION_ID,
            found: found_id,
        });
    }
    if found_id == 0 && has_user_schema {
        return Err(ApplicationError::DatabaseIdentity {
            expected: APPLICATION_ID,
            found: found_id,
        });
    }
    if found_version > SUPPORTED_SCHEMA_VERSION {
        return Err(ApplicationError::UnsupportedSchema {
            found: found_version,
            supported: SUPPORTED_SCHEMA_VERSION,
        });
    }

    if found_version < SUPPORTED_SCHEMA_VERSION {
        let backup_path = if had_database {
            Some(create_migration_backup(
                &connection,
                data_directory,
                found_version,
            )?)
        } else {
            None
        };
        if let Err(message) = migrate(&mut connection, found_version, fault) {
            return Err(ApplicationError::Migration {
                from: found_version,
                to: SUPPORTED_SCHEMA_VERSION,
                backup_path,
                message,
            });
        }
    }

    // 每次打开都显式设置连接语义，不依赖 SQLite 或库的默认值。
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )
        .map_err(|error| ApplicationError::storage("configure_database", error))?;
    Ok(connection)
}

fn create_migration_backup(
    connection: &Connection,
    data_directory: &Path,
    from_version: i32,
) -> Result<PathBuf, ApplicationError> {
    let backup_directory = data_directory.join("backups");
    fs::create_dir_all(&backup_directory).map_err(|error| ApplicationError::MigrationBackup {
        message: error.to_string(),
    })?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let stem = format!(
        "pre-migration-v{from_version}-{timestamp}-{}",
        std::process::id()
    );
    let temporary_path = backup_directory.join(format!(".{stem}.tmp"));
    let published_path = backup_directory.join(format!("{stem}.db"));

    let backup_result = (|| -> Result<(), Box<dyn std::error::Error>> {
        // Online Backup API 会把 WAL 中的已提交页纳入一致快照。
        connection.backup(MAIN_DB, &temporary_path, None)?;
        let verification = Connection::open(&temporary_path)?;
        let quick_check: String =
            verification.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        if quick_check != "ok" {
            return Err(format!("备份 quick_check 返回 {quick_check}").into());
        }
        drop(verification);
        fs::rename(&temporary_path, &published_path)?;
        Ok(())
    })();

    if let Err(error) = backup_result {
        // 该路径由本函数唯一创建，失败时删除不会影响用户文件。
        let _ = fs::remove_file(&temporary_path);
        return Err(ApplicationError::MigrationBackup {
            message: error.to_string(),
        });
    }
    Ok(published_path)
}

fn migrate(
    connection: &mut Connection,
    from_version: i32,
    fault: MigrationFault,
) -> Result<(), String> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;

    if from_version < 1 {
        transaction
            .execute_batch(SCHEMA_V1)
            .map_err(|error| error.to_string())?;
        if fault == MigrationFault::AfterSchema {
            return Err("已注入 schema 创建后的迁移失败".to_owned());
        }
    }
    if from_version < 2 {
        transaction
            .execute_batch(SCHEMA_V2)
            .map_err(|error| error.to_string())?;
    }
    if from_version < 3 {
        transaction
            .execute_batch(SCHEMA_V3)
            .map_err(|error| error.to_string())?;
    }
    if from_version < 4 {
        transaction
            .execute_batch(SCHEMA_V4)
            .map_err(|error| error.to_string())?;
    }

    transaction
        .pragma_update(None, "application_id", APPLICATION_ID)
        .map_err(|error| error.to_string())?;
    transaction
        .pragma_update(None, "user_version", SUPPORTED_SCHEMA_VERSION)
        .map_err(|error| error.to_string())?;
    transaction.commit().map_err(|error| error.to_string())
}

const SCHEMA_V1: &str = r#"
-- 便签正文是派生标题的事实源；生命周期时间由 CHECK 保持一致。
CREATE TABLE notes (
    id BLOB PRIMARY KEY NOT NULL
        CHECK(length(id) = 16)
        CHECK(substr(id, 7, 1) >= x'70' AND substr(id, 7, 1) <= x'7f')
        CHECK(substr(id, 9, 1) >= x'80' AND substr(id, 9, 1) <= x'bf'),
    body TEXT NOT NULL,
    derived_title TEXT NOT NULL,
    content_revision INTEGER NOT NULL CHECK(content_revision >= 1),
    lifecycle TEXT NOT NULL CHECK(lifecycle IN ('active', 'archived', 'trashed')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    archived_at_ms INTEGER,
    trashed_at_ms INTEGER,
    due_at_ms INTEGER,
    CHECK(updated_at_ms >= created_at_ms),
    CHECK(
        (lifecycle = 'active' AND archived_at_ms IS NULL AND trashed_at_ms IS NULL) OR
        (lifecycle = 'archived' AND archived_at_ms IS NOT NULL AND trashed_at_ms IS NULL) OR
        (lifecycle = 'trashed' AND archived_at_ms IS NOT NULL AND trashed_at_ms IS NOT NULL)
    )
) STRICT;

-- 固定 singleton 键保证当前便签为零或一行。
CREATE TABLE current_note (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    note_id BLOB NOT NULL UNIQUE CHECK(length(note_id) = 16),
    FOREIGN KEY(note_id) REFERENCES notes(id) ON DELETE RESTRICT
) STRICT;

-- 每张活跃便签最多持有一个提醒。
CREATE TABLE reminders (
    id BLOB PRIMARY KEY NOT NULL
        CHECK(length(id) = 16)
        CHECK(substr(id, 7, 1) >= x'70' AND substr(id, 7, 1) <= x'7f')
        CHECK(substr(id, 9, 1) >= x'80' AND substr(id, 9, 1) <= x'bf'),
    note_id BLOB NOT NULL UNIQUE CHECK(length(note_id) = 16),
    scheduled_at_ms INTEGER NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('scheduled', 'missed')),
    trigger_version INTEGER NOT NULL CHECK(trigger_version >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    FOREIGN KEY(note_id) REFERENCES notes(id) ON DELETE RESTRICT
) STRICT;

-- Outbox 保留领域提交后的平台投影意图，不依赖提醒行继续存在。
CREATE TABLE reminder_outbox (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    reminder_id BLOB NOT NULL
        CHECK(length(reminder_id) = 16)
        CHECK(substr(reminder_id, 7, 1) >= x'70' AND substr(reminder_id, 7, 1) <= x'7f'),
    trigger_version INTEGER NOT NULL CHECK(trigger_version >= 1),
    projection_kind TEXT NOT NULL CHECK(projection_kind IN ('schedule', 'cancel')),
    payload_json TEXT NOT NULL CHECK(json_valid(payload_json)),
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
    next_attempt_at_ms INTEGER NOT NULL,
    last_error TEXT,
    delivered_at_ms INTEGER,
    UNIQUE(reminder_id, trigger_version, projection_kind)
) STRICT;

-- 收据让重复或过时的通知动作在领域事务中幂等退出。
CREATE TABLE activation_receipts (
    id BLOB PRIMARY KEY NOT NULL
        CHECK(length(id) = 16)
        CHECK(substr(id, 7, 1) >= x'70' AND substr(id, 7, 1) <= x'7f'),
    activation_key TEXT NOT NULL UNIQUE,
    reminder_id BLOB NOT NULL CHECK(length(reminder_id) = 16),
    trigger_version INTEGER NOT NULL CHECK(trigger_version >= 1),
    action TEXT NOT NULL CHECK(action IN ('open', 'snooze')),
    received_at_ms INTEGER NOT NULL
) STRICT;

-- 设置只保存平台中立的用户语义。
CREATE TABLE settings (
    key TEXT PRIMARY KEY NOT NULL,
    value_json TEXT NOT NULL CHECK(json_valid(value_json)),
    updated_at_ms INTEGER NOT NULL
) STRICT;

-- 维护游标与用户设置分离，避免形成宽泛 app_state 表。
CREATE TABLE maintenance_state (
    key TEXT PRIMARY KEY NOT NULL,
    value_json TEXT NOT NULL CHECK(json_valid(value_json)),
    updated_at_ms INTEGER NOT NULL
) STRICT;

CREATE INDEX notes_active_successor
    ON notes(updated_at_ms DESC, id) WHERE lifecycle = 'active';
CREATE INDEX notes_trash_expiry
    ON notes(trashed_at_ms, id) WHERE lifecycle = 'trashed';
CREATE INDEX reminders_due
    ON reminders(scheduled_at_ms, id) WHERE status = 'scheduled';
CREATE INDEX reminder_outbox_pending
    ON reminder_outbox(next_attempt_at_ms, id) WHERE delivered_at_ms IS NULL;

-- 行级触发器阻止当前指针指向非活跃便签。
CREATE TRIGGER current_note_requires_active_insert
BEFORE INSERT ON current_note
BEGIN
    SELECT CASE WHEN NOT EXISTS(
        SELECT 1 FROM notes WHERE id = NEW.note_id AND lifecycle = 'active'
    ) THEN RAISE(ABORT, 'current_note must reference an active note') END;
END;

CREATE TRIGGER current_note_requires_active_update
BEFORE UPDATE OF note_id ON current_note
BEGIN
    SELECT CASE WHEN NOT EXISTS(
        SELECT 1 FROM notes WHERE id = NEW.note_id AND lifecycle = 'active'
    ) THEN RAISE(ABORT, 'current_note must reference an active note') END;
END;

-- 行级触发器阻止非活跃便签持有提醒。
CREATE TRIGGER reminder_requires_active_insert
BEFORE INSERT ON reminders
BEGIN
    SELECT CASE WHEN NOT EXISTS(
        SELECT 1 FROM notes WHERE id = NEW.note_id AND lifecycle = 'active'
    ) THEN RAISE(ABORT, 'reminder must reference an active note') END;
END;

CREATE TRIGGER reminder_requires_active_update
BEFORE UPDATE OF note_id ON reminders
BEGIN
    SELECT CASE WHEN NOT EXISTS(
        SELECT 1 FROM notes WHERE id = NEW.note_id AND lifecycle = 'active'
    ) THEN RAISE(ABORT, 'reminder must reference an active note') END;
END;

-- 生命周期离开活跃前必须在同一事务中先清除当前指针和提醒。
CREATE TRIGGER non_active_note_has_no_current_or_reminder
BEFORE UPDATE OF lifecycle ON notes
WHEN NEW.lifecycle <> 'active'
BEGIN
    SELECT CASE WHEN EXISTS(
        SELECT 1 FROM current_note WHERE note_id = NEW.id
    ) OR EXISTS(
        SELECT 1 FROM reminders WHERE note_id = NEW.id
    ) THEN RAISE(ABORT, 'non-active note cannot be current or hold a reminder') END;
END;

INSERT INTO settings(key, value_json, updated_at_ms)
VALUES ('default_snooze_minutes', '10', unixepoch('subsec') * 1000);
"#;

const SCHEMA_V2: &str = r#"
-- 快捷键保存平台中立规范串；Windows 注册是否成功仍由 adapter 决定。
INSERT OR IGNORE INTO settings(key, value_json, updated_at_ms)
VALUES ('global_shortcut', '"Ctrl+Alt+Q"', unixepoch('subsec') * 1000);
"#;

const SCHEMA_V3: &str = r#"
-- 自启动是用户语义设置；平台注册状态仍由 adapter 投影。
INSERT OR IGNORE INTO settings(key, value_json, updated_at_ms)
VALUES ('startup_enabled', 'false', unixepoch('subsec') * 1000);

-- 自动备份历史通过外键跟随永久清除；正文备份策略可独立演进。
CREATE TABLE IF NOT EXISTS note_backup_history (
    note_id BLOB NOT NULL CHECK(length(note_id) = 16),
    content_revision INTEGER NOT NULL CHECK(content_revision >= 1),
    body TEXT NOT NULL,
    captured_at_ms INTEGER NOT NULL,
    PRIMARY KEY(note_id, content_revision),
    FOREIGN KEY(note_id) REFERENCES notes(id) ON DELETE CASCADE
) STRICT;
"#;

const SCHEMA_V4: &str = r#"
-- 外部内容 FTS5 trigram 索引保留 notes 作为唯一正文事实源，并加速文字子串搜索。
CREATE VIRTUAL TABLE note_search USING fts5(
    derived_title,
    body,
    content='notes',
    content_rowid='rowid',
    tokenize='trigram'
);

CREATE TRIGGER note_search_after_insert
AFTER INSERT ON notes
BEGIN
    INSERT INTO note_search(rowid, derived_title, body)
    VALUES (NEW.rowid, NEW.derived_title, NEW.body);
END;

CREATE TRIGGER note_search_after_delete
AFTER DELETE ON notes
BEGIN
    INSERT INTO note_search(note_search, rowid, derived_title, body)
    VALUES ('delete', OLD.rowid, OLD.derived_title, OLD.body);
END;

CREATE TRIGGER note_search_after_content_update
AFTER UPDATE OF derived_title, body ON notes
BEGIN
    INSERT INTO note_search(note_search, rowid, derived_title, body)
    VALUES ('delete', OLD.rowid, OLD.derived_title, OLD.body);
    INSERT INTO note_search(rowid, derived_title, body)
    VALUES (NEW.rowid, NEW.derived_title, NEW.body);
END;

-- 迁移既有正文；后续变更由同一事务内的触发器同步。
INSERT INTO note_search(note_search) VALUES ('rebuild');
"#;
